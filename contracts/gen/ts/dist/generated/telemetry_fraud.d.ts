import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { OrderSide, TraceMetadata, U128 } from "./common";
/** Taxonomy of market manipulation & financial fraud typologies detected by Sentinel-ML. */
export declare enum AnomalyPatternType {
    ANOMALY_PATTERN_TYPE_UNSPECIFIED = 0,
    ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE = 1,
    ANOMALY_PATTERN_TYPE_SPOOFING_LAYERING = 2,
    ANOMALY_PATTERN_TYPE_VELOCITY_SPIKE = 3,
    ANOMALY_PATTERN_TYPE_MOMENTUM_IGNITION = 4,
    ANOMALY_PATTERN_TYPE_LATENCY_ARBITRAGE = 5,
    UNRECOGNIZED = -1
}
export declare function anomalyPatternTypeFromJSON(object: any): AnomalyPatternType;
export declare function anomalyPatternTypeToJSON(object: AnomalyPatternType): string;
/** Automated or operator-approved protective action executed across Gateway & Ledger. */
export declare enum MitigationActionType {
    MITIGATION_ACTION_TYPE_UNSPECIFIED = 0,
    MITIGATION_ACTION_TYPE_THROTTLE_RATE_LIMIT = 1,
    MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT = 2,
    MITIGATION_ACTION_TYPE_CANCEL_OPEN_ORDERS = 3,
    MITIGATION_ACTION_TYPE_ENABLE_VOLATILITY_GUARD = 4,
    MITIGATION_ACTION_TYPE_RESTORE_NORMAL = 5,
    UNRECOGNIZED = -1
}
export declare function mitigationActionTypeFromJSON(object: any): MitigationActionType;
export declare function mitigationActionTypeToJSON(object: MitigationActionType): string;
/** Directed temporal multigraph edge representing value transfer / trade execution (maker -> taker). */
export interface TransactionGraphEdge {
    edgeId: bigint;
    fillId: bigint;
    engineSeq: bigint;
    symbol: string;
    makerAccountId: bigint;
    takerAccountId: bigint;
    aggressorSide: OrderSide;
    /** Matched price in fixed-point (scale 1e8). */
    priceFp: bigint;
    /** Matched base quantity in fixed-point (scale 1e8). */
    qtyFp: bigint;
    /** Matched quote notional in fixed-point (scale 1e8). */
    notionalFp: U128 | undefined;
    execTsNs: bigint;
    /** Elapsed nanoseconds since the previous edge between the same ordered account pair. */
    prevInteractionDeltaNs: bigint;
    /** Exponential temporal decay edge weight in parts-per-million (0..1_000_000). */
    temporalDecayWeightPpm: number;
}
/** Evidence payload for circular wash-trading rings (e.g. A -> B -> C -> A, depth k <= 6). */
export interface WashTradingCycle {
    cycleId: string;
    /** Ordered ring of colluding account IDs (first == last or closed cycle). */
    participantAccountIds: bigint[];
    cycleEdges: TransactionGraphEdge[];
    cycleLength: number;
    /** Total circular wash volume in fixed-point (scale 1e8). */
    circularVolumeFp: U128 | undefined;
    /** Residual net inventory delta across ring members in fixed-point (scale 1e8). */
    netPositionDeltaFp: U128 | undefined;
    cycleDurationNs: bigint;
    /** Symmetry of leg sizes around the ring in parts-per-million (1_000_000 = identical leg sizes). */
    volumeSymmetryPpm: number;
}
/** Evidence payload for microstructure order-book spoofing & multi-tick layering. */
export interface SpoofingLayering {
    suspectAccountId: bigint;
    spoofedSide: OrderSide;
    /** Number of distinct price ticks simultaneously layered with phantom liquidity. */
    layeredLevelCount: number;
    /** Total phantom order quantity rapidly canceled near best quote (scale 1e8). */
    canceledSpoofQtyFp: U128 | undefined;
    /** Aggressive execution quantity filled on the opposite side before/during cancellation (scale 1e8). */
    oppositeSideExecQtyFp: U128 | undefined;
    /** Nanoseconds elapsed between opposite-side fill and spoof order cancellation. */
    cancelToFillLatencyNs: bigint;
    /** Order-to-Trade Ratio scaled by 1e6 (e.g., 45_000_000 = 45.0x OTR). */
    orderToTradeRatioPpm: bigint;
    /** Distance of spoofed liquidity from best bid/ask in basis points. */
    bestQuoteDistanceBps: number;
}
/** Evidence payload for abnormal high-frequency order burst / quote-stuffing / velocity spike. */
export interface VelocitySpike {
    suspectAccountId: bigint;
    eventsPerSecond: bigint;
    baselineEventsPerSecond: bigint;
    /** Burst ratio relative to rolling account baseline in parts-per-million (1_000_000 = 1.0x). */
    burstMultiplierPpm: bigint;
    /** Cancellation ratio within sliding window in parts-per-million (0..1_000_000). */
    cancelRatePpm: number;
    notionalBurstFp: U128 | undefined;
    windowDurationNs: bigint;
}
/** Local SHAP-style explainable AI feature attribution entry for auditability. */
export interface SHAPFeatureAttribution {
    featureName: string;
    /** Observed streaming feature value in fixed-point (scale 1e8). */
    featureValueFp: bigint;
    /** Expected background baseline value in fixed-point (scale 1e8). */
    baselineValueFp: bigint;
    /** Signed additive SHAP contribution to anomaly probability in parts-per-million (scale 1e6). */
    shapContributionPpm: bigint;
    /** 1-based rank ordered by |shap_contribution_ppm| descending. */
    importanceRank: number;
}
/** Real-time explainable fraud & manipulation alert emitted by Sentinel-ML. */
export interface AnomalyAlert {
    alertId: string;
    engineSeq: bigint;
    symbol: string;
    patternType: AnomalyPatternType;
    /** Normalized integer severity score in [0, 100]. */
    severityScore: number;
    /** Calibrated ensemble confidence in parts-per-million [0, 1_000_000]. */
    confidencePpm: number;
    /** Temporal Autoencoder reconstruction MSE scaled by 1e6. */
    autoencoderReconErrorPpm: bigint;
    /** Isolation Forest anomaly score scaled by 1e6. */
    isolationForestScorePpm: bigint;
    offenderAccountIds: bigint[];
    evidenceSubgraph: TransactionGraphEdge[];
    shapAttributions: SHAPFeatureAttribution[];
    washTradingCycle?: WashTradingCycle | undefined;
    spoofingLayering?: SpoofingLayering | undefined;
    velocitySpike?: VelocitySpike | undefined;
    detectedTsNs: bigint;
    inferenceLatencyUs: bigint;
    modelVersion: string;
    trace: TraceMetadata | undefined;
}
/** Cryptographically signed mitigation command published by Aegis SRE Agent. */
export interface RiskMitigationCommand {
    commandId: string;
    sourceAlertId: string;
    incidentId: string;
    action: MitigationActionType;
    targetAccountIds: bigint[];
    targetSymbol: string;
    /** Enforced token-bucket quota in requests/sec when action == THROTTLE_RATE_LIMIT. */
    rateLimitOverrideRps: number;
    cooldownDurationMs: bigint;
    dryRun: boolean;
    requiresHumanApproval: boolean;
    rollbackToken: string;
    reason: string;
    issuedBy: string;
    issuedTsNs: bigint;
    expiresTsNs: bigint;
    /** Ed25519 signature over canonical serialized command payload. */
    ed25519Signature: Uint8Array;
    /** Immutable hash-chained audit log digest (Blake3). */
    auditChainBlake3: Uint8Array;
    trace: TraceMetadata | undefined;
}
export interface AccountNodeSummary {
    accountId: bigint;
    maxSeverityScore: number;
    orderToTradeRatioPpm: bigint;
    cancelRatePpm: number;
    tradedVolumeFp: U128 | undefined;
    activeAlertCount: number;
}
export interface InspectFraudSubgraphRequest {
    centerAccountId: bigint;
    symbol: string;
    maxHops: number;
    lookbackWindowNs: bigint;
}
export interface InspectFraudSubgraphResponse {
    nodes: AccountNodeSummary[];
    edges: TransactionGraphEdge[];
    relatedAlerts: AnomalyAlert[];
    topAttributions: SHAPFeatureAttribution[];
    snapshotTsNs: bigint;
}
export interface ListRecentAlertsRequest {
    symbol: string;
    minSeverityScore: number;
    limit: number;
}
export interface ListRecentAlertsResponse {
    alerts: AnomalyAlert[];
}
export interface StreamAnomalyAlertsRequest {
    symbol: string;
    minSeverityScore: number;
}
export declare const TransactionGraphEdge: MessageFns<TransactionGraphEdge>;
export declare const WashTradingCycle: MessageFns<WashTradingCycle>;
export declare const SpoofingLayering: MessageFns<SpoofingLayering>;
export declare const VelocitySpike: MessageFns<VelocitySpike>;
export declare const SHAPFeatureAttribution: MessageFns<SHAPFeatureAttribution>;
export declare const AnomalyAlert: MessageFns<AnomalyAlert>;
export declare const RiskMitigationCommand: MessageFns<RiskMitigationCommand>;
export declare const AccountNodeSummary: MessageFns<AccountNodeSummary>;
export declare const InspectFraudSubgraphRequest: MessageFns<InspectFraudSubgraphRequest>;
export declare const InspectFraudSubgraphResponse: MessageFns<InspectFraudSubgraphResponse>;
export declare const ListRecentAlertsRequest: MessageFns<ListRecentAlertsRequest>;
export declare const ListRecentAlertsResponse: MessageFns<ListRecentAlertsResponse>;
export declare const StreamAnomalyAlertsRequest: MessageFns<StreamAnomalyAlertsRequest>;
/** Primary gRPC interface exposed by services/sentinel-ml on port 50153. */
export type FraudTelemetryServiceDefinition = typeof FraudTelemetryServiceDefinition;
export declare const FraudTelemetryServiceDefinition: {
    readonly name: "FraudTelemetryService";
    readonly fullName: "hyperion.v1.FraudTelemetryService";
    readonly methods: {
        readonly inspectFraudSubgraph: {
            readonly name: "InspectFraudSubgraph";
            readonly requestType: typeof InspectFraudSubgraphRequest;
            readonly requestStream: false;
            readonly responseType: typeof InspectFraudSubgraphResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly listRecentAlerts: {
            readonly name: "ListRecentAlerts";
            readonly requestType: typeof ListRecentAlertsRequest;
            readonly requestStream: false;
            readonly responseType: typeof ListRecentAlertsResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamAnomalyAlerts: {
            readonly name: "StreamAnomalyAlerts";
            readonly requestType: typeof StreamAnomalyAlertsRequest;
            readonly requestStream: false;
            readonly responseType: typeof AnomalyAlert;
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
//# sourceMappingURL=telemetry_fraud.d.ts.map