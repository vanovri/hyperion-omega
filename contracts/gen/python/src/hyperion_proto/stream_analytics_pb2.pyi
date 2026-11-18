from . import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class CandleInterval(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    CANDLE_INTERVAL_UNSPECIFIED: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_1S: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_5S: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_1M: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_5M: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_15M: _ClassVar[CandleInterval]
    CANDLE_INTERVAL_1H: _ClassVar[CandleInterval]
CANDLE_INTERVAL_UNSPECIFIED: CandleInterval
CANDLE_INTERVAL_1S: CandleInterval
CANDLE_INTERVAL_5S: CandleInterval
CANDLE_INTERVAL_1M: CandleInterval
CANDLE_INTERVAL_5M: CandleInterval
CANDLE_INTERVAL_15M: CandleInterval
CANDLE_INTERVAL_1H: CandleInterval

class OhlcvCandle(_message.Message):
    __slots__ = ("symbol", "interval", "bucket_open_ts_ns", "bucket_close_ts_ns", "open_fp", "high_fp", "low_fp", "close_fp", "base_volume_fp", "quote_volume_fp", "taker_buy_base_volume_fp", "taker_sell_base_volume_fp", "vwap_fp", "twap_fp", "trade_count", "last_engine_seq", "is_closed")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INTERVAL_FIELD_NUMBER: _ClassVar[int]
    BUCKET_OPEN_TS_NS_FIELD_NUMBER: _ClassVar[int]
    BUCKET_CLOSE_TS_NS_FIELD_NUMBER: _ClassVar[int]
    OPEN_FP_FIELD_NUMBER: _ClassVar[int]
    HIGH_FP_FIELD_NUMBER: _ClassVar[int]
    LOW_FP_FIELD_NUMBER: _ClassVar[int]
    CLOSE_FP_FIELD_NUMBER: _ClassVar[int]
    BASE_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    QUOTE_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    TAKER_BUY_BASE_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    TAKER_SELL_BASE_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    VWAP_FP_FIELD_NUMBER: _ClassVar[int]
    TWAP_FP_FIELD_NUMBER: _ClassVar[int]
    TRADE_COUNT_FIELD_NUMBER: _ClassVar[int]
    LAST_ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    IS_CLOSED_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    interval: CandleInterval
    bucket_open_ts_ns: int
    bucket_close_ts_ns: int
    open_fp: int
    high_fp: int
    low_fp: int
    close_fp: int
    base_volume_fp: _common_pb2.U128
    quote_volume_fp: _common_pb2.U128
    taker_buy_base_volume_fp: _common_pb2.U128
    taker_sell_base_volume_fp: _common_pb2.U128
    vwap_fp: int
    twap_fp: int
    trade_count: int
    last_engine_seq: int
    is_closed: bool
    def __init__(self, symbol: _Optional[str] = ..., interval: _Optional[_Union[CandleInterval, str]] = ..., bucket_open_ts_ns: _Optional[int] = ..., bucket_close_ts_ns: _Optional[int] = ..., open_fp: _Optional[int] = ..., high_fp: _Optional[int] = ..., low_fp: _Optional[int] = ..., close_fp: _Optional[int] = ..., base_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., quote_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., taker_buy_base_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., taker_sell_base_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., vwap_fp: _Optional[int] = ..., twap_fp: _Optional[int] = ..., trade_count: _Optional[int] = ..., last_engine_seq: _Optional[int] = ..., is_closed: _Optional[bool] = ...) -> None: ...

class MicrostructureMetrics(_message.Message):
    __slots__ = ("symbol", "engine_seq", "ts_ns", "mid_price_fp", "spread_fp", "spread_bps", "rolling_vwap_fp", "rolling_twap_fp", "ofi_fp", "book_imbalance_top5_ppm", "book_imbalance_top10_ppm", "book_imbalance_top20_ppm", "kyles_lambda_fp")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    MID_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    SPREAD_FP_FIELD_NUMBER: _ClassVar[int]
    SPREAD_BPS_FIELD_NUMBER: _ClassVar[int]
    ROLLING_VWAP_FP_FIELD_NUMBER: _ClassVar[int]
    ROLLING_TWAP_FP_FIELD_NUMBER: _ClassVar[int]
    OFI_FP_FIELD_NUMBER: _ClassVar[int]
    BOOK_IMBALANCE_TOP5_PPM_FIELD_NUMBER: _ClassVar[int]
    BOOK_IMBALANCE_TOP10_PPM_FIELD_NUMBER: _ClassVar[int]
    BOOK_IMBALANCE_TOP20_PPM_FIELD_NUMBER: _ClassVar[int]
    KYLES_LAMBDA_FP_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    engine_seq: int
    ts_ns: int
    mid_price_fp: int
    spread_fp: int
    spread_bps: int
    rolling_vwap_fp: int
    rolling_twap_fp: int
    ofi_fp: _common_pb2.I128
    book_imbalance_top5_ppm: int
    book_imbalance_top10_ppm: int
    book_imbalance_top20_ppm: int
    kyles_lambda_fp: int
    def __init__(self, symbol: _Optional[str] = ..., engine_seq: _Optional[int] = ..., ts_ns: _Optional[int] = ..., mid_price_fp: _Optional[int] = ..., spread_fp: _Optional[int] = ..., spread_bps: _Optional[int] = ..., rolling_vwap_fp: _Optional[int] = ..., rolling_twap_fp: _Optional[int] = ..., ofi_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., book_imbalance_top5_ppm: _Optional[int] = ..., book_imbalance_top10_ppm: _Optional[int] = ..., book_imbalance_top20_ppm: _Optional[int] = ..., kyles_lambda_fp: _Optional[int] = ...) -> None: ...

class LiquidityHeatmapBucket(_message.Message):
    __slots__ = ("bucket_ts_ns", "price_bucket_fp", "resting_bid_qty_fp", "resting_ask_qty_fp")
    BUCKET_TS_NS_FIELD_NUMBER: _ClassVar[int]
    PRICE_BUCKET_FP_FIELD_NUMBER: _ClassVar[int]
    RESTING_BID_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    RESTING_ASK_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    bucket_ts_ns: int
    price_bucket_fp: int
    resting_bid_qty_fp: int
    resting_ask_qty_fp: int
    def __init__(self, bucket_ts_ns: _Optional[int] = ..., price_bucket_fp: _Optional[int] = ..., resting_bid_qty_fp: _Optional[int] = ..., resting_ask_qty_fp: _Optional[int] = ...) -> None: ...

class VolumeProfileBin(_message.Message):
    __slots__ = ("price_bucket_fp", "buy_volume_fp", "sell_volume_fp", "total_volume_fp")
    PRICE_BUCKET_FP_FIELD_NUMBER: _ClassVar[int]
    BUY_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    SELL_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_VOLUME_FP_FIELD_NUMBER: _ClassVar[int]
    price_bucket_fp: int
    buy_volume_fp: _common_pb2.U128
    sell_volume_fp: _common_pb2.U128
    total_volume_fp: _common_pb2.U128
    def __init__(self, price_bucket_fp: _Optional[int] = ..., buy_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., sell_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., total_volume_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ...) -> None: ...

class GetCandlesRequest(_message.Message):
    __slots__ = ("symbol", "interval", "from_ts_ns", "to_ts_ns", "limit")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INTERVAL_FIELD_NUMBER: _ClassVar[int]
    FROM_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TO_TS_NS_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    interval: CandleInterval
    from_ts_ns: int
    to_ts_ns: int
    limit: int
    def __init__(self, symbol: _Optional[str] = ..., interval: _Optional[_Union[CandleInterval, str]] = ..., from_ts_ns: _Optional[int] = ..., to_ts_ns: _Optional[int] = ..., limit: _Optional[int] = ...) -> None: ...

class GetCandlesResponse(_message.Message):
    __slots__ = ("candles",)
    CANDLES_FIELD_NUMBER: _ClassVar[int]
    candles: _containers.RepeatedCompositeFieldContainer[OhlcvCandle]
    def __init__(self, candles: _Optional[_Iterable[_Union[OhlcvCandle, _Mapping]]] = ...) -> None: ...

class GetVolumeProfileRequest(_message.Message):
    __slots__ = ("symbol", "from_ts_ns", "to_ts_ns", "bucket_size_fp")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    FROM_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TO_TS_NS_FIELD_NUMBER: _ClassVar[int]
    BUCKET_SIZE_FP_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    from_ts_ns: int
    to_ts_ns: int
    bucket_size_fp: int
    def __init__(self, symbol: _Optional[str] = ..., from_ts_ns: _Optional[int] = ..., to_ts_ns: _Optional[int] = ..., bucket_size_fp: _Optional[int] = ...) -> None: ...

class GetVolumeProfileResponse(_message.Message):
    __slots__ = ("symbol", "poc_price_fp", "value_area_high_fp", "value_area_low_fp", "bins")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    POC_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    VALUE_AREA_HIGH_FP_FIELD_NUMBER: _ClassVar[int]
    VALUE_AREA_LOW_FP_FIELD_NUMBER: _ClassVar[int]
    BINS_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    poc_price_fp: int
    value_area_high_fp: int
    value_area_low_fp: int
    bins: _containers.RepeatedCompositeFieldContainer[VolumeProfileBin]
    def __init__(self, symbol: _Optional[str] = ..., poc_price_fp: _Optional[int] = ..., value_area_high_fp: _Optional[int] = ..., value_area_low_fp: _Optional[int] = ..., bins: _Optional[_Iterable[_Union[VolumeProfileBin, _Mapping]]] = ...) -> None: ...

class GetLiquidityHeatmapRequest(_message.Message):
    __slots__ = ("symbol", "from_ts_ns", "to_ts_ns", "price_bucket_size_fp", "time_bucket_ns")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    FROM_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TO_TS_NS_FIELD_NUMBER: _ClassVar[int]
    PRICE_BUCKET_SIZE_FP_FIELD_NUMBER: _ClassVar[int]
    TIME_BUCKET_NS_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    from_ts_ns: int
    to_ts_ns: int
    price_bucket_size_fp: int
    time_bucket_ns: int
    def __init__(self, symbol: _Optional[str] = ..., from_ts_ns: _Optional[int] = ..., to_ts_ns: _Optional[int] = ..., price_bucket_size_fp: _Optional[int] = ..., time_bucket_ns: _Optional[int] = ...) -> None: ...

class GetLiquidityHeatmapResponse(_message.Message):
    __slots__ = ("symbol", "buckets")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    BUCKETS_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    buckets: _containers.RepeatedCompositeFieldContainer[LiquidityHeatmapBucket]
    def __init__(self, symbol: _Optional[str] = ..., buckets: _Optional[_Iterable[_Union[LiquidityHeatmapBucket, _Mapping]]] = ...) -> None: ...

class GetLatestMicrostructureRequest(_message.Message):
    __slots__ = ("symbol",)
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    def __init__(self, symbol: _Optional[str] = ...) -> None: ...

class GetLatestMicrostructureResponse(_message.Message):
    __slots__ = ("metrics",)
    METRICS_FIELD_NUMBER: _ClassVar[int]
    metrics: MicrostructureMetrics
    def __init__(self, metrics: _Optional[_Union[MicrostructureMetrics, _Mapping]] = ...) -> None: ...
