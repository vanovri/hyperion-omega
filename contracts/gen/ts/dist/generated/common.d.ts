import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
/** Canonical order side. */
export declare enum OrderSide {
    ORDER_SIDE_UNSPECIFIED = 0,
    ORDER_SIDE_BUY = 1,
    ORDER_SIDE_SELL = 2,
    UNRECOGNIZED = -1
}
export declare function orderSideFromJSON(object: any): OrderSide;
export declare function orderSideToJSON(object: OrderSide): string;
/** Canonical order execution instruction type. */
export declare enum OrderType {
    ORDER_TYPE_UNSPECIFIED = 0,
    ORDER_TYPE_LIMIT = 1,
    ORDER_TYPE_MARKET = 2,
    ORDER_TYPE_IOC = 3,
    ORDER_TYPE_FOK = 4,
    ORDER_TYPE_POST_ONLY = 5,
    UNRECOGNIZED = -1
}
export declare function orderTypeFromJSON(object: any): OrderType;
export declare function orderTypeToJSON(object: OrderType): string;
/** Self-Trade Prevention (STP) policy when maker_account_id == taker_account_id. */
export declare enum SelfTradePreventionMode {
    SELF_TRADE_PREVENTION_MODE_UNSPECIFIED = 0,
    SELF_TRADE_PREVENTION_MODE_NONE = 1,
    SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING = 2,
    SELF_TRADE_PREVENTION_MODE_CANCEL_AGGRESSING = 3,
    SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH = 4,
    UNRECOGNIZED = -1
}
export declare function selfTradePreventionModeFromJSON(object: any): SelfTradePreventionMode;
export declare function selfTradePreventionModeToJSON(object: SelfTradePreventionMode): string;
/** Operational risk state of a trading account. */
export declare enum AccountStatus {
    ACCOUNT_STATUS_UNSPECIFIED = 0,
    ACCOUNT_STATUS_ACTIVE = 1,
    ACCOUNT_STATUS_THROTTLED = 2,
    ACCOUNT_STATUS_FROZEN = 3,
    ACCOUNT_STATUS_LIQUIDATION_ONLY = 4,
    UNRECOGNIZED = -1
}
export declare function accountStatusFromJSON(object: any): AccountStatus;
export declare function accountStatusToJSON(object: AccountStatus): string;
/** Global or per-instrument exchange protective market regime. */
export declare enum MarketRegime {
    MARKET_REGIME_UNSPECIFIED = 0,
    MARKET_REGIME_NORMAL = 1,
    MARKET_REGIME_VOLATILITY_GUARD = 2,
    MARKET_REGIME_POST_ONLY_AUCTION = 3,
    MARKET_REGIME_HALTED = 4,
    UNRECOGNIZED = -1
}
export declare function marketRegimeFromJSON(object: any): MarketRegime;
export declare function marketRegimeToJSON(object: MarketRegime): string;
/**
 * Two's-complement signed 128-bit fixed-point integer (scale 1e8 unless noted).
 * Value = (hi * 2^64) + lo, where hi is signed int64 and lo is unsigned uint64.
 */
export interface I128 {
    hi: bigint;
    lo: bigint;
}
/**
 * Unsigned 128-bit fixed-point integer (scale 1e8 unless noted).
 * Value = (hi * 2^64) + lo.
 */
export interface U128 {
    hi: bigint;
    lo: bigint;
}
/** W3C / OpenTelemetry distributed tracing & deterministic causality envelope. */
export interface TraceMetadata {
    /** 32-char lowercase hex W3C trace ID (128-bit). */
    traceId: string;
    /** 16-char lowercase hex W3C span ID (64-bit). */
    spanId: string;
    /** End-to-end deterministic request correlation identifier. */
    correlationId: string;
    /** Originating microservice identifier (e.g., "gateway-go", "chaos-sim"). */
    sourceService: string;
    /** Monotonic / UTC epoch nanosecond timestamp at ingress. */
    ingestTsNs: bigint;
}
export declare const I128: MessageFns<I128>;
export declare const U128: MessageFns<U128>;
export declare const TraceMetadata: MessageFns<TraceMetadata>;
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
//# sourceMappingURL=common.d.ts.map