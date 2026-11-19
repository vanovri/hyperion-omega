"""Production gRPC (:50153), FastAPI REST/Prometheus (:8084), & Redpanda Server.

Exposes:
  - gRPC Port 50153 (`hyperion.v1.FraudTelemetryService`):
      * InspectFraudSubgraph
      * ListRecentAlerts
      * StreamAnomalyAlerts
  - HTTP Port 8084:
      * GET  /healthz
      * GET  /metrics
      * GET  /api/v1/fraud/alerts
      * GET  /api/v1/fraud/subgraph/{account_id}
      * POST /api/v1/fraud/ingest/fill
      * POST /api/v1/fraud/ingest/l3-delta
"""

from __future__ import annotations

from collections.abc import Iterator
from concurrent import futures
import hmac
import os
from pathlib import Path
import threading
import time
from typing import Any

from fastapi import FastAPI, HTTPException, Query, Request, Response
import grpc
from hyperion_proto import telemetry_fraud_pb2, telemetry_fraud_pb2_grpc
from prometheus_client import CONTENT_TYPE_LATEST, Counter, Gauge, Histogram, generate_latest
from pydantic import BaseModel, Field
import uvicorn

from sentinel_ml.engine import FraudAlertRecord, SentinelDetectionEngine
from sentinel_ml.fixed_point import parse_decimal_to_fp
from sentinel_ml.graph_engine import SideLiteral
from sentinel_ml.protobuf_codec import (
    alert_to_proto,
    alert_to_rest_json,
    subgraph_report_to_proto,
    subgraph_to_rest_json,
)
from sentinel_ml.redpanda_bus import (
    TOPIC_EXECUTIONS,
    TOPIC_L3_DELTAS,
    TOPIC_MICROSTRUCTURE,
    SentinelRedpandaProcessor,
)

INFERENCE_LATENCY_HIST = Histogram(
    "sentinel_inference_latency_seconds",
    "End-to-end Sentinel-ML detection latency in seconds",
    buckets=(0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.0075, 0.01, 0.025),
)
ALERTS_COUNTER = Counter(
    "sentinel_alerts_emitted_total",
    "Total fraud and manipulation alerts emitted by Sentinel-ML",
    ["pattern_type", "symbol"],
)
EVENTS_COUNTER = Counter(
    "sentinel_events_processed_total",
    "Total market events processed by Sentinel-ML",
    ["event_type"],
)
GRAPH_EDGES_GAUGE = Gauge(
    "sentinel_graph_active_edges",
    "Current active trade edges in sliding temporal multigraph",
)


class FillIngestPayload(BaseModel):
    fill_id: int = Field(ge=0)
    engine_seq: int = Field(ge=0)
    symbol: str = Field(min_length=1)
    maker_account_id: int = Field(gt=0)
    taker_account_id: int = Field(gt=0)
    aggressor_side: SideLiteral = "BUY"
    price_fp: str | int
    qty_fp: str | int
    exec_ts_ns: int = Field(ge=0)
    maker_order_id: int = 0


class L3DeltaIngestPayload(BaseModel):
    op: str
    engine_seq: int = Field(ge=0)
    symbol: str = Field(min_length=1)
    order_id: int = Field(ge=0)
    account_id: int = Field(gt=0)
    side: SideLiteral = "SELL"
    price_fp: str | int
    qty_fp: str | int
    event_ts_ns: int = Field(ge=0)


def _coerce_fp_int(raw: str | int) -> int:
    if isinstance(raw, int) and not isinstance(raw, bool):
        return raw
    if isinstance(raw, str):
        s = raw.strip()
        return parse_decimal_to_fp(s) if "." in s else int(s)
    raise TypeError(f"Unsupported fixed-point value: {raw!r}")


class FraudTelemetryGrpcServicer(telemetry_fraud_pb2_grpc.FraudTelemetryServiceServicer):  # type: ignore[misc]
    """Canonical gRPC implementation of hyperion.v1.FraudTelemetryService."""

    def __init__(self, engine: SentinelDetectionEngine, lock: threading.RLock) -> None:
        self.engine = engine
        self.lock = lock

    def InspectFraudSubgraph(
        self,
        request: telemetry_fraud_pb2.InspectFraudSubgraphRequest,
        context: grpc.ServicerContext,
    ) -> telemetry_fraud_pb2.InspectFraudSubgraphResponse:
        with self.lock:
            report = self.engine.inspect_fraud_subgraph(
                center_account_id=int(request.center_account_id),
                symbol=str(request.symbol),
                max_hops=int(request.max_hops) if request.max_hops > 0 else 2,
                lookback_window_ns=int(request.lookback_window_ns),
            )
        return subgraph_report_to_proto(report)

    def ListRecentAlerts(
        self,
        request: telemetry_fraud_pb2.ListRecentAlertsRequest,
        context: grpc.ServicerContext,
    ) -> telemetry_fraud_pb2.ListRecentAlertsResponse:
        with self.lock:
            alerts = self.engine.list_recent_alerts(
                symbol=str(request.symbol),
                min_severity_score=int(request.min_severity_score),
                limit=int(request.limit) if request.limit > 0 else 50,
            )
        return telemetry_fraud_pb2.ListRecentAlertsResponse(
            alerts=[alert_to_proto(a) for a in alerts]
        )

    def StreamAnomalyAlerts(
        self,
        request: telemetry_fraud_pb2.StreamAnomalyAlertsRequest,
        context: grpc.ServicerContext,
    ) -> Iterator[telemetry_fraud_pb2.AnomalyAlert]:
        seen_ids: set[str] = set()
        with self.lock:
            initial = self.engine.list_recent_alerts(
                symbol=str(request.symbol),
                min_severity_score=int(request.min_severity_score),
                limit=50,
            )
        for alert in reversed(initial):
            seen_ids.add(alert.alert_id)
            yield alert_to_proto(alert)

        while context.is_active():
            time.sleep(0.05)
            with self.lock:
                latest = self.engine.list_recent_alerts(
                    symbol=str(request.symbol),
                    min_severity_score=int(request.min_severity_score),
                    limit=20,
                )
            for alert in reversed(latest):
                if alert.alert_id not in seen_ids:
                    seen_ids.add(alert.alert_id)
                    yield alert_to_proto(alert)


def create_http_app(
    engine: SentinelDetectionEngine, lock: threading.RLock | None = None
) -> FastAPI:
    """Create FastAPI HTTP/REST & Prometheus observability app on port 8084."""
    eff_lock = lock or threading.RLock()
    app = FastAPI(title="HYPERION Sentinel-ML", version="1.0.0")

    def _record_metrics(alerts: list[FraudAlertRecord]) -> None:
        GRAPH_EDGES_GAUGE.set(engine.graph.active_edge_count)
        for a in alerts:
            INFERENCE_LATENCY_HIST.observe(a.inference_latency_us * 1e-6)
            pat_name = telemetry_fraud_pb2.AnomalyPatternType.Name(a.pattern_type)
            ALERTS_COUNTER.labels(pattern_type=pat_name, symbol=a.symbol).inc()

    @app.get("/healthz")
    def healthz() -> dict[str, Any]:
        return {
            "status": "ok",
            "service": "sentinel-ml",
            "model_version": engine.runtime.model_version,
            "active_graph_edges": engine.graph.active_edge_count,
        }

    @app.get("/metrics")
    def metrics() -> Response:
        GRAPH_EDGES_GAUGE.set(engine.graph.active_edge_count)
        return Response(content=generate_latest(), media_type=CONTENT_TYPE_LATEST)

    @app.get("/api/v1/fraud/alerts")
    def get_alerts(
        symbol: str = Query(default=""),
        min_severity: int = Query(default=0, ge=0, le=100),
        limit: int = Query(default=50, ge=1, le=500),
    ) -> dict[str, Any]:
        with eff_lock:
            alerts = engine.list_recent_alerts(
                symbol=symbol, min_severity_score=min_severity, limit=limit
            )
        return {"alerts": [alert_to_rest_json(a) for a in alerts]}

    def _verify_internal_auth(request: Request) -> None:
        expected = os.environ.get("HYPERION_INTERNAL_AUTH_TOKEN", "").strip()
        env_mode = os.environ.get("HYPERION_ENV", "development").strip().lower()
        if not expected and env_mode == "production":
            raise HTTPException(
                status_code=401,
                detail="HYPERION_INTERNAL_AUTH_TOKEN must be configured in production",
            )
        if expected:
            provided = request.headers.get("x-hyperion-internal-token", "").strip()
            if not provided or not hmac.compare_digest(provided, expected):
                raise HTTPException(
                    status_code=401,
                    detail="Missing or invalid X-Hyperion-Internal-Token",
                )

    @app.get("/api/v1/fraud/subgraph/{account_id}")
    def get_subgraph(
        account_id: int,
        symbol: str = Query(default=""),
        max_hops: int = Query(default=2, ge=1, le=6),
        lookback_window_ns: int = Query(default=0, ge=0),
    ) -> dict[str, Any]:
        with eff_lock:
            report = engine.inspect_fraud_subgraph(
                center_account_id=account_id,
                symbol=symbol,
                max_hops=max_hops,
                lookback_window_ns=lookback_window_ns,
            )
        return subgraph_to_rest_json(report)

    @app.get("/api/v1/fraud/subgraph")
    def get_subgraph_query(
        center_account_id: int = Query(..., ge=1),
        symbol: str = Query(default=""),
        max_hops: int = Query(default=2, ge=1, le=6),
        lookback_window_ns: int = Query(default=0, ge=0),
    ) -> dict[str, Any]:
        with eff_lock:
            report = engine.inspect_fraud_subgraph(
                center_account_id=center_account_id,
                symbol=symbol,
                max_hops=max_hops,
                lookback_window_ns=lookback_window_ns,
            )
        return subgraph_to_rest_json(report)

    @app.post("/api/v1/fraud/ingest/fill")
    def post_fill(body: FillIngestPayload, request: Request) -> dict[str, Any]:
        _verify_internal_auth(request)
        price_fp = _coerce_fp_int(body.price_fp)
        qty_fp = _coerce_fp_int(body.qty_fp)
        t0 = time.perf_counter()
        with eff_lock:
            alerts = engine.process_execution_fill(
                fill_id=body.fill_id,
                engine_seq=body.engine_seq,
                symbol=body.symbol,
                maker_account_id=body.maker_account_id,
                taker_account_id=body.taker_account_id,
                aggressor_side=body.aggressor_side,
                price_fp=price_fp,
                qty_fp=qty_fp,
                exec_ts_ns=body.exec_ts_ns,
                maker_order_id=body.maker_order_id,
            )
        EVENTS_COUNTER.labels(event_type="execution_fill").inc()
        INFERENCE_LATENCY_HIST.observe(time.perf_counter() - t0)
        _record_metrics(alerts)
        return {"alerts": [alert_to_rest_json(a) for a in alerts]}

    @app.post("/api/v1/fraud/ingest/l3-delta")
    def post_l3_delta(body: L3DeltaIngestPayload, request: Request) -> dict[str, Any]:
        _verify_internal_auth(request)
        price_fp = _coerce_fp_int(body.price_fp)
        qty_fp = _coerce_fp_int(body.qty_fp)
        t0 = time.perf_counter()
        with eff_lock:
            maybe_alert = engine.process_l3_order_event(
                op=body.op,
                engine_seq=body.engine_seq,
                symbol=body.symbol,
                order_id=body.order_id,
                account_id=body.account_id,
                side=body.side,
                price_fp=price_fp,
                qty_fp=qty_fp,
                event_ts_ns=body.event_ts_ns,
            )
        EVENTS_COUNTER.labels(event_type="l3_delta").inc()
        INFERENCE_LATENCY_HIST.observe(time.perf_counter() - t0)
        alerts = [maybe_alert] if maybe_alert is not None else []
        _record_metrics(alerts)
        return {"alerts": [alert_to_rest_json(a) for a in alerts]}

    return app


def start_grpc_server(
    engine: SentinelDetectionEngine,
    lock: threading.RLock,
    port: int = 50153,
) -> grpc.Server:
    server = grpc.server(futures.ThreadPoolExecutor(max_workers=4))
    telemetry_fraud_pb2_grpc.add_FraudTelemetryServiceServicer_to_server(
        FraudTelemetryGrpcServicer(engine, lock), server
    )
    server.add_insecure_port(f"0.0.0.0:{port}")
    server.start()
    return server


def start_redpanda_background_worker(
    engine: SentinelDetectionEngine,
    lock: threading.RLock,
    brokers: str,
    stop_event: threading.Event,
) -> threading.Thread:
    """Start resilient background Redpanda consumer/producer daemon thread."""

    def _run() -> None:
        try:
            from confluent_kafka import Consumer, Producer
        except Exception:
            return

        group_id = os.environ.get("SENTINEL_CONSUMER_GROUP", "hyperion-sentinel-ml-v1")
        while not stop_event.is_set():
            try:
                producer = Producer({"bootstrap.servers": brokers, "linger.ms": 1})
                consumer = Consumer(
                    {
                        "bootstrap.servers": brokers,
                        "group.id": group_id,
                        "auto.offset.reset": "latest",
                        "enable.auto.commit": True,
                    }
                )
                consumer.subscribe([TOPIC_EXECUTIONS, TOPIC_L3_DELTAS, TOPIC_MICROSTRUCTURE])
                processor = SentinelRedpandaProcessor(engine, producer=producer)
                try:
                    while not stop_event.is_set():
                        msg = consumer.poll(0.05)
                        if msg is None or msg.error():
                            continue
                        raw_val = msg.value()
                        topic_name = msg.topic()
                        if not raw_val or not topic_name:
                            continue
                        with lock:
                            envelopes = processor.handle_record(topic_name, raw_val)
                        EVENTS_COUNTER.labels(event_type=topic_name).inc()
                        GRAPH_EDGES_GAUGE.set(engine.graph.active_edge_count)
                        raw_key = msg.key()
                        sym = raw_key.decode("utf-8", errors="ignore") if raw_key else "UNKNOWN"
                        for _ in envelopes:
                            ALERTS_COUNTER.labels(
                                pattern_type="REDPANDA_STREAM", symbol=sym
                            ).inc()
                finally:
                    consumer.close()
                    producer.flush(2.0)
            except Exception:
                time.sleep(2.0)

    t = threading.Thread(target=_run, name="sentinel-redpanda-worker", daemon=True)
    t.start()
    return t


def main() -> None:
    default_models = Path(__file__).resolve().parents[2] / "models"
    models_dir = Path(os.environ.get("SENTINEL_MODELS_DIR", str(default_models)))

    grpc_addr_env = os.environ.get("HYPERION_SENTINEL_ML_GRPC_ADDR", "")
    default_grpc_port = grpc_addr_env.rsplit(":", 1)[-1] if ":" in grpc_addr_env else "50153"
    grpc_port = int(os.environ.get("SENTINEL_GRPC_PORT", default_grpc_port))
    http_port = int(
        os.environ.get(
            "SENTINEL_HTTP_PORT",
            os.environ.get("HYPERION_SENTINEL_ML_HTTP_PORT", "8084"),
        )
    )

    lock = threading.RLock()
    stop_event = threading.Event()
    engine = SentinelDetectionEngine(models_dir, use_int8=True)
    grpc_srv = start_grpc_server(engine, lock, port=grpc_port)

    brokers = os.environ.get("HYPERION_REDPANDA_BROKERS", "").strip()
    if brokers:
        start_redpanda_background_worker(engine, lock, brokers, stop_event)

    app = create_http_app(engine, lock)
    try:
        uvicorn.run(app, host="0.0.0.0", port=http_port, log_level="info")
    finally:
        stop_event.set()
        grpc_srv.stop(grace=2.0)


if __name__ == "__main__":
    main()
