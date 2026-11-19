"use client";

import React, { useEffect, useRef } from "react";
import type { MutableRefObject } from "react";
import {
  FastForward,
  GitBranch,
  Pause,
  Play,
  Radio,
  SkipBack,
  SkipForward,
  Zap,
} from "lucide-react";
import { formatFp, projectFpRatio } from "@/lib/fixed-point";
import type {
  ChronosReplayState,
  MainToWorkerCommand,
  WorkerFramePacket,
  WorkspaceId,
} from "@/lib/types";

interface ChronosScrubberDockProps {
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  chronos: ChronosReplayState | null;
  symbolQuoteDecimals: number;
  activeWorkspace: WorkspaceId;
  onSelectWorkspace: (ws: WorkspaceId) => void;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

const SPEED_OPTIONS: Array<{ label: string; speedMilli: number }> = [
  { label: "0.1x SLOW", speedMilli: 100 },
  { label: "0.5x", speedMilli: 500 },
  { label: "1.0x", speedMilli: 1000 },
  { label: "5.0x", speedMilli: 5000 },
  { label: "25x", speedMilli: 25000 },
  { label: "100x", speedMilli: 100000 },
];

export function ChronosScrubberDock({
  frameRef,
  chronos,
  symbolQuoteDecimals,
  activeWorkspace,
  onSelectWorkspace,
  sendCommand,
}: ChronosScrubberDockProps) {
  const sparkCanvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    let rafId = 0;
    const canvas = sparkCanvasRef.current;
    let cachedW = 320;
    let cachedH = 26;
    let lastRenderedSeq = -1n;
    let lastReplayFlag = false;
    let lastHistLen = -1;

    let ro: ResizeObserver | null = null;
    if (canvas) {
      const rect = canvas.getBoundingClientRect();
      cachedW = Math.max(120, Math.floor(rect.width));
      cachedH = Math.max(24, Math.floor(rect.height));
      if (typeof ResizeObserver !== "undefined") {
        ro = new ResizeObserver((entries) => {
          for (const entry of entries) {
            cachedW = Math.max(120, Math.floor(entry.contentRect.width));
            cachedH = Math.max(24, Math.floor(entry.contentRect.height));
            lastRenderedSeq = -1n;
          }
        });
        ro.observe(canvas);
      }
    }

    const renderSpark = () => {
      const c = sparkCanvasRef.current;
      const pkt = frameRef.current;
      if (c && pkt) {
        const curSeq = pkt.chronos.currentEngineSeq;
        const replayActive = pkt.chronos.isReplayActive;
        const histLen = pkt.chronos.priceHistoryFp.length;
        if (
          curSeq !== lastRenderedSeq ||
          replayActive !== lastReplayFlag ||
          histLen !== lastHistLen
        ) {
          lastRenderedSeq = curSeq;
          lastReplayFlag = replayActive;
          lastHistLen = histLen;

          const dpr = Math.min(window.devicePixelRatio || 1, 2);
          const w = cachedW;
          const h = cachedH;
          if (c.width !== w * dpr || c.height !== h * dpr) {
            c.width = w * dpr;
            c.height = h * dpr;
          }
          const ctx = c.getContext("2d");
          if (ctx) {
            ctx.save();
            ctx.scale(dpr, dpr);
            ctx.clearRect(0, 0, w, h);

            // Subtle background grid
            ctx.fillStyle = "#05070b";
            ctx.fillRect(0, 0, w, h);

            const hist = pkt.chronos.priceHistoryFp;
            if (hist.length > 1) {
              let minP = hist[0]!.priceFp;
              let maxP = hist[0]!.priceFp;
              for (const pt of hist) {
                if (pt.priceFp < minP) minP = pt.priceFp;
                if (pt.priceFp > maxP) maxP = pt.priceFp;
              }
              if (maxP <= minP) maxP = minP + 100_000_000n;

              // Volatility bands in background
              const stepX = w / (hist.length - 1);
              for (let i = 0; i < hist.length; i++) {
                const v = hist[i]!.volatilityBps;
                if (v > 75) {
                  ctx.fillStyle = v > 120 ? "rgba(255, 59, 92, 0.18)" : "rgba(245, 158, 11, 0.12)";
                  ctx.fillRect(i * stepX, 0, Math.max(2, stepX + 0.5), h);
                }
              }

              // Price trajectory path
              ctx.beginPath();
              for (let i = 0; i < hist.length; i++) {
                const x = i * stepX;
                const yRatio = projectFpRatio(hist[i]!.priceFp, minP, maxP);
                const y = h - 3 - yRatio * (h - 6);
                if (i === 0) ctx.moveTo(x, y);
                else ctx.lineTo(x, y);
              }
              ctx.strokeStyle = replayActive ? "#f59e0b" : "#38bdf8";
              ctx.lineWidth = 1.35;
              ctx.stroke();

              // Event markers
              const minSeq = pkt.chronos.minEngineSeq;
              const maxSeq = pkt.chronos.maxEngineSeq;
              const seqSpan = maxSeq > minSeq ? Number(maxSeq - minSeq) : 1;
              for (const m of pkt.chronos.markers) {
                const mx = (Number(m.engineSeq - minSeq) / seqSpan) * w;
                ctx.strokeStyle =
                  m.kind === "FLASH_CRASH" || m.kind === "WASH_RING"
                    ? "#ff3b5c"
                    : m.kind === "AEGIS_FREEZE"
                      ? "#00d492"
                      : "#f59e0b";
                ctx.lineWidth = 1;
                ctx.beginPath();
                ctx.moveTo(mx, 0);
                ctx.lineTo(mx, h);
                ctx.stroke();
              }

              // Playhead needle
              const curX = (Number(curSeq - minSeq) / seqSpan) * w;
              ctx.strokeStyle = replayActive ? "#fbbf24" : "#f1f5f9";
              ctx.lineWidth = 2;
              ctx.beginPath();
              ctx.moveTo(curX, 0);
              ctx.lineTo(curX, h);
              ctx.stroke();
            }
            ctx.restore();
          }
        }
      }
      rafId = requestAnimationFrame(renderSpark);
    };
    rafId = requestAnimationFrame(renderSpark);
    return () => {
      cancelAnimationFrame(rafId);
      ro?.disconnect();
    };
  }, [frameRef]);

  const minSeq = Number(chronos?.minEngineSeq ?? 1n);
  const maxSeq = Number(chronos?.maxEngineSeq ?? 320n);
  const currentSeq = Number(chronos?.currentEngineSeq ?? 320n);
  const isReplay = chronos?.isReplayActive ?? false;
  const isPaused = chronos?.isPaused ?? false;
  const activeSpeed = chronos?.speedMilli ?? 1000;

  const currentPriceFp =
    chronos?.priceHistoryFp.find((p) => Number(p.seq) === currentSeq)?.priceFp ??
    chronos?.priceHistoryFp[chronos.priceHistoryFp.length - 1]?.priceFp ??
    0n;

  return (
    <footer
      data-testid="chronos-scrubber-dock"
      className="isolated-pane shrink-0 border-t border-[var(--wire-strong)] bg-[#080b12] px-3 py-1.5"
    >
      <div className="flex flex-wrap items-center gap-2.5">
        {/* Status Pill & Transport */}
        <div className="flex items-center gap-1.5">
          <span
            data-testid="chronos-mode-badge"
            className={`inline-flex w-[132px] items-center justify-center gap-1 rounded-[2px] border px-2 py-0.5 text-[11px] font-semibold tracking-tight tabular-mono ${
              isReplay
                ? "border-[#f59e0b]/50 bg-[#f59e0b]/15 text-[#fbbf24]"
                : "border-[#00d492]/40 bg-[#00d492]/12 text-[#00d492]"
            }`}
          >
            <Radio className="h-3 w-3 shrink-0" />
            {isReplay ? "CHRONOS REPLAY" : "LIVE WAL HEAD"}
          </span>

          <button
            type="button"
            data-testid="chronos-step-bwd"
            title="Step Backward (-1 EngineSeq)"
            onClick={() => sendCommand({ type: "CHRONOS_CONTROL", action: "STEP_BWD" })}
            className="cmd-btn inline-flex h-7 w-7 items-center justify-center rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-panel)] text-[var(--ink-secondary)] hover:border-[var(--wire-strong)] hover:text-white"
          >
            <SkipBack className="h-3.5 w-3.5" />
          </button>

          <button
            type="button"
            data-testid="chronos-play-pause"
            title={isReplay && !isPaused ? "Pause Replay" : "Play Historical WAL"}
            onClick={() =>
              sendCommand({
                type: "CHRONOS_CONTROL",
                action: isReplay && !isPaused ? "PAUSE" : "PLAY",
              })
            }
            className="cmd-btn inline-flex h-7 w-7 items-center justify-center rounded-[2px] border border-[var(--wire-strong)] bg-[#141c2b] text-white hover:border-[#38bdf8]"
          >
            {isReplay && !isPaused ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
          </button>

          <button
            type="button"
            data-testid="chronos-step-fwd"
            title="Step Forward (+1 EngineSeq)"
            onClick={() => sendCommand({ type: "CHRONOS_CONTROL", action: "STEP_FWD" })}
            className="cmd-btn inline-flex h-7 w-7 items-center justify-center rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-panel)] text-[var(--ink-secondary)] hover:border-[var(--wire-strong)] hover:text-white"
          >
            <SkipForward className="h-3.5 w-3.5" />
          </button>
        </div>

        {/* Speed Selector */}
        <div className="flex items-center gap-1 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-0.5">
          <FastForward className="ml-1 h-3 w-3 text-[var(--ink-tertiary)]" />
          {SPEED_OPTIONS.map((opt) => {
            const selected = activeSpeed === opt.speedMilli;
            return (
              <button
                key={opt.speedMilli}
                type="button"
                data-testid={`chronos-speed-${opt.speedMilli}`}
                onClick={() =>
                  sendCommand({
                    type: "CHRONOS_CONTROL",
                    action: isReplay && !isPaused ? "PLAY" : "PAUSE",
                    speedMilli: opt.speedMilli,
                  })
                }
                className={`cmd-btn rounded-[2px] px-1.5 py-0.5 text-[10px] font-medium tabular-mono ${
                  selected
                    ? "bg-[#f59e0b] text-[#06080c] font-semibold"
                    : "text-[var(--ink-secondary)] hover:text-white"
                }`}
              >
                {opt.label}
              </button>
            );
          })}
        </div>

        {/* Quick Event Bookmarks */}
        <button
          type="button"
          data-testid="chronos-jump-flash-crash"
          onClick={() => {
            sendCommand({ type: "CHRONOS_SEEK", targetEngineSeq: 152n });
            sendCommand({ type: "CHRONOS_CONTROL", action: "PAUSE", speedMilli: 100 });
          }}
          className="cmd-btn inline-flex h-7 items-center gap-1 rounded-[2px] border border-[#ff3b5c]/40 bg-[#ff3b5c]/10 px-2 text-[11px] font-medium text-[#ff6b84] hover:bg-[#ff3b5c]/20"
        >
          <Zap className="h-3 w-3" />
          Flash-Crash @152 (0.1x)
        </button>

        {/* Interactive Scrubber Track + Volatility Canvas */}
        <div className="relative flex min-w-[240px] flex-1 items-center gap-2">
          <div className="relative h-7 flex-1 overflow-hidden rounded-[2px] border border-[var(--wire-hairline)]">
            <canvas ref={sparkCanvasRef} className="block h-full w-full" />
            <input
              type="range"
              aria-label="Chronos nanosecond WAL sequence scrubber"
              data-testid="chronos-range-slider"
              min={minSeq}
              max={Math.max(minSeq + 1, maxSeq)}
              value={Math.min(Math.max(currentSeq, minSeq), Math.max(minSeq + 1, maxSeq))}
              onChange={(e) =>
                sendCommand({
                  type: "CHRONOS_SEEK",
                  targetEngineSeq: BigInt(e.target.value),
                })
              }
              className="absolute inset-0 h-full w-full cursor-ew-resize opacity-0"
            />
          </div>

          <div className="hidden w-[228px] shrink-0 items-center justify-end gap-1.5 text-[11px] tabular-mono xl:flex">
            <span className="text-[var(--ink-tertiary)]">SEQ</span>
            <span data-testid="chronos-seq-readout" className="font-semibold text-white">
              #{currentSeq}
            </span>
            <span className="text-[var(--ink-muted)]">/ {maxSeq}</span>
            <span className="text-[var(--ink-tertiary)]">MID</span>
            <span className="text-[#38bdf8]">{formatFp(currentPriceFp, symbolQuoteDecimals)}</span>
          </div>
        </div>

        {/* Fork Diff & Return Live Actions */}
        <div className="flex items-center gap-1.5">
          {isReplay && (
            <button
              type="button"
              data-testid="chronos-return-live"
              onClick={() => sendCommand({ type: "CHRONOS_CONTROL", action: "RETURN_LIVE" })}
              className="cmd-btn h-7 rounded-[2px] border border-[#00d492]/50 bg-[#00d492]/15 px-2.5 text-[11px] font-semibold text-[#00d492] hover:bg-[#00d492]/25"
            >
              Return Live
            </button>
          )}

          <button
            type="button"
            data-testid="chronos-open-fork-diff"
            onClick={() => onSelectWorkspace("CHRONOS_FORK_DIFF")}
            className={`cmd-btn inline-flex h-7 items-center gap-1.5 rounded-[2px] border px-2.5 text-[11px] font-semibold ${
              activeWorkspace === "CHRONOS_FORK_DIFF"
                ? "border-[#f59e0b] bg-[#f59e0b] text-[#06080c]"
                : "border-[#f59e0b]/40 bg-[#f59e0b]/12 text-[#fbbf24] hover:bg-[#f59e0b]/20"
            }`}
          >
            <GitBranch className="h-3.5 w-3.5" />
            What-If Fork Diff
          </button>
        </div>
      </div>
    </footer>
  );
}
