import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { I128, OrderSide, TraceMetadata, U128 } from "../common";
/** Option contract payoff right. */
export declare enum OptionRight {
    OPTION_RIGHT_UNSPECIFIED = 0,
    OPTION_RIGHT_CALL = 1,
    OPTION_RIGHT_PUT = 2,
    UNRECOGNIZED = -1
}
export declare function optionRightFromJSON(object: any): OptionRight;
export declare function optionRightToJSON(object: OptionRight): string;
/** Option exercise style supported by the institutional derivatives engine. */
export declare enum OptionExerciseStyle {
    OPTION_EXERCISE_STYLE_UNSPECIFIED = 0,
    OPTION_EXERCISE_STYLE_EUROPEAN = 1,
    OPTION_EXERCISE_STYLE_AMERICAN = 2,
    UNRECOGNIZED = -1
}
export declare function optionExerciseStyleFromJSON(object: any): OptionExerciseStyle;
export declare function optionExerciseStyleToJSON(object: OptionExerciseStyle): string;
/** Portfolio-level cross-margin health classification. */
export declare enum MarginHealthStatus {
    MARGIN_HEALTH_STATUS_UNSPECIFIED = 0,
    MARGIN_HEALTH_STATUS_HEALTHY = 1,
    MARGIN_HEALTH_STATUS_INITIAL_MARGIN_BREACH = 2,
    MARGIN_HEALTH_STATUS_MAINTENANCE_BREACH_LIQUIDATABLE = 3,
    MARGIN_HEALTH_STATUS_BANKRUPT_DEFICIT = 4,
    UNRECOGNIZED = -1
}
export declare function marginHealthStatusFromJSON(object: any): MarginHealthStatus;
export declare function marginHealthStatusToJSON(object: MarginHealthStatus): string;
/** Deterministic stage inside the 4-tier non-bankruptcy liquidation waterfall. */
export declare enum LiquidationStage {
    LIQUIDATION_STAGE_UNSPECIFIED = 0,
    LIQUIDATION_STAGE_CANCEL_OPEN_MARGIN_ORDERS = 1,
    LIQUIDATION_STAGE_PARTIAL_BOOK_LIQUIDATION = 2,
    LIQUIDATION_STAGE_INSURANCE_FUND_ABSORPTION = 3,
    LIQUIDATION_STAGE_AUTO_DELEVERAGING_ADL = 4,
    UNRECOGNIZED = -1
}
export declare function liquidationStageFromJSON(object: any): LiquidationStage;
export declare function liquidationStageToJSON(object: LiquidationStage): string;
/** Single evaluated point inside the 16-point SPAN underlying-price x implied-vol shock grid. */
export interface SpanScenarioPoint {
    scenarioId: number;
    /** Underlying price shock in signed basis points (e.g., -1500 = -15.00%, +1500 = +15.00%). */
    priceShockBps: number;
    /** Implied volatility relative shock in signed basis points (e.g., -3000 = -30%, +3000 = +30%). */
    volShockBps: number;
    /** Simulated portfolio PnL under this scenario in signed 128-bit fixed-point (scale 1e8). */
    scenarioPnlFp: I128 | undefined;
}
/** Per-instrument derivative position & mark-to-market risk contribution (all fixed-point 1e8). */
export interface DerivativePositionRisk {
    instrumentId: number;
    symbol: string;
    /** Signed position quantity (scale 1e8; positive = long, negative = short). */
    signedQtyFp: bigint;
    avgEntryPriceFp: bigint;
    markPriceFp: bigint;
    indexPriceFp: bigint;
    bankruptcyPriceFp: bigint;
    unrealizedPnlFp: I128 | undefined;
    /** Signed perpetual funding rate in fixed-point (scale 1e8; 0 for spot/options). */
    fundingRateFp: bigint;
    netDeltaFp: bigint;
    netGammaFp: bigint;
    netVegaFp: bigint;
}
/** Real-time SPAN / Scenario-Grid Cross-Margin snapshot for an institutional trading account. */
export interface PortfolioMarginSnapshot {
    accountId: bigint;
    engineSeq: bigint;
    collateralAsset: string;
    healthStatus: MarginHealthStatus;
    /** Settled cash/collateral balance before unrealized PnL (scale 1e8). */
    collateralBalanceFp: I128 | undefined;
    /** Net unrealized mark-to-market PnL across perpetuals & options (scale 1e8). */
    unrealizedPnlFp: I128 | undefined;
    /** Total dynamic account equity = collateral_balance_fp + unrealized_pnl_fp (scale 1e8). */
    netEquityFp: I128 | undefined;
    /** Required Initial Margin across 16-point SPAN grid + liquidity add-ons (scale 1e8). */
    initialMarginReqFp: U128 | undefined;
    /** Required Maintenance Margin threshold triggering Liquidation Waterfall (scale 1e8). */
    maintenanceMarginReqFp: U128 | undefined;
    /** Worst-case loss across the 16 SPAN scenarios (scale 1e8). */
    worstSpanLossFp: U128 | undefined;
    /** Aggregate portfolio Greeks in signed fixed-point (scale 1e8). */
    portfolioDeltaFp: bigint;
    portfolioGammaFp: bigint;
    portfolioVegaFp: bigint;
    portfolioThetaFp: bigint;
    portfolioRhoFp: bigint;
    portfolioVannaFp: bigint;
    portfolioVolgaFp: bigint;
    /** Effective leverage in parts-per-million (e.g., 10_000_000 = 10.0x). */
    effectiveLeveragePpm: bigint;
    /** Profit-leverage quantile score in [0, 1_000_000] ppm for ADL counterparty ranking. */
    adlQuantilePpm: number;
    positions: DerivativePositionRisk[];
    spanGrid: SpanScenarioPoint[];
    computedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/**
 * Arbitrage-free SVI (Stochastic Volatility Inspired) slice parameters for one expiry T:
 *   w(k) = a + b * (rho * (k - m) + sqrt((k - m)^2 + sigma^2)), all scaled by 1e8.
 */
export interface SviExpirySlice {
    expiryTsNs: bigint;
    timeToExpirySec: bigint;
    forwardPriceFp: bigint;
    sviAFp: bigint;
    sviBFp: bigint;
    /** Correlation parameter rho in (-1e8, +1e8). */
    sviRhoFp: bigint;
    sviMFp: bigint;
    sviSigmaFp: bigint;
    butterflyArbitrageFree: boolean;
    calendarArbitrageFree: boolean;
}
/** Analytical 1st- and 2nd-order Greeks & converged Implied Volatility for a single option contract. */
export interface OptionGreeksPoint {
    instrumentId: number;
    optionSymbol: string;
    right: OptionRight;
    style: OptionExerciseStyle;
    expiryTsNs: bigint;
    strikePriceFp: bigint;
    underlyingMarkFp: bigint;
    optionMarkPriceFp: bigint;
    /** Annualized Implied Volatility converged to <= 1e-8 (scale 1e8; 65_000_000 = 65.00%). */
    impliedVolFp: bigint;
    solverIterations: number;
    /** 1st- and 2nd-order analytical Greeks quantized to signed fixed-point (scale 1e8). */
    deltaFp: bigint;
    gammaFp: bigint;
    vegaFp: bigint;
    thetaFp: bigint;
    rhoFp: bigint;
    vannaFp: bigint;
    volgaFp: bigint;
}
/** Complete arbitrage-free Implied Volatility & analytical Greeks surface for an underlying asset. */
export interface OptionGreeksSurface {
    underlyingSymbol: string;
    engineSeq: bigint;
    spotIndexPriceFp: bigint;
    riskFreeRateFp: bigint;
    sviSlices: SviExpirySlice[];
    points: OptionGreeksPoint[];
    surfaceArbitrageFree: boolean;
    evaluationLatencyUs: bigint;
    tsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Immutable audit event emitted at each step of the 4-stage Multi-Stage Liquidation Waterfall. */
export interface LiquidationWaterfallEvent {
    liquidationId: string;
    engineSeq: bigint;
    accountId: bigint;
    symbol: string;
    stage: LiquidationStage;
    markPriceFp: bigint;
    bankruptcyPriceFp: bigint;
    executionLimitPriceFp: bigint;
    liquidatedQtyFp: bigint;
    canceledOpenOrders: number;
    equityBeforeFp: I128 | undefined;
    maintenanceMarginFp: U128 | undefined;
    /** Insurance fund delta in signed 128-bit fixed-point (positive = surplus captured, negative = deficit absorbed). */
    insuranceFundDeltaFp: I128 | undefined;
    insuranceFundAfterFp: U128 | undefined;
    /** Residual unabsorbed bankruptcy deficit requiring Stage 4 ADL (scale 1e8; 0 if absorbed). */
    residualDeficitFp: U128 | undefined;
    tsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Counterparty leg selected by profit-leverage quantile ranking during Stage 4 Auto-Deleveraging. */
export interface ADLCounterpartyAllocation {
    counterpartyAccountId: bigint;
    quantileRankPpm: number;
    /** Profit-leverage ranking score (scale 1e8). */
    rankingScoreFp: bigint;
    deleveragedQtyFp: bigint;
    /** Executed at bankrupt account's bankruptcy_price_fp so zero system deficit leaks. */
    settlementPriceFp: bigint;
    realizedPnlSettledFp: I128 | undefined;
}
/** Stage-4 Auto-Deleveraging (ADL) trigger emitted when residual deficit exceeds Insurance Fund capacity. */
export interface ADLTrigger {
    adlId: string;
    sourceLiquidationId: string;
    engineSeq: bigint;
    bankruptAccountId: bigint;
    symbol: string;
    bankruptPositionSide: OrderSide;
    markPriceFp: bigint;
    bankruptcyPriceFp: bigint;
    uncoveredDeficitFp: U128 | undefined;
    totalAdlQtyFp: bigint;
    allocations: ADLCounterpartyAllocation[];
    /** True iff post-ADL aggregate system equity + Insurance Fund >= 0 with zero leaked bad debt. */
    systemSolvencyPreserved: boolean;
    triggeredTsNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface GetPortfolioMarginRequest {
    accountId: bigint;
}
export interface GetOptionGreeksSurfaceRequest {
    underlyingSymbol: string;
}
export interface StreamMarginEventsRequest {
    symbol: string;
    includeLiquidations: boolean;
    includeAdlTriggers: boolean;
}
export interface MarginEventEnvelope {
    engineSeq: bigint;
    tsNs: bigint;
    marginSnapshot?: PortfolioMarginSnapshot | undefined;
    greeksSurface?: OptionGreeksSurface | undefined;
    liquidation?: LiquidationWaterfallEvent | undefined;
    adlTrigger?: ADLTrigger | undefined;
}
export declare const SpanScenarioPoint: MessageFns<SpanScenarioPoint>;
export declare const DerivativePositionRisk: MessageFns<DerivativePositionRisk>;
export declare const PortfolioMarginSnapshot: MessageFns<PortfolioMarginSnapshot>;
export declare const SviExpirySlice: MessageFns<SviExpirySlice>;
export declare const OptionGreeksPoint: MessageFns<OptionGreeksPoint>;
export declare const OptionGreeksSurface: MessageFns<OptionGreeksSurface>;
export declare const LiquidationWaterfallEvent: MessageFns<LiquidationWaterfallEvent>;
export declare const ADLCounterpartyAllocation: MessageFns<ADLCounterpartyAllocation>;
export declare const ADLTrigger: MessageFns<ADLTrigger>;
export declare const GetPortfolioMarginRequest: MessageFns<GetPortfolioMarginRequest>;
export declare const GetOptionGreeksSurfaceRequest: MessageFns<GetOptionGreeksSurfaceRequest>;
export declare const StreamMarginEventsRequest: MessageFns<StreamMarginEventsRequest>;
export declare const MarginEventEnvelope: MessageFns<MarginEventEnvelope>;
/** Primary gRPC interface exposed by services/margin-risk-rs on port 50163. */
export type MarginRiskServiceDefinition = typeof MarginRiskServiceDefinition;
export declare const MarginRiskServiceDefinition: {
    readonly name: "MarginRiskService";
    readonly fullName: "hyperion.omega.v1.MarginRiskService";
    readonly methods: {
        readonly getPortfolioMargin: {
            readonly name: "GetPortfolioMargin";
            readonly requestType: typeof GetPortfolioMarginRequest;
            readonly requestStream: false;
            readonly responseType: typeof PortfolioMarginSnapshot;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getOptionGreeksSurface: {
            readonly name: "GetOptionGreeksSurface";
            readonly requestType: typeof GetOptionGreeksSurfaceRequest;
            readonly requestStream: false;
            readonly responseType: typeof OptionGreeksSurface;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamMarginEvents: {
            readonly name: "StreamMarginEvents";
            readonly requestType: typeof StreamMarginEventsRequest;
            readonly requestStream: false;
            readonly responseType: typeof MarginEventEnvelope;
            readonly responseStream: true;
            readonly options: {};
        };
    };
};
type Builtin = Date | Function | Uint8Array | string | number | boolean | bigint | undefined;
type DeepPartial<T> = T extends bigint ? string | number | bigint : T extends Builtin ? T : T extends globalThis.Array<infer U> ? globalThis.Array<DeepPartial<U>> : T extends ReadonlyArray<infer U> ? ReadonlyArray<DeepPartial<U>> : T extends {} ? {
    [K in keyof T]?: DeepPartial<T[K]>;
} : Partial<T>;
type KeysOfUnion<T> = T extends T ? keyof T : never;
type Exact<P, I> = P extends Builtin ? P : P & {
    [K in keyof P as Extract<K, keyof I>]: Exact<P[K], I[K & keyof I]>;
} & {
    [K in Exclude<keyof I, KeysOfUnion<P>>]: never;
};
interface MessageFns<T> {
    encode(message: T, writer?: BinaryWriter): BinaryWriter;
    decode(input: BinaryReader | Uint8Array, length?: number): T;
    fromJSON(object: any): T;
    toJSON(message: T): unknown;
    create<I extends Exact<DeepPartial<T>, I>>(base?: I): T;
    fromPartial<I extends Exact<DeepPartial<T>, I>>(object: I): T;
}
export {};
//# sourceMappingURL=margin_risk.d.ts.map