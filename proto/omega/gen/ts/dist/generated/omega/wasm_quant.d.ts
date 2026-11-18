import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { TraceMetadata, U128 } from "../common";
/** Deterministic fault & security trap taxonomy raised by Wasmtime Quant Host. */
export declare enum WasmTrapCode {
    WASM_TRAP_CODE_UNSPECIFIED = 0,
    WASM_TRAP_CODE_FUEL_EXHAUSTED = 1,
    WASM_TRAP_CODE_EPOCH_DEADLINE_EXCEEDED = 2,
    WASM_TRAP_CODE_MEMORY_LIMIT_EXCEEDED = 3,
    WASM_TRAP_CODE_STACK_OVERFLOW = 4,
    WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY = 5,
    WASM_TRAP_CODE_INTEGER_DIVIDE_BY_ZERO = 6,
    WASM_TRAP_CODE_INTEGER_OVERFLOW = 7,
    WASM_TRAP_CODE_INVALID_CONVERSION_TO_INT = 8,
    WASM_TRAP_CODE_UNREACHABLE_INSTRUCTION = 9,
    WASM_TRAP_CODE_INDIRECT_CALL_TYPE_MISMATCH = 10,
    WASM_TRAP_CODE_FORBIDDEN_HOST_IMPORT = 11,
    WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED = 12,
    WASM_TRAP_CODE_PRE_TRADE_RISK_REJECTED = 13,
    UNRECOGNIZED = -1
}
export declare function wasmTrapCodeFromJSON(object: any): WasmTrapCode;
export declare function wasmTrapCodeToJSON(object: WasmTrapCode): string;
/** Runtime lifecycle state of a co-located WebAssembly quantitative strategy. */
export declare enum StrategyLifecycleState {
    STRATEGY_LIFECYCLE_STATE_UNSPECIFIED = 0,
    STRATEGY_LIFECYCLE_STATE_VALIDATING = 1,
    STRATEGY_LIFECYCLE_STATE_ACTIVE = 2,
    STRATEGY_LIFECYCLE_STATE_HOT_SWAPPING = 3,
    STRATEGY_LIFECYCLE_STATE_PAUSED = 4,
    STRATEGY_LIFECYCLE_STATE_QUARANTINED_ON_TRAP = 5,
    STRATEGY_LIFECYCLE_STATE_TERMINATED = 6,
    UNRECOGNIZED = -1
}
export declare function strategyLifecycleStateFromJSON(object: any): StrategyLifecycleState;
export declare function strategyLifecycleStateToJSON(object: StrategyLifecycleState): string;
/** Callback hook invoked when fuel telemetry or a sandbox trap occurred. */
export declare enum WasmCallbackHook {
    WASM_CALLBACK_HOOK_UNSPECIFIED = 0,
    WASM_CALLBACK_HOOK_ON_INIT = 1,
    WASM_CALLBACK_HOOK_ON_BOOK_L2_TICK = 2,
    WASM_CALLBACK_HOOK_ON_TRADE_TICK = 3,
    WASM_CALLBACK_HOOK_ON_ORDER_FILL = 4,
    WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_EXPORT = 5,
    WASM_CALLBACK_HOOK_ON_STATE_SNAPSHOT_IMPORT = 6,
    UNRECOGNIZED = -1
}
export declare function wasmCallbackHookFromJSON(object: any): WasmCallbackHook;
export declare function wasmCallbackHookToJSON(object: WasmCallbackHook): string;
/** Pre-trade risk & resource sandbox limits bound to a deployed .wasm strategy instance. */
export interface StrategySandboxLimits {
    /** Maximum linear memory ceiling in bytes (governance hard cap = 16,777,216 = 16 MiB). */
    maxLinearMemoryBytes: number;
    /** Maximum call stack bytes (governance hard cap = 524,288 = 512 KiB). */
    maxStackBytes: number;
    /** Per-tick deterministic instruction fuel budget (default = 50,000; max = 200,000). */
    maxFuelPerTick: bigint;
    /** Hard wall-clock epoch interruption budget in nanoseconds (governance cap = 100,000 ns = 100 us). */
    maxEpochWalltimeNs: bigint;
    /** Maximum host-imported order mutations allowed in a single callback tick (default = 16). */
    maxOrdersPerTick: number;
    /** Maximum single-order notional in fixed-point (scale 1e8). */
    maxOrderNotionalFp: U128 | undefined;
    /** Maximum absolute net base inventory allowed for this bot in fixed-point (scale 1e8). */
    maxAbsPositionQtyFp: bigint;
}
/** Command to validate, JIT-compile, and deploy (or atomically hot-swap) a .wasm strategy module. */
export interface DeployStrategyModule {
    strategyId: string;
    accountId: bigint;
    version: number;
    strategyName: string;
    subscribedSymbols: string[];
    /** Raw compiled WebAssembly binary (.wasm); validated zero-WASI & NaN-canonicalized by host. */
    wasmBytecode: Uint8Array;
    /** 32-byte Blake3 digest of wasm_bytecode. */
    bytecodeBlake3: Uint8Array;
    /** Ed25519 signature of institutional trader over (strategy_id || version || bytecode_blake3). */
    authorEd25519Signature: Uint8Array;
    limits: StrategySandboxLimits | undefined;
    /** If true, atomically snapshots state from (version - 1) and hands off open orders without gap. */
    atomicHotSwap: boolean;
    initialConfigPayload: Uint8Array;
    submittedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface DeployStrategyResponse {
    strategyId: string;
    activeVersion: number;
    state: StrategyLifecycleState;
    jitCompileLatencyUs: bigint;
    handedOffOpenOrders: number;
    rejectReason: string;
}
/** Per-tick or aggregated deterministic CPU fuel & microsecond latency telemetry report. */
export interface FuelMeteringReport {
    strategyId: string;
    accountId: bigint;
    version: number;
    symbol: string;
    engineSeq: bigint;
    callbackHook: WasmCallbackHook;
    fuelAllocated: bigint;
    fuelConsumed: bigint;
    fuelRemaining: bigint;
    /** Host -> Guest -> Host callback execution latency in nanoseconds. */
    executionLatencyNs: bigint;
    linearMemoryPages: number;
    linearMemoryBytes: number;
    ordersEmittedInTick: number;
    ordersRejectedByRiskGate: number;
    reportedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Security & fault isolation event emitted when Wasmtime catches and contains a hostile/faulty guest trap. */
export interface SandboxExecutionTrap {
    trapEventId: string;
    strategyId: string;
    accountId: bigint;
    version: number;
    symbol: string;
    engineSeq: bigint;
    callbackHook: WasmCallbackHook;
    trapCode: WasmTrapCode;
    fuelConsumedAtTrap: bigint;
    elapsedNsAtTrap: bigint;
    memoryBytesAtTrap: number;
    wasmProgramCounterOffset: number;
    openOrdersAutoCanceled: boolean;
    resultingState: StrategyLifecycleState;
    diagnosticMessage: string;
    trappedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface PauseOrTerminateStrategyRequest {
    strategyId: string;
    accountId: bigint;
    terminatePermanently: boolean;
    cancelOpenOrders: boolean;
    trace: TraceMetadata | undefined;
}
export interface PauseOrTerminateStrategyResponse {
    strategyId: string;
    state: StrategyLifecycleState;
    canceledOrdersCount: number;
}
export interface StreamWasmTelemetryRequest {
    strategyId: string;
    includeFuelReports: boolean;
    includeTraps: boolean;
}
export interface WasmTelemetryEnvelope {
    tsNs: bigint;
    fuelReport?: FuelMeteringReport | undefined;
    trapAlert?: SandboxExecutionTrap | undefined;
}
export declare const StrategySandboxLimits: MessageFns<StrategySandboxLimits>;
export declare const DeployStrategyModule: MessageFns<DeployStrategyModule>;
export declare const DeployStrategyResponse: MessageFns<DeployStrategyResponse>;
export declare const FuelMeteringReport: MessageFns<FuelMeteringReport>;
export declare const SandboxExecutionTrap: MessageFns<SandboxExecutionTrap>;
export declare const PauseOrTerminateStrategyRequest: MessageFns<PauseOrTerminateStrategyRequest>;
export declare const PauseOrTerminateStrategyResponse: MessageFns<PauseOrTerminateStrategyResponse>;
export declare const StreamWasmTelemetryRequest: MessageFns<StreamWasmTelemetryRequest>;
export declare const WasmTelemetryEnvelope: MessageFns<WasmTelemetryEnvelope>;
/** Primary gRPC interface exposed by services/quant-wasm-host on port 50164. */
export type WasmQuantHostServiceDefinition = typeof WasmQuantHostServiceDefinition;
export declare const WasmQuantHostServiceDefinition: {
    readonly name: "WasmQuantHostService";
    readonly fullName: "hyperion.omega.v1.WasmQuantHostService";
    readonly methods: {
        readonly deployStrategy: {
            readonly name: "DeployStrategy";
            readonly requestType: typeof DeployStrategyModule;
            readonly requestStream: false;
            readonly responseType: typeof DeployStrategyResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly pauseOrTerminateStrategy: {
            readonly name: "PauseOrTerminateStrategy";
            readonly requestType: typeof PauseOrTerminateStrategyRequest;
            readonly requestStream: false;
            readonly responseType: typeof PauseOrTerminateStrategyResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamWasmTelemetry: {
            readonly name: "StreamWasmTelemetry";
            readonly requestType: typeof StreamWasmTelemetryRequest;
            readonly requestStream: false;
            readonly responseType: typeof WasmTelemetryEnvelope;
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
//# sourceMappingURL=wasm_quant.d.ts.map