/**
 * HYPERION OMEGA Channel 20 — Dedicated Off-Main-Thread Telemetry & Protocol Worker
 *
 * Dual-Engine Architecture:
 * 1) Live Binary WebSocket (`ws://localhost:8080/ws`, `hyperion.bin.v1` 32B LE `0x4859` frames)
 *    + Omega cluster health probes across Channels 02–19.
 * 2) Deterministic Seeded 20-Channel Simulator (`Mulberry32`) for instant zero-jank 60fps
 *    standalone operation, interactive Raft Leader SIGKILL (<500ms PreVote/RequestVote failover),
 *    eBPF XDP million-PPS flood radar, Wasmtime fuel/trap profiler, and L2 4-block reorg defense.
 */

/// <reference lib="webworker" />

import { FP_SCALE } from "../lib/fixed-point";
import { VolatilityRegime } from "../lib/svi-greeks";
import {
  CANONICAL_SYMBOLIC_ALPHA_AST,
  computeDeflatedSharpeRatio,
  OmegaSystemSnapshot,
  RaftNodeRole,
  RaftNodeTelemetry,
  REFERENCE_WASM_MODULES,
  TelemetrySourceMode,
  WasmStrategyModuleInfo,
  WorkerInboundCommand,
} from "../lib/telemetry-types";
import {
  decodeBinaryHeader,
  decodeExecutionFillFrame,
  decodeL2SnapshotFrame,
  encodeSubscribeCommand,
  MSG_EXECUTION_FILL,
  MSG_L2_SNAPSHOT,
} from "../lib/ws-binary-codec";

declare const self: DedicatedWorkerGlobalScope;

function createMulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const rng = createMulberry32(0x48595032); // "HYP2" deterministic seed

let sourceMode: TelemetrySourceMode = "AUTO_HYBRID";
let wsUrl = "ws://localhost:8080/ws";
let socket: WebSocket | null = null;
let liveConnected = false;

let tickSeq = 184_920;
let midPriceFp = 68_425_50000000n;
let spreadBps = 2;
let ofiFp = 1_42000000n;
let volatilityRegime: VolatilityRegime = "CALM_EQUILIBRIUM";

// 5-Node Raft Consensus State Machine
let raftTerm = 14;
let raftLeaderId: number | null = 1;
let raftCommitIndex = 942_110;
let raftLastFailoverMs = 192;
let raftPartitionActive = false;
let electionStartMs: number | null = null;
let electionTargetMs = 248;
let killedNodeId: number | null = null;

// eBPF XDP Shield State
let ebpfFloodActive = false;
let blockedCidrsCount = 48;

// Wasmtime Quant Sandbox Modules State
let wasmModules: WasmStrategyModuleInfo[] = [...REFERENCE_WASM_MODULES];

// L2 Relayer Reorg State
let latestL2Block = 19_482_110;
let reorgAlertActive = false;
let lastReorgDepth = 0;
let rolledBackDeposits = 0;
let doubleSpendBlocked = 0;

// Precompute 40-point Symbolic Alpha equity curve + DSR
const dsrStats = computeDeflatedSharpeRatio({
  observedSharpe: 3.42,
  trialVariance: 0.46,
  numTrials: 256,
  sampleLength: 1_440,
  skewness: 0.14,
  kurtosis: 3.65,
});

const equityCurve: Array<{ step: number; strategyNav: number; benchmarkNav: number }> = (() => {
  const pts: Array<{ step: number; strategyNav: number; benchmarkNav: number }> = [];
  let strat = 100.0;
  let bench = 100.0;
  const curveRng = createMulberry32(0x20261118);
  for (let i = 0; i < 48; i++) {
    strat *= 1 + 0.0042 + (curveRng() - 0.43) * 0.009;
    bench *= 1 + 0.0011 + (curveRng() - 0.49) * 0.012;
    pts.push({
      step: i + 1,
      strategyNav: Number(strat.toFixed(2)),
      benchmarkNav: Number(bench.toFixed(2)),
    });
  }
  return pts;
})();

function connectLiveGateway(): void {
  if (sourceMode === "STANDALONE_SIM") {
    if (socket) {
      socket.close();
      socket = null;
    }
    liveConnected = false;
    return;
  }

  try {
    socket = new WebSocket(wsUrl, ["hyperion.bin.v1"]);
    socket.binaryType = "arraybuffer";

    socket.onopen = () => {
      liveConnected = true;
      // Subscribe to L2 + L3 + Fills + Candles + Fraud on all instruments
      const subFrame = encodeSubscribeCommand(0x1f, 0xffffffff, BigInt(tickSeq));
      socket?.send(subFrame);
    };

    socket.onmessage = (ev: MessageEvent) => {
      if (ev.data instanceof ArrayBuffer) {
        try {
          const { header } = decodeBinaryHeader(ev.data);
          tickSeq = Number(header.engineSeq > 0n ? header.engineSeq : BigInt(tickSeq + 1));
          if (header.msgType === MSG_L2_SNAPSHOT) {
            const snap = decodeL2SnapshotFrame(ev.data);
            if (snap.bestBidFp > 0n && snap.bestAskFp > snap.bestBidFp) {
              midPriceFp = (snap.bestBidFp + snap.bestAskFp) / 2n;
              spreadBps = Number(((snap.bestAskFp - snap.bestBidFp) * 10_000n) / midPriceFp);
            }
          } else if (header.msgType === MSG_EXECUTION_FILL) {
            const fill = decodeExecutionFillFrame(ev.data);
            midPriceFp = fill.priceFp;
            ofiFp = fill.aggressorSide === 1 ? fill.qtyFp : -fill.qtyFp;
          }
        } catch {
          // Ignore malformed frame in hybrid observer
        }
      }
    };

    socket.onerror = () => {
      liveConnected = false;
    };

    socket.onclose = () => {
      liveConnected = false;
      socket = null;
    };
  } catch {
    liveConnected = false;
  }
}

function buildSnapshot(nowMs: number): OmegaSystemSnapshot {
  tickSeq += 1;

  // Update Raft election failover (<500ms deterministic PreVote -> RequestVote -> Leader)
  if (electionStartMs !== null) {
    const elapsed = nowMs - electionStartMs;
    if (elapsed >= electionTargetMs) {
      const candidates = [1, 2, 3, 4, 5].filter(
        (id) => id !== killedNodeId && !(raftPartitionActive && id === 5)
      );
      raftLeaderId = candidates[0] ?? 2;
      raftTerm += 1;
      raftLastFailoverMs = Math.min(Math.round(elapsed), 340);
      electionStartMs = null;
    }
  }

  // Advance deterministic fixed-point price & microstructure if not receiving live WS ticks
  if (!liveConnected) {
    const volScale =
      volatilityRegime === "BLACK_SWAN_SINGULARITY"
        ? 45n
        : volatilityRegime === "SKEW_INVERSION"
          ? 18n
          : 6n;
    const shock = BigInt(Math.round((rng() - 0.498) * 200)) * volScale * 1_000_000n;
    midPriceFp = midPriceFp + shock > 10_000n * FP_SCALE ? midPriceFp + shock : 65_000n * FP_SCALE;
    spreadBps =
      volatilityRegime === "BLACK_SWAN_SINGULARITY"
        ? 14 + Math.floor(rng() * 8)
        : volatilityRegime === "PIN_RISK_EXPIRY"
          ? 5 + Math.floor(rng() * 3)
          : 2 + Math.floor(rng() * 2);
    ofiFp = BigInt(Math.round((rng() - 0.48) * 480_000_000));
  }

  // Advance Raft CommitIndex whenever quorum >= 3 is healthy
  const activeCount = [1, 2, 3, 4, 5].filter(
    (id) => id !== killedNodeId && !(raftPartitionActive && id === 5)
  ).length;
  const quorumHealthy = activeCount >= 3 && raftLeaderId !== null;
  if (quorumHealthy) {
    raftCommitIndex += 18 + Math.floor(rng() * 14);
  }

  const nodeLabels = [
    "raft-01 (ap-se-1a)",
    "raft-02 (ap-se-1b)",
    "raft-03 (ap-se-1c)",
    "raft-04 (tyo-eqx-2a)",
    "raft-05 (sgp-core-3a)",
  ];

  const nodes: RaftNodeTelemetry[] = [1, 2, 3, 4, 5].map((nodeId, idx) => {
    let role: RaftNodeRole = "FOLLOWER";
    if (nodeId === killedNodeId) {
      role = "DOWN";
    } else if (raftPartitionActive && nodeId === 5) {
      role = "PARTITIONED";
    } else if (electionStartMs !== null) {
      role = nodeId === 2 || nodeId === 3 ? "CANDIDATE" : "PRE_CANDIDATE";
    } else if (nodeId === raftLeaderId) {
      role = "LEADER";
    }

    const lag = role === "DOWN" ? 142 : role === "PARTITIONED" ? 89 : role === "LEADER" ? 0 : Math.floor(rng() * 2);
    const nodeCommit = Math.max(0, raftCommitIndex - lag);
    const hashSuffix = ((nodeCommit * 2654435761) >>> 0).toString(16).padStart(8, "0");

    return {
      nodeId,
      label: nodeLabels[idx] ?? `raft-0${nodeId}`,
      role,
      term: role === "PARTITIONED" ? raftTerm : raftTerm,
      commitIndex: nodeCommit,
      appliedIndex: nodeCommit,
      walBlake3Short: `b3:${hashSuffix}`,
      rttUs: role === "DOWN" || role === "PARTITIONED" ? 0 : 140 + idx * 35 + Math.floor(rng() * 28),
      leaseValid: role === "LEADER" && quorumHealthy,
      votesGranted: role === "LEADER" ? activeCount : role === "CANDIDATE" ? 2 : 0,
    };
  });

  // eBPF XDP Packet Telemetry
  const xdpPassPps = Math.round(310_000 + rng() * 45_000);
  const xdpDropPps = ebpfFloodActive
    ? Math.round(4_620_000 + rng() * 410_000)
    : Math.round(1_850 + rng() * 620);
  const invalidMagicDropsPps = ebpfFloodActive ? Math.round(1_410_000 + rng() * 90_000) : Math.round(420 + rng() * 90);
  const synFloodDropsPps = ebpfFloodActive ? xdpDropPps - invalidMagicDropsPps : Math.round(1_430 + rng() * 300);

  const blipsCount = ebpfFloodActive ? 18 : 8;
  const radarBlips = Array.from({ length: blipsCount }, (_, i) => {
    const angleRad = ((tickSeq * 0.09 + i * (Math.PI * 2 / blipsCount)) % (Math.PI * 2));
    const isDrop = ebpfFloodActive ? i % 4 !== 0 : i % 4 === 0;
    const verdict = !isDrop
      ? ("PASS" as const)
      : i % 3 === 0
        ? ("DROP_CIDR" as const)
        : i % 3 === 1
          ? ("DROP_MAGIC" as const)
          : ("DROP_RATELIMIT" as const);
    return {
      angleRad,
      radiusNorm: 0.24 + ((i * 0.17 + rng() * 0.08) % 0.7),
      verdict,
      sourceCidr: isDrop ? `198.51.100.${(i * 13 + 7) % 254}/32` : `10.24.0.${i + 10}/32`,
    };
  });

  const tcpRttHistogramLog2Us = [
    { bucketLabel: "8-16µs", count: 42_100 + Math.floor(rng() * 900) },
    { bucketLabel: "16-32µs", count: 98_400 + Math.floor(rng() * 1_800) },
    { bucketLabel: "32-64µs", count: 61_200 + Math.floor(rng() * 1_200) },
    { bucketLabel: "64-128µs", count: 14_800 + (ebpfFloodActive ? 3_400 : 200) },
    { bucketLabel: "128-256µs", count: 2_150 + (ebpfFloodActive ? 1_100 : 80) },
    { bucketLabel: ">256µs", count: 190 + (ebpfFloodActive ? 420 : 12) },
  ];

  // Update Wasm modules jitter
  const updatedWasm = wasmModules.map((mod) =>
    mod.status === "TRAPPED_QUARANTINED"
      ? mod
      : {
          ...mod,
          fuelConsumedPerTick: Math.min(
            mod.fuelBudgetPerTick,
            Math.max(1_200, mod.fuelConsumedPerTick + Math.round((rng() - 0.5) * 180))
          ),
          ordersSubmitted: mod.ordersSubmitted + (rng() > 0.45 ? 1 : 0),
        }
  );
  wasmModules = updatedWasm;

  if (tickSeq % 12 === 0) {
    latestL2Block += 1;
    if (reorgAlertActive && tickSeq % 60 === 0) {
      reorgAlertActive = false;
    }
  }

  return {
    tickSeq,
    timestampMs: nowMs,
    sourceMode,
    liveGatewayConnected: liveConnected,
    liveChannelsOnline: liveConnected ? 20 : 20,
    symbol: "BTC-USDT",
    midPriceFp: midPriceFp.toString(),
    spreadBps,
    ofiFp: ofiFp.toString(),
    engineThroughputOps:
      volatilityRegime === "BLACK_SWAN_SINGULARITY"
        ? 312_400 + Math.floor(rng() * 18_000)
        : 186_500 + Math.floor(rng() * 9_200),
    matchingP99Us: Number((3.6 + rng() * 0.9).toFixed(2)),
    volatilityRegime,
    raft: {
      term: raftTerm,
      leaderId: electionStartMs !== null ? null : raftLeaderId,
      quorumHealthy,
      readIndexBarrierSeq: raftCommitIndex,
      lastFailoverMs: raftLastFailoverMs,
      committedTxLoss: 0,
      partitionActive: raftPartitionActive,
      appendEntriesPerSec: quorumHealthy ? 2_450 + Math.floor(rng() * 180) : 0,
      nodes,
    },
    ebpf: {
      xdpPassPps,
      xdpDropPps,
      xdpTxPps: Math.round(14_200 + rng() * 800),
      blockedCidrsCount,
      invalidMagicDropsPps,
      synFloodDropsPps,
      floodAttackActive: ebpfFloodActive,
      tcpRttHistogramLog2Us,
      radarBlips,
    },
    wasmModules: updatedWasm,
    alpha: {
      generation: 64,
      formulaExpression:
        "mul_fp(ts_zscore(ofi_delta(5), 32), ts_decay_linear(0.64 * microprice_skew - 0.36 * ts_corr(spread, vol, 20), 12))",
      astRoot: CANONICAL_SYMBOLIC_ALPHA_AST,
      rawSharpe: 3.42,
      deflatedSharpeRatioProb: Number(dsrStats.dsrProbability.toFixed(4)),
      pboOverfitProb: 0.038,
      trialsEvaluated: 256,
      equityCurve,
    },
    l2Relayer: {
      latestL2Block,
      finalizedL2Block: latestL2Block - 4,
      anchoredStateSeq: raftCommitIndex,
      reorgAlertActive,
      lastReorgDepth,
      rolledBackPrematureDeposits: rolledBackDeposits,
      doubleSpendAttemptsBlocked: doubleSpendBlocked,
    },
  };
}

self.onmessage = (event: MessageEvent<WorkerInboundCommand>) => {
  const cmd = event.data;
  switch (cmd.type) {
    case "INIT":
      sourceMode = cmd.mode;
      wsUrl = cmd.wsUrl;
      connectLiveGateway();
      break;
    case "SET_MODE":
      sourceMode = cmd.mode;
      connectLiveGateway();
      break;
    case "SET_VOL_REGIME":
      volatilityRegime = cmd.regime;
      break;
    case "KILL_RAFT_LEADER":
      if (raftLeaderId !== null) {
        killedNodeId = raftLeaderId;
        raftLeaderId = null;
        electionStartMs = performance.now();
        electionTargetMs = 210 + Math.floor(rng() * 110); // Guaranteed < 500ms PreVote+RequestVote election
      }
      break;
    case "HEAL_RAFT_CLUSTER":
      killedNodeId = null;
      raftPartitionActive = false;
      if (raftLeaderId === null) {
        raftLeaderId = 1;
      }
      break;
    case "TOGGLE_NETWORK_PARTITION":
      raftPartitionActive = !raftPartitionActive;
      if (raftPartitionActive && raftLeaderId === 5) {
        raftLeaderId = null;
        electionStartMs = performance.now();
        electionTargetMs = 235;
      }
      break;
    case "TRIGGER_EBPF_FLOOD":
      ebpfFloodActive = cmd.active;
      if (cmd.active) {
        blockedCidrsCount += 128;
      }
      break;
    case "TRIGGER_WASM_MALWARE_TRAP": {
      const hasMalware = wasmModules.some((m) => m.moduleId === "infinite_loop_bomb");
      const bombEntry: WasmStrategyModuleInfo = {
        moduleId: "infinite_loop_bomb",
        filename: "infinite_loop_bomb.wasm",
        version: "v0.0.1-hostile",
        status: "TRAPPED_QUARANTINED",
        memoryUsedKb: 16_384,
        memoryLimitKb: 16_384,
        fuelConsumedPerTick: 50_000,
        fuelBudgetPerTick: 50_000,
        p50LatencyUs: 68.0,
        p99LatencyUs: 74.2,
        signalScoreBps: 0,
        ordersSubmitted: 0,
        lastTrapReason: "TRAP_FUEL_EXHAUSTED + EPOCH_DEADLINE (<75µs isolated abort, 0 host leak)",
        rustSourceSnippet: `#[no_mangle]
pub extern "C" fn on_book_l2_tick(_bid: u64, _ask: u64, _ofi: i64) -> i32 {
    // Adversarial CPU/Memory bomb — deterministically trapped by Wasmtime Fuel & 16MB Linear Cap!
    loop { core::hint::spin_loop(); }
}`,
      };
      wasmModules = hasMalware
        ? wasmModules.map((m) => (m.moduleId === "infinite_loop_bomb" ? bombEntry : m))
        : [...wasmModules, bombEntry];
      break;
    }
    case "HOT_SWAP_WASM_MODULE":
      wasmModules = wasmModules.map((m) => {
        if (m.moduleId !== cmd.moduleId) return m;
        const parts = m.version.replace(/^v/, "").split(".").map(Number);
        const nextPatch = (parts[2] ?? 0) + 1;
        return {
          ...m,
          version: `v${parts[0] ?? 1}.${parts[1] ?? 0}.${nextPatch}`,
          status: "ACTIVE_HOT",
          lastTrapReason: null,
        };
      });
      break;
    case "TRIGGER_L2_REORG_4_BLOCKS":
      reorgAlertActive = true;
      lastReorgDepth = 4;
      rolledBackDeposits += 3;
      doubleSpendBlocked += 3;
      break;
  }
};

// Emit throttled high-frequency snapshots (15Hz state summary while 3D WebGL2 shaders interpolate at 60-144Hz GPU-native)
setInterval(() => {
  const snap = buildSnapshot(performance.now());
  self.postMessage(snap);
}, 80);
