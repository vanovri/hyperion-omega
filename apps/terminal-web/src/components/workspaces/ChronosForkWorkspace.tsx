"use client";

import React, { useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { CheckCircle2, GitBranch, PlayCircle, Scale, SplitSquareHorizontal } from "lucide-react";
import {
  formatCompactNotionalFp,
  formatFp,
  projectFpRatio,
} from "@/lib/fixed-point";
import type {
  CounterfactualForkConfig,
  MainToWorkerCommand,
  WorkerFramePacket,
} from "@/lib/types";
import { INSTRUMENT_META } from "@/lib/types";

interface ChronosForkWorkspaceProps {
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  uiFrame: WorkerFramePacket | null;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

export function ChronosForkWorkspace({
  frameRef,
  uiFrame,
  sendCommand,
}: ChronosForkWorkspaceProps) {
  const baselineCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const forkedCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const syncHoverRatioRef = useRef<{ ratioX: number; active: boolean }>({
    ratioX: 0.5,
    active: false,
  });

  const symbol = uiFrame?.symbol ?? "BTC-USDT";
  const meta = INSTRUMENT_META[symbol];

  const [forkName, setForkName] = useState("early-freeze-syndicate-900x");
  const [startSeq, setStartSeq] = useState(120);
  const [endSeq, setEndSeq] = useState(300);
  const [makerFeePpm, setMakerFeePpm] = useState<string>("-20");
  const [takerFeePpm, setTakerFeePpm] = useState<string>("80");
  const [maxOrderBtc, setMaxOrderBtc] = useState<string>("2.50");
  const [stpMode, setStpMode] = useState<CounterfactualForkConfig["stpModeOverride"]>("CANCEL_BOTH");
  const [regimeOverride, setRegimeOverride] =
    useState<CounterfactualForkConfig["regimeOverride"]>("VOLATILITY_GUARD");
  const [blockSyndicate, setBlockSyndicate] = useState(true);

  const triggerFork = (customConfig?: CounterfactualForkConfig) => {
    if (customConfig) {
      sendCommand({ type: "CHRONOS_FORK", config: customConfig });
      return;
    }
    const maxCapVal = maxOrderBtc.trim() === "" ? null : BigInt(Math.round(Number(maxOrderBtc) * 1e8));
    const cfg: CounterfactualForkConfig = {
      forkName,
      symbol,
      startEngineSeq: BigInt(Math.min(startSeq, endSeq)),
      endEngineSeq: BigInt(Math.max(startSeq + 1, endSeq)),
      makerFeePpmOverride: makerFeePpm.trim() === "" ? null : Number(makerFeePpm),
      takerFeePpmOverride: takerFeePpm.trim() === "" ? null : Number(takerFeePpm),
      maxOrderQtyFpOverride: maxCapVal,
      stpModeOverride: stpMode,
      regimeOverride,
      blockedAccountIds: blockSyndicate ? [9001n, 9002n, 9003n, 9004n] : [],
    };
    sendCommand({ type: "CHRONOS_FORK", config: cfg });
  };

  const applyNoopPreset = () => {
    setForkName("noop-zero-diff-proof");
    setMakerFeePpm("");
    setTakerFeePpm("");
    setMaxOrderBtc("");
    setStpMode("NONE");
    setRegimeOverride("NORMAL");
    setBlockSyndicate(false);
    triggerFork({
      forkName: "noop-zero-diff-proof",
      symbol,
      startEngineSeq: BigInt(startSeq),
      endEngineSeq: BigInt(endSeq),
      makerFeePpmOverride: null,
      takerFeePpmOverride: null,
      maxOrderQtyFpOverride: null,
      stpModeOverride: "NONE",
      regimeOverride: "NORMAL",
      blockedAccountIds: [],
    });
  };

  const applyMitigationPreset = () => {
    setForkName("early-freeze-syndicate-900x");
    setMakerFeePpm("-20");
    setTakerFeePpm("80");
    setMaxOrderBtc("2.50");
    setStpMode("CANCEL_BOTH");
    setRegimeOverride("VOLATILITY_GUARD");
    setBlockSyndicate(true);
    triggerFork({
      forkName: "early-freeze-syndicate-900x",
      symbol,
      startEngineSeq: 120n,
      endEngineSeq: 300n,
      makerFeePpmOverride: -20,
      takerFeePpmOverride: 80,
      maxOrderQtyFpOverride: 250_000_000n,
      stpModeOverride: "CANCEL_BOTH",
      regimeOverride: "VOLATILITY_GUARD",
      blockedAccountIds: [9001n, 9002n, 9003n, 9004n],
    });
  };

  // Synchronized Split-Screen Canvas Renderer
  useEffect(() => {
    let rafId = 0;

    const drawTimelinePane = (
      canvas: HTMLCanvasElement | null,
      seriesFp: bigint[],
      ghostSeriesFp: bigint[] | null,
      accentHex: string,
      title: string,
      minFp: bigint,
      maxFp: bigint,
      quoteDec: number,
      startS: bigint
    ) => {
      if (!canvas || seriesFp.length < 2) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const rect = canvas.getBoundingClientRect();
      const w = Math.max(240, Math.floor(rect.width));
      const h = Math.max(180, Math.floor(rect.height));
      if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
        canvas.width = w * dpr;
        canvas.height = h * dpr;
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      ctx.save();
      ctx.scale(dpr, dpr);
      ctx.fillStyle = "#06080c";
      ctx.fillRect(0, 0, w, h);

      const padTop = 26;
      const padBottom = 22;
      const padRight = 76;
      const plotW = w - padRight;
      const plotH = h - padTop - padBottom;

      // Grid lines
      ctx.strokeStyle = "rgba(226, 232, 240, 0.06)";
      ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillStyle = "#64748b";
      for (let g = 0; g <= 4; g++) {
        const y = Math.floor(padTop + (plotH / 4) * g) + 0.5;
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(plotW, y);
        ctx.stroke();

        const ratio = 1 - g / 4;
        const pFp = minFp + ((maxFp - minFp) * BigInt(Math.round(ratio * 10000))) / 10000n;
        ctx.fillText(formatFp(pFp, quoteDec), plotW + 5, y + 3);
      }

      const toY = (val: bigint) => padTop + (1 - projectFpRatio(val, minFp, maxFp)) * plotH;

      // Ghost baseline reference on Forked canvas
      if (ghostSeriesFp && ghostSeriesFp.length > 1) {
        ctx.save();
        ctx.setLineDash([4, 3]);
        ctx.beginPath();
        for (let i = 0; i < ghostSeriesFp.length; i++) {
          const x = (i / (ghostSeriesFp.length - 1)) * plotW;
          const y = toY(ghostSeriesFp[i]!);
          if (i === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        }
        ctx.strokeStyle = "rgba(148, 163, 184, 0.42)";
        ctx.lineWidth = 1.2;
        ctx.stroke();
        ctx.restore();
      }

      // Main trajectory curve + area fill
      ctx.beginPath();
      for (let i = 0; i < seriesFp.length; i++) {
        const x = (i / (seriesFp.length - 1)) * plotW;
        const y = toY(seriesFp[i]!);
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.strokeStyle = accentHex;
      ctx.lineWidth = 2;
      ctx.stroke();

      ctx.lineTo(plotW, padTop + plotH);
      ctx.lineTo(0, padTop + plotH);
      ctx.closePath();
      const grad = ctx.createLinearGradient(0, padTop, 0, padTop + plotH);
      grad.addColorStop(0, `${accentHex}33`);
      grad.addColorStop(1, `${accentHex}04`);
      ctx.fillStyle = grad;
      ctx.fill();

      // Header overlay
      ctx.fillStyle = accentHex;
      ctx.font = "bold 11px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillText(title, 10, 16);

      // Synchronized dual-pane crosshair
      const sync = syncHoverRatioRef.current;
      if (sync.active) {
        const idx = Math.min(
          seriesFp.length - 1,
          Math.max(0, Math.round(sync.ratioX * (seriesFp.length - 1)))
        );
        const cx = (idx / (seriesFp.length - 1)) * plotW;
        const val = seriesFp[idx]!;
        const cy = toY(val);

        ctx.strokeStyle = "rgba(241, 245, 249, 0.55)";
        ctx.setLineDash([2, 2]);
        ctx.beginPath();
        ctx.moveTo(cx, padTop);
        ctx.lineTo(cx, padTop + plotH);
        ctx.moveTo(0, cy);
        ctx.lineTo(plotW, cy);
        ctx.stroke();
        ctx.setLineDash([]);

        ctx.beginPath();
        ctx.arc(cx, cy, 3.8, 0, Math.PI * 2);
        ctx.fillStyle = "#ffffff";
        ctx.fill();

        const seqAtIdx = startS + BigInt(idx);
        const label = `SEQ #${seqAtIdx.toString()} · ${formatFp(val, quoteDec)}`;
        ctx.fillStyle = "rgba(6, 8, 12, 0.92)";
        ctx.fillRect(Math.min(plotW - 155, Math.max(8, cx - 60)), h - 19, 150, 16);
        ctx.fillStyle = "#f8fafc";
        ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
        ctx.fillText(label, Math.min(plotW - 150, Math.max(12, cx - 56)), h - 8);
      }

      ctx.restore();
    };

    const renderLoop = () => {
      const pkt = frameRef.current;
      const diff = pkt?.chronos.activeForkDiff;
      if (pkt && diff && diff.baselinePriceSeriesFp.length > 1) {
        const qDec = INSTRUMENT_META[diff.symbol].quoteDecimals;
        let minP = diff.baselinePriceSeriesFp[0]!;
        let maxP = diff.baselinePriceSeriesFp[0]!;
        for (const v of diff.baselinePriceSeriesFp) {
          if (v < minP) minP = v;
          if (v > maxP) maxP = v;
        }
        for (const v of diff.forkedPriceSeriesFp) {
          if (v < minP) minP = v;
          if (v > maxP) maxP = v;
        }
        const pad = ((maxP - minP) * 15n) / 100n + INSTRUMENT_META[diff.symbol].tickFp * 10n;
        minP -= pad;
        maxP += pad;

        drawTimelinePane(
          baselineCanvasRef.current,
          diff.baselinePriceSeriesFp,
          null,
          "#38bdf8",
          "BASELINE PRODUCTION WAL",
          minP,
          maxP,
          qDec,
          diff.startEngineSeq
        );
        drawTimelinePane(
          forkedCanvasRef.current,
          diff.forkedPriceSeriesFp,
          diff.baselinePriceSeriesFp,
          "#f59e0b",
          `COUNTERFACTUAL FORK (${diff.forkName})`,
          minP,
          maxP,
          qDec,
          diff.startEngineSeq
        );
      }
      rafId = requestAnimationFrame(renderLoop);
    };

    rafId = requestAnimationFrame(renderLoop);
    return () => cancelAnimationFrame(rafId);
  }, [frameRef]);

  const diff = uiFrame?.chronos.activeForkDiff ?? null;

  const handleSyncMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const plotW = Math.max(1, r.width - 76);
    syncHoverRatioRef.current = {
      ratioX: Math.max(0, Math.min(1, (e.clientX - r.left) / plotW)),
      active: true,
    };
  };

  return (
    <div
      data-testid="workspace-chronos-fork"
      className="flex h-full min-h-0 flex-col gap-px bg-[var(--wire-hairline)]"
    >
      {/* Top Policy Override Workbench */}
      <section className="isolated-pane shrink-0 bg-[var(--obsidian-panel)] p-3">
        <div className="flex flex-wrap items-center justify-between gap-2 pb-2.5">
          <div className="flex items-center gap-2">
            <SplitSquareHorizontal className="h-4 w-4 text-[#f59e0b]" />
            <h2 className="text-xs font-semibold text-white">
              Chronos Counterfactual &ldquo;What-If&rdquo; Sandbox &amp; State-Tree Diff Engine
            </h2>
          </div>

          <div className="flex flex-wrap items-center gap-1.5">
            <button
              type="button"
              data-testid="preset-mitigation-fork"
              onClick={applyMitigationPreset}
              className="cmd-btn rounded-[2px] border border-[#f59e0b]/40 bg-[#f59e0b]/12 px-2.5 py-1 text-[11px] font-medium text-[#fbbf24] hover:bg-[#f59e0b]/20"
            >
              Preset: Freeze Syndicate + Volatility Guard
            </button>
            <button
              type="button"
              data-testid="preset-noop-fork"
              onClick={applyNoopPreset}
              className="cmd-btn rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] px-2.5 py-1 text-[11px] font-medium text-[var(--ink-secondary)] hover:text-white"
            >
              Preset: No-Op Zero-Diff Proof
            </button>
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2 text-[11px] sm:grid-cols-4 lg:grid-cols-8">
          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Fork Name</span>
            <input
              type="text"
              value={forkName}
              onChange={(e) => setForkName(e.target.value)}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Start Seq (T1)</span>
            <input
              type="number"
              value={startSeq}
              onChange={(e) => setStartSeq(Number(e.target.value))}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">End Seq (T2)</span>
            <input
              type="number"
              value={endSeq}
              onChange={(e) => setEndSeq(Number(e.target.value))}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Maker Fee (ppm)</span>
            <input
              type="text"
              placeholder="default"
              value={makerFeePpm}
              onChange={(e) => setMakerFeePpm(e.target.value)}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Taker Fee (ppm)</span>
            <input
              type="text"
              placeholder="default"
              value={takerFeePpm}
              onChange={(e) => setTakerFeePpm(e.target.value)}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Max Order Cap</span>
            <input
              type="text"
              placeholder="unlimited"
              value={maxOrderBtc}
              onChange={(e) => setMaxOrderBtc(e.target.value)}
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-2 text-white tabular-mono"
            />
          </label>

          <label className="flex flex-col gap-1">
            <span className="text-[10px] text-[var(--ink-tertiary)]">Regime Override</span>
            <select
              value={regimeOverride}
              onChange={(e) =>
                setRegimeOverride(e.target.value as CounterfactualForkConfig["regimeOverride"])
              }
              className="h-7 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-1.5 text-white"
            >
              <option value="NORMAL">NORMAL</option>
              <option value="VOLATILITY_GUARD">VOLATILITY_GUARD</option>
              <option value="POST_ONLY_AUCTION">POST_ONLY_AUCTION</option>
            </select>
          </label>

          <div className="flex flex-col justify-end gap-1">
            <label className="inline-flex cursor-pointer items-center gap-1.5 text-[10px] text-[var(--ink-secondary)]">
              <input
                type="checkbox"
                checked={blockSyndicate}
                onChange={(e) => setBlockSyndicate(e.target.checked)}
              />
              <span>Block Ring 9001..9004</span>
            </label>
            <button
              type="button"
              data-testid="execute-fork-btn"
              onClick={() => triggerFork()}
              className="cmd-btn inline-flex h-7 items-center justify-center gap-1 rounded-[2px] bg-[#f59e0b] px-2.5 text-[11px] font-bold text-[#06080c] hover:bg-[#fbbf24]"
            >
              <PlayCircle className="h-3.5 w-3.5" />
              Run Fork Sim
            </button>
          </div>
        </div>
      </section>

      {/* Middle Split-Screen Baseline vs Counterfactual Trajectory Canvases */}
      <div className="grid min-h-[220px] flex-1 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-2">
        <div className="isolated-pane flex flex-col bg-[var(--obsidian-panel)]">
          <div className="flex items-center justify-between border-b border-[var(--wire-hairline)] px-3 py-1 text-[11px] tabular-mono">
            <span className="font-semibold text-[#38bdf8]">LEFT // BASELINE TIMELINE</span>
            <span className="text-[var(--ink-tertiary)]">
              ROOT {diff?.baselineStateRootHex.slice(0, 16) ?? "—"}…
            </span>
          </div>
          <div className="min-h-0 flex-1">
            <canvas
              ref={baselineCanvasRef}
              data-testid="chronos-baseline-canvas"
              onPointerMove={handleSyncMove}
              onPointerLeave={() => {
                syncHoverRatioRef.current.active = false;
              }}
              className="block h-full w-full cursor-crosshair"
            />
          </div>
        </div>

        <div className="isolated-pane flex flex-col bg-[var(--obsidian-panel)]">
          <div className="flex items-center justify-between border-b border-[var(--wire-hairline)] px-3 py-1 text-[11px] tabular-mono">
            <span className="font-semibold text-[#fbbf24]">RIGHT // FORKED COUNTERFACTUAL</span>
            <span className="text-[var(--ink-tertiary)]">
              ROOT {diff?.forkedStateRootHex.slice(0, 16) ?? "—"}…
            </span>
          </div>
          <div className="min-h-0 flex-1">
            <canvas
              ref={forkedCanvasRef}
              data-testid="chronos-forked-canvas"
              onPointerMove={handleSyncMove}
              onPointerLeave={() => {
                syncHoverRatioRef.current.active = false;
              }}
              className="block h-full w-full cursor-crosshair"
            />
          </div>
        </div>
      </div>

      {/* Bottom StateTreeDiff Quantitative Comparison */}
      {diff && (
        <section
          data-testid="chronos-state-tree-diff"
          className="isolated-pane grid shrink-0 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-12"
        >
          <div className="grid grid-cols-2 gap-2 bg-[var(--obsidian-panel)] p-3 sm:grid-cols-3 lg:col-span-7">
            <div className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2 tabular-mono">
              <div className="text-[10px] text-[var(--ink-tertiary)]">Clearing Price Δ</div>
              <div
                data-testid="diff-clearing-price"
                className={`mt-0.5 text-sm font-bold ${
                  diff.clearingPriceDeltaFp >= 0n ? "text-[#00d492]" : "text-[#ff5470]"
                }`}
              >
                {formatFp(diff.clearingPriceDeltaFp, meta.quoteDecimals, { signed: true })} USDT
              </div>
              <div className="text-[10px] text-[var(--ink-secondary)]">
                {formatFp(diff.baselineClearingPriceFp, meta.quoteDecimals)} →{" "}
                {formatFp(diff.forkedClearingPriceFp, meta.quoteDecimals)}
              </div>
            </div>

            <div className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2 tabular-mono">
              <div className="text-[10px] text-[var(--ink-tertiary)]">Prevented Wash Volume</div>
              <div
                data-testid="diff-prevented-wash"
                className="mt-0.5 text-sm font-bold text-[#38bdf8]"
              >
                {formatCompactNotionalFp(diff.preventedWashVolumeFp)} USDT
              </div>
              <div className="text-[10px] text-[var(--ink-secondary)]">
                {diff.preventedFillsCount.toString()} fills · {diff.rejectedOrdersCount.toString()} rejected
              </div>
            </div>

            <div className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2 tabular-mono">
              <div className="text-[10px] text-[var(--ink-tertiary)]">State Tree Identity</div>
              <div
                data-testid="diff-state-identity"
                className="mt-0.5 inline-flex items-center gap-1 text-xs font-bold text-white"
              >
                {diff.stateIdentical ? (
                  <span className="inline-flex items-center gap-1 text-[#00d492]">
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    BIT-IDENTICAL (0 DIFF)
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1 text-[#fbbf24]">
                    <GitBranch className="h-3.5 w-3.5" />
                    DIVERGENT ({diff.maxSlippageDeltaBps} bps slip)
                  </span>
                )}
              </div>
              <div className="text-[10px] text-[var(--ink-secondary)]">
                Replayed {diff.replayedEventsCount.toString()} WAL frames in {diff.computationLatencyUs} µs
              </div>
            </div>
          </div>

          <div
            tabIndex={0}
            role="region"
            aria-label="Counterfactual account balance deltas"
            className="data-table-region max-h-36 bg-[var(--obsidian-panel)] p-2 lg:col-span-5"
          >
            <div className="mb-1 flex items-center gap-1.5 text-[10px] font-semibold text-[var(--ink-secondary)]">
              <Scale className="h-3 w-3 text-[#f59e0b]" />
              Account Equity Counterfactual Delta (Scale 1e8)
            </div>
            <table className="text-[11px] tabular-mono">
              <thead>
                <tr className="text-[10px] text-[var(--ink-tertiary)]">
                  <th scope="col" className="py-0.5 text-left">
                    Account
                  </th>
                  <th scope="col" className="py-0.5 text-right">
                    Baseline
                  </th>
                  <th scope="col" className="py-0.5 text-right">
                    Forked Δ
                  </th>
                </tr>
              </thead>
              <tbody>
                {diff.accountDiffs.map((ad) => (
                  <tr key={`${ad.accountId.toString()}-${ad.asset}`}>
                    <th scope="row" className="py-0.5 text-left font-normal text-white">
                      {ad.accountId === 0n ? "TREASURY" : `#${ad.accountId.toString()}`}
                    </th>
                    <td className="py-0.5 text-right text-[var(--ink-secondary)]">
                      {formatCompactNotionalFp(ad.baselineTotalFp)}
                    </td>
                    <td
                      className={`py-0.5 text-right font-semibold ${
                        ad.deltaTotalFp > 0n
                          ? "text-[#00d492]"
                          : ad.deltaTotalFp < 0n
                            ? "text-[#ff5470]"
                            : "text-[var(--ink-tertiary)]"
                      }`}
                    >
                      {ad.deltaTotalFp === 0n
                        ? "0.00"
                        : `${ad.deltaTotalFp > 0n ? "+" : ""}${formatFp(ad.deltaTotalFp, 2)}`}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  );
}
