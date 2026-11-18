import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { AccountStatus, I128, MarketRegime, OrderSide, SelfTradePreventionMode, TraceMetadata, U128 } from "./common";
import { ExecutionFill, L2BookSnapshot, L3BookDelta, MerkleLedgerProof } from "./orderbook";
/** Interactive VCR playback transport control commands. */
export declare enum ReplayControlAction {
    REPLAY_CONTROL_ACTION_UNSPECIFIED = 0,
    REPLAY_CONTROL_ACTION_PLAY = 1,
    REPLAY_CONTROL_ACTION_PAUSE = 2,
    REPLAY_CONTROL_ACTION_STEP_FORWARD = 3,
    REPLAY_CONTROL_ACTION_STEP_BACKWARD = 4,
    REPLAY_CONTROL_ACTION_REWIND = 5,
    REPLAY_CONTROL_ACTION_SET_SPEED = 6,
    UNRECOGNIZED = -1
}
export declare function replayControlActionFromJSON(object: any): ReplayControlAction;
export declare function replayControlActionToJSON(object: ReplayControlAction): string;
/** Request to deterministically reconstruct exact exchange & ledger state at sequence or nanosecond T. */
export interface TimeTravelSeekRequest {
    symbol: string;
    targetEngineSeq?: bigint | undefined;
    targetTsNs?: bigint | undefined;
    l2Depth: number;
    includeAccountBalances: boolean;
    verifyWalHashChain: boolean;
    trace: TraceMetadata | undefined;
}
/** Reconstructed historical point-in-time state returned by Chronos. */
export interface TimeTravelSeekResponse {
    resolvedEngineSeq: bigint;
    resolvedTsNs: bigint;
    baseSnapshotSeq: bigint;
    replayedWalEvents: bigint;
    reconstructionLatencyUs: bigint;
    walChainVerified: boolean;
    stateRootBlake3: Uint8Array;
    l2Snapshot: L2BookSnapshot | undefined;
    accountStates: MerkleLedgerProof[];
}
/** Counterfactual override freezing or throttling a specific account at a chosen point in the fork. */
export interface AccountPolicyOverride {
    accountId: bigint;
    statusOverride: AccountStatus;
    /** Engine sequence from which this override takes effect (0 = from fork start_seq). */
    effectiveFromSeq: bigint;
    /** Effective nanosecond timestamp from which this override applies (0 = from fork start_ts_ns). */
    effectiveFromTsNs: bigint;
    rateLimitRps: number;
    cancelRestingOrdersOnActivation: boolean;
}
/** Counterfactual fee & risk parameter bundle for "What-If" sandbox simulation. */
export interface SimulationPolicyOverride {
    /** Optional maker fee override in signed parts-per-million (scale 1e6; negative = rebate). */
    makerFeePpmOverride?: bigint | undefined;
    /** Optional taker fee override in signed parts-per-million (scale 1e6). */
    takerFeePpmOverride?: bigint | undefined;
    /** Optional cap on single order quantity in fixed-point (scale 1e8). */
    maxOrderQtyFpOverride?: bigint | undefined;
    /** Optional cap on single order quote notional in fixed-point (scale 1e8). */
    maxOrderNotionalFpOverride: U128 | undefined;
    /** Optional forced Self-Trade Prevention policy across all accounts. */
    stpModeOverride: SelfTradePreventionMode;
    /** Optional market regime override (e.g., VOLATILITY_GUARD or POST_ONLY_AUCTION). */
    regimeOverride: MarketRegime;
    /** Optional maximum allowed spread in bps before automatic post-only protection triggers. */
    volatilityGuardSpreadBps?: number | undefined;
    accountOverrides: AccountPolicyOverride[];
}
/** Request to branch historical WAL stream [T1, T2] under counterfactual fee/risk rules. */
export interface TimelineForkRequest {
    forkName: string;
    symbol: string;
    startEngineSeq: bigint;
    endEngineSeq: bigint;
    startTsNs: bigint;
    endTsNs: bigint;
    /** Empty / unset overrides MUST yield a bit-identical zero diff against baseline. */
    policyOverride: SimulationPolicyOverride | undefined;
    trace: TraceMetadata | undefined;
}
/** Per-account balance delta between Baseline and Counterfactual Forked timeline at T2. */
export interface AccountBalanceDiff {
    accountId: bigint;
    asset: string;
    baselineTotalFp: I128 | undefined;
    forkedTotalFp: I128 | undefined;
    /** forked_total_fp - baseline_total_fp in signed 128-bit fixed-point (scale 1e8). */
    deltaTotalFp: I128 | undefined;
}
/** Per-price-tick L2 liquidity difference between Baseline and Forked book at T2. */
export interface PriceLevelDiff {
    side: OrderSide;
    priceFp: bigint;
    baselineQtyFp: bigint;
    forkedQtyFp: bigint;
    deltaQtyFp: bigint;
}
/** High-precision comparison between actual production history (Baseline) and Counterfactual Fork. */
export interface StateTreeDiffResponse {
    forkId: string;
    symbol: string;
    startEngineSeq: bigint;
    endEngineSeq: bigint;
    baselineStateRootBlake3: Uint8Array;
    forkedStateRootBlake3: Uint8Array;
    /** True iff baseline_state_root_blake3 == forked_state_root_blake3 and all diffs == 0. */
    stateIdentical: boolean;
    baselineClearingPriceFp: bigint;
    forkedClearingPriceFp: bigint;
    clearingPriceDeltaFp: bigint;
    baselineVolatilityBps: number;
    forkedVolatilityBps: number;
    maxSlippageDeltaBps: number;
    /** Total circular / manipulative volume prevented by the counterfactual policy (scale 1e8). */
    preventedWashVolumeFp: U128 | undefined;
    preventedFillsCount: bigint;
    rejectedOrdersCount: bigint;
    treasuryFeeDeltaFp: I128 | undefined;
    accountDiffs: AccountBalanceDiff[];
    l2LevelDiffs: PriceLevelDiff[];
    replayedEventsCount: bigint;
    computationLatencyUs: bigint;
}
/** Variable-speed VCR frame emitted during historical or counterfactual timeline playback. */
export interface ReplayFrameStream {
    sessionId: string;
    /** "baseline" or a counterfactual fork_id. */
    timelineId: string;
    symbol: string;
    frameIndex: bigint;
    engineSeq: bigint;
    eventTsNs: bigint;
    /** Integer milli-multiplier: 100 = 0.1x slow-mo, 1000 = 1.0x real-time, 100000 = 100.0x fast-forward. */
    speedMilli: number;
    isPaused: boolean;
    stateRootBlake3: Uint8Array;
    l2Snapshot: L2BookSnapshot | undefined;
    frameDeltas: L3BookDelta[];
    frameFills: ExecutionFill[];
}
export interface StreamReplayFramesRequest {
    sessionId: string;
    timelineId: string;
    symbol: string;
    fromEngineSeq: bigint;
    toEngineSeq: bigint;
    /** Playback speed in milli-units (100 = 0.1x, 1000 = 1.0x, 100000 = 100x). */
    initialSpeedMilli: number;
    l2Depth: number;
}
export interface ControlReplaySessionRequest {
    sessionId: string;
    action: ReplayControlAction;
    speedMilli: number;
    stepCount: bigint;
}
export interface ControlReplaySessionResponse {
    sessionId: string;
    currentEngineSeq: bigint;
    effectiveSpeedMilli: number;
    isPaused: boolean;
}
export declare const TimeTravelSeekRequest: MessageFns<TimeTravelSeekRequest>;
export declare const TimeTravelSeekResponse: MessageFns<TimeTravelSeekResponse>;
export declare const AccountPolicyOverride: MessageFns<AccountPolicyOverride>;
export declare const SimulationPolicyOverride: MessageFns<SimulationPolicyOverride>;
export declare const TimelineForkRequest: MessageFns<TimelineForkRequest>;
export declare const AccountBalanceDiff: MessageFns<AccountBalanceDiff>;
export declare const PriceLevelDiff: MessageFns<PriceLevelDiff>;
export declare const StateTreeDiffResponse: MessageFns<StateTreeDiffResponse>;
export declare const ReplayFrameStream: MessageFns<ReplayFrameStream>;
export declare const StreamReplayFramesRequest: MessageFns<StreamReplayFramesRequest>;
export declare const ControlReplaySessionRequest: MessageFns<ControlReplaySessionRequest>;
export declare const ControlReplaySessionResponse: MessageFns<ControlReplaySessionResponse>;
/** Primary gRPC interface exposed by services/chronos-replay on port 50154. */
export type ChronosReplayServiceDefinition = typeof ChronosReplayServiceDefinition;
export declare const ChronosReplayServiceDefinition: {
    readonly name: "ChronosReplayService";
    readonly fullName: "hyperion.v1.ChronosReplayService";
    readonly methods: {
        readonly timeTravelSeek: {
            readonly name: "TimeTravelSeek";
            readonly requestType: typeof TimeTravelSeekRequest;
            readonly requestStream: false;
            readonly responseType: typeof TimeTravelSeekResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly timelineFork: {
            readonly name: "TimelineFork";
            readonly requestType: typeof TimelineForkRequest;
            readonly requestStream: false;
            readonly responseType: typeof StateTreeDiffResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamReplayFrames: {
            readonly name: "StreamReplayFrames";
            readonly requestType: typeof StreamReplayFramesRequest;
            readonly requestStream: false;
            readonly responseType: typeof ReplayFrameStream;
            readonly responseStream: true;
            readonly options: {};
        };
        readonly controlReplaySession: {
            readonly name: "ControlReplaySession";
            readonly requestType: typeof ControlReplaySessionRequest;
            readonly requestStream: false;
            readonly responseType: typeof ControlReplaySessionResponse;
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
//# sourceMappingURL=chronos.d.ts.map