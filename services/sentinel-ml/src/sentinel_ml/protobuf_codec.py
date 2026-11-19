"""Canonical Protobuf v3 & REST JSON Serialization Codec for Sentinel-ML.

Binds Sentinel-ML domain structures 1:1 to CH01's pre-generated `hyperion_proto` package:
  - hyperion.v1.AnomalyAlert, TransactionGraphEdge, WashTradingCycle, SpoofingLayering,
    VelocitySpike, SHAPFeatureAttribution, InspectFraudSubgraphResponse
  - hyperion.v1.ExecutionFill, L3BookDelta, MicrostructureMetrics
  - Enforces stringified decimal serialization for all `_fp` fields in JSON/REST payloads.
"""

from __future__ import annotations

from pathlib import Path
import sys
from typing import Any

for _parent in Path(__file__).resolve().parents:
    _cand = _parent / "contracts" / "gen" / "python" / "src"
    if _cand.is_dir() and str(_cand) not in sys.path:
        sys.path.insert(0, str(_cand))
        break

from hyperion_proto import (  # noqa: E402
    common_pb2,
    from_u128,
    orderbook_pb2,
    stream_analytics_pb2,
    telemetry_fraud_pb2,
    to_u128,
)

from sentinel_ml.engine import (  # noqa: E402
    FraudAlertRecord,
    SentinelDetectionEngine,
    SubgraphInspectionReport,
)
from sentinel_ml.graph_engine import (  # noqa: E402
    DetectedWashCycle,
    SideLiteral,
    TradeExecutionEdge,
)
from sentinel_ml.microstructure import (  # noqa: E402
    SpoofingLayeringEvidence,
    VelocitySpikeEvidence,
)
from sentinel_ml.shap_explainer import FeatureAttributionItem  # noqa: E402

DEFAULT_DECAY_TAU_NS: int = 15_000_000_000


def side_to_proto(side: SideLiteral) -> int:
    return int(common_pb2.ORDER_SIDE_BUY if side == "BUY" else common_pb2.ORDER_SIDE_SELL)


def proto_to_side(proto_side: int) -> SideLiteral:
    return "SELL" if proto_side == common_pb2.ORDER_SIDE_SELL else "BUY"


def edge_to_proto(
    edge: TradeExecutionEdge,
    *,
    ref_ts_ns: int = 0,
    tau_ns: int = DEFAULT_DECAY_TAU_NS,
) -> telemetry_fraud_pb2.TransactionGraphEdge:
    eff_ref = ref_ts_ns if ref_ts_ns > 0 else edge.exec_ts_ns
    return telemetry_fraud_pb2.TransactionGraphEdge(
        edge_id=edge.edge_id,
        fill_id=edge.fill_id,
        engine_seq=edge.engine_seq,
        symbol=edge.symbol,
        maker_account_id=edge.maker_account_id,
        taker_account_id=edge.taker_account_id,
        aggressor_side=side_to_proto(edge.aggressor_side),
        price_fp=edge.price_fp,
        qty_fp=edge.qty_fp,
        notional_fp=to_u128(edge.notional_fp),
        exec_ts_ns=edge.exec_ts_ns,
        prev_interaction_delta_ns=edge.prev_interaction_delta_ns,
        temporal_decay_weight_ppm=edge.temporal_decay_ppm(eff_ref, tau_ns),
    )


def proto_to_edge(msg: telemetry_fraud_pb2.TransactionGraphEdge) -> TradeExecutionEdge:
    side = proto_to_side(msg.aggressor_side)
    seller = int(msg.maker_account_id) if side == "BUY" else int(msg.taker_account_id)
    buyer = int(msg.taker_account_id) if side == "BUY" else int(msg.maker_account_id)
    return TradeExecutionEdge(
        edge_id=int(msg.edge_id),
        fill_id=int(msg.fill_id),
        engine_seq=int(msg.engine_seq),
        symbol=str(msg.symbol),
        maker_account_id=int(msg.maker_account_id),
        taker_account_id=int(msg.taker_account_id),
        seller_account_id=seller,
        buyer_account_id=buyer,
        aggressor_side=side,
        price_fp=int(msg.price_fp),
        qty_fp=int(msg.qty_fp),
        notional_fp=from_u128(msg.notional_fp),
        exec_ts_ns=int(msg.exec_ts_ns),
        prev_interaction_delta_ns=int(msg.prev_interaction_delta_ns),
    )


def shap_to_proto(item: FeatureAttributionItem) -> telemetry_fraud_pb2.SHAPFeatureAttribution:
    return telemetry_fraud_pb2.SHAPFeatureAttribution(
        feature_name=item.feature_name,
        feature_value_fp=item.feature_value_fp,
        baseline_value_fp=item.baseline_value_fp,
        shap_contribution_ppm=item.shap_contribution_ppm,
        importance_rank=item.importance_rank,
    )


def wash_cycle_to_proto(cycle: DetectedWashCycle) -> telemetry_fraud_pb2.WashTradingCycle:
    edges_pb = [edge_to_proto(e, ref_ts_ns=cycle.detected_ts_ns) for e in cycle.cycle_edges]
    return telemetry_fraud_pb2.WashTradingCycle(
        cycle_id=cycle.cycle_id,
        participant_account_ids=list(cycle.participant_account_ids),
        cycle_edges=edges_pb,
        cycle_length=cycle.cycle_length,
        circular_volume_fp=to_u128(cycle.circular_volume_fp),
        net_position_delta_fp=to_u128(cycle.net_position_delta_fp),
        cycle_duration_ns=cycle.cycle_duration_ns,
        volume_symmetry_ppm=cycle.volume_symmetry_ppm,
    )


def spoofing_to_proto(ev: SpoofingLayeringEvidence) -> telemetry_fraud_pb2.SpoofingLayering:
    return telemetry_fraud_pb2.SpoofingLayering(
        suspect_account_id=ev.suspect_account_id,
        spoofed_side=side_to_proto(ev.spoofed_side),
        layered_level_count=ev.layered_level_count,
        canceled_spoof_qty_fp=to_u128(ev.canceled_spoof_qty_fp),
        opposite_side_exec_qty_fp=to_u128(ev.opposite_side_exec_qty_fp),
        cancel_to_fill_latency_ns=ev.cancel_to_fill_latency_ns,
        order_to_trade_ratio_ppm=ev.order_to_trade_ratio_ppm,
        best_quote_distance_bps=ev.best_quote_distance_bps,
    )


def velocity_to_proto(ev: VelocitySpikeEvidence) -> telemetry_fraud_pb2.VelocitySpike:
    return telemetry_fraud_pb2.VelocitySpike(
        suspect_account_id=ev.suspect_account_id,
        events_per_second=ev.events_per_second,
        baseline_events_per_second=ev.baseline_events_per_second,
        burst_multiplier_ppm=ev.burst_multiplier_ppm,
        cancel_rate_ppm=ev.cancel_rate_ppm,
        notional_burst_fp=to_u128(ev.notional_burst_fp),
        window_duration_ns=ev.window_duration_ns,
    )


def alert_to_proto(alert: FraudAlertRecord) -> telemetry_fraud_pb2.AnomalyAlert:
    trace_msg = common_pb2.TraceMetadata(
        trace_id=alert.trace_id or f"{alert.engine_seq:032x}",
        span_id=alert.span_id or f"{alert.detected_ts_ns & ((1 << 64) - 1):016x}",
        correlation_id=alert.correlation_id or alert.alert_id,
        source_service="sentinel-ml",
        ingest_ts_ns=alert.detected_ts_ns,
    )
    subgraph_pb = [edge_to_proto(e, ref_ts_ns=alert.detected_ts_ns) for e in alert.evidence_subgraph]
    msg = telemetry_fraud_pb2.AnomalyAlert(
        alert_id=alert.alert_id,
        engine_seq=alert.engine_seq,
        symbol=alert.symbol,
        pattern_type=alert.pattern_type,
        severity_score=alert.severity_score,
        confidence_ppm=alert.confidence_ppm,
        autoencoder_recon_error_ppm=alert.autoencoder_recon_error_ppm,
        isolation_forest_score_ppm=alert.isolation_forest_score_ppm,
        offender_account_ids=list(alert.offender_account_ids),
        evidence_subgraph=subgraph_pb,
        shap_attributions=[shap_to_proto(s) for s in alert.shap_attributions],
        detected_ts_ns=alert.detected_ts_ns,
        inference_latency_us=alert.inference_latency_us,
        model_version=alert.model_version,
        trace=trace_msg,
    )
    if alert.wash_trading_cycle is not None:
        msg.wash_trading_cycle.CopyFrom(wash_cycle_to_proto(alert.wash_trading_cycle))
    elif alert.spoofing_layering is not None:
        msg.spoofing_layering.CopyFrom(spoofing_to_proto(alert.spoofing_layering))
    elif alert.velocity_spike is not None:
        msg.velocity_spike.CopyFrom(velocity_to_proto(alert.velocity_spike))
    return msg


def serialize_alert_bytes(alert: FraudAlertRecord) -> bytes:
    return bytes(alert_to_proto(alert).SerializeToString())


def deserialize_alert_proto(payload: bytes) -> telemetry_fraud_pb2.AnomalyAlert:
    msg = telemetry_fraud_pb2.AnomalyAlert()
    msg.ParseFromString(payload)
    return msg


def subgraph_report_to_proto(
    report: SubgraphInspectionReport,
) -> telemetry_fraud_pb2.InspectFraudSubgraphResponse:
    node_msgs = [
        telemetry_fraud_pb2.AccountNodeSummary(
            account_id=n.account_id,
            max_severity_score=n.max_severity_score,
            order_to_trade_ratio_ppm=n.order_to_trade_ratio_ppm,
            cancel_rate_ppm=n.cancel_rate_ppm,
            traded_volume_fp=to_u128(n.traded_volume_fp),
            active_alert_count=n.active_alert_count,
        )
        for n in report.nodes
    ]
    return telemetry_fraud_pb2.InspectFraudSubgraphResponse(
        nodes=node_msgs,
        edges=[edge_to_proto(e, ref_ts_ns=report.snapshot_ts_ns) for e in report.edges],
        related_alerts=[alert_to_proto(a) for a in report.related_alerts],
        top_attributions=[shap_to_proto(s) for s in report.top_attributions],
        snapshot_ts_ns=report.snapshot_ts_ns,
    )


def ingest_proto_execution_fill(
    engine: SentinelDetectionEngine, payload: bytes
) -> list[FraudAlertRecord]:
    """Decode binary hyperion.v1.ExecutionFill and run Sentinel-ML detection."""
    fill = orderbook_pb2.ExecutionFill()
    fill.ParseFromString(payload)
    return engine.process_execution_fill(
        fill_id=int(fill.fill_id),
        engine_seq=int(fill.engine_seq),
        symbol=str(fill.symbol),
        maker_account_id=int(fill.maker_account_id),
        taker_account_id=int(fill.taker_account_id),
        aggressor_side=proto_to_side(fill.aggressor_side),
        price_fp=int(fill.price_fp),
        qty_fp=int(fill.qty_fp),
        exec_ts_ns=int(fill.exec_ts_ns),
        maker_order_id=int(fill.maker_order_id),
        trace_id=str(fill.trace.trace_id) if fill.HasField("trace") else "",
        span_id=str(fill.trace.span_id) if fill.HasField("trace") else "",
        correlation_id=str(fill.trace.correlation_id) if fill.HasField("trace") else "",
    )


def ingest_proto_l3_delta(
    engine: SentinelDetectionEngine, payload: bytes
) -> FraudAlertRecord | None:
    """Decode binary hyperion.v1.L3BookDelta and run Sentinel-ML order-book forensics."""
    delta = orderbook_pb2.L3BookDelta()
    delta.ParseFromString(payload)
    op_map = {
        orderbook_pb2.L3_DELTA_OP_ADD: "ADD",
        orderbook_pb2.L3_DELTA_OP_MODIFY: "MODIFY",
        orderbook_pb2.L3_DELTA_OP_CANCEL: "CANCEL",
    }
    op_str = op_map.get(delta.op)
    if op_str is None:
        return None
    rem_qty = int(delta.remaining_qty_fp)
    eff_qty = abs(int(delta.delta_qty_fp)) if rem_qty == 0 else rem_qty
    return engine.process_l3_order_event(
        op=op_str,
        engine_seq=int(delta.engine_seq),
        symbol=str(delta.symbol),
        order_id=int(delta.order_id),
        account_id=int(delta.account_id),
        side=proto_to_side(delta.side),
        price_fp=int(delta.price_fp),
        qty_fp=max(1, eff_qty),
        event_ts_ns=int(delta.event_ts_ns),
        trace_id=str(delta.trace.trace_id) if delta.HasField("trace") else "",
        span_id=str(delta.trace.span_id) if delta.HasField("trace") else "",
        correlation_id=str(delta.trace.correlation_id) if delta.HasField("trace") else "",
    )


def ingest_proto_microstructure_metrics(
    engine: SentinelDetectionEngine, payload: bytes
) -> None:
    """Decode binary hyperion.v1.MicrostructureMetrics to synchronize BBO reference quotes."""
    m = stream_analytics_pb2.MicrostructureMetrics()
    m.ParseFromString(payload)
    if m.mid_price_fp > 0:
        half_spread = int(m.spread_fp) // 2
        best_bid = max(1, int(m.mid_price_fp) - half_spread)
        best_ask = int(m.mid_price_fp) + half_spread
        engine.extractor.update_bbo(str(m.symbol), best_bid, best_ask)


def alert_to_rest_json(alert: FraudAlertRecord) -> dict[str, Any]:
    """Serialize FraudAlertRecord to JSON dict with stringified `_fp` fields per CH01 contract."""
    return {
        "alert_id": alert.alert_id,
        "engine_seq": alert.engine_seq,
        "symbol": alert.symbol,
        "pattern_type": telemetry_fraud_pb2.AnomalyPatternType.Name(alert.pattern_type),
        "severity_score": alert.severity_score,
        "confidence_ppm": alert.confidence_ppm,
        "autoencoder_recon_error_ppm": alert.autoencoder_recon_error_ppm,
        "isolation_forest_score_ppm": alert.isolation_forest_score_ppm,
        "offender_account_ids": [str(a) for a in alert.offender_account_ids],
        "evidence_subgraph": [
            {
                "edge_id": e.edge_id,
                "fill_id": e.fill_id,
                "engine_seq": e.engine_seq,
                "symbol": e.symbol,
                "maker_account_id": str(e.maker_account_id),
                "taker_account_id": str(e.taker_account_id),
                "aggressor_side": e.aggressor_side,
                "price_fp": str(e.price_fp),
                "qty_fp": str(e.qty_fp),
                "notional_fp": str(e.notional_fp),
                "exec_ts_ns": e.exec_ts_ns,
                "prev_interaction_delta_ns": e.prev_interaction_delta_ns,
            }
            for e in alert.evidence_subgraph
        ],
        "shap_attributions": [
            {
                "feature_name": s.feature_name,
                "feature_value_fp": str(s.feature_value_fp),
                "baseline_value_fp": str(s.baseline_value_fp),
                "shap_contribution_ppm": s.shap_contribution_ppm,
                "importance_rank": s.importance_rank,
            }
            for s in alert.shap_attributions
        ],
        "detected_ts_ns": alert.detected_ts_ns,
        "inference_latency_us": alert.inference_latency_us,
        "model_version": alert.model_version,
    }


def subgraph_to_rest_json(report: SubgraphInspectionReport) -> dict[str, Any]:
    """Serialize SubgraphInspectionReport to REST JSON with stringified `_fp` fields."""
    return {
        "nodes": [
            {
                "account_id": str(n.account_id),
                "max_severity_score": n.max_severity_score,
                "order_to_trade_ratio_ppm": n.order_to_trade_ratio_ppm,
                "cancel_rate_ppm": n.cancel_rate_ppm,
                "traded_volume_fp": str(n.traded_volume_fp),
                "active_alert_count": n.active_alert_count,
            }
            for n in report.nodes
        ],
        "edges": [
            {
                "edge_id": e.edge_id,
                "fill_id": e.fill_id,
                "symbol": e.symbol,
                "maker_account_id": str(e.maker_account_id),
                "taker_account_id": str(e.taker_account_id),
                "aggressor_side": e.aggressor_side,
                "price_fp": str(e.price_fp),
                "qty_fp": str(e.qty_fp),
                "notional_fp": str(e.notional_fp),
                "exec_ts_ns": e.exec_ts_ns,
            }
            for e in report.edges
        ],
        "related_alerts": [alert_to_rest_json(a) for a in report.related_alerts],
        "top_attributions": [
            {
                "feature_name": s.feature_name,
                "feature_value_fp": str(s.feature_value_fp),
                "baseline_value_fp": str(s.baseline_value_fp),
                "shap_contribution_ppm": s.shap_contribution_ppm,
                "importance_rank": s.importance_rank,
            }
            for s in report.top_attributions
        ],
        "snapshot_ts_ns": report.snapshot_ts_ns,
    }
