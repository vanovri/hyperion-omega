export * as ConsensusProto from "./generated/omega/consensus";
export * as ZkSolvencyProto from "./generated/omega/zk_solvency";
export * as MarginRiskProto from "./generated/omega/margin_risk";
export * as WasmQuantProto from "./generated/omega/wasm_quant";
export * as SettlementL2Proto from "./generated/omega/settlement_l2";

export const OMEGA_GOVERNANCE = {
  WASM_MAX_LINEAR_MEMORY_BYTES: 16 * 1024 * 1024,
  WASM_MAX_STACK_BYTES: 512 * 1024,
  WASM_DEFAULT_FUEL_PER_TICK: 50_000n,
  WASM_MAX_EPOCH_WALLTIME_NS: 100_000n,
  L2_MIN_FINALITY_BLOCKS: 12,
  HYPERION_L2_CANONICAL_CHAIN_ID: 994_201n,
  FP_SCALE: 100_000_000n,
  PPM_SCALE: 1_000_000n,
} as const;
