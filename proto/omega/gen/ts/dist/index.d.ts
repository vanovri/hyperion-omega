export * as ConsensusProto from "./generated/omega/consensus";
export * as ZkSolvencyProto from "./generated/omega/zk_solvency";
export * as MarginRiskProto from "./generated/omega/margin_risk";
export * as WasmQuantProto from "./generated/omega/wasm_quant";
export * as SettlementL2Proto from "./generated/omega/settlement_l2";
export declare const OMEGA_GOVERNANCE: {
    readonly WASM_MAX_LINEAR_MEMORY_BYTES: number;
    readonly WASM_MAX_STACK_BYTES: number;
    readonly WASM_DEFAULT_FUEL_PER_TICK: 50000n;
    readonly WASM_MAX_EPOCH_WALLTIME_NS: 100000n;
    readonly L2_MIN_FINALITY_BLOCKS: 12;
    readonly HYPERION_L2_CANONICAL_CHAIN_ID: 994201n;
    readonly FP_SCALE: 100000000n;
    readonly PPM_SCALE: 1000000n;
};
//# sourceMappingURL=index.d.ts.map