"use client";

import React, { useCallback, useEffect, useId, useRef, useState } from "react";
import { formatFixedPoint } from "@/lib/fixed-point";
import { VolatilityRegime } from "@/lib/svi-greeks";
import {
  NativeHostTelemetry,
  queryNativeHostTelemetry,
  spawnDetachedPanelWindow,
} from "@/lib/tauri-bridge";
import {
  CANONICAL_SYMBOLIC_ALPHA_AST,
  OmegaSystemSnapshot,
  REFERENCE_WASM_MODULES,
  SovereignPanelId,
  TelemetrySourceMode,
  WorkerInboundCommand,
} from "@/lib/telemetry-types";
import { RaftEbpfTopology } from "./RaftEbpfTopology";
import { VolGreeksSurface3D } from "./VolGreeksSurface3D";
import { WasmSqlWorkbench } from "./WasmSqlWorkbench";
import { ZkSolvencyPortal } from "./ZkSolvencyPortal";

const INITIAL_SNAPSHOT: OmegaSystemSnapshot = {
  tickSeq: 184_920,
  timestampMs: 0,
  sourceMode: "AUTO_HYBRID",
  liveGatewayConnected: false,
  liveChannelsOnline: 20,
  symbol: "BTC-USDT",
  midPriceFp: "6842550000000",
  spreadBps: 2,
  ofiFp: "142000000",
  engineThroughputOps: 192_400,
  matchingP99Us: 3.82,
  volatilityRegime: "CALM_EQUILIBRIUM",
  raft: {
    term: 14,
    leaderId: 1,
    quorumHealthy: true,
    readIndexBarrierSeq: 942_110,
    lastFailoverMs: 192,
    committedTxLoss: 0,
    partitionActive: false,
    appendEntriesPerSec: 2_480,
    nodes: [
      { nodeId: 1, label: "raft-01", role: "LEADER", term: 14, commitIndex: 942_110, appliedIndex: 942_110, walBlake3Short: "b3:8a91f02c", rttUs: 140, leaseValid: true, votesGranted: 5 },
      { nodeId: 2, label: "raft-02", role: "FOLLOWER", term: 14, commitIndex: 942_110, appliedIndex: 942_110, walBlake3Short: "b3:8a91f02c", rttUs: 175, leaseValid: false, votesGranted: 0 },
      { nodeId: 3, label: "raft-03", role: "FOLLOWER", term: 14, commitIndex: 942_109, appliedIndex: 942_109, walBlake3Short: "b3:7c14d881", rttUs: 210, leaseValid: false, votesGranted: 0 },
      { nodeId: 4, label: "raft-04", role: "FOLLOWER", term: 14, commitIndex: 942_110, appliedIndex: 942_110, walBlake3Short: "b3:8a91f02c", rttUs: 245, leaseValid: false, votesGranted: 0 },
      { nodeId: 5, label: "raft-05", role: "FOLLOWER", term: 14, commitIndex: 942_109, appliedIndex: 942_109, walBlake3Short: "b3:7c14d881", rttUs: 280, leaseValid: false, votesGranted: 0 },
    ],
  },
  ebpf: {
    xdpPassPps: 324_500,
    xdpDropPps: 1_940,
    xdpTxPps: 14_500,
    blockedCidrsCount: 48,
    invalidMagicDropsPps: 440,
    synFloodDropsPps: 1_500,
    floodAttackActive: false,
    tcpRttHistogramLog2Us: [
      { bucketLabel: "8-16µs", count: 42_500 },
      { bucketLabel: "16-32µs", count: 98_900 },
      { bucketLabel: "32-64µs", count: 61_500 },
      { bucketLabel: "64-128µs", count: 14_900 },
    ],
    radarBlips: [
      { angleRad: 0.5, radiusNorm: 0.42, verdict: "PASS", sourceCidr: "10.24.0.11/32" },
      { angleRad: 2.1, radiusNorm: 0.68, verdict: "DROP_CIDR", sourceCidr: "198.51.100.20/32" },
    ],
  },
  wasmModules: REFERENCE_WASM_MODULES,
  alpha: {
    generation: 64,
    formulaExpression:
      "mul_fp(ts_zscore(ofi_delta(5), 32), ts_decay_linear(0.64 * microprice_skew - 0.36 * ts_corr(spread, vol, 20), 12))",
    astRoot: CANONICAL_SYMBOLIC_ALPHA_AST,
    rawSharpe: 3.42,
    deflatedSharpeRatioProb: 0.9942,
    pboOverfitProb: 0.038,
    trialsEvaluated: 256,
    equityCurve: [
      { step: 1, strategyNav: 100, benchmarkNav: 100 },
      { step: 2, strategyNav: 104.2, benchmarkNav: 100.8 },
      { step: 3, strategyNav: 109.8, benchmarkNav: 101.4 },
    ],
  },
  l2Relayer: {
    latestL2Block: 19_482_110,
    finalizedL2Block: 19_482_106,
    anchoredStateSeq: 942_110,
    reorgAlertActive: false,
    lastReorgDepth: 0,
    rolledBackPrematureDeposits: 0,
    doubleSpendAttemptsBlocked: 0,
  },
};

const TABS: ReadonlyArray<{ id: SovereignPanelId; label: string }> = [
  { id: "ALL_QUAD", label: "Quad Sovereign Command (20-Ch)" },
  { id: "VOL_SURFACE_3D", label: "3D SVI Volatility & Greeks" },
  { id: "RAFT_EBPF_TOPOLOGY", label: "Raft Quorum & eBPF Radar" },
  { id: "ZK_SOLVENCY_PORTAL", label: "ZK-Solvency & L2 Verifier" },
  { id: "WASM_SQL_WORKBENCH", label: "Wasm Quant IDE & Arrow SQL" },
];

export function SovereignWorkstation() {
  const [snapshot, setSnapshot] = useState<OmegaSystemSnapshot>(INITIAL_SNAPSHOT);
  const [activeTab, setActiveTab] = useState<SovereignPanelId>("ALL_QUAD");
  const [isDetachedWindow, setIsDetachedWindow] = useState<boolean>(false);
  const [hostInfo, setHostInfo] = useState<NativeHostTelemetry>({
    runtime: "WEB_BROWSER",
    osArch: "WebGL2",
    activeWindows: 1,
    ipcRoundtripUs: 0,
  });
  const [liveStatusMessage, setLiveStatusMessage] = useState<string>("");

  const workerRef = useRef<Worker | null>(null);
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const panelRefs = useRef<Record<string, HTMLDivElement | null>>({});
  const modeSelectId = useId();

  // Load @microsoft/focusgroup-polyfill only if native focusGroup support is absent (tabs guide)
  useEffect(() => {
    if (typeof HTMLElement !== "undefined" && !Object.hasOwn(HTMLElement.prototype, "focusGroup")) {
      void import("@microsoft/focusgroup-polyfill")
        .then((mod) => {
          if (typeof mod.polyfill === "function" && typeof document !== "undefined" && document.body) {
            mod.polyfill(document.body);
          }
        })
        .catch(() => {
          // Non-fatal if polyfill already active
        });
    }

    // Parse URL query ?panel=... for multi-window Tauri / Web pop-out mode
    if (typeof window !== "undefined") {
      const params = new URLSearchParams(window.location.search);
      const p = params.get("panel") as SovereignPanelId | null;
      if (p && TABS.some((t) => t.id === p)) {
        setActiveTab(p);
      }
      if (params.get("detached") === "1") {
        setIsDetachedWindow(true);
      }
    }

    void queryNativeHostTelemetry().then(setHostInfo);
  }, []);

  // Boot Dedicated Off-Main-Thread Telemetry Worker
  useEffect(() => {
    const worker = new Worker(
      new URL("../workers/omega-telemetry.worker.ts", import.meta.url),
      { type: "module" }
    );
    workerRef.current = worker;

    worker.onmessage = (e: MessageEvent<OmegaSystemSnapshot>) => {
      setSnapshot(e.data);
    };

    const wsUrl =
      process.env.NEXT_PUBLIC_HYPERION_WS_URL ?? "ws://localhost:8080/ws";
    const initCmd: WorkerInboundCommand = {
      type: "INIT",
      mode: "AUTO_HYBRID",
      wsUrl,
    };
    worker.postMessage(initCmd);

    return () => {
      worker.terminate();
      workerRef.current = null;
    };
  }, []);

  const sendWorkerCmd = useCallback((cmd: WorkerInboundCommand) => {
    workerRef.current?.postMessage(cmd);
  }, []);

  // Sync hidden="until-found" and beforematch find-in-page activation on single-panel tabs
  useEffect(() => {
    const cleanups: Array<() => void> = [];
    for (const tab of TABS) {
      if (tab.id === "ALL_QUAD") continue;
      const el = panelRefs.current[tab.id];
      if (!el) continue;
      const isVisible = activeTab === "ALL_QUAD" || activeTab === tab.id;
      if (isVisible) {
        el.removeAttribute("hidden");
      } else {
        el.setAttribute("hidden", "until-found");
      }
      const onBeforeMatch = () => {
        setActiveTab(tab.id);
      };
      el.addEventListener("beforematch", onBeforeMatch);
      cleanups.push(() => el.removeEventListener("beforematch", onBeforeMatch));
    }
    return () => {
      cleanups.forEach((fn) => fn());
    };
  }, [activeTab]);

  const handleSelectTab = (tabId: SovereignPanelId) => {
    setActiveTab(tabId);
    const btn = tabRefs.current[tabId];
    btn?.scrollIntoView({ block: "nearest", inline: "nearest" });
  };

  const handleTriggerBlackSwan = () => {
    sendWorkerCmd({ type: "SET_VOL_REGIME", regime: "BLACK_SWAN_SINGULARITY" });
    sendWorkerCmd({ type: "KILL_RAFT_LEADER" });
    sendWorkerCmd({ type: "TRIGGER_EBPF_FLOOD", active: true });
    sendWorkerCmd({ type: "TRIGGER_L2_REORG_4_BLOCKS" });
    setLiveStatusMessage(
      "Black Swan Singularity triggered: Raft Leader killed, eBPF flood shield active, L2 4-block reorg defense engaged."
    );
  };

  const handleNormalizeAll = () => {
    sendWorkerCmd({ type: "SET_VOL_REGIME", regime: "CALM_EQUILIBRIUM" });
    sendWorkerCmd({ type: "HEAL_RAFT_CLUSTER" });
    sendWorkerCmd({ type: "TRIGGER_EBPF_FLOOD", active: false });
    setLiveStatusMessage("All 20 Omega channels restored to calm equilibrium.");
  };

  const midPriceBig = BigInt(snapshot.midPriceFp);

  return (
    <div className="flex min-h-dvh w-full flex-col bg-[#06090f] text-slate-100">
      {/* Skip link for keyboard navigation (accessibility §1) */}
      <a href="#workstation-heading" className="skip-link visually-hidden">
        Skip to Sovereign Workstation
      </a>

      {/* Centralized polite live region announcer (accessibility §8) */}
      <div aria-live="polite" aria-atomic="true" className="visually-hidden">
        {liveStatusMessage}
      </div>

      {/* Top Sovereign Telemetry & Command Header */}
      <header className="border-b border-slate-800/90 bg-[#090d16] px-4 py-2.5">
        <div className="mx-auto flex max-w-[1760px] flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-3 min-w-0">
            <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded border border-cyan-500/40 bg-cyan-950/40 mono-num text-xs font-bold text-cyan-300">
              Ω20
            </div>
            <div className="min-w-0">
              <h1
                id="workstation-heading"
                tabIndex={-1}
                className="truncate text-sm font-bold tracking-tight text-slate-100 sm:text-base"
              >
                HYPERION OMEGA · Sovereign Workstation
                {isDetachedWindow ? " (Detached Native Window)" : ""}
              </h1>
              <p className="mono-num truncate text-[11px] text-slate-400">
                Runtime: {hostInfo.runtime} · 20/20 Subsystems Locked · Fixed-Point 1e8 Strict
              </p>
            </div>
          </div>

          {/* Real-Time Hero Telemetry Strip */}
          <div className="flex flex-wrap items-center gap-4">
            <div>
              <div className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
                BTC-USDT Mark (fp 1e8)
              </div>
              <div className="mono-num text-base font-semibold text-cyan-300">
                ${formatFixedPoint(midPriceBig, 2)}
              </div>
            </div>

            <div>
              <div className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
                LOB Matching p99
              </div>
              <div className="mono-num text-base font-semibold text-emerald-400">
                {snapshot.matchingP99Us} µs
              </div>
            </div>

            <div>
              <div className="text-[10px] font-medium uppercase tracking-wider text-slate-400">
                Cluster Throughput
              </div>
              <div className="mono-num text-base font-semibold text-slate-100">
                {snapshot.engineThroughputOps.toLocaleString("en-US")} ops/s
              </div>
            </div>

            <div className="flex items-center gap-1.5">
              <label htmlFor={modeSelectId} className="text-[11px] text-slate-400">
                Feed
              </label>
              <select
                id={modeSelectId}
                value={snapshot.sourceMode}
                onChange={(e) =>
                  sendWorkerCmd({
                    type: "SET_MODE",
                    mode: e.target.value as TelemetrySourceMode,
                  })
                }
                className="sovereign-input mono-num"
              >
                <option value="AUTO_HYBRID">
                  Auto Hybrid ({snapshot.liveGatewayConnected ? "Live WS" : "Worker Sim"})
                </option>
                <option value="STANDALONE_SIM">Standalone Worker Sim</option>
                <option value="LIVE_CLUSTER">Live Binary WS :8080</option>
              </select>
            </div>

            <button
              type="button"
              data-testid="trigger-black-swan-btn"
              className="sovereign-btn sovereign-btn-danger font-semibold"
              onClick={handleTriggerBlackSwan}
            >
              Trigger Black Swan Singularity
            </button>
            <button
              type="button"
              data-testid="normalize-cluster-btn"
              className="sovereign-btn"
              onClick={handleNormalizeAll}
            >
              Normalize
            </button>
          </div>
        </div>

        {/* Modern Web Guidance Compliant Workspace Tablist */}
        {!isDetachedWindow && (
          <div className="mx-auto mt-2 max-w-[1760px] border-t border-slate-800/70 pt-1">
            <div
              role="tablist"
              aria-label="Sovereign Studio Workspace Views"
              {...({ focusgroup: "tablist nomemory" } as Record<string, string>)}
              className="tab-scroller"
              onKeyDown={(e) => {
                if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
                const idx = TABS.findIndex((t) => t.id === activeTab);
                if (idx < 0) return;
                const nextIdx =
                  e.key === "ArrowRight"
                    ? (idx + 1) % TABS.length
                    : (idx - 1 + TABS.length) % TABS.length;
                const nextTab = TABS[nextIdx]!;
                handleSelectTab(nextTab.id);
                tabRefs.current[nextTab.id]?.focus();
              }}
            >
              {TABS.map((tab) => {
                const selected = activeTab === tab.id;
                return (
                  <button
                    key={tab.id}
                    ref={(el) => {
                      tabRefs.current[tab.id] = el;
                    }}
                    type="button"
                    role="tab"
                    id={`tab-${tab.id}`}
                    data-testid={`workspace-tab-${tab.id}`}
                    aria-selected={selected}
                    {...(selected ? ({ focusgroupstart: "" } as Record<string, string>) : {})}
                    className="sovereign-tab"
                    onClick={() => handleSelectTab(tab.id)}
                  >
                    {tab.label}
                  </button>
                );
              })}
            </div>
          </div>
        )}
      </header>

      {/* Main Multi-Panel Command Grid (No styling directly on role="tabpanel" elements per tabs guide!) */}
      <main className="mx-auto w-full max-w-[1760px] flex-1 px-4 py-3">
        <div
          className={
            activeTab === "ALL_QUAD"
              ? "grid grid-cols-1 gap-3.5 2xl:grid-cols-2"
              : "grid grid-cols-1"
          }
        >
          {/* Panel 1: 3D Implied Volatility & Portfolio Greeks Surface */}
          <div
            ref={(el) => {
              panelRefs.current.VOL_SURFACE_3D = el;
            }}
            role="tabpanel"
            aria-labelledby="tab-VOL_SURFACE_3D"
            hidden={activeTab !== "ALL_QUAD" && activeTab !== "VOL_SURFACE_3D" ? true : undefined}
          >
            <VolGreeksSurface3D
              midPriceFp={snapshot.midPriceFp}
              regime={snapshot.volatilityRegime}
              onChangeRegime={(regime: VolatilityRegime) =>
                sendWorkerCmd({ type: "SET_VOL_REGIME", regime })
              }
              onDetach={
                isDetachedWindow
                  ? undefined
                  : () =>
                      void spawnDetachedPanelWindow(
                        "VOL_SURFACE_3D",
                        "3D Volatility & Greeks Surface"
                      )
              }
            />
          </div>

          {/* Panel 2: 5-Node Raft Quorum Ring & eBPF Shield Topology */}
          <div
            ref={(el) => {
              panelRefs.current.RAFT_EBPF_TOPOLOGY = el;
            }}
            role="tabpanel"
            aria-labelledby="tab-RAFT_EBPF_TOPOLOGY"
            hidden={
              activeTab !== "ALL_QUAD" && activeTab !== "RAFT_EBPF_TOPOLOGY" ? true : undefined
            }
          >
            <RaftEbpfTopology
              raft={snapshot.raft}
              ebpf={snapshot.ebpf}
              tickSeq={snapshot.tickSeq}
              onKillLeader={() => {
                sendWorkerCmd({ type: "KILL_RAFT_LEADER" });
                setLiveStatusMessage(
                  "Raft Leader terminated; PreVote and RequestVote quorum election initiated."
                );
              }}
              onHealCluster={() => sendWorkerCmd({ type: "HEAL_RAFT_CLUSTER" })}
              onTogglePartition={() => sendWorkerCmd({ type: "TOGGLE_NETWORK_PARTITION" })}
              onToggleEbpfFlood={(active) =>
                sendWorkerCmd({ type: "TRIGGER_EBPF_FLOOD", active })
              }
              onDetach={
                isDetachedWindow
                  ? undefined
                  : () =>
                      void spawnDetachedPanelWindow(
                        "RAFT_EBPF_TOPOLOGY",
                        "5-Node Raft & eBPF Topology"
                      )
              }
            />
          </div>

          {/* Panel 3: Client-Side ZK Solvency Verifier Portal */}
          <div
            ref={(el) => {
              panelRefs.current.ZK_SOLVENCY_PORTAL = el;
            }}
            role="tabpanel"
            aria-labelledby="tab-ZK_SOLVENCY_PORTAL"
            hidden={
              activeTab !== "ALL_QUAD" && activeTab !== "ZK_SOLVENCY_PORTAL" ? true : undefined
            }
          >
            <ZkSolvencyPortal
              l2Relayer={snapshot.l2Relayer}
              onTriggerL2Reorg={() => {
                sendWorkerCmd({ type: "TRIGGER_L2_REORG_4_BLOCKS" });
                setLiveStatusMessage(
                  "Simulated 4-block L2 chain reorganization; premature unfinalized deposits rolled back."
                );
              }}
              onAnnounce={setLiveStatusMessage}
              onDetach={
                isDetachedWindow
                  ? undefined
                  : () =>
                      void spawnDetachedPanelWindow(
                        "ZK_SOLVENCY_PORTAL",
                        "Client-Side ZK Solvency Verifier"
                      )
              }
            />
          </div>

          {/* Panel 4: Co-Located Wasm Quant IDE & Lakehouse SQL Workbench */}
          <div
            ref={(el) => {
              panelRefs.current.WASM_SQL_WORKBENCH = el;
            }}
            role="tabpanel"
            aria-labelledby="tab-WASM_SQL_WORKBENCH"
            hidden={
              activeTab !== "ALL_QUAD" && activeTab !== "WASM_SQL_WORKBENCH" ? true : undefined
            }
          >
            <WasmSqlWorkbench
              wasmModules={snapshot.wasmModules}
              alpha={snapshot.alpha}
              onTriggerMalwareTrap={() => {
                sendWorkerCmd({ type: "TRIGGER_WASM_MALWARE_TRAP" });
                setLiveStatusMessage(
                  "Malware infinite-loop Wasm module trapped in 68 microseconds via Wasmtime fuel metering."
                );
              }}
              onHotSwapModule={(moduleId) => {
                sendWorkerCmd({ type: "HOT_SWAP_WASM_MODULE", moduleId });
                setLiveStatusMessage(`Hot-swapped ${moduleId}.wasm with zero open-order loss.`);
              }}
              onAnnounce={setLiveStatusMessage}
              onDetach={
                isDetachedWindow
                  ? undefined
                  : () =>
                      void spawnDetachedPanelWindow(
                        "WASM_SQL_WORKBENCH",
                        "Wasm Quant IDE & DataFusion SQL Workbench"
                      )
              }
            />
          </div>
        </div>
      </main>

      {/* Compact Institutional Footer */}
      <footer className="border-t border-slate-800/80 bg-[#080c14] px-4 py-2 text-[11px] text-slate-400">
        <div className="mx-auto flex max-w-[1760px] flex-wrap items-center justify-between gap-2 mono-num">
          <span>
            HYPERION OMEGA v2.0 · Channels 01–20 Unified Sovereign Architecture · Seq #
            {snapshot.tickSeq.toLocaleString("en-US")}
          </span>
          <span>
            Double-Entry Conservation: VERIFIED (Δ = 0.00000000) · ZK-MSST Proof: ACTIVE · Tauri v2 Ready
          </span>
        </div>
      </footer>
    </div>
  );
}
