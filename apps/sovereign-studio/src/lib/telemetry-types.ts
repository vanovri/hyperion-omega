/**
 * HYPERION OMEGA Channel 20 — Canonical Telemetry Types, 20-Channel Port Matrix,
 * Wasm Strategy Catalog, and Symbolic Alpha / Deflated Sharpe Ratio (DSR) Kernel.
 */

import { normCdf, VolatilityRegime } from "./svi-greeks";

export type TelemetrySourceMode = "AUTO_HYBRID" | "LIVE_CLUSTER" | "STANDALONE_SIM";

export type SovereignPanelId =
  | "ALL_QUAD"
  | "VOL_SURFACE_3D"
  | "RAFT_EBPF_TOPOLOGY"
  | "ZK_SOLVENCY_PORTAL"
  | "WASM_SQL_WORKBENCH";

export type RaftNodeRole = "LEADER" | "FOLLOWER" | "PRE_CANDIDATE" | "CANDIDATE" | "DOWN" | "PARTITIONED";

export interface RaftNodeTelemetry {
  readonly nodeId: number;
  readonly label: string;
  readonly role: RaftNodeRole;
  readonly term: number;
  readonly commitIndex: number;
  readonly appliedIndex: number;
  readonly walBlake3Short: string;
  readonly rttUs: number;
  readonly leaseValid: boolean;
  readonly votesGranted: number;
}

export interface RaftClusterState {
  readonly term: number;
  readonly leaderId: number | null;
  readonly quorumHealthy: boolean;
  readonly readIndexBarrierSeq: number;
  readonly lastFailoverMs: number;
  readonly committedTxLoss: number;
  readonly partitionActive: boolean;
  readonly appendEntriesPerSec: number;
  readonly nodes: ReadonlyArray<RaftNodeTelemetry>;
}

export interface EbpfShieldTelemetry {
  readonly xdpPassPps: number;
  readonly xdpDropPps: number;
  readonly xdpTxPps: number;
  readonly blockedCidrsCount: number;
  readonly invalidMagicDropsPps: number;
  readonly synFloodDropsPps: number;
  readonly floodAttackActive: boolean;
  readonly tcpRttHistogramLog2Us: ReadonlyArray<{ bucketLabel: string; count: number }>;
  readonly radarBlips: ReadonlyArray<{
    readonly angleRad: number;
    readonly radiusNorm: number;
    readonly verdict: "PASS" | "DROP_CIDR" | "DROP_MAGIC" | "DROP_RATELIMIT";
    readonly sourceCidr: string;
  }>;
}

export interface WasmStrategyModuleInfo {
  readonly moduleId: string;
  readonly filename: string;
  readonly version: string;
  readonly status: "ACTIVE_HOT" | "TRAPPED_QUARANTINED" | "STANDBY";
  readonly memoryUsedKb: number;
  readonly memoryLimitKb: number;
  readonly fuelConsumedPerTick: number;
  readonly fuelBudgetPerTick: number;
  readonly p50LatencyUs: number;
  readonly p99LatencyUs: number;
  readonly signalScoreBps: number;
  readonly ordersSubmitted: number;
  readonly lastTrapReason: string | null;
  readonly rustSourceSnippet: string;
}

export interface SymbolicAlphaNode {
  readonly id: string;
  readonly op: string;
  readonly weight?: string;
  readonly children?: ReadonlyArray<SymbolicAlphaNode>;
}

export interface AlphaEvolutionTelemetry {
  readonly generation: number;
  readonly formulaExpression: string;
  readonly astRoot: SymbolicAlphaNode;
  readonly rawSharpe: number;
  readonly deflatedSharpeRatioProb: number;
  readonly pboOverfitProb: number;
  readonly trialsEvaluated: number;
  readonly equityCurve: ReadonlyArray<{ step: number; strategyNav: number; benchmarkNav: number }>;
}

export interface L2RelayerTelemetry {
  readonly latestL2Block: number;
  readonly finalizedL2Block: number;
  readonly anchoredStateSeq: number;
  readonly reorgAlertActive: boolean;
  readonly lastReorgDepth: number;
  readonly rolledBackPrematureDeposits: number;
  readonly doubleSpendAttemptsBlocked: number;
}

export interface OmegaSystemSnapshot {
  readonly tickSeq: number;
  readonly timestampMs: number;
  readonly sourceMode: TelemetrySourceMode;
  readonly liveGatewayConnected: boolean;
  readonly liveChannelsOnline: number;
  readonly symbol: string;
  readonly midPriceFp: string;
  readonly spreadBps: number;
  readonly ofiFp: string;
  readonly engineThroughputOps: number;
  readonly matchingP99Us: number;
  readonly volatilityRegime: VolatilityRegime;
  readonly raft: RaftClusterState;
  readonly ebpf: EbpfShieldTelemetry;
  readonly wasmModules: ReadonlyArray<WasmStrategyModuleInfo>;
  readonly alpha: AlphaEvolutionTelemetry;
  readonly l2Relayer: L2RelayerTelemetry;
}

export type WorkerInboundCommand =
  | { type: "INIT"; mode: TelemetrySourceMode; wsUrl: string }
  | { type: "SET_MODE"; mode: TelemetrySourceMode }
  | { type: "SET_VOL_REGIME"; regime: VolatilityRegime }
  | { type: "KILL_RAFT_LEADER" }
  | { type: "HEAL_RAFT_CLUSTER" }
  | { type: "TOGGLE_NETWORK_PARTITION" }
  | { type: "TRIGGER_EBPF_FLOOD"; active: boolean }
  | { type: "TRIGGER_WASM_MALWARE_TRAP" }
  | { type: "HOT_SWAP_WASM_MODULE"; moduleId: string }
  | { type: "TRIGGER_L2_REORG_4_BLOCKS" };

const EULER_MASCHERONI = 0.5772156649015329;

/**
 * Rational inverse standard normal CDF Phi^{-1}(p) (Peter Acklam / Beasley-Springer-Moro approximation)
 * Used for exact Deflated Sharpe Ratio (Bailey & López de Prado, 2014).
 */
export function normInv(p: number): number {
  const clamped = Math.min(Math.max(p, 1e-12), 1 - 1e-12);
  const a = [
    -3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2,
    1.38357751867269e2, -3.066479806614716e1, 2.506628277459239,
  ];
  const b = [
    -5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2,
    6.680131188771972e1, -1.328068155288572e1,
  ];
  const c = [
    -7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838,
    -2.549732539343734, 4.374664141464968, 2.938163982698783,
  ];
  const d = [
    7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996,
    3.754408661907416,
  ];
  const pLow = 0.02425;
  const pHigh = 1 - pLow;

  if (clamped < pLow) {
    const q = Math.sqrt(-2 * Math.log(clamped));
    return (
      (((((c[0]! * q + c[1]!) * q + c[2]!) * q + c[3]!) * q + c[4]!) * q + c[5]!) /
      ((((d[0]! * q + d[1]!) * q + d[2]!) * q + d[3]!) * q + 1)
    );
  }
  if (clamped <= pHigh) {
    const q = clamped - 0.5;
    const r = q * q;
    return (
      ((((((a[0]! * r + a[1]!) * r + a[2]!) * r + a[3]!) * r + a[4]!) * r + a[5]!) * q) /
      (((((b[0]! * r + b[1]!) * r + b[2]!) * r + b[3]!) * r + b[4]!) * r + 1)
    );
  }
  const q = Math.sqrt(-2 * Math.log(1 - clamped));
  return -(
    (((((c[0]! * q + c[1]!) * q + c[2]!) * q + c[3]!) * q + c[4]!) * q + c[5]!) /
    ((((d[0]! * q + d[1]!) * q + d[2]!) * q + d[3]!) * q + 1)
  );
}

/**
 * Bailey & López de Prado (2014) Deflated Sharpe Ratio (DSR):
 * Corrects observed Sharpe ratio for multiple-testing selection bias (N trials)
 * and non-normal return skewness (gamma3) & kurtosis (gamma4).
 */
export function computeDeflatedSharpeRatio(params: {
  observedSharpe: number;
  trialVariance: number;
  numTrials: number;
  sampleLength: number;
  skewness: number;
  kurtosis: number;
}): { expectedMaxNullSharpe: number; dsrProbability: number } {
  const N = Math.max(params.numTrials, 2);
  const T = Math.max(params.sampleLength, 10);
  const stdTrials = Math.sqrt(Math.max(params.trialVariance, 1e-8));

  const z1 = normInv(1.0 - 1.0 / N);
  const z2 = normInv(1.0 - 1.0 / (N * Math.E));
  const sr0 = stdTrials * ((1.0 - EULER_MASCHERONI) * z1 + EULER_MASCHERONI * z2);

  const sr = params.observedSharpe;
  const denomVar = Math.max(
    1.0 - params.skewness * sr + ((params.kurtosis - 1.0) / 4.0) * sr * sr,
    1e-6
  );
  const zStat = ((sr - sr0) * Math.sqrt(T - 1.0)) / Math.sqrt(denomVar);
  return {
    expectedMaxNullSharpe: sr0,
    dsrProbability: normCdf(zStat),
  };
}

export const REFERENCE_WASM_MODULES: ReadonlyArray<WasmStrategyModuleInfo> = [
  {
    moduleId: "stat_arb_zscore",
    filename: "stat_arb_zscore.wasm",
    version: "v1.4.2",
    status: "ACTIVE_HOT",
    memoryUsedKb: 1_840,
    memoryLimitKb: 16_384,
    fuelConsumedPerTick: 4_120,
    fuelBudgetPerTick: 50_000,
    p50LatencyUs: 4.8,
    p99LatencyUs: 12.6,
    signalScoreBps: 18.4,
    ordersSubmitted: 14_820,
    lastTrapReason: null,
    rustSourceSnippet: `#[no_mangle]
pub extern "C" fn on_book_l2_tick(bid_fp: u64, ask_fp: u64, ofi_fp: i64) -> i32 {
    // Zero-syscall Wasmtime guest: rolling Z-score + OFI mean reversion
    let mid_fp = (bid_fp >> 1) + (ask_fp >> 1);
    let z_ppm = STATE.update_spread_zscore(mid_fp, ofi_fp);
    if z_ppm < -2_150_000 {
        host_submit_limit_order(SIDE_BUY, bid_fp, 25_000_000) // 0.25 BTC (fp 1e8)
    } else if z_ppm > 2_150_000 {
        host_submit_limit_order(SIDE_SELL, ask_fp, 25_000_000)
    } else { 0 }
}`,
  },
  {
    moduleId: "obi_sniper",
    filename: "obi_sniper.wasm",
    version: "v2.1.0",
    status: "ACTIVE_HOT",
    memoryUsedKb: 1_280,
    memoryLimitKb: 16_384,
    fuelConsumedPerTick: 2_890,
    fuelBudgetPerTick: 50_000,
    p50LatencyUs: 3.4,
    p99LatencyUs: 9.8,
    signalScoreBps: 24.1,
    ordersSubmitted: 29_410,
    lastTrapReason: null,
    rustSourceSnippet: `#[no_mangle]
pub extern "C" fn on_book_l2_tick(top5_bid_qty_fp: u64, top5_ask_qty_fp: u64, best_ask_fp: u64) -> i32 {
    // 5-level Order Book Imbalance microstructure liquidity sweep sniper
    let denom = top5_bid_qty_fp.saturating_add(top5_ask_qty_fp).max(1);
    let obi_ppm = ((top5_bid_qty_fp as i128 - top5_ask_qty_fp as i128) * 1_000_000) / (denom as i128);
    if obi_ppm > 680_000 {
        host_submit_ioc_order(SIDE_BUY, best_ask_fp, 15_000_000)
    } else { 0 }
}`,
  },
  {
    moduleId: "delta_hedger",
    filename: "delta_hedger.wasm",
    version: "v1.9.5",
    status: "ACTIVE_HOT",
    memoryUsedKb: 2_112,
    memoryLimitKb: 16_384,
    fuelConsumedPerTick: 5_340,
    fuelBudgetPerTick: 50_000,
    p50LatencyUs: 6.1,
    p99LatencyUs: 16.4,
    signalScoreBps: 9.2,
    ordersSubmitted: 8_905,
    lastTrapReason: null,
    rustSourceSnippet: `#[no_mangle]
pub extern "C" fn on_order_fill(net_options_delta_fp: i64, perp_position_fp: i64, mark_fp: u64) -> i32 {
    // Continuous cross-margin portfolio Delta-neutrality hedger (Ch14 <-> Ch15)
    let residual_fp = net_options_delta_fp.saturating_add(perp_position_fp);
    if residual_fp.abs() > 10_000_000 {
        let hedge_side = if residual_fp > 0 { SIDE_SELL } else { SIDE_BUY };
        host_submit_limit_order(hedge_side, mark_fp, residual_fp.unsigned_abs())
    } else { 0 }
}`,
  },
];

export const CANONICAL_SYMBOLIC_ALPHA_AST: SymbolicAlphaNode = {
  id: "root-mul",
  op: "mul_fp",
  children: [
    {
      id: "left-sub",
      op: "ts_zscore(w=32)",
      children: [
        {
          id: "ofi-term",
          op: "ofi_delta(depth=5)",
        },
      ],
    },
    {
      id: "right-add",
      op: "ts_decay_linear(d=12)",
      children: [
        { id: "skew-leaf", op: "microprice_skew", weight: "0.64" },
        { id: "corr-leaf", op: "neg(ts_corr( spread, vol, 20 ))", weight: "0.36" },
      ],
    },
  ],
};
