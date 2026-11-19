"""Unit, Property & Integration Tests for Protobuf v3, Redpanda Bus, gRPC & REST."""

from __future__ import annotations

from pathlib import Path
import threading

from fastapi.testclient import TestClient
import grpc
from hypothesis import given, settings, strategies as st
from hyperion_proto import (
    common_pb2,
    from_u128,
    orderbook_pb2,
    stream_analytics_pb2,
    telemetry_fraud_pb2,
    telemetry_fraud_pb2_grpc,
    to_u128,
)

from sentinel_ml.engine import SentinelDetectionEngine
from sentinel_ml.fixed_point import SCALE
from sentinel_ml.graph_engine import TradeExecutionEdge
from sentinel_ml.protobuf_codec import (
    deserialize_alert_proto,
    edge_to_proto,
    proto_to_edge,
)
from sentinel_ml.redpanda_bus import (
    TOPIC_EXECUTIONS,
    TOPIC_FRAUD_ALERTS,
    TOPIC_L3_DELTAS,
    TOPIC_MICROSTRUCTURE,
    SentinelRedpandaProcessor,
)
from sentinel_ml.server import create_http_app, start_grpc_server


@settings(max_examples=80, deadline=None)
@given(
    edge_id=st.integers(min_value=1, max_value=(1 << 63) - 1),
    fill_id=st.integers(min_value=1, max_value=(1 << 63) - 1),
    maker=st.integers(min_value=1, max_value=(1 << 63) - 1),
    taker=st.integers(min_value=1, max_value=(1 << 63) - 1),
    price_fp=st.integers(min_value=1, max_value=(1 << 60)),
    qty_fp=st.integers(min_value=1, max_value=(1 << 60)),
    ts_ns=st.integers(min_value=1_000_000, max_value=(1 << 62)),
    side=st.sampled_from(["BUY", "SELL"]),
)
def test_property_protobuf_edge_lossless_roundtrip(
    edge_id: int,
    fill_id: int,
    maker: int,
    taker: int,
    price_fp: int,
    qty_fp: int,
    ts_ns: int,
    side: str,
) -> None:
    side_lit = "BUY" if side == "BUY" else "SELL"
    seller = maker if side_lit == "BUY" else taker
    buyer = taker if side_lit == "BUY" else maker
    notional = (price_fp * qty_fp) // SCALE
    edge = TradeExecutionEdge(
        edge_id=edge_id,
        fill_id=fill_id,
        engine_seq=42,
        symbol="BTC-USDT",
        maker_account_id=maker,
        taker_account_id=taker,
        seller_account_id=seller,
        buyer_account_id=buyer,
        aggressor_side=side_lit,
        price_fp=price_fp,
        qty_fp=qty_fp,
        notional_fp=notional,
        exec_ts_ns=ts_ns,
        prev_interaction_delta_ns=5_000,
    )
    raw = edge_to_proto(edge, ref_ts_ns=ts_ns + 1_000_000).SerializeToString()
    parsed_msg = telemetry_fraud_pb2.TransactionGraphEdge()
    parsed_msg.ParseFromString(raw)
    recovered = proto_to_edge(parsed_msg)
    assert recovered == edge


def test_redpanda_processor_grpc_and_rest_end_to_end(models_dir: Path) -> None:
    engine = SentinelDetectionEngine(models_dir, use_int8=True)
    bus = SentinelRedpandaProcessor(engine)

    px = 68_500 * SCALE
    tick = 1_000_000
    t0 = 1_700_000_000_000_000_000
    m_msg = stream_analytics_pb2.MicrostructureMetrics(
        symbol="BTC-USDT",
        engine_seq=1,
        ts_ns=t0,
        mid_price_fp=px,
        spread_fp=2 * tick,
        spread_bps=1,
    )
    assert bus.handle_record(TOPIC_MICROSTRUCTURE, m_msg.SerializeToString()) == []

    legs = [(5001, 5002), (5002, 5003), (5003, 5001)]
    emitted = []
    for idx, (m_acct, t_acct) in enumerate(legs, start=1):
        fill = orderbook_pb2.ExecutionFill(
            fill_id=idx,
            engine_seq=10 + idx,
            symbol="BTC-USDT",
            instrument_id=1,
            maker_order_id=100 + idx,
            taker_order_id=200 + idx,
            maker_account_id=m_acct,
            taker_account_id=t_acct,
            aggressor_side=common_pb2.ORDER_SIDE_BUY,
            price_fp=px,
            qty_fp=3 * SCALE,
            notional_fp=to_u128((px * 3 * SCALE) // SCALE),
            exec_ts_ns=t0 + idx * 20_000_000,
            trace=common_pb2.TraceMetadata(
                trace_id="a" * 32,
                span_id="b" * 16,
                correlation_id=f"corr-{idx}",
                source_service="ledger-rs",
                ingest_ts_ns=t0 + idx * 20_000_000,
            ),
        )
        emitted.extend(bus.handle_record(TOPIC_EXECUTIONS, fill.SerializeToString()))

    assert len(emitted) >= 1
    env = emitted[0]
    assert env.topic == TOPIC_FRAUD_ALERTS
    assert env.key == b"BTC-USDT"
    hdr_dict = dict(env.headers)
    assert hdr_dict["x-schema-type"] == b"hyperion.v1.AnomalyAlert"
    assert hdr_dict["x-source-service"] == b"sentinel-ml"
    assert hdr_dict["traceparent"].startswith(b"00-" + b"a" * 32)

    decoded = deserialize_alert_proto(env.value)
    assert decoded.pattern_type == telemetry_fraud_pb2.ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE
    assert 0 <= decoded.severity_score <= 100
    assert set(decoded.offender_account_ids) == {5001, 5002, 5003}
    assert len(decoded.shap_attributions) == 16
    assert decoded.WhichOneof("pattern_detail") == "wash_trading_cycle"
    assert from_u128(decoded.wash_trading_cycle.circular_volume_fp) == 9 * SCALE

    l3_add = orderbook_pb2.L3BookDelta(
        engine_seq=30,
        prev_engine_seq=29,
        symbol="BTC-USDT",
        instrument_id=1,
        op=orderbook_pb2.L3_DELTA_OP_ADD,
        side=common_pb2.ORDER_SIDE_SELL,
        order_id=999,
        account_id=6001,
        price_fp=px + tick,
        remaining_qty_fp=10 * SCALE,
        delta_qty_fp=10 * SCALE,
        event_ts_ns=t0 + 100_000_000,
    )
    bus.handle_record(TOPIC_L3_DELTAS, l3_add.SerializeToString())

    lock = threading.RLock()
    app = create_http_app(engine, lock)
    client = TestClient(app)

    r_health = client.get("/healthz")
    assert r_health.status_code == 200
    assert r_health.json()["status"] == "ok"

    r_alerts = client.get("/api/v1/fraud/alerts")
    assert r_alerts.status_code == 200
    alerts_json = r_alerts.json()["alerts"]
    assert len(alerts_json) >= 1
    first_alert = alerts_json[0]
    assert isinstance(first_alert["shap_attributions"][0]["feature_value_fp"], str)
    assert isinstance(first_alert["evidence_subgraph"][0]["price_fp"], str)

    r_sub = client.get("/api/v1/fraud/subgraph/5001")
    assert r_sub.status_code == 200
    sub_json = r_sub.json()
    assert isinstance(sub_json["nodes"][0]["traded_volume_fp"], str)

    r_metrics = client.get("/metrics")
    assert r_metrics.status_code == 200
    assert "sentinel_graph_active_edges" in r_metrics.text

    grpc_srv = start_grpc_server(engine, lock, port=50195)
    try:
        with grpc.insecure_channel("127.0.0.1:50195") as channel:
            stub = telemetry_fraud_pb2_grpc.FraudTelemetryServiceStub(channel)
            sub_resp = stub.InspectFraudSubgraph(
                telemetry_fraud_pb2.InspectFraudSubgraphRequest(
                    center_account_id=5001,
                    symbol="BTC-USDT",
                    max_hops=2,
                )
            )
            assert len(sub_resp.nodes) >= 3
            assert len(sub_resp.edges) >= 3
            list_resp = stub.ListRecentAlerts(
                telemetry_fraud_pb2.ListRecentAlertsRequest(
                    symbol="BTC-USDT", min_severity_score=50, limit=10
                )
            )
            assert len(list_resp.alerts) >= 1
    finally:
        grpc_srv.stop(grace=0.5)
