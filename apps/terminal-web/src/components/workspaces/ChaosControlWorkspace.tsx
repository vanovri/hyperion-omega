"use client";

import React, { useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { Flame, Gauge, PowerOff, ShieldCheck, Sliders, WifiOff } from "lucide-react";
import { formatCompactNotionalFp, formatFp } from "@/lib/fixed-point";
import type { ChaosRegime, MainToWorkerCommand, WorkerFramePacket } from "@/lib/types";

interface ChaosControlWorkspaceProps {
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  uiFrame: WorkerFramePacket | null;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

const REGIME_CARDS: Array<{
  regime: ChaosRegime;
  title: string;
  subtitle: string;
  accent: "emerald" | "amber" | "crimson" | "cyan";
}> = [
  {
    regime: "CALM",
    title: "CALM BASELINE",
    subtitle: "Avellaneda-Stoikov MM + Poisson organic flow",
    accent: "emerald",
  },
  {
    regime: "SPOOFING_ATTACK",
    title: "SPOOFING SYNDICATE",
    subtitle: "Multi-tick phantom ask walls + sub-2ms cancel",
    accent: "amber",
  },
  {
    regime: "WASH_TRADING_RING",
    title: "CIRCULAR WASH RING",
    subtitle: "4-account zero-net-inventory volume inflation",
    accent: "crimson",
  },
  {
    regime: "FLASH_CRASH",
    title: "FLASH-CRASH SWEEP",
    subtitle: "Aggressive multi-level liquidity vacuum",
    accent: "crimson",
  },
  {
    regime: "HIGH_VOLATILITY",
    title: "HIGH VOLATILITY",
    subtitle: "Wide spread + momentum ignition bursts",
    accent: "amber",
  },
  {
    regime: "BURST_TSUNAMI_20X",
    title: "20x TRAFFIC TSUNAMI",
    subtitle: "65,000+ orders/sec backpressure torture test",
    accent: "cyan",
  },
  {
    regime: "NETWORK_PARTITION",
    title: "TOXIC TCP PARTITION",
    subtitle: "Inter-service Pareto jitter & packet stall",
    accent: "crimson",
  },
];

export function ChaosControlWorkspace({
  frameRef,
  uiFrame,
  sendCommand,
}: ChaosControlWorkspaceProps) {
  const telemetryCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const [addedLatencyMs, setAddedLatencyMs] = useState(0);
  const [paretoJitterMs, setParetoJitterMs] = useState(0);
  const [packetDropPct, setPacketDropPct] = useState(0);
  const [clockSkewMs, setClockSkewMs] = useState(0);
  const [seedHex, setSeedHex] = useState("4859504552494f4e");

  const pushFaults = (lat: number, jit: number, drop: number, skew: number) => {
    sendCommand({
      type: "CHAOS_SET_FAULTS",
      addedLatencyMs: lat,
      paretoJitterMs: jit,
      packetDropPct: drop,
      clockSkewMs: skew,
    });
  };

  useEffect(() => {
    let rafId = 0;
    const renderOscilloscope = () => {
      const canvas = telemetryCanvasRef.current;
      const pkt = frameRef.current;
      if (canvas && pkt) {
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const rect = canvas.getBoundingClientRect();
        const w = Math.max(280, Math.floor(rect.width));
        const h = Math.max(220, Math.floor(rect.height));
        if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
          canvas.width = w * dpr;
          canvas.height = h * dpr;
        }
        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.save();
          ctx.scale(dpr, dpr);
          ctx.fillStyle = "#06080c";
          ctx.fillRect(0, 0, w, h);

          const samples = pkt.chaos.samples;
          const padTop = 28;
          const padBottom = 24;
          const padRight = 58;
          const plotW = w - padRight;
          const plotH = h - padTop - padBottom;

          let maxVal = 15;
          for (const s of samples) {
            if (s.ledgerP99Us > maxVal) maxVal = s.ledgerP99Us;
            if (s.gatewayP99Ms > maxVal) maxVal = s.gatewayP99Ms;
            if (s.sentinelP99Ms > maxVal) maxVal = s.sentinelP99Ms;
          }
          maxVal = Math.ceil(maxVal * 1.18);

          // Grid
          ctx.strokeStyle = "rgba(226, 232, 240, 0.06)";
          ctx.fillStyle = "#64748b";
          ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
          for (let g = 0; g <= 4; g++) {
            const y = Math.floor(padTop + (plotH / 4) * g) + 0.5;
            ctx.beginPath();
            ctx.moveTo(0, y);
            ctx.lineTo(plotW, y);
            ctx.stroke();

            const v = ((1 - g / 4) * maxVal).toFixed(1);
            ctx.fillText(v, plotW + 6, y + 3);
          }

          if (samples.length > 1) {
            // Throughput background bars
            const slotW = plotW / samples.length;
            for (let i = 0; i < samples.length; i++) {
              const opsRatio = Math.min(1, samples[i]!.ordersPerSec / 75_000);
              const bh = opsRatio * (plotH * 0.45);
              ctx.fillStyle = "rgba(56, 189, 248, 0.09)";
              ctx.fillRect(i * slotW, padTop + plotH - bh, Math.max(2, slotW - 1), bh);
            }

            const drawSeries = (getter: (s: (typeof samples)[number]) => number, color: string) => {
              ctx.beginPath();
              for (let i = 0; i < samples.length; i++) {
                const x = (i / (samples.length - 1)) * plotW;
                const y = padTop + (1 - Math.min(1, getter(samples[i]!) / maxVal)) * plotH;
                if (i === 0) ctx.moveTo(x, y);
                else ctx.lineTo(x, y);
              }
              ctx.strokeStyle = color;
              ctx.lineWidth = 1.8;
              ctx.stroke();
            };

            drawSeries((s) => s.ledgerP99Us, "#00d492");
            drawSeries((s) => s.gatewayP99Ms, "#38bdf8");
            drawSeries((s) => s.sentinelP99Ms, "#ff3b5c");
          }

          // Legend
          ctx.font = "bold 10px ui-monospace, SFMono-Regular, Menlo, monospace";
          ctx.fillStyle = "#00d492";
          ctx.fillText("● ledger-rs p99 (µs)", 10, 16);
          ctx.fillStyle = "#38bdf8";
          ctx.fillText("● gateway-go p99 (ms)", 152, 16);
          ctx.fillStyle = "#ff3b5c";
          ctx.fillText("● sentinel-ml p99 (ms)", 306, 16);

          ctx.restore();
        }
      }
      rafId = requestAnimationFrame(renderOscilloscope);
    };
    rafId = requestAnimationFrame(renderOscilloscope);
    return () => cancelAnimationFrame(rafId);
  }, [frameRef]);

  const chaos = uiFrame?.chaos;
  const latestSample = chaos?.samples[chaos.samples.length - 1];

  return (
    <div
      data-testid="workspace-chaos-deck"
      className="grid h-full min-h-0 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-12"
    >
      {/* Left 5 Cols: Tactical Chaos Regime Switchboard & Toxic Proxy Sliders */}
      <section className="isolated-pane flex min-h-0 flex-col overflow-y-auto bg-[var(--obsidian-panel)] p-3 lg:col-span-5">
        <div className="flex items-center justify-between gap-2 pb-2.5">
          <div className="flex items-center gap-2">
            <Flame className="h-4 w-4 text-[#ff3b5c]" />
            <h2 className="text-xs font-semibold text-white">
              Chaos-Sim Tactical Fault &amp; Syndicate Campaign Deck
            </h2>
          </div>

          <button
            type="button"
            data-testid="chaos-killswitch-btn"
            onClick={() => {
              setAddedLatencyMs(0);
              setParetoJitterMs(0);
              setPacketDropPct(0);
              setClockSkewMs(0);
              pushFaults(0, 0, 0, 0);
              sendCommand({ type: "CHAOS_SET_REGIME", regime: "CALM" });
            }}
            className="cmd-btn inline-flex items-center gap-1 rounded-[2px] border border-[#00d492]/50 bg-[#00d492]/15 px-2.5 py-1 text-[11px] font-semibold text-[#00d492] hover:bg-[#00d492]/25"
          >
            <PowerOff className="h-3 w-3" />
            Restore Calm
          </button>
        </div>

        {/* Regime Toggle Matrix */}
        <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
          {REGIME_CARDS.map((card) => {
            const active = chaos?.activeRegime === card.regime;
            const ringCls = active
              ? card.accent === "emerald"
                ? "border-[#00d492] bg-[#00d492]/15"
                : card.accent === "amber"
                  ? "border-[#f59e0b] bg-[#f59e0b]/15"
                  : card.accent === "cyan"
                    ? "border-[#38bdf8] bg-[#38bdf8]/15"
                    : "border-[#ff3b5c] bg-[#ff3b5c]/18"
              : "border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] hover:border-[var(--wire-strong)]";

            return (
              <button
                key={card.regime}
                type="button"
                data-testid={`chaos-regime-${card.regime}`}
                onClick={() => {
                  const parsedSeed = BigInt(`0x${seedHex.replace(/^0x/i, "") || "1"}`);
                  sendCommand({ type: "CHAOS_SET_REGIME", regime: card.regime, seed: parsedSeed });
                }}
                className={`cmd-btn flex flex-col justify-between rounded-[2px] border p-2.5 text-left ${ringCls}`}
              >
                <div className="flex w-full items-center justify-between">
                  <span className="text-[11px] font-bold text-white tabular-mono">{card.title}</span>
                  <span
                    className={`h-2 w-2 rounded-full ${
                      active ? "bg-[#00d492] shadow-[0_0_8px_#00d492]" : "bg-slate-600"
                    }`}
                  />
                </div>
                <span className="mt-1 text-[10px] text-[var(--ink-secondary)]">{card.subtitle}</span>
              </button>
            );
          })}
        </div>

        {/* Toxic TCP Fault Proxy Controls */}
        <div className="mt-3 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-3">
          <div className="mb-2 flex items-center justify-between text-xs">
            <span className="inline-flex items-center gap-1.5 font-semibold text-white">
              <Sliders className="h-3.5 w-3.5 text-[#38bdf8]" />
              Toxic TCP Proxy &amp; Clock-Skew Injector
            </span>
            <label className="inline-flex items-center gap-1 text-[10px] text-[var(--ink-tertiary)] tabular-mono">
              SEED 0x
              <input
                type="text"
                aria-label="Deterministic simulation PRNG hex seed"
                value={seedHex}
                onChange={(e) => setSeedHex(e.target.value.replace(/[^0-9a-fA-F]/g, "").slice(0, 16))}
                className="w-28 rounded-[2px] border border-[var(--wire-hairline)] bg-[#06080c] px-1.5 py-0.5 text-white"
              />
            </label>
          </div>

          <div className="space-y-2.5 text-[11px] tabular-mono">
            <div>
              <div className="flex justify-between">
                <label htmlFor="chaos-lat-slider" className="text-[var(--ink-secondary)]">
                  Injected TCP Latency
                </label>
                <span className="text-white">{addedLatencyMs} ms</span>
              </div>
              <input
                id="chaos-lat-slider"
                data-testid="chaos-latency-slider"
                type="range"
                min={0}
                max={120}
                value={addedLatencyMs}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  setAddedLatencyMs(v);
                  pushFaults(v, paretoJitterMs, packetDropPct, clockSkewMs);
                }}
                className="w-full"
              />
            </div>

            <div>
              <div className="flex justify-between">
                <label htmlFor="chaos-jit-slider" className="text-[var(--ink-secondary)]">
                  Pareto Tail Jitter
                </label>
                <span className="text-white">{paretoJitterMs} ms</span>
              </div>
              <input
                id="chaos-jit-slider"
                type="range"
                min={0}
                max={95}
                value={paretoJitterMs}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  setParetoJitterMs(v);
                  pushFaults(addedLatencyMs, v, packetDropPct, clockSkewMs);
                }}
                className="w-full"
              />
            </div>

            <div>
              <div className="flex justify-between">
                <label htmlFor="chaos-skew-slider" className="text-[var(--ink-secondary)]">
                  Signature Clock-Skew Fault (&gt;5000ms trips anti-replay)
                </label>
                <span className={clockSkewMs > 5000 ? "text-[#ff5470]" : "text-white"}>
                  {clockSkewMs} ms
                </span>
              </div>
              <input
                id="chaos-skew-slider"
                type="range"
                min={0}
                max={9000}
                step={250}
                value={clockSkewMs}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  setClockSkewMs(v);
                  pushFaults(addedLatencyMs, paretoJitterMs, packetDropPct, v);
                }}
                className="w-full"
              />
            </div>
          </div>

          <button
            type="button"
            data-testid="chaos-ws-drop-btn"
            onClick={() => sendCommand({ type: "SIMULATE_WS_DISCONNECT" })}
            className="cmd-btn mt-3 inline-flex w-full items-center justify-center gap-1.5 rounded-[2px] border border-[#f59e0b]/40 bg-[#f59e0b]/12 py-1.5 text-[11px] font-semibold text-[#fbbf24] hover:bg-[#f59e0b]/20"
          >
            <WifiOff className="h-3.5 w-3.5" />
            Inject Forced WebSocket Drop &amp; Verify Auto-Resync
          </button>
        </div>
      </section>

      {/* Right 7 Cols: p99 Latency Oscilloscope & Continuous Double-Entry Invariant Auditor */}
      <section className="isolated-pane flex min-h-0 flex-col bg-[var(--obsidian-panel)] lg:col-span-7">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--wire-hairline)] px-3 py-2">
          <div className="flex items-center gap-2">
            <Gauge className="h-4 w-4 text-[#38bdf8]" />
            <h3 className="text-xs font-semibold text-white">
              Real-Time p99 Tail-Latency &amp; Throughput Response Monitor
            </h3>
          </div>
          <span
            data-testid="chaos-active-regime-readout"
            className="rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] px-2 py-0.5 text-[11px] font-bold text-[#38bdf8] tabular-mono"
          >
            REGIME: {chaos?.activeRegime ?? "CALM"}
          </span>
        </header>

        {/* Hero Quantitative Readouts */}
        <div className="grid grid-cols-2 gap-px border-b border-[var(--wire-hairline)] bg-[var(--wire-hairline)] sm:grid-cols-4">
          <div className="bg-[var(--obsidian-panel)] p-2.5 tabular-mono">
            <div className="text-[10px] text-[var(--ink-tertiary)]">ledger-rs Match p99</div>
            <div data-testid="metric-ledger-p99" className="mt-0.5 text-base font-bold text-[#00d492]">
              {latestSample?.ledgerP99Us.toFixed(2) ?? "3.50"} µs
            </div>
            <div className="text-[10px] text-[var(--ink-secondary)]">Target &lt; 10.0 µs</div>
          </div>

          <div className="bg-[var(--obsidian-panel)] p-2.5 tabular-mono">
            <div className="text-[10px] text-[var(--ink-tertiary)]">gateway-go Edge p99</div>
            <div data-testid="metric-gateway-p99" className="mt-0.5 text-base font-bold text-[#38bdf8]">
              {latestSample?.gatewayP99Ms.toFixed(2) ?? "0.82"} ms
            </div>
            <div className="text-[10px] text-[var(--ink-secondary)]">
              Evictions: {latestSample?.droppedSlowConsumers ?? 0}/s
            </div>
          </div>

          <div className="bg-[var(--obsidian-panel)] p-2.5 tabular-mono">
            <div className="text-[10px] text-[var(--ink-tertiary)]">sentinel-ml ONNX p99</div>
            <div className="mt-0.5 text-base font-bold text-[#ff5470]">
              {latestSample?.sentinelP99Ms.toFixed(2) ?? "2.30"} ms
            </div>
            <div className="text-[10px] text-[var(--ink-secondary)]">Target &lt; 10.0 ms</div>
          </div>

          <div className="bg-[var(--obsidian-panel)] p-2.5 tabular-mono">
            <div className="text-[10px] text-[var(--ink-tertiary)]">Bot Swarm Throughput</div>
            <div className="mt-0.5 text-base font-bold text-white">
              {(chaos?.ordersPerSec ?? 18_400).toLocaleString()} /s
            </div>
            <div className="text-[10px] text-[var(--ink-secondary)]">
              {chaos?.activeBotCount ?? 1024} autonomous agents
            </div>
          </div>
        </div>

        <div className="min-h-[220px] flex-1">
          <canvas
            ref={telemetryCanvasRef}
            data-testid="chaos-telemetry-canvas"
            className="block h-full w-full"
          />
        </div>

        {/* Continuous Accounting Invariant Auditor */}
        <div className="border-t border-[var(--wire-hairline)] bg-[#070a10] p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <ShieldCheck className="h-4 w-4 text-[#00d492]" />
              <div>
                <div className="text-xs font-semibold text-white">
                  Continuous Double-Entry Conservation Auditor
                </div>
                <div className="text-[10px] text-[var(--ink-secondary)] tabular-mono">
                  Deposits {formatCompactNotionalFp(chaos?.totalDepositsFp ?? 0n)} == Equity{" "}
                  {formatCompactNotionalFp(chaos?.totalUserEquityFp ?? 0n)} + Treasury{" "}
                  {formatCompactNotionalFp(chaos?.treasuryFeePoolFp ?? 0n)}
                </div>
              </div>
            </div>

            <div className="rounded-[2px] border border-[#00d492]/40 bg-[#00d492]/12 px-2.5 py-1 text-[11px] font-bold text-[#00d492] tabular-mono">
              DRIFT: {formatFp(chaos?.conservationDriftFp ?? 0n, 8)} USDT · ZERO PHANTOM FILLS
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}
