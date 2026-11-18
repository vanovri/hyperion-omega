from . import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class AnomalyPatternType(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ANOMALY_PATTERN_TYPE_UNSPECIFIED: _ClassVar[AnomalyPatternType]
    ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE: _ClassVar[AnomalyPatternType]
    ANOMALY_PATTERN_TYPE_SPOOFING_LAYERING: _ClassVar[AnomalyPatternType]
    ANOMALY_PATTERN_TYPE_VELOCITY_SPIKE: _ClassVar[AnomalyPatternType]
    ANOMALY_PATTERN_TYPE_MOMENTUM_IGNITION: _ClassVar[AnomalyPatternType]
    ANOMALY_PATTERN_TYPE_LATENCY_ARBITRAGE: _ClassVar[AnomalyPatternType]

class MitigationActionType(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MITIGATION_ACTION_TYPE_UNSPECIFIED: _ClassVar[MitigationActionType]
    MITIGATION_ACTION_TYPE_THROTTLE_RATE_LIMIT: _ClassVar[MitigationActionType]
    MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT: _ClassVar[MitigationActionType]
    MITIGATION_ACTION_TYPE_CANCEL_OPEN_ORDERS: _ClassVar[MitigationActionType]
    MITIGATION_ACTION_TYPE_ENABLE_VOLATILITY_GUARD: _ClassVar[MitigationActionType]
    MITIGATION_ACTION_TYPE_RESTORE_NORMAL: _ClassVar[MitigationActionType]
ANOMALY_PATTERN_TYPE_UNSPECIFIED: AnomalyPatternType
ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE: AnomalyPatternType
ANOMALY_PATTERN_TYPE_SPOOFING_LAYERING: AnomalyPatternType
ANOMALY_PATTERN_TYPE_VELOCITY_SPIKE: AnomalyPatternType
ANOMALY_PATTERN_TYPE_MOMENTUM_IGNITION: AnomalyPatternType
ANOMALY_PATTERN_TYPE_LATENCY_ARBITRAGE: AnomalyPatternType
MITIGATION_ACTION_TYPE_UNSPECIFIED: MitigationActionType
MITIGATION_ACTION_TYPE_THROTTLE_RATE_LIMIT: MitigationActionType
MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT: MitigationActionType
MITIGATION_ACTION_TYPE_CANCEL_OPEN_ORDERS: MitigationActionType
MITIGATION_ACTION_TYPE_ENABLE_VOLATILITY_GUARD: MitigationActionType
MITIGATION_ACTION_TYPE_RESTORE_NORMAL: MitigationActionType

class TransactionGraphEdge(_message.Message):
    __slots__ = ("edge_id", "fill_id", "engine_seq", "symbol", "maker_account_id", "taker_account_id", "aggressor_side", "price_fp", "qty_fp", "notional_fp", "exec_ts_ns", "prev_interaction_delta_ns", "temporal_decay_weight_ppm")
    EDGE_ID_FIELD_NUMBER: _ClassVar[int]
    FILL_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    MAKER_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    TAKER_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    AGGRESSOR_SIDE_FIELD_NUMBER: _ClassVar[int]
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    QTY_FP_FIELD_NUMBER: _ClassVar[int]
    NOTIONAL_FP_FIELD_NUMBER: _ClassVar[int]
    EXEC_TS_NS_FIELD_NUMBER: _ClassVar[int]
    PREV_INTERACTION_DELTA_NS_FIELD_NUMBER: _ClassVar[int]
    TEMPORAL_DECAY_WEIGHT_PPM_FIELD_NUMBER: _ClassVar[int]
    edge_id: int
    fill_id: int
    engine_seq: int
    symbol: str
    maker_account_id: int
    taker_account_id: int
    aggressor_side: _common_pb2.OrderSide
    price_fp: int
    qty_fp: int
    notional_fp: _common_pb2.U128
    exec_ts_ns: int
    prev_interaction_delta_ns: int
    temporal_decay_weight_ppm: int
    def __init__(self, edge_id: _Optional[int] = ..., fill_id: _Optional[int] = ..., engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., maker_account_id: _Optional[int] = ..., taker_account_id: _Optional[int] = ..., aggressor_side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., price_fp: _Optional[int] = ..., qty_fp: _Optional[int] = ..., notional_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., exec_ts_ns: _Optional[int] = ..., prev_interaction_delta_ns: _Optional[int] = ..., temporal_decay_weight_ppm: _Optional[int] = ...) -> None: ...

class WashTradingCycle(_message.Message):
    __slots__ = ("cycle_id", "participant_account_ids", "cycle_edges", "cycle_length", "circular_volume_fp", "net_position_delta_fp", "cycle_duration_ns", "volume_symmetry_ppm")
    CYCLE_ID_FIELD_NUMBER: _ClassVar[int]
    PARTICIPANT_ACCOUNT_IDS_FIELD_NUMBER: _ClassVar[int]
    CYCLE_EDGES_FIELD_NUMBER: _ClassVar[int]
    CYCLE_LENGTH_FIELD_NUMBER: _ClassVar[int]
    CIRCULAR_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    NET_POSITION_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    CYCLE_DURATION_NS_FIELD_NUMBER: _ClassVar[int]
    VOLUME_SYMMETRY_PPM_FIELD_NUMBER: _ClassVar[int]
    cycle_id: str
    participant_account_ids: _containers.RepeatedScalarFieldContainer[int]
    cycle_edges: _containers.RepeatedCompositeFieldContainer[TransactionGraphEdge]
    cycle_length: int
    circular_volume_fp: _common_pb2.U128
    net_position_delta_fp: _common_pb2.U128
    cycle_duration_ns: int
    volume_symmetry_ppm: int
    def __init__(self, cycle_id: _Optional[str] = ..., participant_account_ids: _Optional[_Iterable[int]] = ..., cycle_edges: _Optional[_Iterable[_Union[TransactionGraphEdge, _Mapping]]] = ..., cycle_length: _Optional[int] = ..., circular_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., net_position_delta_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., cycle_duration_ns: _Optional[int] = ..., volume_symmetry_ppm: _Optional[int] = ...) -> None: ...

class SpoofingLayering(_message.Message):
    __slots__ = ("suspect_account_id", "spoofed_side", "layered_level_count", "canceled_spoof_qty_fp", "opposite_side_exec_qty_fp", "cancel_to_fill_latency_ns", "order_to_trade_ratio_ppm", "best_quote_distance_bps")
    SUSPECT_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SPOOFED_SIDE_FIELD_NUMBER: _ClassVar[int]
    LAYERED_LEVEL_COUNT_FIELD_NUMBER: _ClassVar[int]
    CANCELED_SPOOF_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    OPPOSITE_SIDE_EXEC_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    CANCEL_TO_FILL_LATENCY_NS_FIELD_NUMBER: _ClassVar[int]
    ORDER_TO_TRADE_RATIO_PPM_FIELD_NUMBER: _ClassVar[int]
    BEST_QUOTE_DISTANCE_BPS_FIELD_NUMBER: _ClassVar[int]
    suspect_account_id: int
    spoofed_side: _common_pb2.OrderSide
    layered_level_count: int
    canceled_spoof_qty_fp: _common_pb2.U128
    opposite_side_exec_qty_fp: _common_pb2.U128
    cancel_to_fill_latency_ns: int
    order_to_trade_ratio_ppm: int
    best_quote_distance_bps: int
    def __init__(self, suspect_account_id: _Optional[int] = ..., spoofed_side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., layered_level_count: _Optional[int] = ..., canceled_spoof_qty_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., opposite_side_exec_qty_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., cancel_to_fill_latency_ns: _Optional[int] = ..., order_to_trade_ratio_ppm: _Optional[int] = ..., best_quote_distance_bps: _Optional[int] = ...) -> None: ...

class VelocitySpike(_message.Message):
    __slots__ = ("suspect_account_id", "events_per_second", "baseline_events_per_second", "burst_multiplier_ppm", "cancel_rate_ppm", "notional_burst_fp", "window_duration_ns")
    SUSPECT_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    EVENTS_PER_SECOND_FIELD_NUMBER: _ClassVar[int]
    BASELINE_EVENTS_PER_SECOND_FIELD_NUMBER: _ClassVar[int]
    BURST_MULTIPLIER_PPM_FIELD_NUMBER: _ClassVar[int]
    CANCEL_RATE_PPM_FIELD_NUMBER: _ClassVar[int]
    NOTIONAL_BURST_FP_FIELD_NUMBER: _ClassVar[int]
    WINDOW_DURATION_NS_FIELD_NUMBER: _ClassVar[int]
    suspect_account_id: int
    events_per_second: int
    baseline_events_per_second: int
    burst_multiplier_ppm: int
    cancel_rate_ppm: int
    notional_burst_fp: _common_pb2.U128
    window_duration_ns: int
    def __init__(self, suspect_account_id: _Optional[int] = ..., events_per_second: _Optional[int] = ..., baseline_events_per_second: _Optional[int] = ..., burst_multiplier_ppm: _Optional[int] = ..., cancel_rate_ppm: _Optional[int] = ..., notional_burst_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., window_duration_ns: _Optional[int] = ...) -> None: ...

class SHAPFeatureAttribution(_message.Message):
    __slots__ = ("feature_name", "feature_value_fp", "baseline_value_fp", "shap_contribution_ppm", "importance_rank")
    FEATURE_NAME_FIELD_NUMBER: _ClassVar[int]
    FEATURE_VALUE_FP_FIELD_NUMBER: _ClassVar[int]
    BASELINE_VALUE_FP_FIELD_NUMBER: _ClassVar[int]
    SHAP_CONTRIBUTION_PPM_FIELD_NUMBER: _ClassVar[int]
    IMPORTANCE_RANK_FIELD_NUMBER: _ClassVar[int]
    feature_name: str
    feature_value_fp: int
    baseline_value_fp: int
    shap_contribution_ppm: int
    importance_rank: int
    def __init__(self, feature_name: _Optional[str] = ..., feature_value_fp: _Optional[int] = ..., baseline_value_fp: _Optional[int] = ..., shap_contribution_ppm: _Optional[int] = ..., importance_rank: _Optional[int] = ...) -> None: ...

class AnomalyAlert(_message.Message):
    __slots__ = ("alert_id", "engine_seq", "symbol", "pattern_type", "severity_score", "confidence_ppm", "autoencoder_recon_error_ppm", "isolation_forest_score_ppm", "offender_account_ids", "evidence_subgraph", "shap_attributions", "wash_trading_cycle", "spoofing_layering", "velocity_spike", "detected_ts_ns", "inference_latency_us", "model_version", "trace")
    ALERT_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    PATTERN_TYPE_FIELD_NUMBER: _ClassVar[int]
    SEVERITY_SCORE_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_PPM_FIELD_NUMBER: _ClassVar[int]
    AUTOENCODER_RECON_ERROR_PPM_FIELD_NUMBER: _ClassVar[int]
    ISOLATION_FOREST_SCORE_PPM_FIELD_NUMBER: _ClassVar[int]
    OFFENDER_ACCOUNT_IDS_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_SUBGRAPH_FIELD_NUMBER: _ClassVar[int]
    SHAP_ATTRIBUTIONS_FIELD_NUMBER: _ClassVar[int]
    WASH_TRADING_CYCLE_FIELD_NUMBER: _ClassVar[int]
    SPOOFING_LAYERING_FIELD_NUMBER: _ClassVar[int]
    VELOCITY_SPIKE_FIELD_NUMBER: _ClassVar[int]
    DETECTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    INFERENCE_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    MODEL_VERSION_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    alert_id: str
    engine_seq: int
    symbol: str
    pattern_type: AnomalyPatternType
    severity_score: int
    confidence_ppm: int
    autoencoder_recon_error_ppm: int
    isolation_forest_score_ppm: int
    offender_account_ids: _containers.RepeatedScalarFieldContainer[int]
    evidence_subgraph: _containers.RepeatedCompositeFieldContainer[TransactionGraphEdge]
    shap_attributions: _containers.RepeatedCompositeFieldContainer[SHAPFeatureAttribution]
    wash_trading_cycle: WashTradingCycle
    spoofing_layering: SpoofingLayering
    velocity_spike: VelocitySpike
    detected_ts_ns: int
    inference_latency_us: int
    model_version: str
    trace: _common_pb2.TraceMetadata
    def __init__(self, alert_id: _Optional[str] = ..., engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., pattern_type: _Optional[_Union[AnomalyPatternType, str]] = ..., severity_score: _Optional[int] = ..., confidence_ppm: _Optional[int] = ..., autoencoder_recon_error_ppm: _Optional[int] = ..., isolation_forest_score_ppm: _Optional[int] = ..., offender_account_ids: _Optional[_Iterable[int]] = ..., evidence_subgraph: _Optional[_Iterable[_Union[TransactionGraphEdge, _Mapping]]] = ..., shap_attributions: _Optional[_Iterable[_Union[SHAPFeatureAttribution, _Mapping]]] = ..., wash_trading_cycle: _Optional[_Union[WashTradingCycle, _Mapping]] = ..., spoofing_layering: _Optional[_Union[SpoofingLayering, _Mapping]] = ..., velocity_spike: _Optional[_Union[VelocitySpike, _Mapping]] = ..., detected_ts_ns: _Optional[int] = ..., inference_latency_us: _Optional[int] = ..., model_version: _Optional[str] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class RiskMitigationCommand(_message.Message):
    __slots__ = ("command_id", "source_alert_id", "incident_id", "action", "target_account_ids", "target_symbol", "rate_limit_override_rps", "cooldown_duration_ms", "dry_run", "requires_human_approval", "rollback_token", "reason", "issued_by", "issued_ts_ns", "expires_ts_ns", "ed25519_signature", "audit_chain_blake3", "trace")
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ALERT_ID_FIELD_NUMBER: _ClassVar[int]
    INCIDENT_ID_FIELD_NUMBER: _ClassVar[int]
    ACTION_FIELD_NUMBER: _ClassVar[int]
    TARGET_ACCOUNT_IDS_FIELD_NUMBER: _ClassVar[int]
    TARGET_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    RATE_LIMIT_OVERRIDE_RPS_FIELD_NUMBER: _ClassVar[int]
    COOLDOWN_DURATION_MS_FIELD_NUMBER: _ClassVar[int]
    DRY_RUN_FIELD_NUMBER: _ClassVar[int]
    REQUIRES_HUMAN_APPROVAL_FIELD_NUMBER: _ClassVar[int]
    ROLLBACK_TOKEN_FIELD_NUMBER: _ClassVar[int]
    REASON_FIELD_NUMBER: _ClassVar[int]
    ISSUED_BY_FIELD_NUMBER: _ClassVar[int]
    ISSUED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_TS_NS_FIELD_NUMBER: _ClassVar[int]
    ED25519_SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    AUDIT_CHAIN_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    command_id: str
    source_alert_id: str
    incident_id: str
    action: MitigationActionType
    target_account_ids: _containers.RepeatedScalarFieldContainer[int]
    target_symbol: str
    rate_limit_override_rps: int
    cooldown_duration_ms: int
    dry_run: bool
    requires_human_approval: bool
    rollback_token: str
    reason: str
    issued_by: str
    issued_ts_ns: int
    expires_ts_ns: int
    ed25519_signature: bytes
    audit_chain_blake3: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, command_id: _Optional[str] = ..., source_alert_id: _Optional[str] = ..., incident_id: _Optional[str] = ..., action: _Optional[_Union[MitigationActionType, str]] = ..., target_account_ids: _Optional[_Iterable[int]] = ..., target_symbol: _Optional[str] = ..., rate_limit_override_rps: _Optional[int] = ..., cooldown_duration_ms: _Optional[int] = ..., dry_run: _Optional[bool] = ..., requires_human_approval: _Optional[bool] = ..., rollback_token: _Optional[str] = ..., reason: _Optional[str] = ..., issued_by: _Optional[str] = ..., issued_ts_ns: _Optional[int] = ..., expires_ts_ns: _Optional[int] = ..., ed25519_signature: _Optional[bytes] = ..., audit_chain_blake3: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class AccountNodeSummary(_message.Message):
    __slots__ = ("account_id", "max_severity_score", "order_to_trade_ratio_ppm", "cancel_rate_ppm", "traded_volume_fp", "active_alert_count")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    MAX_SEVERITY_SCORE_FIELD_NUMBER: _ClassVar[int]
    ORDER_TO_TRADE_RATIO_PPM_FIELD_NUMBER: _ClassVar[int]
    CANCEL_RATE_PPM_FIELD_NUMBER: _ClassVar[int]
    TRADED_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_ALERT_COUNT_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    max_severity_score: int
    order_to_trade_ratio_ppm: int
    cancel_rate_ppm: int
    traded_volume_fp: _common_pb2.U128
    active_alert_count: int
    def __init__(self, account_id: _Optional[int] = ..., max_severity_score: _Optional[int] = ..., order_to_trade_ratio_ppm: _Optional[int] = ..., cancel_rate_ppm: _Optional[int] = ..., traded_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., active_alert_count: _Optional[int] = ...) -> None: ...

class InspectFraudSubgraphRequest(_message.Message):
    __slots__ = ("center_account_id", "symbol", "max_hops", "lookback_window_ns")
    CENTER_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    MAX_HOPS_FIELD_NUMBER: _ClassVar[int]
    LOOKBACK_WINDOW_NS_FIELD_NUMBER: _ClassVar[int]
    center_account_id: int
    symbol: str
    max_hops: int
    lookback_window_ns: int
    def __init__(self, center_account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., max_hops: _Optional[int] = ..., lookback_window_ns: _Optional[int] = ...) -> None: ...

class InspectFraudSubgraphResponse(_message.Message):
    __slots__ = ("nodes", "edges", "related_alerts", "top_attributions", "snapshot_ts_ns")
    NODES_FIELD_NUMBER: _ClassVar[int]
    EDGES_FIELD_NUMBER: _ClassVar[int]
    RELATED_ALERTS_FIELD_NUMBER: _ClassVar[int]
    TOP_ATTRIBUTIONS_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    nodes: _containers.RepeatedCompositeFieldContainer[AccountNodeSummary]
    edges: _containers.RepeatedCompositeFieldContainer[TransactionGraphEdge]
    related_alerts: _containers.RepeatedCompositeFieldContainer[AnomalyAlert]
    top_attributions: _containers.RepeatedCompositeFieldContainer[SHAPFeatureAttribution]
    snapshot_ts_ns: int
    def __init__(self, nodes: _Optional[_Iterable[_Union[AccountNodeSummary, _Mapping]]] = ..., edges: _Optional[_Iterable[_Union[TransactionGraphEdge, _Mapping]]] = ..., related_alerts: _Optional[_Iterable[_Union[AnomalyAlert, _Mapping]]] = ..., top_attributions: _Optional[_Iterable[_Union[SHAPFeatureAttribution, _Mapping]]] = ..., snapshot_ts_ns: _Optional[int] = ...) -> None: ...

class ListRecentAlertsRequest(_message.Message):
    __slots__ = ("symbol", "min_severity_score", "limit")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    MIN_SEVERITY_SCORE_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    min_severity_score: int
    limit: int
    def __init__(self, symbol: _Optional[str] = ..., min_severity_score: _Optional[int] = ..., limit: _Optional[int] = ...) -> None: ...

class ListRecentAlertsResponse(_message.Message):
    __slots__ = ("alerts",)
    ALERTS_FIELD_NUMBER: _ClassVar[int]
    alerts: _containers.RepeatedCompositeFieldContainer[AnomalyAlert]
    def __init__(self, alerts: _Optional[_Iterable[_Union[AnomalyAlert, _Mapping]]] = ...) -> None: ...

class StreamAnomalyAlertsRequest(_message.Message):
    __slots__ = ("symbol", "min_severity_score")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    MIN_SEVERITY_SCORE_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    min_severity_score: int
    def __init__(self, symbol: _Optional[str] = ..., min_severity_score: _Optional[int] = ...) -> None: ...
