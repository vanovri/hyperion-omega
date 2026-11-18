from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from typing import ClassVar as _ClassVar, Optional as _Optional

DESCRIPTOR: _descriptor.FileDescriptor

class OrderSide(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ORDER_SIDE_UNSPECIFIED: _ClassVar[OrderSide]
    ORDER_SIDE_BUY: _ClassVar[OrderSide]
    ORDER_SIDE_SELL: _ClassVar[OrderSide]

class OrderType(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ORDER_TYPE_UNSPECIFIED: _ClassVar[OrderType]
    ORDER_TYPE_LIMIT: _ClassVar[OrderType]
    ORDER_TYPE_MARKET: _ClassVar[OrderType]
    ORDER_TYPE_IOC: _ClassVar[OrderType]
    ORDER_TYPE_FOK: _ClassVar[OrderType]
    ORDER_TYPE_POST_ONLY: _ClassVar[OrderType]

class SelfTradePreventionMode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    SELF_TRADE_PREVENTION_MODE_UNSPECIFIED: _ClassVar[SelfTradePreventionMode]
    SELF_TRADE_PREVENTION_MODE_NONE: _ClassVar[SelfTradePreventionMode]
    SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING: _ClassVar[SelfTradePreventionMode]
    SELF_TRADE_PREVENTION_MODE_CANCEL_AGGRESSING: _ClassVar[SelfTradePreventionMode]
    SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH: _ClassVar[SelfTradePreventionMode]

class AccountStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ACCOUNT_STATUS_UNSPECIFIED: _ClassVar[AccountStatus]
    ACCOUNT_STATUS_ACTIVE: _ClassVar[AccountStatus]
    ACCOUNT_STATUS_THROTTLED: _ClassVar[AccountStatus]
    ACCOUNT_STATUS_FROZEN: _ClassVar[AccountStatus]
    ACCOUNT_STATUS_LIQUIDATION_ONLY: _ClassVar[AccountStatus]

class MarketRegime(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MARKET_REGIME_UNSPECIFIED: _ClassVar[MarketRegime]
    MARKET_REGIME_NORMAL: _ClassVar[MarketRegime]
    MARKET_REGIME_VOLATILITY_GUARD: _ClassVar[MarketRegime]
    MARKET_REGIME_POST_ONLY_AUCTION: _ClassVar[MarketRegime]
    MARKET_REGIME_HALTED: _ClassVar[MarketRegime]
ORDER_SIDE_UNSPECIFIED: OrderSide
ORDER_SIDE_BUY: OrderSide
ORDER_SIDE_SELL: OrderSide
ORDER_TYPE_UNSPECIFIED: OrderType
ORDER_TYPE_LIMIT: OrderType
ORDER_TYPE_MARKET: OrderType
ORDER_TYPE_IOC: OrderType
ORDER_TYPE_FOK: OrderType
ORDER_TYPE_POST_ONLY: OrderType
SELF_TRADE_PREVENTION_MODE_UNSPECIFIED: SelfTradePreventionMode
SELF_TRADE_PREVENTION_MODE_NONE: SelfTradePreventionMode
SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING: SelfTradePreventionMode
SELF_TRADE_PREVENTION_MODE_CANCEL_AGGRESSING: SelfTradePreventionMode
SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH: SelfTradePreventionMode
ACCOUNT_STATUS_UNSPECIFIED: AccountStatus
ACCOUNT_STATUS_ACTIVE: AccountStatus
ACCOUNT_STATUS_THROTTLED: AccountStatus
ACCOUNT_STATUS_FROZEN: AccountStatus
ACCOUNT_STATUS_LIQUIDATION_ONLY: AccountStatus
MARKET_REGIME_UNSPECIFIED: MarketRegime
MARKET_REGIME_NORMAL: MarketRegime
MARKET_REGIME_VOLATILITY_GUARD: MarketRegime
MARKET_REGIME_POST_ONLY_AUCTION: MarketRegime
MARKET_REGIME_HALTED: MarketRegime

class I128(_message.Message):
    __slots__ = ("hi", "lo")
    HI_FIELD_NUMBER: _ClassVar[int]
    LO_FIELD_NUMBER: _ClassVar[int]
    hi: int
    lo: int
    def __init__(self, hi: _Optional[int] = ..., lo: _Optional[int] = ...) -> None: ...

class U128(_message.Message):
    __slots__ = ("hi", "lo")
    HI_FIELD_NUMBER: _ClassVar[int]
    LO_FIELD_NUMBER: _ClassVar[int]
    hi: int
    lo: int
    def __init__(self, hi: _Optional[int] = ..., lo: _Optional[int] = ...) -> None: ...

class TraceMetadata(_message.Message):
    __slots__ = ("trace_id", "span_id", "correlation_id", "source_service", "ingest_ts_ns")
    TRACE_ID_FIELD_NUMBER: _ClassVar[int]
    SPAN_ID_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_SERVICE_FIELD_NUMBER: _ClassVar[int]
    INGEST_TS_NS_FIELD_NUMBER: _ClassVar[int]
    trace_id: str
    span_id: str
    correlation_id: str
    source_service: str
    ingest_ts_ns: int
    def __init__(self, trace_id: _Optional[str] = ..., span_id: _Optional[str] = ..., correlation_id: _Optional[str] = ..., source_service: _Optional[str] = ..., ingest_ts_ns: _Optional[int] = ...) -> None: ...
