from hyperion_proto import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class WasmTrapCode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WASM_TRAP_CODE_UNSPECIFIED: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_FUEL_EXHAUSTED: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_EPOCH_DEADLINE_EXCEEDED: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_MEMORY_LIMIT_EXCEEDED: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_STACK_OVERFLOW: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_INTEGER_DIVIDE_BY_ZERO: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_INTEGER_OVERFLOW: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_INVALID_CONVERSION_TO_INT: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_UNREACHABLE_INSTRUCTION: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_INDIRECT_CALL_TYPE_MISMATCH: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_FORBIDDEN_HOST_IMPORT: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED: _ClassVar[WasmTrapCode]
    WASM_TRAP_CODE_PRE_TRADE_RISK_REJECTED: _ClassVar[WasmTrapCode]

class StrategyLifecycleState(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    STRATEGY_LIFECYCLE_STATE_UNSPECIFIED: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_VALIDATING: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_ACTIVE: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_HOT_SWAPPING: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_PAUSED: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_QUARANTINED_ON_TRAP: _ClassVar[StrategyLifecycleState]
    STRATEGY_LIFECYCLE_STATE_TERMINATED: _ClassVar[StrategyLifecycleState]

class WasmCallbackHook(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WASM_CALLBACK_HOOK_UNSPECIFIED: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_INIT: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_BOOK_L2_TICK: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_TRADE_TICK: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_ORDER_FILL: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_EXPORT: _ClassVar[WasmCallbackHook]
    WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_IMPORT: _ClassVar[WasmCallbackHook]
WASM_TRAP_CODE_UNSPECIFIED: WasmTrapCode
WASM_TRAP_CODE_FUEL_EXHAUSTED: WasmTrapCode
WASM_TRAP_CODE_EPOCH_DEADLINE_EXCEEDED: WasmTrapCode
WASM_TRAP_CODE_MEMORY_LIMIT_EXCEEDED: WasmTrapCode
WASM_TRAP_CODE_STACK_OVERFLOW: WasmTrapCode
WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY: WasmTrapCode
WASM_TRAP_CODE_INTEGER_DIVIDE_BY_ZERO: WasmTrapCode
WASM_TRAP_CODE_INTEGER_OVERFLOW: WasmTrapCode
WASM_TRAP_CODE_INVALID_CONVERSION_TO_INT: WasmTrapCode
WASM_TRAP_CODE_UNREACHABLE_INSTRUCTION: WasmTrapCode
WASM_TRAP_CODE_INDIRECT_CALL_TYPE_MISMATCH: WasmTrapCode
WASM_TRAP_CODE_FORBIDDEN_HOST_IMPORT: WasmTrapCode
WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED: WasmTrapCode
WASM_TRAP_CODE_PRE_TRADE_RISK_REJECTED: WasmTrapCode
STRATEGY_LIFECYCLE_STATE_UNSPECIFIED: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_VALIDATING: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_ACTIVE: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_HOT_SWAPPING: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_PAUSED: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_QUARANTINED_ON_TRAP: StrategyLifecycleState
STRATEGY_LIFECYCLE_STATE_TERMINATED: StrategyLifecycleState
WASM_CALLBACK_HOOK_UNSPECIFIED: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_INIT: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_BOOK_L2_TICK: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_TRADE_TICK: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_ORDER_FILL: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_EXPORT: WasmCallbackHook
WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_IMPORT: WasmCallbackHook

class StrategySandboxLimits(_message.Message):
    __slots__ = ("max_linear_memory_bytes", "max_stack_bytes", "max_fuel_per_tick", "max_epoch_walltime_ns", "max_orders_per_tick", "max_order_notional_fp", "max_abs_position_qty_fp")
    MAX_LINEAR_MEMORY_BYTES_FIELD_NUMBER: _ClassVar[int]
    MAX_STACK_BYTES_FIELD_NUMBER: _ClassVar[int]
    MAX_FUEL_PER_TICK_FIELD_NUMBER: _ClassVar[int]
    MAX_EPOCH_WALLTIME_NS_FIELD_NUMBER: _ClassVar[int]
    MAX_ORDERS_PER_TICK_FIELD_NUMBER: _ClassVar[int]
    MAX_ORDER_NOTIONAL_FP_FIELD_NUMBER: _ClassVar[int]
    MAX_ABS_POSITION_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    max_linear_memory_bytes: int
    max_stack_bytes: int
    max_fuel_per_tick: int
    max_epoch_walltime_ns: int
    max_orders_per_tick: int
    max_order_notional_fp: _common_pb2.U128
    max_abs_position_qty_fp: int
    def __init__(self, max_linear_memory_bytes: _Optional[int] = ..., max_stack_bytes: _Optional[int] = ..., max_fuel_per_tick: _Optional[int] = ..., max_epoch_walltime_ns: _Optional[int] = ..., max_orders_per_tick: _Optional[int] = ..., max_order_notional_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., max_abs_position_qty_fp: _Optional[int] = ...) -> None: ...

class DeployStrategyModule(_message.Message):
    __slots__ = ("strategy_id", "account_id", "version", "strategy_name", "subscribed_symbols", "wasm_bytecode", "bytecode_blake3", "author_ed25519_signature", "limits", "atomic_hot_swap", "initial_config_payload", "submitted_ts_ns", "trace")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    STRATEGY_NAME_FIELD_NUMBER: _ClassVar[int]
    SUBSCRIBED_SYMBOLS_FIELD_NUMBER: _ClassVar[int]
    WASM_BYTECODE_FIELD_NUMBER: _ClassVar[int]
    BYTECODE_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    AUTHOR_ED25519_SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    LIMITS_FIELD_NUMBER: _ClassVar[int]
    ATOMIC_HOT_SWAP_FIELD_NUMBER: _ClassVar[int]
    INITIAL_CONFIG_PAYLOAD_FIELD_NUMBER: _ClassVar[int]
    SUBMITTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    account_id: int
    version: int
    strategy_name: str
    subscribed_symbols: _containers.RepeatedScalarFieldContainer[str]
    wasm_bytecode: bytes
    bytecode_blake3: bytes
    author_ed25519_signature: bytes
    limits: StrategySandboxLimits
    atomic_hot_swap: bool
    initial_config_payload: bytes
    submitted_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, strategy_id: _Optional[str] = ..., account_id: _Optional[int] = ..., version: _Optional[int] = ..., strategy_name: _Optional[str] = ..., subscribed_symbols: _Optional[_Iterable[str]] = ..., wasm_bytecode: _Optional[bytes] = ..., bytecode_blake3: _Optional[bytes] = ..., author_ed25519_signature: _Optional[bytes] = ..., limits: _Optional[_Union[StrategySandboxLimits, _Mapping]] = ..., atomic_hot_swap: _Optional[bool] = ..., initial_config_payload: _Optional[bytes] = ..., submitted_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class DeployStrategyResponse(_message.Message):
    __slots__ = ("strategy_id", "active_version", "state", "jit_compile_latency_us", "handed_off_open_orders", "reject_reason")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_VERSION_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    JIT_COMPILE_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    HANDED_OFF_OPEN_ORDERS_FIELD_NUMBER: _ClassVar[int]
    REJECT_REASON_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    active_version: int
    state: StrategyLifecycleState
    jit_compile_latency_us: int
    handed_off_open_orders: int
    reject_reason: str
    def __init__(self, strategy_id: _Optional[str] = ..., active_version: _Optional[int] = ..., state: _Optional[_Union[StrategyLifecycleState, str]] = ..., jit_compile_latency_us: _Optional[int] = ..., handed_off_open_orders: _Optional[int] = ..., reject_reason: _Optional[str] = ...) -> None: ...

class FuelMeteringReport(_message.Message):
    __slots__ = ("strategy_id", "account_id", "version", "symbol", "engine_seq", "callback_hook", "fuel_allocated", "fuel_consumed", "fuel_remaining", "execution_latency_ns", "linear_memory_pages", "linear_memory_bytes", "orders_emitted_in_tick", "orders_rejected_by_risk_gate", "reported_ts_ns", "trace")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    CALLBACK_HOOK_FIELD_NUMBER: _ClassVar[int]
    FUEL_ALLOCATED_FIELD_NUMBER: _ClassVar[int]
    FUEL_CONSUMED_FIELD_NUMBER: _ClassVar[int]
    FUEL_REMAINING_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_LATENCY_NS_FIELD_NUMBER: _ClassVar[int]
    LINEAR_MEMORY_PAGES_FIELD_NUMBER: _ClassVar[int]
    LINEAR_MEMORY_BYTES_FIELD_NUMBER: _ClassVar[int]
    ORDERS_EMITTED_IN_TICK_FIELD_NUMBER: _ClassVar[int]
    ORDERS_REJECTED_BY_RISK_GATE_FIELD_NUMBER: _ClassVar[int]
    REPORTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    account_id: int
    version: int
    symbol: str
    engine_seq: int
    callback_hook: WasmCallbackHook
    fuel_allocated: int
    fuel_consumed: int
    fuel_remaining: int
    execution_latency_ns: int
    linear_memory_pages: int
    linear_memory_bytes: int
    orders_emitted_in_tick: int
    orders_rejected_by_risk_gate: int
    reported_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, strategy_id: _Optional[str] = ..., account_id: _Optional[int] = ..., version: _Optional[int] = ..., symbol: _Optional[str] = ..., engine_seq: _Optional[int] = ..., callback_hook: _Optional[_Union[WasmCallbackHook, str]] = ..., fuel_allocated: _Optional[int] = ..., fuel_consumed: _Optional[int] = ..., fuel_remaining: _Optional[int] = ..., execution_latency_ns: _Optional[int] = ..., linear_memory_pages: _Optional[int] = ..., linear_memory_bytes: _Optional[int] = ..., orders_emitted_in_tick: _Optional[int] = ..., orders_rejected_by_risk_gate: _Optional[int] = ..., reported_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class SandboxExecutionTrap(_message.Message):
    __slots__ = ("trap_event_id", "strategy_id", "account_id", "version", "symbol", "engine_seq", "callback_hook", "trap_code", "fuel_consumed_at_trap", "elapsed_ns_at_trap", "memory_bytes_at_trap", "wasm_program_counter_offset", "open_orders_auto_canceled", "resulting_state", "diagnostic_message", "trapped_ts_ns", "trace")
    TRAP_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    CALLBACK_HOOK_FIELD_NUMBER: _ClassVar[int]
    TRAP_CODE_FIELD_NUMBER: _ClassVar[int]
    FUEL_CONSUMED_AT_TRAP_FIELD_NUMBER: _ClassVar[int]
    ELAPSED_NS_AT_TRAP_FIELD_NUMBER: _ClassVar[int]
    MEMORY_BYTES_AT_TRAP_FIELD_NUMBER: _ClassVar[int]
    WASM_PROGRAM_COUNTER_OFFSET_FIELD_NUMBER: _ClassVar[int]
    OPEN_ORDERS_AUTO_CANCELED_FIELD_NUMBER: _ClassVar[int]
    RESULTING_STATE_FIELD_NUMBER: _ClassVar[int]
    DIAGNOSTIC_MESSAGE_FIELD_NUMBER: _ClassVar[int]
    TRAPPED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    trap_event_id: str
    strategy_id: str
    account_id: int
    version: int
    symbol: str
    engine_seq: int
    callback_hook: WasmCallbackHook
    trap_code: WasmTrapCode
    fuel_consumed_at_trap: int
    elapsed_ns_at_trap: int
    memory_bytes_at_trap: int
    wasm_program_counter_offset: int
    open_orders_auto_canceled: bool
    resulting_state: StrategyLifecycleState
    diagnostic_message: str
    trapped_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, trap_event_id: _Optional[str] = ..., strategy_id: _Optional[str] = ..., account_id: _Optional[int] = ..., version: _Optional[int] = ..., symbol: _Optional[str] = ..., engine_seq: _Optional[int] = ..., callback_hook: _Optional[_Union[WasmCallbackHook, str]] = ..., trap_code: _Optional[_Union[WasmTrapCode, str]] = ..., fuel_consumed_at_trap: _Optional[int] = ..., elapsed_ns_at_trap: _Optional[int] = ..., memory_bytes_at_trap: _Optional[int] = ..., wasm_program_counter_offset: _Optional[int] = ..., open_orders_auto_canceled: _Optional[bool] = ..., resulting_state: _Optional[_Union[StrategyLifecycleState, str]] = ..., diagnostic_message: _Optional[str] = ..., trapped_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class PauseOrTerminateStrategyRequest(_message.Message):
    __slots__ = ("strategy_id", "account_id", "terminate_permanently", "cancel_open_orders", "trace")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    TERMINATE_PERMANENTLY_FIELD_NUMBER: _ClassVar[int]
    CANCEL_OPEN_ORDERS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    account_id: int
    terminate_permanently: bool
    cancel_open_orders: bool
    trace: _common_pb2.TraceMetadata
    def __init__(self, strategy_id: _Optional[str] = ..., account_id: _Optional[int] = ..., terminate_permanently: _Optional[bool] = ..., cancel_open_orders: _Optional[bool] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class PauseOrTerminateStrategyResponse(_message.Message):
    __slots__ = ("strategy_id", "state", "canceled_orders_count")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    STATE_FIELD_NUMBER: _ClassVar[int]
    CANCELED_ORDERS_COUNT_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    state: StrategyLifecycleState
    canceled_orders_count: int
    def __init__(self, strategy_id: _Optional[str] = ..., state: _Optional[_Union[StrategyLifecycleState, str]] = ..., canceled_orders_count: _Optional[int] = ...) -> None: ...

class StreamWasmTelemetryRequest(_message.Message):
    __slots__ = ("strategy_id", "include_fuel_reports", "include_traps")
    STRATEGY_ID_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_FUEL_REPORTS_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_TRAPS_FIELD_NUMBER: _ClassVar[int]
    strategy_id: str
    include_fuel_reports: bool
    include_traps: bool
    def __init__(self, strategy_id: _Optional[str] = ..., include_fuel_reports: _Optional[bool] = ..., include_traps: _Optional[bool] = ...) -> None: ...

class WasmTelemetryEnvelope(_message.Message):
    __slots__ = ("ts_ns", "fuel_report", "trap_alert")
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    FUEL_REPORT_FIELD_NUMBER: _ClassVar[int]
    TRAP_ALERT_FIELD_NUMBER: _ClassVar[int]
    ts_ns: int
    fuel_report: FuelMeteringReport
    trap_alert: SandboxExecutionTrap
    def __init__(self, ts_ns: _Optional[int] = ..., fuel_report: _Optional[_Union[FuelMeteringReport, _Mapping]] = ..., trap_alert: _Optional[_Union[SandboxExecutionTrap, _Mapping]] = ...) -> None: ...
