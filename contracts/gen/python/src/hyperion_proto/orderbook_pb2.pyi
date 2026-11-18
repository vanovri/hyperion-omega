from . import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class OrderStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ORDER_STATUS_UNSPECIFIED: _ClassVar[OrderStatus]
    ORDER_STATUS_ACCEPTED: _ClassVar[OrderStatus]
    ORDER_STATUS_PARTIALLY_FILLED: _ClassVar[OrderStatus]
    ORDER_STATUS_FILLED: _ClassVar[OrderStatus]
    ORDER_STATUS_CANCELED: _ClassVar[OrderStatus]
    ORDER_STATUS_MODIFIED: _ClassVar[OrderStatus]
    ORDER_STATUS_REJECTED: _ClassVar[OrderStatus]
    ORDER_STATUS_EXPIRED: _ClassVar[OrderStatus]

class RejectReason(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    REJECT_REASON_UNSPECIFIED: _ClassVar[RejectReason]
    REJECT_REASON_NONE: _ClassVar[RejectReason]
    REJECT_REASON_INSUFFICIENT_BALANCE: _ClassVar[RejectReason]
    REJECT_REASON_POST_ONLY_WOULD_CROSS: _ClassVar[RejectReason]
    REJECT_REASON_FOK_INSUFFICIENT_LIQUIDITY: _ClassVar[RejectReason]
    REJECT_REASON_SELF_TRADE_PREVENTED: _ClassVar[RejectReason]
    REJECT_REASON_ACCOUNT_FROZEN: _ClassVar[RejectReason]
    REJECT_REASON_MARKET_HALTED: _ClassVar[RejectReason]
    REJECT_REASON_INVALID_PRICE_OR_QTY: _ClassVar[RejectReason]
    REJECT_REASON_DUPLICATE_IDEMPOTENCY_KEY: _ClassVar[RejectReason]
    REJECT_REASON_ORDER_NOT_FOUND: _ClassVar[RejectReason]
    REJECT_REASON_MAX_ORDER_SIZE_EXCEEDED: _ClassVar[RejectReason]
    REJECT_REASON_RATE_LIMITED: _ClassVar[RejectReason]
    REJECT_REASON_INVALID_SIGNATURE_OR_NONCE: _ClassVar[RejectReason]

class L3DeltaOp(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    L3_DELTA_OP_UNSPECIFIED: _ClassVar[L3DeltaOp]
    L3_DELTA_OP_ADD: _ClassVar[L3DeltaOp]
    L3_DELTA_OP_MODIFY: _ClassVar[L3DeltaOp]
    L3_DELTA_OP_CANCEL: _ClassVar[L3DeltaOp]
    L3_DELTA_OP_FILL: _ClassVar[L3DeltaOp]
    L3_DELTA_OP_CLEAR: _ClassVar[L3DeltaOp]
ORDER_STATUS_UNSPECIFIED: OrderStatus
ORDER_STATUS_ACCEPTED: OrderStatus
ORDER_STATUS_PARTIALLY_FILLED: OrderStatus
ORDER_STATUS_FILLED: OrderStatus
ORDER_STATUS_CANCELED: OrderStatus
ORDER_STATUS_MODIFIED: OrderStatus
ORDER_STATUS_REJECTED: OrderStatus
ORDER_STATUS_EXPIRED: OrderStatus
REJECT_REASON_UNSPECIFIED: RejectReason
REJECT_REASON_NONE: RejectReason
REJECT_REASON_INSUFFICIENT_BALANCE: RejectReason
REJECT_REASON_POST_ONLY_WOULD_CROSS: RejectReason
REJECT_REASON_FOK_INSUFFICIENT_LIQUIDITY: RejectReason
REJECT_REASON_SELF_TRADE_PREVENTED: RejectReason
REJECT_REASON_ACCOUNT_FROZEN: RejectReason
REJECT_REASON_MARKET_HALTED: RejectReason
REJECT_REASON_INVALID_PRICE_OR_QTY: RejectReason
REJECT_REASON_DUPLICATE_IDEMPOTENCY_KEY: RejectReason
REJECT_REASON_ORDER_NOT_FOUND: RejectReason
REJECT_REASON_MAX_ORDER_SIZE_EXCEEDED: RejectReason
REJECT_REASON_RATE_LIMITED: RejectReason
REJECT_REASON_INVALID_SIGNATURE_OR_NONCE: RejectReason
L3_DELTA_OP_UNSPECIFIED: L3DeltaOp
L3_DELTA_OP_ADD: L3DeltaOp
L3_DELTA_OP_MODIFY: L3DeltaOp
L3_DELTA_OP_CANCEL: L3DeltaOp
L3_DELTA_OP_FILL: L3DeltaOp
L3_DELTA_OP_CLEAR: L3DeltaOp

class SubmitOrder(_message.Message):
    __slots__ = ("client_order_id", "account_id", "symbol", "instrument_id", "side", "order_type", "stp_mode", "price_fp", "qty_fp", "max_slippage_bps", "client_ts_ns", "nonce", "signature", "idempotency_key", "trace")
    CLIENT_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    SIDE_FIELD_NUMBER: _ClassVar[int]
    ORDER_TYPE_FIELD_NUMBER: _ClassVar[int]
    STP_MODE_FIELD_NUMBER: _ClassVar[int]
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    QTY_FP_FIELD_NUMBER: _ClassVar[int]
    MAX_SLIPPAGE_BPS_FIELD_NUMBER: _ClassVar[int]
    CLIENT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    NONCE_FIELD_NUMBER: _ClassVar[int]
    SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    client_order_id: str
    account_id: int
    symbol: str
    instrument_id: int
    side: _common_pb2.OrderSide
    order_type: _common_pb2.OrderType
    stp_mode: _common_pb2.SelfTradePreventionMode
    price_fp: int
    qty_fp: int
    max_slippage_bps: int
    client_ts_ns: int
    nonce: int
    signature: bytes
    idempotency_key: str
    trace: _common_pb2.TraceMetadata
    def __init__(self, client_order_id: _Optional[str] = ..., account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., order_type: _Optional[_Union[_common_pb2.OrderType, str]] = ..., stp_mode: _Optional[_Union[_common_pb2.SelfTradePreventionMode, str]] = ..., price_fp: _Optional[int] = ..., qty_fp: _Optional[int] = ..., max_slippage_bps: _Optional[int] = ..., client_ts_ns: _Optional[int] = ..., nonce: _Optional[int] = ..., signature: _Optional[bytes] = ..., idempotency_key: _Optional[str] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class CancelOrder(_message.Message):
    __slots__ = ("order_id", "client_order_id", "account_id", "symbol", "instrument_id", "client_ts_ns", "nonce", "signature", "idempotency_key", "trace")
    ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    CLIENT_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    CLIENT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    NONCE_FIELD_NUMBER: _ClassVar[int]
    SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    order_id: int
    client_order_id: str
    account_id: int
    symbol: str
    instrument_id: int
    client_ts_ns: int
    nonce: int
    signature: bytes
    idempotency_key: str
    trace: _common_pb2.TraceMetadata
    def __init__(self, order_id: _Optional[int] = ..., client_order_id: _Optional[str] = ..., account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., client_ts_ns: _Optional[int] = ..., nonce: _Optional[int] = ..., signature: _Optional[bytes] = ..., idempotency_key: _Optional[str] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ModifyOrder(_message.Message):
    __slots__ = ("order_id", "client_order_id", "account_id", "symbol", "instrument_id", "new_price_fp", "new_qty_fp", "client_ts_ns", "nonce", "signature", "idempotency_key", "trace")
    ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    CLIENT_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    NEW_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    NEW_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    CLIENT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    NONCE_FIELD_NUMBER: _ClassVar[int]
    SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    order_id: int
    client_order_id: str
    account_id: int
    symbol: str
    instrument_id: int
    new_price_fp: int
    new_qty_fp: int
    client_ts_ns: int
    nonce: int
    signature: bytes
    idempotency_key: str
    trace: _common_pb2.TraceMetadata
    def __init__(self, order_id: _Optional[int] = ..., client_order_id: _Optional[str] = ..., account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., new_price_fp: _Optional[int] = ..., new_qty_fp: _Optional[int] = ..., client_ts_ns: _Optional[int] = ..., nonce: _Optional[int] = ..., signature: _Optional[bytes] = ..., idempotency_key: _Optional[str] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ExecutionFill(_message.Message):
    __slots__ = ("fill_id", "engine_seq", "symbol", "instrument_id", "maker_order_id", "taker_order_id", "maker_account_id", "taker_account_id", "aggressor_side", "price_fp", "qty_fp", "notional_fp", "maker_fee_fp", "taker_fee_fp", "exec_ts_ns", "wal_blake3_hash", "trace")
    FILL_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    MAKER_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    TAKER_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    MAKER_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    TAKER_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    AGGRESSOR_SIDE_FIELD_NUMBER: _ClassVar[int]
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    QTY_FP_FIELD_NUMBER: _ClassVar[int]
    NOTIONAL_FP_FIELD_NUMBER: _ClassVar[int]
    MAKER_FEE_FP_FIELD_NUMBER: _ClassVar[int]
    TAKER_FEE_FP_FIELD_NUMBER: _ClassVar[int]
    EXEC_TS_NS_FIELD_NUMBER: _ClassVar[int]
    WAL_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    fill_id: int
    engine_seq: int
    symbol: str
    instrument_id: int
    maker_order_id: int
    taker_order_id: int
    maker_account_id: int
    taker_account_id: int
    aggressor_side: _common_pb2.OrderSide
    price_fp: int
    qty_fp: int
    notional_fp: _common_pb2.U128
    maker_fee_fp: int
    taker_fee_fp: int
    exec_ts_ns: int
    wal_blake3_hash: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, fill_id: _Optional[int] = ..., engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., maker_order_id: _Optional[int] = ..., taker_order_id: _Optional[int] = ..., maker_account_id: _Optional[int] = ..., taker_account_id: _Optional[int] = ..., aggressor_side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., price_fp: _Optional[int] = ..., qty_fp: _Optional[int] = ..., notional_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., maker_fee_fp: _Optional[int] = ..., taker_fee_fp: _Optional[int] = ..., exec_ts_ns: _Optional[int] = ..., wal_blake3_hash: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class OrderAck(_message.Message):
    __slots__ = ("engine_seq", "order_id", "client_order_id", "account_id", "symbol", "instrument_id", "status", "reject_reason", "reject_detail", "filled_qty_fp", "remaining_qty_fp", "avg_fill_price_fp", "engine_ts_ns", "wal_blake3_hash", "fills", "trace")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    CLIENT_ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    REJECT_REASON_FIELD_NUMBER: _ClassVar[int]
    REJECT_DETAIL_FIELD_NUMBER: _ClassVar[int]
    FILLED_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    REMAINING_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    AVG_FILL_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    ENGINE_TS_NS_FIELD_NUMBER: _ClassVar[int]
    WAL_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    FILLS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    order_id: int
    client_order_id: str
    account_id: int
    symbol: str
    instrument_id: int
    status: OrderStatus
    reject_reason: RejectReason
    reject_detail: str
    filled_qty_fp: int
    remaining_qty_fp: int
    avg_fill_price_fp: int
    engine_ts_ns: int
    wal_blake3_hash: bytes
    fills: _containers.RepeatedCompositeFieldContainer[ExecutionFill]
    trace: _common_pb2.TraceMetadata
    def __init__(self, engine_seq: _Optional[int] = ..., order_id: _Optional[int] = ..., client_order_id: _Optional[str] = ..., account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., status: _Optional[_Union[OrderStatus, str]] = ..., reject_reason: _Optional[_Union[RejectReason, str]] = ..., reject_detail: _Optional[str] = ..., filled_qty_fp: _Optional[int] = ..., remaining_qty_fp: _Optional[int] = ..., avg_fill_price_fp: _Optional[int] = ..., engine_ts_ns: _Optional[int] = ..., wal_blake3_hash: _Optional[bytes] = ..., fills: _Optional[_Iterable[_Union[ExecutionFill, _Mapping]]] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class PriceLevel(_message.Message):
    __slots__ = ("price_fp", "total_qty_fp", "order_count")
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    ORDER_COUNT_FIELD_NUMBER: _ClassVar[int]
    price_fp: int
    total_qty_fp: int
    order_count: int
    def __init__(self, price_fp: _Optional[int] = ..., total_qty_fp: _Optional[int] = ..., order_count: _Optional[int] = ...) -> None: ...

class L2BookSnapshot(_message.Message):
    __slots__ = ("engine_seq", "symbol", "instrument_id", "bids", "asks", "best_bid_fp", "best_ask_fp", "mid_price_fp", "spread_fp", "snapshot_ts_ns", "state_root_blake3")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    BIDS_FIELD_NUMBER: _ClassVar[int]
    ASKS_FIELD_NUMBER: _ClassVar[int]
    BEST_BID_FP_FIELD_NUMBER: _ClassVar[int]
    BEST_ASK_FP_FIELD_NUMBER: _ClassVar[int]
    MID_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    SPREAD_FP_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    symbol: str
    instrument_id: int
    bids: _containers.RepeatedCompositeFieldContainer[PriceLevel]
    asks: _containers.RepeatedCompositeFieldContainer[PriceLevel]
    best_bid_fp: int
    best_ask_fp: int
    mid_price_fp: int
    spread_fp: int
    snapshot_ts_ns: int
    state_root_blake3: bytes
    def __init__(self, engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., bids: _Optional[_Iterable[_Union[PriceLevel, _Mapping]]] = ..., asks: _Optional[_Iterable[_Union[PriceLevel, _Mapping]]] = ..., best_bid_fp: _Optional[int] = ..., best_ask_fp: _Optional[int] = ..., mid_price_fp: _Optional[int] = ..., spread_fp: _Optional[int] = ..., snapshot_ts_ns: _Optional[int] = ..., state_root_blake3: _Optional[bytes] = ...) -> None: ...

class L3BookDelta(_message.Message):
    __slots__ = ("engine_seq", "prev_engine_seq", "symbol", "instrument_id", "op", "side", "order_id", "account_id", "price_fp", "remaining_qty_fp", "delta_qty_fp", "event_ts_ns", "state_root_blake3", "trace")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    PREV_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    OP_FIELD_NUMBER: _ClassVar[int]
    SIDE_FIELD_NUMBER: _ClassVar[int]
    ORDER_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    REMAINING_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    DELTA_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    EVENT_TS_NS_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    prev_engine_seq: int
    symbol: str
    instrument_id: int
    op: L3DeltaOp
    side: _common_pb2.OrderSide
    order_id: int
    account_id: int
    price_fp: int
    remaining_qty_fp: int
    delta_qty_fp: int
    event_ts_ns: int
    state_root_blake3: bytes
    trace: _common_pb2.TraceMetadata
    def __init__(self, engine_seq: _Optional[int] = ..., prev_engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., instrument_id: _Optional[int] = ..., op: _Optional[_Union[L3DeltaOp, str]] = ..., side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., order_id: _Optional[int] = ..., account_id: _Optional[int] = ..., price_fp: _Optional[int] = ..., remaining_qty_fp: _Optional[int] = ..., delta_qty_fp: _Optional[int] = ..., event_ts_ns: _Optional[int] = ..., state_root_blake3: _Optional[bytes] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class LedgerEntryLine(_message.Message):
    __slots__ = ("account_id", "asset", "sub_account", "debit_fp", "credit_fp", "balance_after_fp")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    SUB_ACCOUNT_FIELD_NUMBER: _ClassVar[int]
    DEBIT_FP_FIELD_NUMBER: _ClassVar[int]
    CREDIT_FP_FIELD_NUMBER: _ClassVar[int]
    BALANCE_AFTER_FP_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    asset: str
    sub_account: str
    debit_fp: _common_pb2.I128
    credit_fp: _common_pb2.I128
    balance_after_fp: _common_pb2.I128
    def __init__(self, account_id: _Optional[int] = ..., asset: _Optional[str] = ..., sub_account: _Optional[str] = ..., debit_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., credit_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., balance_after_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ...) -> None: ...

class LedgerJournalEntry(_message.Message):
    __slots__ = ("journal_seq", "engine_seq", "reference_type", "reference_id", "lines", "ts_ns", "prev_journal_blake3", "journal_blake3")
    JOURNAL_SEQ_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    REFERENCE_TYPE_FIELD_NUMBER: _ClassVar[int]
    REFERENCE_ID_FIELD_NUMBER: _ClassVar[int]
    LINES_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    PREV_JOURNAL_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    JOURNAL_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    journal_seq: int
    engine_seq: int
    reference_type: str
    reference_id: int
    lines: _containers.RepeatedCompositeFieldContainer[LedgerEntryLine]
    ts_ns: int
    prev_journal_blake3: bytes
    journal_blake3: bytes
    def __init__(self, journal_seq: _Optional[int] = ..., engine_seq: _Optional[int] = ..., reference_type: _Optional[str] = ..., reference_id: _Optional[int] = ..., lines: _Optional[_Iterable[_Union[LedgerEntryLine, _Mapping]]] = ..., ts_ns: _Optional[int] = ..., prev_journal_blake3: _Optional[bytes] = ..., journal_blake3: _Optional[bytes] = ...) -> None: ...

class MerkleProofNode(_message.Message):
    __slots__ = ("sibling_hash", "is_left_sibling", "depth")
    SIBLING_HASH_FIELD_NUMBER: _ClassVar[int]
    IS_LEFT_SIBLING_FIELD_NUMBER: _ClassVar[int]
    DEPTH_FIELD_NUMBER: _ClassVar[int]
    sibling_hash: bytes
    is_left_sibling: bool
    depth: int
    def __init__(self, sibling_hash: _Optional[bytes] = ..., is_left_sibling: _Optional[bool] = ..., depth: _Optional[int] = ...) -> None: ...

class AccountAssetBalance(_message.Message):
    __slots__ = ("asset", "available_fp", "locked_fp", "total_fp")
    ASSET_FIELD_NUMBER: _ClassVar[int]
    AVAILABLE_FP_FIELD_NUMBER: _ClassVar[int]
    LOCKED_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_FP_FIELD_NUMBER: _ClassVar[int]
    asset: str
    available_fp: _common_pb2.I128
    locked_fp: _common_pb2.I128
    total_fp: _common_pb2.I128
    def __init__(self, asset: _Optional[str] = ..., available_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., locked_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., total_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ...) -> None: ...

class MerkleLedgerProof(_message.Message):
    __slots__ = ("engine_seq", "account_id", "balances", "leaf_blake3_hash", "state_root_blake3", "proof_path", "system_total_deposits_fp", "system_total_user_equity_fp", "treasury_fee_pool_fp", "double_entry_balanced", "proof_ts_ns")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    BALANCES_FIELD_NUMBER: _ClassVar[int]
    LEAF_BLAKE3_HASH_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    PROOF_PATH_FIELD_NUMBER: _ClassVar[int]
    SYSTEM_TOTAL_DEPOSITS_FP_FIELD_NUMBER: _ClassVar[int]
    SYSTEM_TOTAL_USER_EQUITY_FP_FIELD_NUMBER: _ClassVar[int]
    TREASURY_FEE_POOL_FP_FIELD_NUMBER: _ClassVar[int]
    DOUBLE_ENTRY_BALANCED_FIELD_NUMBER: _ClassVar[int]
    PROOF_TS_NS_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    account_id: int
    balances: _containers.RepeatedCompositeFieldContainer[AccountAssetBalance]
    leaf_blake3_hash: bytes
    state_root_blake3: bytes
    proof_path: _containers.RepeatedCompositeFieldContainer[MerkleProofNode]
    system_total_deposits_fp: _common_pb2.U128
    system_total_user_equity_fp: _common_pb2.U128
    treasury_fee_pool_fp: _common_pb2.I128
    double_entry_balanced: bool
    proof_ts_ns: int
    def __init__(self, engine_seq: _Optional[int] = ..., account_id: _Optional[int] = ..., balances: _Optional[_Iterable[_Union[AccountAssetBalance, _Mapping]]] = ..., leaf_blake3_hash: _Optional[bytes] = ..., state_root_blake3: _Optional[bytes] = ..., proof_path: _Optional[_Iterable[_Union[MerkleProofNode, _Mapping]]] = ..., system_total_deposits_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., system_total_user_equity_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., treasury_fee_pool_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., double_entry_balanced: _Optional[bool] = ..., proof_ts_ns: _Optional[int] = ...) -> None: ...

class SubmitOrderRequest(_message.Message):
    __slots__ = ("order",)
    ORDER_FIELD_NUMBER: _ClassVar[int]
    order: SubmitOrder
    def __init__(self, order: _Optional[_Union[SubmitOrder, _Mapping]] = ...) -> None: ...

class SubmitOrderResponse(_message.Message):
    __slots__ = ("ack",)
    ACK_FIELD_NUMBER: _ClassVar[int]
    ack: OrderAck
    def __init__(self, ack: _Optional[_Union[OrderAck, _Mapping]] = ...) -> None: ...

class CancelOrderRequest(_message.Message):
    __slots__ = ("cancel",)
    CANCEL_FIELD_NUMBER: _ClassVar[int]
    cancel: CancelOrder
    def __init__(self, cancel: _Optional[_Union[CancelOrder, _Mapping]] = ...) -> None: ...

class CancelOrderResponse(_message.Message):
    __slots__ = ("ack",)
    ACK_FIELD_NUMBER: _ClassVar[int]
    ack: OrderAck
    def __init__(self, ack: _Optional[_Union[OrderAck, _Mapping]] = ...) -> None: ...

class ModifyOrderRequest(_message.Message):
    __slots__ = ("modify",)
    MODIFY_FIELD_NUMBER: _ClassVar[int]
    modify: ModifyOrder
    def __init__(self, modify: _Optional[_Union[ModifyOrder, _Mapping]] = ...) -> None: ...

class ModifyOrderResponse(_message.Message):
    __slots__ = ("ack",)
    ACK_FIELD_NUMBER: _ClassVar[int]
    ack: OrderAck
    def __init__(self, ack: _Optional[_Union[OrderAck, _Mapping]] = ...) -> None: ...

class CreditDepositRequest(_message.Message):
    __slots__ = ("deposit_id", "account_id", "asset", "amount_fp", "ts_ns", "trace")
    DEPOSIT_ID_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    AMOUNT_FP_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    deposit_id: str
    account_id: int
    asset: str
    amount_fp: _common_pb2.U128
    ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, deposit_id: _Optional[str] = ..., account_id: _Optional[int] = ..., asset: _Optional[str] = ..., amount_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class CreditDepositResponse(_message.Message):
    __slots__ = ("engine_seq", "journal_seq", "updated_balance", "state_root_blake3")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    JOURNAL_SEQ_FIELD_NUMBER: _ClassVar[int]
    UPDATED_BALANCE_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    journal_seq: int
    updated_balance: AccountAssetBalance
    state_root_blake3: bytes
    def __init__(self, engine_seq: _Optional[int] = ..., journal_seq: _Optional[int] = ..., updated_balance: _Optional[_Union[AccountAssetBalance, _Mapping]] = ..., state_root_blake3: _Optional[bytes] = ...) -> None: ...

class GetL2BookSnapshotRequest(_message.Message):
    __slots__ = ("symbol", "depth")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    DEPTH_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    depth: int
    def __init__(self, symbol: _Optional[str] = ..., depth: _Optional[int] = ...) -> None: ...

class GetL2BookSnapshotResponse(_message.Message):
    __slots__ = ("snapshot",)
    SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    snapshot: L2BookSnapshot
    def __init__(self, snapshot: _Optional[_Union[L2BookSnapshot, _Mapping]] = ...) -> None: ...

class GetMerkleLedgerProofRequest(_message.Message):
    __slots__ = ("account_id", "at_engine_seq")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    AT_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    at_engine_seq: int
    def __init__(self, account_id: _Optional[int] = ..., at_engine_seq: _Optional[int] = ...) -> None: ...

class GetMerkleLedgerProofResponse(_message.Message):
    __slots__ = ("proof",)
    PROOF_FIELD_NUMBER: _ClassVar[int]
    proof: MerkleLedgerProof
    def __init__(self, proof: _Optional[_Union[MerkleLedgerProof, _Mapping]] = ...) -> None: ...

class VerifySystemLedgerInvariantRequest(_message.Message):
    __slots__ = ("asset",)
    ASSET_FIELD_NUMBER: _ClassVar[int]
    asset: str
    def __init__(self, asset: _Optional[str] = ...) -> None: ...

class VerifySystemLedgerInvariantResponse(_message.Message):
    __slots__ = ("engine_seq", "asset", "total_deposits_fp", "total_available_fp", "total_locked_fp", "treasury_fee_pool_fp", "invariant_holds", "crossed_book_free", "state_root_blake3", "verified_ts_ns")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ASSET_FIELD_NUMBER: _ClassVar[int]
    TOTAL_DEPOSITS_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_AVAILABLE_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_LOCKED_FP_FIELD_NUMBER: _ClassVar[int]
    TREASURY_FEE_POOL_FP_FIELD_NUMBER: _ClassVar[int]
    INVARIANT_HOLDS_FIELD_NUMBER: _ClassVar[int]
    CROSSED_BOOK_FREE_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    VERIFIED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    asset: str
    total_deposits_fp: _common_pb2.U128
    total_available_fp: _common_pb2.U128
    total_locked_fp: _common_pb2.U128
    treasury_fee_pool_fp: _common_pb2.I128
    invariant_holds: bool
    crossed_book_free: bool
    state_root_blake3: bytes
    verified_ts_ns: int
    def __init__(self, engine_seq: _Optional[int] = ..., asset: _Optional[str] = ..., total_deposits_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., total_available_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., total_locked_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., treasury_fee_pool_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., invariant_holds: _Optional[bool] = ..., crossed_book_free: _Optional[bool] = ..., state_root_blake3: _Optional[bytes] = ..., verified_ts_ns: _Optional[int] = ...) -> None: ...

class ApplyAccountRiskActionRequest(_message.Message):
    __slots__ = ("account_id", "new_status", "symbol_regime_override", "target_symbol", "cancel_all_open_orders", "command_id", "reason", "trace")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    NEW_STATUS_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_REGIME_OVERRIDE_FIELD_NUMBER: _ClassVar[int]
    TARGET_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    CANCEL_ALL_OPEN_ORDERS_FIELD_NUMBER: _ClassVar[int]
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    REASON_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    new_status: _common_pb2.AccountStatus
    symbol_regime_override: _common_pb2.MarketRegime
    target_symbol: str
    cancel_all_open_orders: bool
    command_id: str
    reason: str
    trace: _common_pb2.TraceMetadata
    def __init__(self, account_id: _Optional[int] = ..., new_status: _Optional[_Union[_common_pb2.AccountStatus, str]] = ..., symbol_regime_override: _Optional[_Union[_common_pb2.MarketRegime, str]] = ..., target_symbol: _Optional[str] = ..., cancel_all_open_orders: _Optional[bool] = ..., command_id: _Optional[str] = ..., reason: _Optional[str] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ApplyAccountRiskActionResponse(_message.Message):
    __slots__ = ("applied", "engine_seq", "canceled_orders_count", "effective_status", "state_root_blake3")
    APPLIED_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    CANCELED_ORDERS_COUNT_FIELD_NUMBER: _ClassVar[int]
    EFFECTIVE_STATUS_FIELD_NUMBER: _ClassVar[int]
    STATE_ROOT_BLAKE3_FIELD_NUMBER: _ClassVar[int]
    applied: bool
    engine_seq: int
    canceled_orders_count: int
    effective_status: _common_pb2.AccountStatus
    state_root_blake3: bytes
    def __init__(self, applied: _Optional[bool] = ..., engine_seq: _Optional[int] = ..., canceled_orders_count: _Optional[int] = ..., effective_status: _Optional[_Union[_common_pb2.AccountStatus, str]] = ..., state_root_blake3: _Optional[bytes] = ...) -> None: ...

class StreamMarketEventsRequest(_message.Message):
    __slots__ = ("symbols", "from_engine_seq", "include_l3_deltas", "include_fills")
    SYMBOLS_FIELD_NUMBER: _ClassVar[int]
    FROM_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_L3_DELTAS_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_FILLS_FIELD_NUMBER: _ClassVar[int]
    symbols: _containers.RepeatedScalarFieldContainer[str]
    from_engine_seq: int
    include_l3_deltas: bool
    include_fills: bool
    def __init__(self, symbols: _Optional[_Iterable[str]] = ..., from_engine_seq: _Optional[int] = ..., include_l3_deltas: _Optional[bool] = ..., include_fills: _Optional[bool] = ...) -> None: ...

class MarketEventEnvelope(_message.Message):
    __slots__ = ("engine_seq", "symbol", "ts_ns", "l3_delta", "fill", "l2_snapshot", "order_ack", "journal_entry")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    L3_DELTA_FIELD_NUMBER: _ClassVar[int]
    FILL_FIELD_NUMBER: _ClassVar[int]
    L2_SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    ORDER_ACK_FIELD_NUMBER: _ClassVar[int]
    JOURNAL_ENTRY_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    symbol: str
    ts_ns: int
    l3_delta: L3BookDelta
    fill: ExecutionFill
    l2_snapshot: L2BookSnapshot
    order_ack: OrderAck
    journal_entry: LedgerJournalEntry
    def __init__(self, engine_seq: _Optional[int] = ..., symbol: _Optional[str] = ..., ts_ns: _Optional[int] = ..., l3_delta: _Optional[_Union[L3BookDelta, _Mapping]] = ..., fill: _Optional[_Union[ExecutionFill, _Mapping]] = ..., l2_snapshot: _Optional[_Union[L2BookSnapshot, _Mapping]] = ..., order_ack: _Optional[_Union[OrderAck, _Mapping]] = ..., journal_entry: _Optional[_Union[LedgerJournalEntry, _Mapping]] = ...) -> None: ...
