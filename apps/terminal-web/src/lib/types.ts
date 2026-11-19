/**
 * HYPERION Institutional Terminal — Strict Domain & Worker Protocol Types
 */

export type InstrumentSymbol = "BTC-USDT" | "ETH-USDT" | "SOL-USDT" | "HYPE-USDT";

export type CandleTimeframe = "1s" | "5s" | "1m" | "5m" | "15m" | "1h";

export type EngineMode = "AUTO_HYBRID" | "LIVE_MESH" | "STANDALONE_SIM";

export type ConnectionState = "CONNECTED_LIVE" | "RECONNECTING" | "STANDALONE_ACTIVE" | "CONNECTING";

export type WorkspaceId =
  | "ORDERBOOK_HEATMAP"
  | "FRAUD_FORCE_GRAPH"
  | "CHRONOS_FORK_DIFF"
  | "CHAOS_CONTROL_DECK"
  | "AEGIS_SRE_CONSOLE";

export type ChaosRegime =
  | "CALM"
  | "HIGH_VOLATILITY"
  | "FLASH_CRASH"
  | "SPOOFING_ATTACK"
  | "WASH_TRADING_RING"
  | "BURST_TSUNAMI_20X"
  | "NETWORK_PARTITION";

export const INSTRUMENT_META: Record<
  InstrumentSymbol,
  { id: number; tickFp: bigint; lotFp: bigint; baseDecimals: number; quoteDecimals: number; basePriceFp: bigint }
> = {
  "BTC-USDT": {
    id: 1,
    tickFp: 1_000_000n,
    lotFp: 1_000n,
    baseDecimals: 4,
    quoteDecimals: 2,
    basePriceFp: 6_842_550_000_000n, // 68,425.50 USDT
  },
  "ETH-USDT": {
    id: 2,
    tickFp: 1_000_000n,
    lotFp: 10_000n,
    baseDecimals: 3,
    quoteDecimals: 2,
    basePriceFp: 348_220_000_000n, // 3,482.20 USDT
  },
  "SOL-USDT": {
    id: 3,
    tickFp: 100_000n,
    lotFp: 100_000n,
    baseDecimals: 2,
    quoteDecimals: 3,
    basePriceFp: 18_465_000_000n, // 184.650 USDT
  },
  "HYPE-USDT": {
    id: 4,
    tickFp: 10_000n,
    lotFp: 1_000_000n,
    baseDecimals: 2,
    quoteDecimals: 4,
    basePriceFp: 4_285_000_000n, // 42.8500 USDT
  },
};

export const ID_TO_SYMBOL: Record<number, InstrumentSymbol> = {
  1: "BTC-USDT",
  2: "ETH-USDT",
  3: "SOL-USDT",
  4: "HYPE-USDT",
};

export interface L2Level {
  priceFp: bigint;
  totalQtyFp: bigint;
  cumulativeQtyFp: bigint;
  cumulativeNotionalFp: bigint;
  orderCount: number;
}

export interface TradeFillPrint {
  fillId: bigint;
  engineSeq: bigint;
  symbol: InstrumentSymbol;
  makerAccountId: bigint;
  takerAccountId: bigint;
  priceFp: bigint;
  qtyFp: bigint;
  notionalFp: bigint;
  side: "BUY" | "SELL";
  tsNs: bigint;
  isWashSuspect?: boolean;
}

export interface CandleBar {
  bucketOpenTsNs: bigint;
  openFp: bigint;
  highFp: bigint;
  lowFp: bigint;
  closeFp: bigint;
  volumeFp: bigint;
  takerBuyVolumeFp: bigint;
  vwapFp: bigint;
  twapFp: bigint;
  ofiFp: bigint;
  imbalanceTop5Ppm: number;
  kylesLambdaFp: bigint;
  tradeCount: bigint;
  isClosed: boolean;
}

export interface HeatmapSlice {
  tsNs: bigint;
  engineSeq: bigint;
  midPriceFp: bigint;
  /** 32 price buckets centered around reference price; intensity in [0, 1000] permille, signed (+bid, -ask). */
  bucketIntensities: number[];
  minBucketPriceFp: bigint;
  bucketStepFp: bigint;
}

export interface ShapFeatureEntry {
  featureName: string;
  featureValueFp: bigint;
  baselineValueFp: bigint;
  shapContributionPpm: number;
  importanceRank: number;
}

export interface FraudGraphNode {
  accountId: bigint;
  label: string;
  role: "MARKET_MAKER" | "QUANT_ARB" | "RETAIL" | "WASH_SYNDICATE" | "SPOOFER_BOT";
  status: "ACTIVE" | "THROTTLED" | "FROZEN";
  anomalyScore: number; // 0..100
  confidencePpm: number;
  orderToTradeRatioPpm: bigint;
  cancelRatePpm: number;
  tradedVolumeFp: bigint;
  inWashCycle: boolean;
  cycleId: string | null;
  shapAttributions: ShapFeatureEntry[];
}

export interface FraudGraphEdge {
  edgeId: bigint;
  makerAccountId: bigint;
  takerAccountId: bigint;
  priceFp: bigint;
  qtyFp: bigint;
  notionalFp: bigint;
  tsNs: bigint;
  decayWeightPpm: number;
  isWashCycleEdge: boolean;
  cycleId: string | null;
}

export interface FraudAlertRecord {
  alertId: string;
  engineSeq: bigint;
  symbol: InstrumentSymbol;
  patternType: "WASH_TRADING_CYCLE" | "SPOOFING_LAYERING" | "VELOCITY_SPIKE" | "MOMENTUM_IGNITION" | "LATENCY_ARBITRAGE";
  severityScore: number;
  confidencePpm: number;
  autoencoderReconPpm: bigint;
  isolationForestPpm: bigint;
  offenderAccountIds: bigint[];
  cycleRingIds: bigint[];
  circularVolumeFp: bigint;
  volumeSymmetryPpm: number;
  cancelToFillNs: bigint;
  shapAttributions: ShapFeatureEntry[];
  detectedTsNs: bigint;
  inferenceLatencyUs: number;
  modelVersion: string;
}

export interface ChronosTimelineMarker {
  engineSeq: bigint;
  tsNs: bigint;
  kind: "FLASH_CRASH" | "WASH_RING" | "SPOOF_BURST" | "AEGIS_FREEZE" | "SNAPSHOT";
  label: string;
  midPriceFp: bigint;
}

export interface CounterfactualForkConfig {
  forkName: string;
  symbol: InstrumentSymbol;
  startEngineSeq: bigint;
  endEngineSeq: bigint;
  makerFeePpmOverride: number | null;
  takerFeePpmOverride: number | null;
  maxOrderQtyFpOverride: bigint | null;
  stpModeOverride: "NONE" | "CANCEL_RESTING" | "CANCEL_AGGRESSING" | "CANCEL_BOTH";
  regimeOverride: "NORMAL" | "VOLATILITY_GUARD" | "POST_ONLY_AUCTION";
  blockedAccountIds: bigint[];
}

export interface AccountBalanceDiffItem {
  accountId: bigint;
  asset: string;
  baselineTotalFp: bigint;
  forkedTotalFp: bigint;
  deltaTotalFp: bigint;
}

export interface ChronosForkDiffResult {
  forkId: string;
  forkName: string;
  symbol: InstrumentSymbol;
  startEngineSeq: bigint;
  endEngineSeq: bigint;
  baselineStateRootHex: string;
  forkedStateRootHex: string;
  stateIdentical: boolean;
  baselineClearingPriceFp: bigint;
  forkedClearingPriceFp: bigint;
  clearingPriceDeltaFp: bigint;
  baselineVolatilityBps: number;
  forkedVolatilityBps: number;
  maxSlippageDeltaBps: number;
  preventedWashVolumeFp: bigint;
  preventedFillsCount: bigint;
  rejectedOrdersCount: bigint;
  treasuryFeeDeltaFp: bigint;
  replayedEventsCount: bigint;
  computationLatencyUs: number;
  baselinePriceSeriesFp: bigint[];
  forkedPriceSeriesFp: bigint[];
  baselineBids: L2Level[];
  baselineAsks: L2Level[];
  forkedBids: L2Level[];
  forkedAsks: L2Level[];
  accountDiffs: AccountBalanceDiffItem[];
}

export interface ChronosReplayState {
  isReplayActive: boolean;
  isPaused: boolean;
  speedMilli: number; // 100 = 0.1x, 1000 = 1.0x, 5000 = 5x, 25000 = 25x, 100000 = 100x
  minEngineSeq: bigint;
  maxEngineSeq: bigint;
  currentEngineSeq: bigint;
  currentTsNs: bigint;
  walChainVerified: boolean;
  reconstructionLatencyUs: number;
  markers: ChronosTimelineMarker[];
  priceHistoryFp: { seq: bigint; priceFp: bigint; volatilityBps: number }[];
  activeForkDiff: ChronosForkDiffResult | null;
}

export interface ChaosTelemetrySample {
  tsMs: number;
  ledgerP99Us: number;
  gatewayP99Ms: number;
  sentinelP99Ms: number;
  ordersPerSec: number;
  droppedSlowConsumers: number;
  replayRejections: number;
}

export interface ChaosDeckState {
  activeRegime: ChaosRegime;
  seed: bigint;
  addedLatencyMs: number;
  paretoJitterMs: number;
  packetDropPct: number;
  clockSkewMs: number;
  ordersPerSec: number;
  activeBotCount: number;
  invariantHolds: boolean;
  uncrossedBookHolds: boolean;
  totalDepositsFp: bigint;
  totalUserEquityFp: bigint;
  treasuryFeePoolFp: bigint;
  conservationDriftFp: bigint;
  samples: ChaosTelemetrySample[];
}

export type AegisStage =
  | "OBSERVE"
  | "HYPOTHESIZE"
  | "VERIFY_MCP"
  | "POLICY_GATE"
  | "REMEDIATE"
  | "POST_MORTEM";

export interface AegisThoughtLog {
  id: string;
  tsNs: bigint;
  stage: AegisStage;
  reasoningMode: "DETERMINISTIC_RULE" | "LLM_STRUCTURED";
  summary: string;
  mcpTool?: {
    name:
      | "query_ledger_merkle_audit"
      | "inspect_fraud_subgraph"
      | "analyze_system_telemetry"
      | "execute_risk_mitigation";
    argsJson: string;
    resultSummary: string;
    latencyMs: number;
  };
  guardrailBlocked?: {
    attackVector: string;
    reason: string;
  };
}

export interface AegisApprovalItem {
  commandId: string;
  incidentId: string;
  sourceAlertId: string;
  action: "FREEZE_ACCOUNT" | "THROTTLE_RATE_LIMIT" | "CANCEL_OPEN_ORDERS" | "ENABLE_VOLATILITY_GUARD";
  targetAccountIds: bigint[];
  targetSymbol: InstrumentSymbol;
  rateLimitRps: number;
  cooldownMs: number;
  reason: string;
  ed25519SigHex: string;
  auditChainBlake3Hex: string;
  rollbackToken: string;
  status: "PENDING_APPROVAL" | "APPROVED_EXECUTED" | "REJECTED_OPERATOR" | "ROLLED_BACK";
  issuedTsNs: bigint;
}

export interface AegisPostMortemReport {
  incidentId: string;
  title: string;
  severity: "SEV-1" | "SEV-2";
  createdTsNs: bigint;
  rootCause: string;
  mitigationCommandId: string;
  rollbackToken: string;
  preventedLossFp: bigint;
  markdownBody: string;
  timelineEvents: { tsOffsetMs: number; actor: string; detail: string }[];
}

export interface AegisConsoleState {
  currentStage: AegisStage;
  reasoningMode: "DETERMINISTIC_RULE" | "LLM_STRUCTURED";
  promptInjectionsBlocked: number;
  thoughts: AegisThoughtLog[];
  approvals: AegisApprovalItem[];
  postMortems: AegisPostMortemReport[];
}

export interface PipelineStats {
  workerFramesDecoded: number;
  workerBytesDecoded: number;
  framesPerSec: number;
  bytesPerSec: number;
  crc32cVerifiedCount: number;
  sequenceGapResyncs: number;
  reconnectCount: number;
  lastRawHeaderHex: string;
  lastMsgType: number;
  stateRootBlake3Hex: string;
  serviceHealth: Record<string, "ONLINE" | "SIMULATED" | "DEGRADED">;
}

/** Complete coalesced 60Hz frame snapshot posted from Dedicated WebWorker to Main Thread. */
export interface WorkerFramePacket {
  frameId: number;
  workerNowMs: number;
  mode: EngineMode;
  connectionState: ConnectionState;
  symbol: InstrumentSymbol;
  timeframe: CandleTimeframe;
  engineSeq: bigint;
  timestampNs: bigint;
  bestBidFp: bigint;
  bestAskFp: bigint;
  midPriceFp: bigint;
  spreadFp: bigint;
  spreadBps: number;
  vwapFp: bigint;
  twapFp: bigint;
  ofiFp: bigint;
  imbalanceTop5Ppm: number;
  imbalanceTop10Ppm: number;
  imbalanceTop20Ppm: number;
  kylesLambdaFp: bigint;
  bids: L2Level[];
  asks: L2Level[];
  recentFills: TradeFillPrint[];
  candles: CandleBar[];
  heatmap: HeatmapSlice[];
  fraudNodes: FraudGraphNode[];
  fraudEdges: FraudGraphEdge[];
  fraudAlerts: FraudAlertRecord[];
  chronos: ChronosReplayState;
  chaos: ChaosDeckState;
  aegis: AegisConsoleState;
  pipeline: PipelineStats;
}

/** Commands sent from Main Thread UI to Dedicated WebWorker. */
export type MainToWorkerCommand =
  | { type: "INIT"; symbol: InstrumentSymbol; timeframe: CandleTimeframe; mode: EngineMode }
  | { type: "SET_SYMBOL"; symbol: InstrumentSymbol }
  | { type: "SET_TIMEFRAME"; timeframe: CandleTimeframe }
  | { type: "SET_MODE"; mode: EngineMode }
  | { type: "SET_PAUSED_BY_VISIBILITY"; paused: boolean }
  | { type: "SIMULATE_WS_DISCONNECT" }
  | { type: "CHRONOS_SEEK"; targetEngineSeq: bigint }
  | { type: "CHRONOS_CONTROL"; action: "PLAY" | "PAUSE" | "STEP_FWD" | "STEP_BWD" | "RETURN_LIVE"; speedMilli?: number }
  | { type: "CHRONOS_FORK"; config: CounterfactualForkConfig }
  | { type: "CHAOS_SET_REGIME"; regime: ChaosRegime; seed?: bigint }
  | {
      type: "CHAOS_SET_FAULTS";
      addedLatencyMs: number;
      paretoJitterMs: number;
      packetDropPct: number;
      clockSkewMs: number;
    }
  | { type: "AEGIS_DECIDE_APPROVAL"; commandId: string; decision: "APPROVE" | "REJECT" | "ROLLBACK" }
  | { type: "AEGIS_ESCALATE_ACCOUNT"; accountId: bigint; reason: string }
  | { type: "AEGIS_SIMULATE_PROMPT_INJECTION" };
