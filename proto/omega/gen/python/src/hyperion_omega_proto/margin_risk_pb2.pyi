from hyperion_proto import common_pb2 as _common_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class OptionRight(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    OPTION_RIGHT_UNSPECIFIED: _ClassVar[OptionRight]
    OPTION_RIGHT_CALL: _ClassVar[OptionRight]
    OPTION_RIGHT_PUT: _ClassVar[OptionRight]

class OptionExerciseStyle(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    OPTION_EXERCISE_STYLE_UNSPECIFIED: _ClassVar[OptionExerciseStyle]
    OPTION_EXERCISE_STYLE_EUROPEAN: _ClassVar[OptionExerciseStyle]
    OPTION_EXERCISE_STYLE_AMERICAN: _ClassVar[OptionExerciseStyle]

class MarginHealthStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MARGIN_HEALTH_STATUS_UNSPECIFIED: _ClassVar[MarginHealthStatus]
    MARGIN_HEALTH_STATUS_HEALTHY: _ClassVar[MarginHealthStatus]
    MARGIN_HEALTH_STATUS_INITIAL_MARGIN_BREACH: _ClassVar[MarginHealthStatus]
    MARGIN_HEALTH_STATUS_MAINTENANCE_BREACH_LIQUIDATABLE: _ClassVar[MarginHealthStatus]
    MARGIN_HEALTH_STATUS_BANKRUPT_DEFICIT: _ClassVar[MarginHealthStatus]

class LiquidationStage(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    LIQUIDATION_STAGE_UNSPECIFIED: _ClassVar[LiquidationStage]
    LIQUIDATION_STAGE_CANCEL_OPEN_MARGIN_ORDERS: _ClassVar[LiquidationStage]
    LIQUIDATION_STAGE_PARTIAL_BOOK_LIQUIDATION: _ClassVar[LiquidationStage]
    LIQUIDATION_STAGE_INSURANCE_FUND_ABSORPTION: _ClassVar[LiquidationStage]
    LIQUIDATION_STAGE_AUTO_DELEVERAGING_ADL: _ClassVar[LiquidationStage]
OPTION_RIGHT_UNSPECIFIED: OptionRight
OPTION_RIGHT_CALL: OptionRight
OPTION_RIGHT_PUT: OptionRight
OPTION_EXERCISE_STYLE_UNSPECIFIED: OptionExerciseStyle
OPTION_EXERCISE_STYLE_EUROPEAN: OptionExerciseStyle
OPTION_EXERCISE_STYLE_AMERICAN: OptionExerciseStyle
MARGIN_HEALTH_STATUS_UNSPECIFIED: MarginHealthStatus
MARGIN_HEALTH_STATUS_HEALTHY: MarginHealthStatus
MARGIN_HEALTH_STATUS_INITIAL_MARGIN_BREACH: MarginHealthStatus
MARGIN_HEALTH_STATUS_MAINTENANCE_BREACH_LIQUIDATABLE: MarginHealthStatus
MARGIN_HEALTH_STATUS_BANKRUPT_DEFICIT: MarginHealthStatus
LIQUIDATION_STAGE_UNSPECIFIED: LiquidationStage
LIQUIDATION_STAGE_CANCEL_OPEN_MARGIN_ORDERS: LiquidationStage
LIQUIDATION_STAGE_PARTIAL_BOOK_LIQUIDATION: LiquidationStage
LIQUIDATION_STAGE_INSURANCE_FUND_ABSORPTION: LiquidationStage
LIQUIDATION_STAGE_AUTO_DELEVERAGING_ADL: LiquidationStage

class SpanScenarioPoint(_message.Message):
    __slots__ = ("scenario_id", "price_shock_bps", "vol_shock_bps", "scenario_pnl_fp")
    SCENARIO_ID_FIELD_NUMBER: _ClassVar[int]
    PRICE_SHOCK_BPS_FIELD_NUMBER: _ClassVar[int]
    VOL_SHOCK_BPS_FIELD_NUMBER: _ClassVar[int]
    SCENARIO_PNL_FP_FIELD_NUMBER: _ClassVar[int]
    scenario_id: int
    price_shock_bps: int
    vol_shock_bps: int
    scenario_pnl_fp: _common_pb2.I128
    def __init__(self, scenario_id: _Optional[int] = ..., price_shock_bps: _Optional[int] = ..., vol_shock_bps: _Optional[int] = ..., scenario_pnl_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ...) -> None: ...

class DerivativePositionRisk(_message.Message):
    __slots__ = ("instrument_id", "symbol", "signed_qty_fp", "avg_entry_price_fp", "mark_price_fp", "index_price_fp", "bankruptcy_price_fp", "unrealized_pnl_fp", "funding_rate_fp", "net_delta_fp", "net_gamma_fp", "net_vega_fp")
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    SIGNED_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    AVG_ENTRY_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    MARK_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    INDEX_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    BANKRUPTCY_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    UNREALIZED_PNL_FP_FIELD_NUMBER: _ClassVar[int]
    FUNDING_RATE_FP_FIELD_NUMBER: _ClassVar[int]
    NET_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    NET_GAMMA_FP_FIELD_NUMBER: _ClassVar[int]
    NET_VEGA_FP_FIELD_NUMBER: _ClassVar[int]
    instrument_id: int
    symbol: str
    signed_qty_fp: int
    avg_entry_price_fp: int
    mark_price_fp: int
    index_price_fp: int
    bankruptcy_price_fp: int
    unrealized_pnl_fp: _common_pb2.I128
    funding_rate_fp: int
    net_delta_fp: int
    net_gamma_fp: int
    net_vega_fp: int
    def __init__(self, instrument_id: _Optional[int] = ..., symbol: _Optional[str] = ..., signed_qty_fp: _Optional[int] = ..., avg_entry_price_fp: _Optional[int] = ..., mark_price_fp: _Optional[int] = ..., index_price_fp: _Optional[int] = ..., bankruptcy_price_fp: _Optional[int] = ..., unrealized_pnl_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., funding_rate_fp: _Optional[int] = ..., net_delta_fp: _Optional[int] = ..., net_gamma_fp: _Optional[int] = ..., net_vega_fp: _Optional[int] = ...) -> None: ...

class PortfolioMarginSnapshot(_message.Message):
    __slots__ = ("account_id", "engine_seq", "collateral_asset", "health_status", "collateral_balance_fp", "unrealized_pnl_fp", "net_equity_fp", "initial_margin_req_fp", "maintenance_margin_req_fp", "worst_span_loss_fp", "portfolio_delta_fp", "portfolio_gamma_fp", "portfolio_vega_fp", "portfolio_theta_fp", "portfolio_rho_fp", "portfolio_vanna_fp", "portfolio_volga_fp", "effective_leverage_ppm", "adl_quantile_ppm", "positions", "span_grid", "computed_ts_ns", "trace")
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    COLLATERAL_ASSET_FIELD_NUMBER: _ClassVar[int]
    HEALTH_STATUS_FIELD_NUMBER: _ClassVar[int]
    COLLATERAL_BALANCE_FP_FIELD_NUMBER: _ClassVar[int]
    UNREALIZED_PNL_FP_FIELD_NUMBER: _ClassVar[int]
    NET_EQUITY_FP_FIELD_NUMBER: _ClassVar[int]
    INITIAL_MARGIN_REQ_FP_FIELD_NUMBER: _ClassVar[int]
    MAINTENANCE_MARGIN_REQ_FP_FIELD_NUMBER: _ClassVar[int]
    WORST_SPAN_LOSS_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_GAMMA_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_VEGA_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_THETA_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_RHO_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_VANNA_FP_FIELD_NUMBER: _ClassVar[int]
    PORTFOLIO_VOLGA_FP_FIELD_NUMBER: _ClassVar[int]
    EFFECTIVE_LEVERAGE_PPM_FIELD_NUMBER: _ClassVar[int]
    ADL_QUANTILE_PPM_FIELD_NUMBER: _ClassVar[int]
    POSITIONS_FIELD_NUMBER: _ClassVar[int]
    SPAN_GRID_FIELD_NUMBER: _ClassVar[int]
    COMPUTED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    engine_seq: int
    collateral_asset: str
    health_status: MarginHealthStatus
    collateral_balance_fp: _common_pb2.I128
    unrealized_pnl_fp: _common_pb2.I128
    net_equity_fp: _common_pb2.I128
    initial_margin_req_fp: _common_pb2.U128
    maintenance_margin_req_fp: _common_pb2.U128
    worst_span_loss_fp: _common_pb2.U128
    portfolio_delta_fp: int
    portfolio_gamma_fp: int
    portfolio_vega_fp: int
    portfolio_theta_fp: int
    portfolio_rho_fp: int
    portfolio_vanna_fp: int
    portfolio_volga_fp: int
    effective_leverage_ppm: int
    adl_quantile_ppm: int
    positions: _containers.RepeatedCompositeFieldContainer[DerivativePositionRisk]
    span_grid: _containers.RepeatedCompositeFieldContainer[SpanScenarioPoint]
    computed_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, account_id: _Optional[int] = ..., engine_seq: _Optional[int] = ..., collateral_asset: _Optional[str] = ..., health_status: _Optional[_Union[MarginHealthStatus, str]] = ..., collateral_balance_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., unrealized_pnl_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., net_equity_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., initial_margin_req_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., maintenance_margin_req_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., worst_span_loss_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., portfolio_delta_fp: _Optional[int] = ..., portfolio_gamma_fp: _Optional[int] = ..., portfolio_vega_fp: _Optional[int] = ..., portfolio_theta_fp: _Optional[int] = ..., portfolio_rho_fp: _Optional[int] = ..., portfolio_vanna_fp: _Optional[int] = ..., portfolio_volga_fp: _Optional[int] = ..., effective_leverage_ppm: _Optional[int] = ..., adl_quantile_ppm: _Optional[int] = ..., positions: _Optional[_Iterable[_Union[DerivativePositionRisk, _Mapping]]] = ..., span_grid: _Optional[_Iterable[_Union[SpanScenarioPoint, _Mapping]]] = ..., computed_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class SviExpirySlice(_message.Message):
    __slots__ = ("expiry_ts_ns", "time_to_expiry_sec", "forward_price_fp", "svi_a_fp", "svi_b_fp", "svi_rho_fp", "svi_m_fp", "svi_sigma_fp", "butterfly_arbitrage_free", "calendar_arbitrage_free")
    EXPIRY_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TIME_TO_EXPIRY_SEC_FIELD_NUMBER: _ClassVar[int]
    FORWARD_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_A_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_B_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_RHO_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_M_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_SIGMA_FP_FIELD_NUMBER: _ClassVar[int]
    BUTTERFLY_ARBITRAGE_FREE_FIELD_NUMBER: _ClassVar[int]
    CALENDAR_ARBITRAGE_FREE_FIELD_NUMBER: _ClassVar[int]
    expiry_ts_ns: int
    time_to_expiry_sec: int
    forward_price_fp: int
    svi_a_fp: int
    svi_b_fp: int
    svi_rho_fp: int
    svi_m_fp: int
    svi_sigma_fp: int
    butterfly_arbitrage_free: bool
    calendar_arbitrage_free: bool
    def __init__(self, expiry_ts_ns: _Optional[int] = ..., time_to_expiry_sec: _Optional[int] = ..., forward_price_fp: _Optional[int] = ..., svi_a_fp: _Optional[int] = ..., svi_b_fp: _Optional[int] = ..., svi_rho_fp: _Optional[int] = ..., svi_m_fp: _Optional[int] = ..., svi_sigma_fp: _Optional[int] = ..., butterfly_arbitrage_free: _Optional[bool] = ..., calendar_arbitrage_free: _Optional[bool] = ...) -> None: ...

class OptionGreeksPoint(_message.Message):
    __slots__ = ("instrument_id", "option_symbol", "right", "style", "expiry_ts_ns", "strike_price_fp", "underlying_mark_fp", "option_mark_price_fp", "implied_vol_fp", "solver_iterations", "delta_fp", "gamma_fp", "vega_fp", "theta_fp", "rho_fp", "vanna_fp", "volga_fp")
    INSTRUMENT_ID_FIELD_NUMBER: _ClassVar[int]
    OPTION_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    RIGHT_FIELD_NUMBER: _ClassVar[int]
    STYLE_FIELD_NUMBER: _ClassVar[int]
    EXPIRY_TS_NS_FIELD_NUMBER: _ClassVar[int]
    STRIKE_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    UNDERLYING_MARK_FP_FIELD_NUMBER: _ClassVar[int]
    OPTION_MARK_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    IMPLIED_VOL_FP_FIELD_NUMBER: _ClassVar[int]
    SOLVER_ITERATIONS_FIELD_NUMBER: _ClassVar[int]
    DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    GAMMA_FP_FIELD_NUMBER: _ClassVar[int]
    VEGA_FP_FIELD_NUMBER: _ClassVar[int]
    THETA_FP_FIELD_NUMBER: _ClassVar[int]
    RHO_FP_FIELD_NUMBER: _ClassVar[int]
    VANNA_FP_FIELD_NUMBER: _ClassVar[int]
    VOLGA_FP_FIELD_NUMBER: _ClassVar[int]
    instrument_id: int
    option_symbol: str
    right: OptionRight
    style: OptionExerciseStyle
    expiry_ts_ns: int
    strike_price_fp: int
    underlying_mark_fp: int
    option_mark_price_fp: int
    implied_vol_fp: int
    solver_iterations: int
    delta_fp: int
    gamma_fp: int
    vega_fp: int
    theta_fp: int
    rho_fp: int
    vanna_fp: int
    volga_fp: int
    def __init__(self, instrument_id: _Optional[int] = ..., option_symbol: _Optional[str] = ..., right: _Optional[_Union[OptionRight, str]] = ..., style: _Optional[_Union[OptionExerciseStyle, str]] = ..., expiry_ts_ns: _Optional[int] = ..., strike_price_fp: _Optional[int] = ..., underlying_mark_fp: _Optional[int] = ..., option_mark_price_fp: _Optional[int] = ..., implied_vol_fp: _Optional[int] = ..., solver_iterations: _Optional[int] = ..., delta_fp: _Optional[int] = ..., gamma_fp: _Optional[int] = ..., vega_fp: _Optional[int] = ..., theta_fp: _Optional[int] = ..., rho_fp: _Optional[int] = ..., vanna_fp: _Optional[int] = ..., volga_fp: _Optional[int] = ...) -> None: ...

class OptionGreeksSurface(_message.Message):
    __slots__ = ("underlying_symbol", "engine_seq", "spot_index_price_fp", "risk_free_rate_fp", "svi_slices", "points", "surface_arbitrage_free", "evaluation_latency_us", "ts_ns", "trace")
    UNDERLYING_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    SPOT_INDEX_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    RISK_FREE_RATE_FP_FIELD_NUMBER: _ClassVar[int]
    SVI_SLICES_FIELD_NUMBER: _ClassVar[int]
    POINTS_FIELD_NUMBER: _ClassVar[int]
    SURFACE_ARBITRAGE_FREE_FIELD_NUMBER: _ClassVar[int]
    EVALUATION_LATENCY_US_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    underlying_symbol: str
    engine_seq: int
    spot_index_price_fp: int
    risk_free_rate_fp: int
    svi_slices: _containers.RepeatedCompositeFieldContainer[SviExpirySlice]
    points: _containers.RepeatedCompositeFieldContainer[OptionGreeksPoint]
    surface_arbitrage_free: bool
    evaluation_latency_us: int
    ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, underlying_symbol: _Optional[str] = ..., engine_seq: _Optional[int] = ..., spot_index_price_fp: _Optional[int] = ..., risk_free_rate_fp: _Optional[int] = ..., svi_slices: _Optional[_Iterable[_Union[SviExpirySlice, _Mapping]]] = ..., points: _Optional[_Iterable[_Union[OptionGreeksPoint, _Mapping]]] = ..., surface_arbitrage_free: _Optional[bool] = ..., evaluation_latency_us: _Optional[int] = ..., ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class LiquidationWaterfallEvent(_message.Message):
    __slots__ = ("liquidation_id", "engine_seq", "account_id", "symbol", "stage", "mark_price_fp", "bankruptcy_price_fp", "execution_limit_price_fp", "liquidated_qty_fp", "canceled_open_orders", "equity_before_fp", "maintenance_margin_fp", "insurance_fund_delta_fp", "insurance_fund_after_fp", "residual_deficit_fp", "ts_ns", "trace")
    LIQUIDATION_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    STAGE_FIELD_NUMBER: _ClassVar[int]
    MARK_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    BANKRUPTCY_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_LIMIT_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    LIQUIDATED_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    CANCELED_OPEN_ORDERS_FIELD_NUMBER: _ClassVar[int]
    EQUITY_BEFORE_FP_FIELD_NUMBER: _ClassVar[int]
    MAINTENANCE_MARGIN_FP_FIELD_NUMBER: _ClassVar[int]
    INSURANCE_FUND_DELTA_FP_FIELD_NUMBER: _ClassVar[int]
    INSURANCE_FUND_AFTER_FP_FIELD_NUMBER: _ClassVar[int]
    RESIDUAL_DEFICIT_FP_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    liquidation_id: str
    engine_seq: int
    account_id: int
    symbol: str
    stage: LiquidationStage
    mark_price_fp: int
    bankruptcy_price_fp: int
    execution_limit_price_fp: int
    liquidated_qty_fp: int
    canceled_open_orders: int
    equity_before_fp: _common_pb2.I128
    maintenance_margin_fp: _common_pb2.U128
    insurance_fund_delta_fp: _common_pb2.I128
    insurance_fund_after_fp: _common_pb2.U128
    residual_deficit_fp: _common_pb2.U128
    ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, liquidation_id: _Optional[str] = ..., engine_seq: _Optional[int] = ..., account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., stage: _Optional[_Union[LiquidationStage, str]] = ..., mark_price_fp: _Optional[int] = ..., bankruptcy_price_fp: _Optional[int] = ..., execution_limit_price_fp: _Optional[int] = ..., liquidated_qty_fp: _Optional[int] = ..., canceled_open_orders: _Optional[int] = ..., equity_before_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., maintenance_margin_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., insurance_fund_delta_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ..., insurance_fund_after_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., residual_deficit_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class ADLCounterpartyAllocation(_message.Message):
    __slots__ = ("counterparty_account_id", "quantile_rank_ppm", "ranking_score_fp", "deleveraged_qty_fp", "settlement_price_fp", "realized_pnl_settled_fp")
    COUNTERPARTY_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    QUANTILE_RANK_PPM_FIELD_NUMBER: _ClassVar[int]
    RANKING_SCORE_FP_FIELD_NUMBER: _ClassVar[int]
    DELEVERAGED_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    SETTLEMENT_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    REALIZED_PNL_SETTLED_FP_FIELD_NUMBER: _ClassVar[int]
    counterparty_account_id: int
    quantile_rank_ppm: int
    ranking_score_fp: int
    deleveraged_qty_fp: int
    settlement_price_fp: int
    realized_pnl_settled_fp: _common_pb2.I128
    def __init__(self, counterparty_account_id: _Optional[int] = ..., quantile_rank_ppm: _Optional[int] = ..., ranking_score_fp: _Optional[int] = ..., deleveraged_qty_fp: _Optional[int] = ..., settlement_price_fp: _Optional[int] = ..., realized_pnl_settled_fp: _Optional[_Union[_common_pb2.I128, _Mapping]] = ...) -> None: ...

class ADLTrigger(_message.Message):
    __slots__ = ("adl_id", "source_liquidation_id", "engine_seq", "bankrupt_account_id", "symbol", "bankrupt_position_side", "mark_price_fp", "bankruptcy_price_fp", "uncovered_deficit_fp", "total_adl_qty_fp", "allocations", "system_solvency_preserved", "triggered_ts_ns", "trace")
    ADL_ID_FIELD_NUMBER: _ClassVar[int]
    SOURCE_LIQUIDATION_ID_FIELD_NUMBER: _ClassVar[int]
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    BANKRUPT_ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    BANKRUPT_POSITION_SIDE_FIELD_NUMBER: _ClassVar[int]
    MARK_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    BANKRUPTCY_PRICE_FP_FIELD_NUMBER: _ClassVar[int]
    UNCOVERED_DEFICIT_FP_FIELD_NUMBER: _ClassVar[int]
    TOTAL_ADL_QTY_FP_FIELD_NUMBER: _ClassVar[int]
    ALLOCATIONS_FIELD_NUMBER: _ClassVar[int]
    SYSTEM_SOLVENCY_PRESERVED_FIELD_NUMBER: _ClassVar[int]
    TRIGGERED_TS_NS_FIELD_NUMBER: _ClassVar[int]
    TRACE_FIELD_NUMBER: _ClassVar[int]
    adl_id: str
    source_liquidation_id: str
    engine_seq: int
    bankrupt_account_id: int
    symbol: str
    bankrupt_position_side: _common_pb2.OrderSide
    mark_price_fp: int
    bankruptcy_price_fp: int
    uncovered_deficit_fp: _common_pb2.U128
    total_adl_qty_fp: int
    allocations: _containers.RepeatedCompositeFieldContainer[ADLCounterpartyAllocation]
    system_solvency_preserved: bool
    triggered_ts_ns: int
    trace: _common_pb2.TraceMetadata
    def __init__(self, adl_id: _Optional[str] = ..., source_liquidation_id: _Optional[str] = ..., engine_seq: _Optional[int] = ..., bankrupt_account_id: _Optional[int] = ..., symbol: _Optional[str] = ..., bankrupt_position_side: _Optional[_Union[_common_pb2.OrderSide, str]] = ..., mark_price_fp: _Optional[int] = ..., bankruptcy_price_fp: _Optional[int] = ..., uncovered_deficit_fp: _Optional[_Union[_common_pb2.U128, _Mapping]] = ..., total_adl_qty_fp: _Optional[int] = ..., allocations: _Optional[_Iterable[_Union[ADLCounterpartyAllocation, _Mapping]]] = ..., system_solvency_preserved: _Optional[bool] = ..., triggered_ts_ns: _Optional[int] = ..., trace: _Optional[_Union[_common_pb2.TraceMetadata, _Mapping]] = ...) -> None: ...

class GetPortfolioMarginRequest(_message.Message):
    __slots__ = ("account_id",)
    ACCOUNT_ID_FIELD_NUMBER: _ClassVar[int]
    account_id: int
    def __init__(self, account_id: _Optional[int] = ...) -> None: ...

class GetOptionGreeksSurfaceRequest(_message.Message):
    __slots__ = ("underlying_symbol",)
    UNDERLYING_SYMBOL_FIELD_NUMBER: _ClassVar[int]
    underlying_symbol: str
    def __init__(self, underlying_symbol: _Optional[str] = ...) -> None: ...

class StreamMarginEventsRequest(_message.Message):
    __slots__ = ("symbol", "include_liquidations", "include_adl_triggers")
    SYMBOL_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_LIQUIDATIONS_FIELD_NUMBER: _ClassVar[int]
    INCLUDE_ADL_TRIGGERS_FIELD_NUMBER: _ClassVar[int]
    symbol: str
    include_liquidations: bool
    include_adl_triggers: bool
    def __init__(self, symbol: _Optional[str] = ..., include_liquidations: _Optional[bool] = ..., include_adl_triggers: _Optional[bool] = ...) -> None: ...

class MarginEventEnvelope(_message.Message):
    __slots__ = ("engine_seq", "ts_ns", "margin_snapshot", "greeks_surface", "liquidation", "adl_trigger")
    ENGINE_SEQ_FIELD_NUMBER: _ClassVar[int]
    TS_NS_FIELD_NUMBER: _ClassVar[int]
    MARGIN_SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    GREEKS_SURFACE_FIELD_NUMBER: _ClassVar[int]
    LIQUIDATION_FIELD_NUMBER: _ClassVar[int]
    ADL_TRIGGER_FIELD_NUMBER: _ClassVar[int]
    engine_seq: int
    ts_ns: int
    margin_snapshot: PortfolioMarginSnapshot
    greeks_surface: OptionGreeksSurface
    liquidation: LiquidationWaterfallEvent
    adl_trigger: ADLTrigger
    def __init__(self, engine_seq: _Optional[int] = ..., ts_ns: _Optional[int] = ..., margin_snapshot: _Optional[_Union[PortfolioMarginSnapshot, _Mapping]] = ..., greeks_surface: _Optional[_Union[OptionGreeksSurface, _Mapping]] = ..., liquidation: _Optional[_Union[LiquidationWaterfallEvent, _Mapping]] = ..., adl_trigger: _Optional[_Union[ADLTrigger, _Mapping]] = ...) -> None: ...
