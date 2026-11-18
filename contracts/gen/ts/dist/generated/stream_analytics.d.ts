import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { I128, U128 } from "./common";
/** Supported multi-timeframe OHLCV candlestick intervals. */
export declare enum CandleInterval {
    CANDLE_INTERVAL_UNSPECIFIED = 0,
    CANDLE_INTERVAL_1S = 1,
    CANDLE_INTERVAL_5S = 2,
    CANDLE_INTERVAL_1M = 3,
    CANDLE_INTERVAL_5M = 4,
    CANDLE_INTERVAL_15M = 5,
    CANDLE_INTERVAL_1H = 6,
    UNRECOGNIZED = -1
}
export declare function candleIntervalFromJSON(object: any): CandleInterval;
export declare function candleIntervalToJSON(object: CandleInterval): string;
/** Fixed-point OHLCV + aggressive flow candlestick computed by stream-aggregator. */
export interface OhlcvCandle {
    symbol: string;
    interval: CandleInterval;
    bucketOpenTsNs: bigint;
    bucketCloseTsNs: bigint;
    openFp: bigint;
    highFp: bigint;
    lowFp: bigint;
    closeFp: bigint;
    baseVolumeFp: U128 | undefined;
    quoteVolumeFp: U128 | undefined;
    takerBuyBaseVolumeFp: U128 | undefined;
    takerSellBaseVolumeFp: U128 | undefined;
    vwapFp: bigint;
    twapFp: bigint;
    tradeCount: bigint;
    lastEngineSeq: bigint;
    isClosed: boolean;
}
/** Real-time quantitative market microstructure snapshot per instrument. */
export interface MicrostructureMetrics {
    symbol: string;
    engineSeq: bigint;
    tsNs: bigint;
    midPriceFp: bigint;
    spreadFp: bigint;
    spreadBps: number;
    rollingVwapFp: bigint;
    rollingTwapFp: bigint;
    /** Signed Order Flow Imbalance in fixed-point base units (scale 1e8). */
    ofiFp: I128 | undefined;
    /** Book Imbalance Ratio (Bid - Ask) / (Bid + Ask) in signed ppm [-1_000_000, +1_000_000]. */
    bookImbalanceTop5Ppm: bigint;
    bookImbalanceTop10Ppm: bigint;
    bookImbalanceTop20Ppm: bigint;
    /** Kyle's Lambda price-impact coefficient scaled by 1e8. */
    kylesLambdaFp: bigint;
}
/** Single price-bucket cell in a time-series liquidity depth heatmap matrix. */
export interface LiquidityHeatmapBucket {
    bucketTsNs: bigint;
    priceBucketFp: bigint;
    restingBidQtyFp: bigint;
    restingAskQtyFp: bigint;
}
/** Horizontal Volume Profile histogram bin & Point of Control (POC) component. */
export interface VolumeProfileBin {
    priceBucketFp: bigint;
    buyVolumeFp: U128 | undefined;
    sellVolumeFp: U128 | undefined;
    totalVolumeFp: U128 | undefined;
}
export interface GetCandlesRequest {
    symbol: string;
    interval: CandleInterval;
    fromTsNs: bigint;
    toTsNs: bigint;
    limit: number;
}
export interface GetCandlesResponse {
    candles: OhlcvCandle[];
}
export interface GetVolumeProfileRequest {
    symbol: string;
    fromTsNs: bigint;
    toTsNs: bigint;
    bucketSizeFp: bigint;
}
export interface GetVolumeProfileResponse {
    symbol: string;
    pocPriceFp: bigint;
    valueAreaHighFp: bigint;
    valueAreaLowFp: bigint;
    bins: VolumeProfileBin[];
}
export interface GetLiquidityHeatmapRequest {
    symbol: string;
    fromTsNs: bigint;
    toTsNs: bigint;
    priceBucketSizeFp: bigint;
    timeBucketNs: bigint;
}
export interface GetLiquidityHeatmapResponse {
    symbol: string;
    buckets: LiquidityHeatmapBucket[];
}
export interface GetLatestMicrostructureRequest {
    symbol: string;
}
export interface GetLatestMicrostructureResponse {
    metrics: MicrostructureMetrics | undefined;
}
export declare const OhlcvCandle: MessageFns<OhlcvCandle>;
export declare const MicrostructureMetrics: MessageFns<MicrostructureMetrics>;
export declare const LiquidityHeatmapBucket: MessageFns<LiquidityHeatmapBucket>;
export declare const VolumeProfileBin: MessageFns<VolumeProfileBin>;
export declare const GetCandlesRequest: MessageFns<GetCandlesRequest>;
export declare const GetCandlesResponse: MessageFns<GetCandlesResponse>;
export declare const GetVolumeProfileRequest: MessageFns<GetVolumeProfileRequest>;
export declare const GetVolumeProfileResponse: MessageFns<GetVolumeProfileResponse>;
export declare const GetLiquidityHeatmapRequest: MessageFns<GetLiquidityHeatmapRequest>;
export declare const GetLiquidityHeatmapResponse: MessageFns<GetLiquidityHeatmapResponse>;
export declare const GetLatestMicrostructureRequest: MessageFns<GetLatestMicrostructureRequest>;
export declare const GetLatestMicrostructureResponse: MessageFns<GetLatestMicrostructureResponse>;
/** Primary gRPC interface exposed by services/stream-aggregator on port 50152. */
export type StreamAnalyticsServiceDefinition = typeof StreamAnalyticsServiceDefinition;
export declare const StreamAnalyticsServiceDefinition: {
    readonly name: "StreamAnalyticsService";
    readonly fullName: "hyperion.v1.StreamAnalyticsService";
    readonly methods: {
        readonly getCandles: {
            readonly name: "GetCandles";
            readonly requestType: typeof GetCandlesRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetCandlesResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getVolumeProfile: {
            readonly name: "GetVolumeProfile";
            readonly requestType: typeof GetVolumeProfileRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetVolumeProfileResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getLiquidityHeatmap: {
            readonly name: "GetLiquidityHeatmap";
            readonly requestType: typeof GetLiquidityHeatmapRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetLiquidityHeatmapResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getLatestMicrostructure: {
            readonly name: "GetLatestMicrostructure";
            readonly requestType: typeof GetLatestMicrostructureRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetLatestMicrostructureResponse;
            readonly responseStream: false;
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
//# sourceMappingURL=stream_analytics.d.ts.map