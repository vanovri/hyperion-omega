"use client";

import React, { useEffect, useState } from "react";
import {
  Activity,
  Bot,
  Cpu,
  Flame,
  GitBranch,
  Network,
  Radio,
  RefreshCw,
} from "lucide-react";
import { useHyperionWorker } from "@/hooks/useHyperionWorker";
import { formatFp } from "@/lib/fixed-point";
import type { EngineMode, InstrumentSymbol, WorkspaceId } from "@/lib/types";
import { INSTRUMENT_META } from "@/lib/types";
import { ChronosScrubberDock } from "./ChronosScrubberDock";
import { AegisSreWorkspace } from "./workspaces/AegisSreWorkspace";
import { ChaosControlWorkspace } from "./workspaces/ChaosControlWorkspace";
import { ChronosForkWorkspace } from "./workspaces/ChronosForkWorkspace";
import { FraudForceGraphWorkspace } from "./workspaces/FraudForceGraphWorkspace";
import { OrderBookHeatmapWorkspace } from "./workspaces/OrderBookHeatmapWorkspace";

const SYMBOLS: InstrumentSymbol[] = ["BTC-USDT", "ETH-USDT", "SOL-USDT", "HYPE-USDT"];

const WORKSPACE_TABS: Array<{
  id: WorkspaceId;
  shortcut: string;
  label: string;
  icon: React.ComponentType<{ className?: string }>;
}> = [
  { id: "ORDERBOOK_HEATMAP", shortcut: "1", label: "Order Book & Heatmap", icon: Activity },
  { id: "FRAUD_FORCE_GRAPH", shortcut: "2", label: "Fraud Force Graph", icon: Network },
  { id: "CHRONOS_FORK_DIFF", shortcut: "3", label: "Chronos Fork Diff", icon: GitBranch },
  { id: "CHAOS_CONTROL_DECK", shortcut: "4", label: "Chaos Control Deck", icon: Flame },
  { id: "AEGIS_SRE_CONSOLE", shortcut: "5", label: "Aegis SRE Console", icon: Bot },
];

export function HyperionTerminalShell() {
  const { frameRef, uiFrame, perf, sendCommand } = useHyperionWorker("BTC-USDT", "1s", "AUTO_HYBRID");
  const [activeWorkspace, setActiveWorkspace] = useState<WorkspaceId>("ORDERBOOK_HEATMAP");

  // Institutional Hotkey Navigation (1..5 workspaces, Space play/pause Chronos)
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "SELECT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }
      if (e.key === "1") setActiveWorkspace("ORDERBOOK_HEATMAP");
      else if (e.key === "2") setActiveWorkspace("FRAUD_FORCE_GRAPH");
      else if (e.key === "3") setActiveWorkspace("CHRONOS_FORK_DIFF");
      else if (e.key === "4") setActiveWorkspace("CHAOS_CONTROL_DECK");
      else if (e.key === "5") setActiveWorkspace("AEGIS_SRE_CONSOLE");
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const symbol = uiFrame?.symbol ?? "BTC-USDT";
  const meta = INSTRUMENT_META[symbol];
  const pendingApprovals =
    uiFrame?.aegis.approvals.filter((a) => a.status === "PENDING_APPROVAL").length ?? 0;
  const highAlerts =
    uiFrame?.fraudAlerts.filter((a) => a.severityScore >= 85).length ?? 0;

  const connState = uiFrame?.connectionState ?? "STANDALONE_ACTIVE";
  const activeMode = uiFrame?.mode ?? "AUTO_HYBRID";

  return (
    <div className="flex h-dvh w-full flex-col overflow-hidden bg-[var(--obsidian-void)] text-[var(--ink-primary)]">
      {/* Top Institutional Telemetry & Command Bus */}
      <header
        data-testid="hyperion-top-bar"
        className="isolated-pane shrink-0 border-b border-[var(--wire-strong)] bg-[#080b12] px-3 py-1.5"
      >
        <div className="flex flex-wrap items-center justify-between gap-2">
          {/* Brand + Instrument Selector + Best Bid/Ask */}
          <div className="flex flex-wrap items-center gap-2.5">
            <div className="flex items-center gap-1.5 pr-1">
              <span className="inline-flex h-5 w-5 items-center justify-center rounded-[2px] bg-[#38bdf8] text-[11px] font-black text-[#040609]">
                H
              </span>
              <span className="text-xs font-bold tracking-wider text-white">HYPERION</span>
              <span className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-1 py-0.5 text-[9px] text-[var(--ink-tertiary)] tabular-mono">
                CH09
              </span>
            </div>

            <div
              role="group"
              aria-label="Instrument selector"
              className="flex items-center rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-0.5"
            >
              {SYMBOLS.map((sym) => {
                const selected = sym === symbol;
                return (
                  <button
                    key={sym}
                    type="button"
                    data-testid={`symbol-btn-${sym}`}
                    onClick={() => sendCommand({ type: "SET_SYMBOL", symbol: sym })}
                    className={`cmd-btn rounded-[2px] px-2 py-0.5 text-[11px] font-semibold tabular-mono ${
                      selected
                        ? "bg-[#162032] text-[#38bdf8] ring-1 ring-[#38bdf8]/50"
                        : "text-[var(--ink-secondary)] hover:text-white"
                    }`}
                  >
                    {sym}
                  </button>
                );
              })}
            </div>

            <div className="hidden items-center gap-3 text-[11px] tabular-mono md:flex">
              <div>
                <span className="text-[var(--ink-tertiary)]">MID </span>
                <span data-testid="top-mid-price" className="font-bold text-white">
                  {formatFp(uiFrame?.midPriceFp ?? meta.basePriceFp, meta.quoteDecimals)}
                </span>
              </div>
              <div>
                <span className="text-[var(--ink-tertiary)]">BID </span>
                <span className="text-[#00d492]">
                  {formatFp(uiFrame?.bestBidFp ?? meta.basePriceFp, meta.quoteDecimals)}
                </span>
              </div>
              <div>
                <span className="text-[var(--ink-tertiary)]">ASK </span>
                <span className="text-[#ff5470]">
                  {formatFp(uiFrame?.bestAskFp ?? meta.basePriceFp, meta.quoteDecimals)}
                </span>
              </div>
            </div>
          </div>

          {/* Pipeline Performance & Mode Switcher */}
          <div className="flex flex-wrap items-center gap-2 text-[11px] tabular-mono">
            <span
              data-testid="fps-meter"
              className="inline-flex w-[136px] items-center justify-center gap-1 whitespace-nowrap rounded-[2px] border border-[#00d492]/35 bg-[#00d492]/10 px-2 py-0.5 text-[#00d492]"
            >
              <Cpu className="h-3 w-3 shrink-0" />
              {perf.fps} FPS · {perf.frameBudgetMs}ms
            </span>

            <span
              data-testid="worker-throughput-meter"
              className="hidden w-[184px] whitespace-nowrap rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 py-0.5 text-center text-[var(--ink-secondary)] xl:inline-block"
            >
              WORKER {uiFrame?.pipeline.framesPerSec ?? 0} f/s ·{" "}
              {((uiFrame?.pipeline.bytesPerSec ?? 0) / 1024).toFixed(1)} KB/s
            </span>

            {/* Connection & Reconnect Resilience Badge */}
            <span
              data-testid="ws-connection-badge"
              className={`inline-flex w-[160px] items-center justify-center gap-1 whitespace-nowrap rounded-[2px] border px-2 py-0.5 font-semibold ${
                connState === "CONNECTED_LIVE"
                  ? "border-[#00d492]/50 bg-[#00d492]/15 text-[#00d492]"
                  : connState === "RECONNECTING"
                    ? "border-[#ff3b5c]/60 bg-[#ff3b5c]/20 text-[#ff6b84]"
                    : "border-[#38bdf8]/45 bg-[#38bdf8]/12 text-[#38bdf8]"
              }`}
            >
              {connState === "RECONNECTING" ? (
                <RefreshCw className="h-3 w-3 shrink-0 animate-spin" />
              ) : (
                <Radio className="h-3 w-3 shrink-0" />
              )}
              {connState}
            </span>

            <button
              type="button"
              data-testid="top-simulate-ws-drop"
              title="Simulate WebSocket disconnect & verify automatic snapshot resync"
              onClick={() => sendCommand({ type: "SIMULATE_WS_DISCONNECT" })}
              className="cmd-btn rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-panel)] px-2 py-0.5 text-[10px] text-[var(--ink-secondary)] hover:border-[#f59e0b] hover:text-[#fbbf24]"
            >
              Test WS Drop
            </button>

            <select
              aria-label="Telemetry engine mode"
              data-testid="engine-mode-select"
              value={activeMode}
              onChange={(e) =>
                sendCommand({ type: "SET_MODE", mode: e.target.value as EngineMode })
              }
              className="h-6 rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] px-1.5 text-[11px] font-medium text-white"
            >
              <option value="AUTO_HYBRID">AUTO: LIVE + WORKER FALLBACK</option>
              <option value="STANDALONE_SIM">STANDALONE WORKER ENGINE</option>
              <option value="LIVE_MESH">STRICT LIVE BACKEND MESH</option>
            </select>
          </div>
        </div>

        {/* 5 Custom Workspace Navigation Tabs */}
        <nav
          aria-label="Primary surveillance workspaces"
          className="mt-1.5 flex flex-wrap items-center gap-1 border-t border-[var(--wire-hairline)] pt-1.5"
        >
          {WORKSPACE_TABS.map((tab) => {
            const Icon = tab.icon;
            const active = activeWorkspace === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                data-testid={`nav-tab-${tab.id}`}
                onClick={() => setActiveWorkspace(tab.id)}
                className={`cmd-btn inline-flex items-center gap-1.5 rounded-[2px] border px-2.5 py-1 text-xs font-medium ${
                  active
                    ? "border-[#38bdf8] bg-[#38bdf8]/15 text-white"
                    : "border-transparent bg-transparent text-[var(--ink-secondary)] hover:border-[var(--wire-hairline)] hover:bg-white/[0.03] hover:text-white"
                }`}
              >
                <span className="rounded-[2px] bg-white/[0.07] px-1 text-[10px] text-[var(--ink-tertiary)] tabular-mono">
                  {tab.shortcut}
                </span>
                <Icon className="h-3.5 w-3.5" />
                <span>{tab.label}</span>

                {tab.id === "FRAUD_FORCE_GRAPH" && (
                  <span
                    className={`inline-block w-5 rounded-[2px] bg-[#ff3b5c]/25 text-center text-[10px] font-bold text-[#ff6b84] tabular-mono ${
                      highAlerts > 0 ? "opacity-100" : "opacity-0"
                    }`}
                  >
                    {highAlerts}
                  </span>
                )}
                {tab.id === "AEGIS_SRE_CONSOLE" && (
                  <span
                    className={`inline-block w-5 rounded-[2px] bg-[#f59e0b]/25 text-center text-[10px] font-bold text-[#fbbf24] tabular-mono ${
                      pendingApprovals > 0 ? "opacity-100" : "opacity-0"
                    }`}
                  >
                    {pendingApprovals}
                  </span>
                )}
              </button>
            );
          })}
        </nav>
      </header>

      {/* Active Workspace Viewport */}
      <main className="min-h-0 flex-1 overflow-hidden">
        {activeWorkspace === "ORDERBOOK_HEATMAP" && (
          <OrderBookHeatmapWorkspace
            frameRef={frameRef}
            uiFrame={uiFrame}
            sendCommand={sendCommand}
          />
        )}
        {activeWorkspace === "FRAUD_FORCE_GRAPH" && (
          <FraudForceGraphWorkspace
            frameRef={frameRef}
            uiFrame={uiFrame}
            sendCommand={sendCommand}
          />
        )}
        {activeWorkspace === "CHRONOS_FORK_DIFF" && (
          <ChronosForkWorkspace
            frameRef={frameRef}
            uiFrame={uiFrame}
            sendCommand={sendCommand}
          />
        )}
        {activeWorkspace === "CHAOS_CONTROL_DECK" && (
          <ChaosControlWorkspace
            frameRef={frameRef}
            uiFrame={uiFrame}
            sendCommand={sendCommand}
          />
        )}
        {activeWorkspace === "AEGIS_SRE_CONSOLE" && (
          <AegisSreWorkspace uiFrame={uiFrame} sendCommand={sendCommand} />
        )}
      </main>

      {/* Persistent Bottom Chronos Nanosecond Time-Travel Scrubber */}
      <ChronosScrubberDock
        frameRef={frameRef}
        chronos={uiFrame?.chronos ?? null}
        symbolQuoteDecimals={meta.quoteDecimals}
        activeWorkspace={activeWorkspace}
        onSelectWorkspace={setActiveWorkspace}
        sendCommand={sendCommand}
      />
    </div>
  );
}
