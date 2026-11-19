"""Redpanda (Kafka-Compatible) Event Stream Adapter for Sentinel-ML.

Implements canonical topic consumption and deterministic alert publication conforming to
`.agents/rules/00-hyperion-contract.md` Section 4:
  - Consumes:
      * hyperion.market.executions.v1 (ExecutionFill, key=<SYMBOL>)
      * hyperion.market.l3-deltas.v1 (L3BookDelta, key=<SYMBOL>)
      * hyperion.analytics.microstructure.v1 (MicrostructureMetrics, key=<SYMBOL>)
  - Produces:
      * hyperion.fraud.alerts.v1 (AnomalyAlert, key=<SYMBOL>, mandatory headers:
        traceparent, x-engine-seq, x-schema-type, x-source-service)
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol

from sentinel_ml.engine import FraudAlertRecord, SentinelDetectionEngine
from sentinel_ml.protobuf_codec import (
    ingest_proto_execution_fill,
    ingest_proto_l3_delta,
    ingest_proto_microstructure_metrics,
    serialize_alert_bytes,
)

TOPIC_EXECUTIONS: str = "hyperion.market.executions.v1"
TOPIC_L3_DELTAS: str = "hyperion.market.l3-deltas.v1"
TOPIC_MICROSTRUCTURE: str = "hyperion.analytics.microstructure.v1"
TOPIC_FRAUD_ALERTS: str = "hyperion.fraud.alerts.v1"


@dataclass(frozen=True, slots=True)
class KafkaRecordEnvelope:
    """Deterministic wire envelope for Redpanda producer/consumer verification & transport."""

    topic: str
    key: bytes
    value: bytes
    headers: tuple[tuple[str, bytes], ...]


class KafkaProducerLike(Protocol):
    def produce(
        self,
        topic: str,
        value: Any = None,
        key: Any = None,
        *,
        headers: Any = None,
    ) -> None: ...

    def poll(self, timeout: float = 0.0) -> int: ...


def build_alert_kafka_envelope(alert: FraudAlertRecord) -> KafkaRecordEnvelope:
    """Build deterministic Redpanda message with mandatory W3C traceparent & schema headers."""
    trace_id = (alert.trace_id or f"{alert.engine_seq:032x}")[:32].ljust(32, "0")
    span_fallback = f"{alert.detected_ts_ns & ((1 << 64) - 1):016x}"
    span_id = (alert.span_id or span_fallback)[:16].ljust(16, "0")
    traceparent = f"00-{trace_id}-{span_id}-01".encode("ascii")

    headers: tuple[tuple[str, bytes], ...] = (
        ("traceparent", traceparent),
        ("x-engine-seq", str(alert.engine_seq).encode("ascii")),
        ("x-schema-type", b"hyperion.v1.AnomalyAlert"),
        ("x-source-service", b"sentinel-ml"),
    )
    return KafkaRecordEnvelope(
        topic=TOPIC_FRAUD_ALERTS,
        key=alert.symbol.encode("utf-8"),
        value=serialize_alert_bytes(alert),
        headers=headers,
    )


class SentinelRedpandaProcessor:
    """Routes binary Protobuf records from Redpanda topics through SentinelDetectionEngine."""

    def __init__(
        self,
        engine: SentinelDetectionEngine,
        producer: KafkaProducerLike | None = None,
    ) -> None:
        self.engine = engine
        self.producer = producer
        self.published_envelopes: list[KafkaRecordEnvelope] = []

    def handle_record(self, topic: str, payload: bytes) -> list[KafkaRecordEnvelope]:
        """Consume a single Redpanda record and publish any triggered AnomalyAlerts."""
        triggered_alerts: list[FraudAlertRecord] = []
        if topic == TOPIC_EXECUTIONS:
            triggered_alerts = ingest_proto_execution_fill(self.engine, payload)
        elif topic == TOPIC_L3_DELTAS:
            maybe_alert = ingest_proto_l3_delta(self.engine, payload)
            if maybe_alert is not None:
                triggered_alerts = [maybe_alert]
        elif topic == TOPIC_MICROSTRUCTURE:
            ingest_proto_microstructure_metrics(self.engine, payload)
            return []
        else:
            raise ValueError(f"Unsupported Redpanda topic: {topic}")

        out_envelopes: list[KafkaRecordEnvelope] = []
        for alert in triggered_alerts:
            env = build_alert_kafka_envelope(alert)
            out_envelopes.append(env)
            if self.producer is not None:
                self.producer.produce(
                    topic=env.topic,
                    value=env.value,
                    key=env.key,
                    headers=list(env.headers),
                )
                self.producer.poll(0.0)
            else:
                self.published_envelopes.append(env)

        return out_envelopes
