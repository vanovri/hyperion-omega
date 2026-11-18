from . import common_pb2 as _common_pb2
from . import orderbook_pb2 as _orderbook_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ReplayControlAction(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    REPLAY_CONTROL_ACTION_UNSPECIFIED: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_PLAY: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_PAUSE: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_STEP_FORWARD: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_STEP_BACKWARD: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_REWIND: _ClassVar[ReplayControlAction]
    REPLAY_CONTROL_ACTION_SET_SPEED: _ClassVar[ReplayControlAction]
REPLAY_CONTROL_ACTION_UNSPECIFIED: ReplayControlAction
REPLAY_CONTROL_ACTION_PLAY: ReplayControlAction
REPLAY_CONTROL_ACTION_PAUSE: ReplayControlAction
REPLAY_CONTROL_ACTION_STEP_FORWARD: ReplayControlAction
REPLAY_CONTROL_ACTION_STEP_BACKWARD: ReplayControlAction
REPLAY_CONTROL_ACTION_REWIND: ReplayControlAction
REPLAY_CONTROL_ACTION_SET_SPEED: ReplayControlAction

class TimeTravelSeekRequest(_message.Message):
    __slots__ = ("symbol", "target_engine_seq", "target_ts_ns", "l2_depth", "include_account_balances", "verify_wal_hash_chain", "trace")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    TARGET_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    TARGET_TS_NS_FIELD_NUMBER: _ClassVar[int]
    L2_DEPTH_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_ACCOUNT_BALANCES_FIELD_NUMBER: _ClassVar[int]
    VERIFY_WAL_HASH_CHAIN_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    target_engine_seq: int
    target_ts_ns: int
    l2_depth: int
    include_account_balances: bool
    verify_wal_hash_chain: bool
    trace: _common_pb2.TraceMetadata
    def __init__(self, symbol: _Optional[str] = ..., target_engine_seq: _Optional[int] = ..., target_ts_ns: _Optional[int] = ..., l2_depth: _Optional[int] = ..., include_account_balances: _Optional[bool] = ..., verify_wal_hash_chain: _Optional[bool] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class TimeTravelSeekResponse(_message.Message):
    __slots__ = ("resolved_engine_seq", "resolved_ts_ns", "base_snapshot_seq", "replayed_wal_events", "reconstruction_latency_us", "wal_chain_verified", "state_root_blake3", "l2_snapshot", "account_states")
    RESOLVED_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    RESOLVED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    BASE_SNAPSHOT_SEQ_FIELD_NUMBER: _ClassVar[int]
    REPLAYED_WAL_EVENTS_FIELD_NUMBER: _ClassVar[int]
    RECONSTRUCTION_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    WAL_CHAIN_VERIFIED_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    L2_SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_STATES_FIELD_NUMBER: _ClassVar[int]
    resolved_engine_seq: int
    resolved_ts_ns: int
    base_snapshot_seq: int
    replayed_wal_events: int
    reconstruction_latency_us: int
    wal_chain_verified: bool
    state_root_blake3: bytes
    l2_snapshot: _orderbook_pb2.L2BookSnapshot
    account_states: _containers.RepeatedCompositeFieldContainer[_orderbook_pb2.MerkleLedgerProof]
    def __init__(self, resolved_engine_seq: _Optional[int] = ..., resolved_ts_ns: _Optional[int] = ..., base_snapshot_seq: _Optional[int] = ..., replayed_wal_events: _Optional[int] = ..., reconstruction_latency_us: _Optional[int] = ..., wal_chain_verified: _Optional[bool] = ..., state_root_blake3: _Optional[bytes] = ..., l2_snapshot: _Optional[_Union[_orderbook_pb2.L2BookSnapshot, _Mapping]] = ..., account_states: _Optional[_Iterable[_Union[_orderbook_pb2.MerkleLedgerProof, _Mapping]]] = ...) -> None: ...

class AccountPolicyOverride(_message.Message):
    __slots__ = ("account_id", "status_override", "effective_from_seq", "effective_from_ts_ns", "rate_limit_rps", "cancel_resting_orders_on_activation")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    EFFECTIVE_FROM_SEQ_FIELD_NUMBER: _ClassVar[int]
    EFFECTIVE_FROM_TS_NS_FIELD_NUMBER: _ClassVar[int]
    RATE_LIMIT_RPS_FIELD_NUMBER: _ClassVar[int]
    CANCEL_RESTING_ORDERS_ON_ACTIVATION_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    status_override: _common_pb2.AccountStatus
    effective_from_seq: int
    effective_from_ts_ns: int
    rate_limit_rps: int
    cancel_resting_orders_on_activation: bool
    def __init__(self, account_id: _Optional[int] = ..., status_override: _Optional[_Union[_common_pb2.AccountStatus, str]] = ..., effective_from_seq: _Optional[int] = ..., effective_from_ts_ns: _Optional[int] = ..., rate_limit_rps: _Optional[int] = ..., cancel_resting_orders_on_activation: _Optional[bool] = ...) -> None: ...

class SimulationPolicyOverride(_message.Message):
    __slots__ = ("maker_fee_ppm_override", "taker_fee_ppm_override", "max_order_qty_fp_override", "max_order_notional_fp_override", "stp_mode_override", "regime_override", "volatility_guard_spread_bps", "account_overrides")
    MAKER_FEE_PPM_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    TAKER_FEE_PPM_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    MAX_ORDER_QTY_FP_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    MAX_ORDER_NOTIONAL_FP_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    STP_MODE_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    REGIME_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    VOLATILITY_GUARD_SPREAD_BPS_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_OVERRIDES_FIELD_NUMBER: _ClassVar[int]
    maker_fee_ppm_override: int
    taker_fee_ppm_override: int
    max_order_qty_fp_override: int
    max_order_notional_fp_override: _common_pb2.U128
    stp_mode_override: _common_pb2.SelfTradePreventionMode
    regime_override: _common_pb2.MarketRegime
    volatility_guard_spread_bps: int
    account_overrides: _containers.RepeatedCompositeFieldContainer[AccountPolicyOverride]
    def __init__(self, maker_fee_ppm_override: _Optional[int] = ..., taker_fee_ppm_override: _Optional[int] = ..., max_order_qty_fp_override: _Optional[int] = ..., max_order_notional_fp_override: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., stp_mode_override: _Optional[_Union[_common_pb2.SelfTradePreventionMode, str]] = ..., regime_override: _Optional[_Union[_common_pb2.MarketRegime, str]] = ..., volatility_guard_spread_bps: _Optional[int] = ..., account_overrides: _Optional[_Iterable[_Union[AccountPolicyOverride, _Mapping]]] = ...) -> None: ...

class TimelineForkRequest(_message.Message):
    __slots__ = ("fork_name", "symbol", "start_engine_seq", "end_engine_seq", "start_ts_ns", "end_ts_ns", "policy_override", "trace")
    FORK_NAME_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    START_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    END_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    START_TS_NS_FIELD_NUMBER: _ClassVar[int]
    END_TS_NS_FIELD_NUMBER: _ClassVar[int]
    POLICY_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    fork_name: str
    symbol: str
    start_engine_seq: int
    end_engine_seq: int
    start_ts_ns: int
    end_ts_ns: int
    policy_override: SimulationPolicyOverride
    trace: _common_pb2.TraceMetadata
    def __init__(self, fork_name: _Optional[str] = ..., symbol: _Optional[str] = ..., start_engine_seq: _Optional[int] = ..., end_engine_seq: _Optional[int] = ..., start_ts_ns: _Optional[int] = ..., end_ts_ns: _Optional[int] = ..., policy_override: _Optional[_Union[SimulationPolicyOverride, _Mapping]] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class AccountBalanceDiff(_message.Message):
    __slots__ = ("account_id", "asset", "baseline_total_fp", "forked_total_fp", "delta_total_fp")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    BASELINE_TOTAL_FP_FIELD_NUMBER: _ClassVar[int]
    FORKED_TOTAL_FP_FIELD_NUMBER: _ClassVar[int]
    DELTA_TOTAL_FP_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    asset: str
    baseline_total_fp: _common_pb2.I128
    forked_total_fp: _common_pb2.I128
    delta_total_fp: _common_pb2.I128
    def __init__(self, account_id: _Optional[int] = ..., asset: _Optional[str] = ..., baseline_total_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., forked_total_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., delta_total_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ...) -> None: ...

class PriceLevelDiff(_message.Message):
    __slots__ = ("side", "price_fp", "baseline_qty_fp", "forked_qty_fp", "delta_qty_fp")
    SIDE_FIELD_NUMBER: _ClassVar[int]
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    BASELINE_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    FORKED_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    DELTA_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    side: _common_pb2.OrderSide
    price_fp: int
    baseline_qty_fp: int
    forked_qty_fp: int
    delta_qty_fp: int
    def __init__(self, side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., price_fp: _Optional[int] = ..., baseline_qty_fp: _Optional[int] = ..., forked_qty_fp: _Optional[int] = ..., delta_qty_fp: _Optional[int] = ...) -> None: ...

class StateTreeDiffResponse(_message.Message):
    __slots__ = ("fork_id", "symbol", "start_engine_seq", "end_engine_seq", "baseline_state_root_blake3", "forked_state_root_blake3", "state_identical", "baseline_clearing_price_fp", "forked_clearing_price_fp", "clearing_price_delta_fp", "baseline_volatility_bps", "forked_volatility_bps", "max_slippage_delta_bps", "prevented_wash_volume_fp", "prevented_fills_count", "rejected_orders_count", "treasury_fee_delta_fp", "account_diffs", "l2_level_diffs", "replayed_events_count", "computation_latency_us")
    FORK_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    START_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    END_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    BASELINE_STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    FORKED_STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    STATE_IDENTICAL_FIELD_NUMBER: _ClassVar[int]
    BASELINE_CLEARING_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    FORKED_CLEARING_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    CLEARING_PRICE_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    BASELINE_VOLATILITY_BPS_FIELD_NUMBER: _ClassVar[int]
    FORKED_VOLATILITY_BPS_FIELD_NUMBER: _ClassVar[int]
    MAX_SLIPPAGE_DELTA_BPS_FIELD_NUMBER: _ClassVar[int]
    PREVENTED_WASH_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    PREVENTED_FILLS_COUNT_FIELD_NUMBER: _ClassVar[int]
    REJECTED_ORDERS_COUNT_FIELD_NUMBER: _ClassVar[int]
    TREASURY_FEE_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_DIFFS_FIELD_NUMBER: _ClassVar[int]
    L2_LEVEL_DIFFS_FIELD_NUMBER: _ClassVar[int]
    REPLAYED_EVENTS_COUNT_FIELD_NUMBER: _ClassVar[int]
    COMPUTATION_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    fork_id: str
    symbol: str
    start_engine_seq: int
    end_engine_seq: int
    baseline_state_root_blake3: bytes
    forked_state_root_blake3: bytes
    state_identical: bool
    baseline_clearing_price_fp: int
    forked_clearing_price_fp: int
    clearing_price_delta_fp: int
    baseline_volatility_bps: int
    forked_volatility_bps: int
    max_slippage_delta_bps: int
    prevented_wash_volume_fp: _common_pb2.U128
    prevented_fills_count: int
    rejected_orders_count: int
    treasury_fee_delta_fp: _common_pb2.I128
    account_diffs: _containers.RepeatedCompositeFieldContainer[AccountBalanceDiff]
    l2_level_diffs: _containers.RepeatedCompositeFieldContainer[PriceLevelDiff]
    replayed_events_count: int
    computation_latency_us: int
    def __init__(self, fork_id: _Optional[str] = ..., symbol: _Optional[str] = ..., start_engine_seq: _Optional[int] = ..., end_engine_seq: _Optional[int] = ..., baseline_state_root_blake3: _Optional[bytes] = ..., forked_state_root_blake3: _Optional[bytes] = ..., state_identical: _Optional[bool] = ..., baseline_clearing_price_fp: _Optional[int] = ..., forked_clearing_price_fp: _Optional[int] = ..., clearing_price_delta_fp: _Optional[int] = ..., baseline_volatility_bps: _Optional[int] = ..., forked_volatility_bps: _Optional[int] = ..., max_slippage_delta_bps: _Optional[int] = ..., prevented_wash_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., prevented_fills_count: _Optional[int] = ..., rejected_orders_count: _Optional[int] = ..., treasury_fee_delta_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., account_diffs: _Optional[_Iterable[_Union[AccountBalanceDiff, _Mapping]]] = ..., l2_level_diffs: _Optional[_Iterable[_Union[PriceLevelDiff, _Mapping]]] = ..., replayed_events_count: _Optional[int] = ..., computation_latency_us: _Optional[int] = ...) -> None: ...

class ReplayFrameStream(_message.Message):
    __slots__ = ("session_id", "timeline_id", "symbol", "frame_index", "engine_seq", "event_ts_ns", "speed_milli", "is_paused", "state_root_blake3", "l2_snapshot", "frame_deltas", "frame_fills")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    TIMELINE_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    FRAME_INDEX_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    EVENT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    SPEED_MILLI_FIELD_NUMBER: _ClassVar[int]
    IS_PAUSED_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    L2_SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    FRAME_DELTAS_FIELD_NUMBER: _ClassVar[int]
    FRAME_FILLS_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    timeline_id: str
    symbol: str
    frame_index: int
    engine_seq: int
    event_ts_ns: int
    speed_milli: int
    is_paused: bool
    state_root_blake3: bytes
    l2_snapshot: _orderbook_pb2.L2BookSnapshot
    frame_deltas: _containers.RepeatedCompositeFieldContainer[_orderbook_pb2.L3BookDelta]
    frame_fills: _containers.RepeatedCompositeFieldContainer[_orderbook_pb2.ExecutionFill]
    def __init__(self, session_id: _Optional[str] = ..., timeline_id: _Optional[str] = ..., symbol: _Optional[str] = ..., frame_index: _Optional[int] = ..., engine_seq: _Optional[int] = ..., event_ts_ns: _Optional[int] = ..., speed_milli: _Optional[int] = ..., is_paused: _Optional[bool] = ..., state_root_blake3: _Optional[bytes] = ..., l2_snapshot: _Optional[_Union[_orderbook_pb2.L2BookSnapshot, _Mapping]] = ..., frame_deltas: _Optional[_Iterable[_Union[_orderbook_pb2.L3BookDelta, _Mapping]]] = ..., frame_fills: _Optional[_Iterable[_Union[_orderbook_pb2.ExecutionFill, _Mapping]]] = ...) -> None: ...

class StreamReplayFramesRequest(_message.Message):
    __slots__ = ("session_id", "timeline_id", "symbol", "from_engine_seq", "to_engine_seq", "initial_speed_milli", "l2_depth")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    TIMELINE_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    FROM_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    TO_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    INITIAL_SPEED_MILLI_FIELD_NUMBER: _ClassVar[int]
    L2_DEPTH_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    timeline_id: str
    symbol: str
    from_engine_seq: int
    to_engine_seq: int
    initial_speed_milli: int
    l2_depth: int
    def __init__(self, session_id: _Optional[str] = ..., timeline_id: _Optional[str] = ..., symbol: _Optional[str] = ..., from_engine_seq: _Optional[int] = ..., to_engine_seq: _Optional[int] = ..., initial_speed_milli: _Optional[int] = ..., l2_depth: _Optional[int] = ...) -> None: ...

class ControlReplaySessionRequest(_message.Message):
    __slots__ = ("session_id", "action", "speed_milli", "step_count")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    ACTION_FIELD_NUMBER: _ClassVar[int]
    SPEED_MILLI_FIELD_NUMBER: _ClassVar[int]
    STEP_COUNT_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    action: ReplayControlAction
    speed_milli: int
    step_count: int
    def __init__(self, session_id: _Optional[str] = ..., action: _Optional[_Union[ReplayControlAction, str]] = ..., speed_milli: _Optional[int] = ..., step_count: _Optional[int] = ...) -> None: ...

class ControlReplaySessionResponse(_message.Message):
    __slots__ = ("session_id", "current_engine_seq", "effective_speed_milli", "is_paused")
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    CURRENT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    EFFECTIVE_SPEED_MILLI_FIELD_NUMBER: _ClassVar[int]
    IS_PAUSED_FIELD_NUMBER: _ClassVar[int]
    session_id: str
    current_engine_seq: int
    effective_speed_milli: int
    is_paused: bool
    def __init__(self, session_id: _Optional[str] = ..., current_engine_seq: _Optional[int] = ..., effective_speed_milli: _Optional[int] = ..., is_paused: _Optional[bool] = ...) -> None: ...
