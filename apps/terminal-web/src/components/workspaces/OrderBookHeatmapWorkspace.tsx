"use client";

import React, { useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { Activity, Eye, Layers } from "lucide-react";
import {
  formatCompactNotionalFp,
  formatFp,
  formatPpmPercent,
  projectFpRatio,
} from "@/lib/fixed-point";
import type {
  CandleTimeframe,
  MainToWorkerCommand,
  WorkerFramePacket,
} from "@/lib/types";
import { INSTRUMENT_META } from "@/lib/types";

interface OrderBookHeatmapWorkspaceProps {
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  uiFrame: WorkerFramePacket | null;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

const TIMEFRAMES: CandleTimeframe[] = ["1s", "5s", "1m", "5m", "15m", "1h"];

const LUT_SLOTS = 201; // -100..+100 quantized intensity
const HEATMAP_LUT_RGBA = new Uint8ClampedArray(LUT_SLOTS * 4);
for (let idx = 0; idx < LUT_SLOTS; idx++) {
  const signed = (idx - 100) * 10; // -1000..+1000
  const abs = Math.abs(signed);
  const base = idx * 4;
  if (abs < 40) {
    HEATMAP_LUT_RGBA[base] = 0;
    HEATMAP_LUT_RGBA[base + 1] = 0;
    HEATMAP_LUT_RGBA[base + 2] = 0;
    HEATMAP_LUT_RGBA[base + 3] = 0;
  } else {
    const norm = Math.min(1, abs / 1000);
    if (signed > 0) {
      // Resting Bid Liquidity: Deep Cobalt -> Cyan -> Phosphor Emerald
      HEATMAP_LUT_RGBA[base] = Math.round(6 + norm * 14);
      HEATMAP_LUT_RGBA[base + 1] = Math.round(45 + norm * 167);
      HEATMAP_LUT_RGBA[base + 2] = Math.round(92 + norm * 68);
      HEATMAP_LUT_RGBA[base + 3] = Math.round((0.1 + norm * 0.44) * 255);
    } else {
      // Resting Ask / Spoofing Liquidity: Deep Plum -> Crimson Signal
      HEATMAP_LUT_RGBA[base] = Math.round(95 + norm * 160);
      HEATMAP_LUT_RGBA[base + 1] = Math.round(16 + norm * 43);
      HEATMAP_LUT_RGBA[base + 2] = Math.round(52 + norm * 48);
      HEATMAP_LUT_RGBA[base + 3] = Math.round((0.1 + norm * 0.46) * 255);
    }
  }
}

export function OrderBookHeatmapWorkspace({
  frameRef,
  uiFrame,
  sendCommand,
}: OrderBookHeatmapWorkspaceProps) {
  const marketCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const depthCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const marketCursorRef = useRef<{ x: number; y: number; active: boolean }>({
    x: 0,
    y: 0,
    active: false,
  });
  const depthCursorRef = useRef<{ x: number; y: number; active: boolean }>({
    x: 0,
    y: 0,
    active: false,
  });

  const [showHeatmap, setShowHeatmap] = useState(true);
  const [showVwap, setShowVwap] = useState(true);

  const showHeatmapRef = useRef(showHeatmap);
  const showVwapRef = useRef(showVwap);
  showHeatmapRef.current = showHeatmap;
  showVwapRef.current = showVwap;

  // 60 FPS Imperative Canvas Loop (Zero React VDOM Re-renders!)
  useEffect(() => {
    let rafId = 0;
    let canvasPaused = false;

    const marketEl = marketCanvasRef.current;
    const depthEl = depthCanvasRef.current;
    const onCvStateChange = (ev: Event) => {
      const skipped = (ev as Event & { skipped?: boolean }).skipped;
      canvasPaused = Boolean(skipped);
    };
    marketEl?.addEventListener("contentvisibilityautostatechange", onCvStateChange);

    // Cache canvas dimensions via ResizeObserver (zero per-frame getBoundingClientRect layout reads)
    let marketW = marketEl ? Math.max(320, Math.floor(marketEl.getBoundingClientRect().width)) : 960;
    let marketH = marketEl ? Math.max(240, Math.floor(marketEl.getBoundingClientRect().height)) : 560;
    let depthW = depthEl ? Math.max(220, Math.floor(depthEl.getBoundingClientRect().width)) : 420;
    let depthH = depthEl ? Math.max(150, Math.floor(depthEl.getBoundingClientRect().height)) : 160;
    let forceRedraw = true;

    let resizeObserver: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver((entries) => {
        for (const entry of entries) {
          if (entry.target === marketEl) {
            marketW = Math.max(320, Math.floor(entry.contentRect.width));
            marketH = Math.max(240, Math.floor(entry.contentRect.height));
            forceRedraw = true;
          } else if (entry.target === depthEl) {
            depthW = Math.max(220, Math.floor(entry.contentRect.width));
            depthH = Math.max(150, Math.floor(entry.contentRect.height));
            forceRedraw = true;
          }
        }
      });
      if (marketEl) resizeObserver.observe(marketEl);
      if (depthEl) resizeObserver.observe(depthEl);
    }

    // Offscreen 64x96 RGBA bitmap buffer for single-blit liquidity heatmap rendering
    const HM_ROWS = 96;
    const offscreenHeatmap = document.createElement("canvas");
    offscreenHeatmap.width = 64;
    offscreenHeatmap.height = HM_ROWS;
    const offscreenCtx = offscreenHeatmap.getContext("2d");
    let heatmapImgData = offscreenCtx ? offscreenCtx.createImageData(64, HM_ROWS) : null;
    let cachedHeatmapRef: WorkerFramePacket["heatmap"] | null = null;
    let cachedMinPriceFp = 0n;
    let cachedMaxPriceFp = 0n;

    let lastDrawnPkt: WorkerFramePacket | null = null;
    let lastDrawnCursorKey = "";
    let lastDrawnDepthCursorKey = "";
    let lastShowHeatmap = showHeatmapRef.current;
    let lastShowVwap = showVwapRef.current;

    let fallbackObserver: IntersectionObserver | null = null;
    if (marketEl && !("contentVisibility" in document.documentElement.style)) {
      fallbackObserver = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            canvasPaused = !entry.isIntersecting;
          }
        },
        { rootMargin: "150px" }
      );
      fallbackObserver.observe(marketEl);
    }

    const renderFrame = () => {
      const pkt = frameRef.current;
      if (!canvasPaused && pkt) {
        const mc = marketCursorRef.current;
        const dc = depthCursorRef.current;
        const mKey = mc.active ? `${mc.x | 0}:${mc.y | 0}` : "off";
        const dKey = dc.active ? `${dc.x | 0}:${dc.y | 0}` : "off";
        const layersChanged =
          lastShowHeatmap !== showHeatmapRef.current || lastShowVwap !== showVwapRef.current;

        if (forceRedraw || pkt !== lastDrawnPkt || mKey !== lastDrawnCursorKey || layersChanged) {
          drawMarketAndHeatmapCanvas(pkt);
          lastDrawnCursorKey = mKey;
          lastShowHeatmap = showHeatmapRef.current;
          lastShowVwap = showVwapRef.current;
        }
        if (forceRedraw || pkt !== lastDrawnPkt || dKey !== lastDrawnDepthCursorKey) {
          drawDepthCanvas(pkt);
          lastDrawnDepthCursorKey = dKey;
        }
        lastDrawnPkt = pkt;
        forceRedraw = false;
      }
      rafId = requestAnimationFrame(renderFrame);
    };

    const drawMarketAndHeatmapCanvas = (pkt: WorkerFramePacket) => {
      const canvas = marketCanvasRef.current;
      if (!canvas) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const width = marketW;
      const height = marketH;
      if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
        canvas.width = width * dpr;
        canvas.height = height * dpr;
      }

      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      ctx.save();
      ctx.scale(dpr, dpr);

      // Clear background
      ctx.fillStyle = "#06080c";
      ctx.fillRect(0, 0, width, height);

      const meta = INSTRUMENT_META[pkt.symbol];
      const rightAxisW = 84;
      const bottomAxisH = 22;
      const plotW = width - rightAxisW;
      const mainH = Math.floor((height - bottomAxisH) * 0.76);
      const ofiTop = mainH + 6;
      const ofiH = height - bottomAxisH - ofiTop;

      const candles = pkt.candles.slice(-64);
      const heatmap = pkt.heatmap.slice(-64);

      // Compute price bounds across visible candles + top L2 levels
      let minPriceFp = pkt.midPriceFp - meta.tickFp * 80n;
      let maxPriceFp = pkt.midPriceFp + meta.tickFp * 80n;
      if (candles.length > 0) {
        minPriceFp = candles[0]!.lowFp;
        maxPriceFp = candles[0]!.highFp;
        for (const c of candles) {
          if (c.lowFp < minPriceFp) minPriceFp = c.lowFp;
          if (c.highFp > maxPriceFp) maxPriceFp = c.highFp;
        }
        const pad = ((maxPriceFp - minPriceFp) * 14n) / 100n + meta.tickFp * 12n;
        minPriceFp -= pad;
        maxPriceFp += pad;
      }
      if (maxPriceFp <= minPriceFp) {
        maxPriceFp = minPriceFp + meta.tickFp * 100n;
      }

      const priceSpanNum = Math.max(1, Number(maxPriceFp - minPriceFp));
      const priceToY = (pFp: bigint): number => {
        const ratio = projectFpRatio(pFp, minPriceFp, maxPriceFp);
        return mainH - 6 - ratio * (mainH - 12);
      };

      // 1. Single-Blit Offscreen RGBA Bitmap Liquidity Heatmap (0 per-frame string allocs)
      if (showHeatmapRef.current && heatmap.length > 0 && offscreenCtx) {
        const cols = heatmap.length;
        if (offscreenHeatmap.width !== cols) {
          offscreenHeatmap.width = cols;
          heatmapImgData = offscreenCtx.createImageData(cols, HM_ROWS);
          cachedHeatmapRef = null;
        }
        if (
          heatmapImgData &&
          (cachedHeatmapRef !== pkt.heatmap ||
            cachedMinPriceFp !== minPriceFp ||
            cachedMaxPriceFp !== maxPriceFp)
        ) {
          const data = heatmapImgData.data;
          data.fill(0);
          for (let col = 0; col < cols; col++) {
            const slice = heatmap[col]!;
            const bucketCount = slice.bucketIntensities.length;
            const baseDelta = Number(slice.minBucketPriceFp - minPriceFp);
            const stepNum = Number(slice.bucketStepFp);
            for (let b = 0; b < bucketCount; b++) {
              const intensity = slice.bucketIntensities[b] ?? 0;
              if (intensity > -40 && intensity < 40) continue;
              const lowRatio = (baseDelta + b * stepNum) / priceSpanNum;
              const highRatio = (baseDelta + (b + 1) * stepNum) / priceSpanNum;
              if (highRatio < 0 || lowRatio > 1) continue;
              const rowTop = Math.max(0, Math.floor((1 - highRatio) * (HM_ROWS - 1)));
              const rowBot = Math.min(HM_ROWS - 1, Math.ceil((1 - lowRatio) * (HM_ROWS - 1)));
              const clamped = Math.max(-1000, Math.min(1000, intensity));
              const lutBase = (Math.round(clamped / 10) + 100) * 4;
              const r = HEATMAP_LUT_RGBA[lutBase]!;
              const g = HEATMAP_LUT_RGBA[lutBase + 1]!;
              const bl = HEATMAP_LUT_RGBA[lutBase + 2]!;
              const a = HEATMAP_LUT_RGBA[lutBase + 3]!;
              for (let row = rowTop; row <= rowBot; row++) {
                const pIdx = (row * cols + col) * 4;
                data[pIdx] = r;
                data[pIdx + 1] = g;
                data[pIdx + 2] = bl;
                data[pIdx + 3] = a;
              }
            }
          }
          offscreenCtx.putImageData(heatmapImgData, 0, 0);
          cachedHeatmapRef = pkt.heatmap;
          cachedMinPriceFp = minPriceFp;
          cachedMaxPriceFp = maxPriceFp;
        }
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(offscreenHeatmap, 0, 6, plotW, Math.max(1, mainH - 12));
        ctx.imageSmoothingEnabled = true;
      }

      // 2. Precision Hairline Price & Time Grid
      ctx.strokeStyle = "rgba(226, 232, 240, 0.06)";
      ctx.lineWidth = 1;
      ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
      ctx.fillStyle = "#64748b";

      const hGridSteps = 6;
      for (let g = 0; g <= hGridSteps; g++) {
        const y = Math.floor((mainH / hGridSteps) * g) + 0.5;
        ctx.beginPath();
        ctx.moveTo(0, y);
        ctx.lineTo(plotW, y);
        ctx.stroke();

        const ratio = 1 - g / hGridSteps;
        const gridPriceFp =
          minPriceFp + ((maxPriceFp - minPriceFp) * BigInt(Math.round(ratio * 10000))) / 10000n;
        const labelY = g === 0 ? y + 10 : g === hGridSteps ? y - 3 : y + 3;
        ctx.fillText(formatFp(gridPriceFp, meta.quoteDecimals), plotW + 6, labelY);
      }

      const vGridSteps = 6;
      for (let v = 1; v < vGridSteps; v++) {
        const x = Math.floor((plotW / vGridSteps) * v) + 0.5;
        ctx.beginPath();
        ctx.moveTo(x, 0);
        ctx.lineTo(x, height - bottomAxisH);
        ctx.stroke();
      }

      // 3. Multi-Timeframe Candlesticks + VWAP / TWAP Overlays
      if (candles.length > 0) {
        const slotW = plotW / candles.length;
        const bodyW = Math.max(3, Math.min(13, slotW * 0.66));

        for (let i = 0; i < candles.length; i++) {
          const c = candles[i]!;
          const cx = (i + 0.5) * slotW;
          const openY = priceToY(c.openFp);
          const closeY = priceToY(c.closeFp);
          const highY = priceToY(c.highFp);
          const lowY = priceToY(c.lowFp);
          const isBull = c.closeFp >= c.openFp;
          const color = isBull ? "#00d492" : "#ff3b5c";

          // Wick
          ctx.strokeStyle = color;
          ctx.lineWidth = 1.1;
          ctx.beginPath();
          ctx.moveTo(Math.floor(cx) + 0.5, highY);
          ctx.lineTo(Math.floor(cx) + 0.5, lowY);
          ctx.stroke();

          // Body
          const topY = Math.min(openY, closeY);
          const boxH = Math.max(1.8, Math.abs(closeY - openY));
          ctx.fillStyle = isBull ? "rgba(0, 212, 146, 0.85)" : "rgba(255, 59, 92, 0.85)";
          ctx.fillRect(cx - bodyW / 2, topY, bodyW, boxH);
        }

        if (showVwapRef.current) {
          // Rolling VWAP line (#38bdf8)
          ctx.beginPath();
          for (let i = 0; i < candles.length; i++) {
            const cx = (i + 0.5) * slotW;
            const vy = priceToY(candles[i]!.vwapFp);
            if (i === 0) ctx.moveTo(cx, vy);
            else ctx.lineTo(cx, vy);
          }
          ctx.strokeStyle = "#38bdf8";
          ctx.lineWidth = 1.6;
          ctx.stroke();

          // Rolling TWAP dashed line (#f59e0b)
          ctx.save();
          ctx.setLineDash([4, 3]);
          ctx.beginPath();
          for (let i = 0; i < candles.length; i++) {
            const cx = (i + 0.5) * slotW;
            const ty = priceToY(candles[i]!.twapFp);
            if (i === 0) ctx.moveTo(cx, ty);
            else ctx.lineTo(cx, ty);
          }
          ctx.strokeStyle = "rgba(245, 158, 11, 0.82)";
          ctx.lineWidth = 1.15;
          ctx.stroke();
          ctx.restore();
        }
      }

      // Live Mid-Price Laser Line & Right Tag
      const midY = priceToY(pkt.midPriceFp);
      ctx.save();
      ctx.setLineDash([2, 2]);
      ctx.strokeStyle = "rgba(56, 189, 248, 0.65)";
      ctx.beginPath();
      ctx.moveTo(0, midY);
      ctx.lineTo(plotW, midY);
      ctx.stroke();
      ctx.restore();

      ctx.fillStyle = "#0ea5e9";
      ctx.fillRect(plotW + 2, midY - 8, rightAxisW - 4, 16);
      ctx.fillStyle = "#040609";
      ctx.font = "bold 10px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillText(formatFp(pkt.midPriceFp, meta.quoteDecimals), plotW + 6, midY + 3.5);

      // 4. Lower Synchronized OFI (Order Flow Imbalance) Sub-Pane
      ctx.strokeStyle = "rgba(226, 232, 240, 0.14)";
      ctx.beginPath();
      ctx.moveTo(0, ofiTop - 2.5);
      ctx.lineTo(width, ofiTop - 2.5);
      ctx.stroke();

      const ofiZeroY = ofiTop + ofiH / 2;
      ctx.strokeStyle = "rgba(148, 163, 184, 0.22)";
      ctx.beginPath();
      ctx.moveTo(0, ofiZeroY);
      ctx.lineTo(plotW, ofiZeroY);
      ctx.stroke();

      if (candles.length > 0) {
        let maxAbsOfi = 1n;
        for (const c of candles) {
          const abs = c.ofiFp < 0n ? -c.ofiFp : c.ofiFp;
          if (abs > maxAbsOfi) maxAbsOfi = abs;
        }
        const slotW = plotW / candles.length;
        const barW = Math.max(2, slotW * 0.62);
        for (let i = 0; i < candles.length; i++) {
          const ofi = candles[i]!.ofiFp;
          const norm = Number((ofi * 1000n) / maxAbsOfi) / 1000;
          const bh = Math.abs(norm) * (ofiH * 0.44);
          const x = (i + 0.5) * slotW - barW / 2;
          ctx.fillStyle = ofi >= 0n ? "rgba(0, 212, 146, 0.72)" : "rgba(255, 59, 92, 0.72)";
          ctx.fillRect(x, ofi >= 0n ? ofiZeroY - bh : ofiZeroY, barW, Math.max(1, bh));
        }
      }

      ctx.fillStyle = "#94a3b8";
      ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillText(
        `OFI ${formatFp(pkt.ofiFp, 2, { signed: true })} | KYLE-λ ${formatFp(pkt.kylesLambdaFp, 4)} | IMB5 ${formatPpmPercent(pkt.imbalanceTop5Ppm, 1)}`,
        8,
        ofiTop + 11
      );

      // Right axis separator
      ctx.strokeStyle = "rgba(226, 232, 240, 0.12)";
      ctx.beginPath();
      ctx.moveTo(plotW + 0.5, 0);
      ctx.lineTo(plotW + 0.5, height);
      ctx.stroke();

      // 5. Sub-Pixel Interactive Crosshair & Quantitative Callout
      const cur = marketCursorRef.current;
      if (cur.active && cur.x >= 0 && cur.x <= plotW && cur.y >= 0 && cur.y <= height - bottomAxisH) {
        ctx.save();
        ctx.strokeStyle = "rgba(241, 245, 249, 0.55)";
        ctx.lineWidth = 0.9;
        ctx.setLineDash([3, 3]);

        ctx.beginPath();
        ctx.moveTo(cur.x, 0);
        ctx.lineTo(cur.x, height - bottomAxisH);
        ctx.moveTo(0, cur.y);
        ctx.lineTo(plotW, cur.y);
        ctx.stroke();
        ctx.restore();

        if (cur.y <= mainH) {
          const yNorm = Math.max(0, Math.min(1, (mainH - 6 - cur.y) / Math.max(1, mainH - 12)));
          const hoverPriceFp =
            minPriceFp + ((maxPriceFp - minPriceFp) * BigInt(Math.round(yNorm * 100_000))) / 100_000n;
          ctx.fillStyle = "#f1f5f9";
          ctx.fillRect(plotW + 2, cur.y - 8, rightAxisW - 4, 16);
          ctx.fillStyle = "#06080c";
          ctx.font = "bold 10px ui-monospace, SFMono-Regular, Menlo, monospace";
          ctx.fillText(formatFp(hoverPriceFp, meta.quoteDecimals), plotW + 6, cur.y + 3.5);
        }

        if (candles.length > 0) {
          const idx = Math.min(
            candles.length - 1,
            Math.max(0, Math.floor((cur.x / plotW) * candles.length))
          );
          const hc = candles[idx]!;
          const hudText = `O ${formatFp(hc.openFp, meta.quoteDecimals)}  H ${formatFp(hc.highFp, meta.quoteDecimals)}  L ${formatFp(hc.lowFp, meta.quoteDecimals)}  C ${formatFp(hc.closeFp, meta.quoteDecimals)}  VWAP ${formatFp(hc.vwapFp, meta.quoteDecimals)}  VOL ${formatFp(hc.volumeFp, meta.baseDecimals)}`;
          ctx.fillStyle = "rgba(6, 8, 12, 0.88)";
          ctx.fillRect(8, 8, Math.min(plotW - 16, hudText.length * 6.2 + 16), 20);
          ctx.strokeStyle = "rgba(56, 189, 248, 0.35)";
          ctx.strokeRect(8, 8, Math.min(plotW - 16, hudText.length * 6.2 + 16), 20);
          ctx.fillStyle = "#e2e8f0";
          ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
          ctx.fillText(hudText, 14, 21);
        }
      }

      // Bottom timestamp axis
      ctx.fillStyle = "#64748b";
      ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillText(`SEQ #${pkt.engineSeq.toString()}`, 8, height - 6);
      ctx.fillText(
        `BLAKE3 ${pkt.pipeline.stateRootBlake3Hex.slice(0, 16)}…`,
        Math.max(120, plotW - 175),
        height - 6
      );

      ctx.restore();
    };

    const drawDepthCanvas = (pkt: WorkerFramePacket) => {
      const canvas = depthCanvasRef.current;
      if (!canvas) return;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const width = depthW;
      const height = depthH;
      if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
        canvas.width = width * dpr;
        canvas.height = height * dpr;
      }
      const ctx = canvas.getContext("2d");
      if (!ctx) return;

      ctx.save();
      ctx.scale(dpr, dpr);
      ctx.fillStyle = "#070a10";
      ctx.fillRect(0, 0, width, height);

      const bids = pkt.bids.slice(0, 20);
      const asks = pkt.asks.slice(0, 20);
      if (bids.length === 0 || asks.length === 0) {
        ctx.restore();
        return;
      }

      const maxCumQty =
        (bids[bids.length - 1]?.cumulativeQtyFp ?? 1n) > (asks[asks.length - 1]?.cumulativeQtyFp ?? 1n)
          ? bids[bids.length - 1]!.cumulativeQtyFp
          : asks[asks.length - 1]!.cumulativeQtyFp;
      const safeMax = maxCumQty > 0n ? maxCumQty : 1n;

      const midX = Math.floor(width / 2) + 0.5;
      const plotH = height - 20;

      // Bid stepped wall (midX leftward)
      ctx.beginPath();
      ctx.moveTo(midX - 2, plotH);
      for (let i = 0; i < bids.length; i++) {
        const x = midX - 2 - ((i + 1) / bids.length) * (midX - 10);
        const yRatio = Number((bids[i]!.cumulativeQtyFp * 10_000n) / safeMax) / 10_000;
        const y = plotH - yRatio * (plotH - 18);
        ctx.lineTo(x, y);
      }
      ctx.strokeStyle = "#00d492";
      ctx.lineWidth = 1.6;
      ctx.stroke();
      ctx.lineTo(8, plotH);
      ctx.closePath();
      const bidGrad = ctx.createLinearGradient(0, 12, 0, plotH);
      bidGrad.addColorStop(0, "rgba(0, 212, 146, 0.34)");
      bidGrad.addColorStop(1, "rgba(0, 212, 146, 0.03)");
      ctx.fillStyle = bidGrad;
      ctx.fill();

      // Ask stepped wall (midX rightward)
      ctx.beginPath();
      ctx.moveTo(midX + 2, plotH);
      for (let i = 0; i < asks.length; i++) {
        const x = midX + 2 + ((i + 1) / asks.length) * (width - midX - 10);
        const yRatio = Number((asks[i]!.cumulativeQtyFp * 10_000n) / safeMax) / 10_000;
        const y = plotH - yRatio * (plotH - 18);
        ctx.lineTo(x, y);
      }
      ctx.strokeStyle = "#ff3b5c";
      ctx.lineWidth = 1.6;
      ctx.stroke();
      ctx.lineTo(width - 8, plotH);
      ctx.closePath();
      const askGrad = ctx.createLinearGradient(0, 12, 0, plotH);
      askGrad.addColorStop(0, "rgba(255, 59, 92, 0.34)");
      askGrad.addColorStop(1, "rgba(255, 59, 92, 0.03)");
      ctx.fillStyle = askGrad;
      ctx.fill();

      // Mid-price divider
      ctx.strokeStyle = "rgba(226, 232, 240, 0.22)";
      ctx.setLineDash([2, 2]);
      ctx.beginPath();
      ctx.moveTo(midX, 8);
      ctx.lineTo(midX, plotH);
      ctx.stroke();
      ctx.setLineDash([]);

      ctx.fillStyle = "#94a3b8";
      ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
      ctx.fillText(`SPREAD ${pkt.spreadBps} bps`, midX - 38, 14);

      const dCur = depthCursorRef.current;
      if (dCur.active && dCur.x >= 0 && dCur.x <= width) {
        ctx.strokeStyle = "rgba(241, 245, 249, 0.45)";
        ctx.beginPath();
        ctx.moveTo(dCur.x, 0);
        ctx.lineTo(dCur.x, plotH);
        ctx.stroke();

        const meta = INSTRUMENT_META[pkt.symbol];
        if (dCur.x < midX && bids.length > 0) {
          const idx = Math.min(
            bids.length - 1,
            Math.max(0, Math.floor(((midX - dCur.x) / midX) * bids.length))
          );
          const lvl = bids[idx]!;
          ctx.fillStyle = "#00d492";
          ctx.fillText(
            `BID ${formatFp(lvl.priceFp, meta.quoteDecimals)} | Σ ${formatCompactNotionalFp(lvl.cumulativeNotionalFp)}`,
            8,
            height - 5
          );
        } else if (asks.length > 0) {
          const idx = Math.min(
            asks.length - 1,
            Math.max(0, Math.floor(((dCur.x - midX) / (width - midX)) * asks.length))
          );
          const lvl = asks[idx]!;
          ctx.fillStyle = "#ff3b5c";
          ctx.fillText(
            `ASK ${formatFp(lvl.priceFp, meta.quoteDecimals)} | Σ ${formatCompactNotionalFp(lvl.cumulativeNotionalFp)}`,
            8,
            height - 5
          );
        }
      } else {
        ctx.fillStyle = "#64748b";
        ctx.fillText(
          `Σ BID ${formatCompactNotionalFp(bids[bids.length - 1]!.cumulativeNotionalFp)}`,
          8,
          height - 5
        );
        ctx.fillText(
          `Σ ASK ${formatCompactNotionalFp(asks[asks.length - 1]!.cumulativeNotionalFp)}`,
          width - 108,
          height - 5
        );
      }

      ctx.restore();
    };

    rafId = requestAnimationFrame(renderFrame);
    return () => {
      cancelAnimationFrame(rafId);
      marketEl?.removeEventListener("contentvisibilityautostatechange", onCvStateChange);
      resizeObserver?.disconnect();
      fallbackObserver?.disconnect();
    };
  }, [frameRef]);

  const symbol = uiFrame?.symbol ?? "BTC-USDT";
  const meta = INSTRUMENT_META[symbol];
  const topAsks = (uiFrame?.asks ?? []).slice(0, 10).reverse();
  const topBids = (uiFrame?.bids ?? []).slice(0, 10);
  const maxBookCum =
    (uiFrame?.bids[9]?.cumulativeQtyFp ?? 1n) > (uiFrame?.asks[9]?.cumulativeQtyFp ?? 1n)
      ? (uiFrame?.bids[9]?.cumulativeQtyFp ?? 1n)
      : (uiFrame?.asks[9]?.cumulativeQtyFp ?? 1n);

  return (
    <div
      data-testid="workspace-orderbook-heatmap"
      className="grid h-full min-h-0 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-12"
    >
      {/* Left 8 Cols: Multi-Timeframe Candlesticks + Liquidity Heatmap + Sub-Pixel Crosshair */}
      <section className="isolated-pane flex min-h-[380px] flex-col bg-[var(--obsidian-panel)] lg:col-span-8">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--wire-hairline)] px-3 py-1.5">
          <div className="flex items-center gap-2">
            <Activity className="h-3.5 w-3.5 text-[#38bdf8]" />
            <h2 className="text-xs font-semibold tracking-tight text-white">
              Microstructure Candlesticks &amp; Resting Liquidity Heatmap
            </h2>
            <span className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] px-1.5 py-0.5 text-[10px] text-[var(--ink-secondary)] tabular-mono">
              FP 1e8 · HiDPI 60FPS
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-1.5">
            <div className="flex items-center rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-0.5">
              {TIMEFRAMES.map((tf) => {
                const active = (uiFrame?.timeframe ?? "1s") === tf;
                return (
                  <button
                    key={tf}
                    type="button"
                    data-testid={`tf-btn-${tf}`}
                    onClick={() => sendCommand({ type: "SET_TIMEFRAME", timeframe: tf })}
                    className={`cmd-btn rounded-[2px] px-2 py-0.5 text-[11px] tabular-mono ${
                      active
                        ? "bg-[#38bdf8] font-semibold text-[#040609]"
                        : "text-[var(--ink-secondary)] hover:text-white"
                    }`}
                  >
                    {tf}
                  </button>
                );
              })}
            </div>

            <button
              type="button"
              data-testid="toggle-heatmap-layer"
              onClick={() => setShowHeatmap((v) => !v)}
              className={`cmd-btn inline-flex items-center gap-1 rounded-[2px] border px-2 py-0.5 text-[11px] ${
                showHeatmap
                  ? "border-[#38bdf8]/50 bg-[#38bdf8]/12 text-[#38bdf8]"
                  : "border-[var(--wire-hairline)] text-[var(--ink-tertiary)]"
              }`}
            >
              <Layers className="h-3 w-3" />
              Heatmap
            </button>

            <button
              type="button"
              data-testid="toggle-vwap-layer"
              onClick={() => setShowVwap((v) => !v)}
              className={`cmd-btn inline-flex items-center gap-1 rounded-[2px] border px-2 py-0.5 text-[11px] ${
                showVwap
                  ? "border-[#00d492]/50 bg-[#00d492]/12 text-[#00d492]"
                  : "border-[var(--wire-hairline)] text-[var(--ink-tertiary)]"
              }`}
            >
              <Eye className="h-3 w-3" />
              VWAP/TWAP
            </button>
          </div>
        </header>

        <div className="relative min-h-0 flex-1">
          <canvas
            ref={marketCanvasRef}
            data-testid="market-heatmap-canvas"
            onPointerMove={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              marketCursorRef.current = {
                x: e.clientX - r.left,
                y: e.clientY - r.top,
                active: true,
              };
            }}
            onPointerLeave={() => {
              marketCursorRef.current.active = false;
            }}
            className="block h-full w-full cursor-crosshair"
          />
        </div>

        {/* Microstructure Quantitative Strip */}
        <div className="grid grid-cols-2 gap-2 border-t border-[var(--wire-hairline)] bg-[#070a10] px-3 py-1.5 text-[11px] tabular-mono sm:grid-cols-5">
          <div>
            <span className="text-[var(--ink-tertiary)]">VWAP </span>
            <span className="font-semibold text-[#38bdf8]">
              {formatFp(uiFrame?.vwapFp ?? 0n, meta.quoteDecimals)}
            </span>
          </div>
          <div>
            <span className="text-[var(--ink-tertiary)]">TWAP </span>
            <span className="font-semibold text-[#fbbf24]">
              {formatFp(uiFrame?.twapFp ?? 0n, meta.quoteDecimals)}
            </span>
          </div>
          <div>
            <span className="text-[var(--ink-tertiary)]">IMB (5/10/20) </span>
            <span
              className={
                (uiFrame?.imbalanceTop5Ppm ?? 0) >= 0 ? "text-[#00d492]" : "text-[#ff3b5c]"
              }
            >
              {formatPpmPercent(uiFrame?.imbalanceTop5Ppm ?? 0, 0)} /{" "}
              {formatPpmPercent(uiFrame?.imbalanceTop10Ppm ?? 0, 0)}
            </span>
          </div>
          <div>
            <span className="text-[var(--ink-tertiary)]">CRC32C OK </span>
            <span className="text-white">{uiFrame?.pipeline.crc32cVerifiedCount ?? 0}</span>
          </div>
          <div className="truncate" title={uiFrame?.pipeline.lastRawHeaderHex}>
            <span className="text-[var(--ink-tertiary)]">LE HDR </span>
            <span className="text-[var(--ink-secondary)]">
              0x{uiFrame?.pipeline.lastRawHeaderHex.slice(0, 12) ?? "59480102"}
            </span>
          </div>
        </div>
      </section>

      {/* Right 4 Cols: Stepped L2 Depth Chart + Tabular Order Book Ladder + Execution Tape */}
      <section className="isolated-pane flex min-h-0 flex-col bg-[var(--obsidian-panel)] lg:col-span-4">
        <div className="border-b border-[var(--wire-hairline)] px-3 py-1.5">
          <div className="flex items-center justify-between text-xs">
            <span className="font-semibold text-white">Cumulative L2 Depth Curve</span>
            <span className="text-[11px] text-[var(--ink-secondary)] tabular-mono">
              Spread: {formatFp(uiFrame?.spreadFp ?? 0n, meta.quoteDecimals)} ({uiFrame?.spreadBps ?? 0} bps)
            </span>
          </div>
        </div>

        <div className="h-40 shrink-0 border-b border-[var(--wire-hairline)]">
          <canvas
            ref={depthCanvasRef}
            data-testid="l2-depth-canvas"
            onPointerMove={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              depthCursorRef.current = { x: e.clientX - r.left, y: e.clientY - r.top, active: true };
            }}
            onPointerLeave={() => {
              depthCursorRef.current.active = false;
            }}
            className="block h-full w-full cursor-crosshair"
          />
        </div>

        {/* Semantic Accessible L2 Order Book Ladder */}
        <div
          tabIndex={0}
          role="region"
          aria-labelledby="l2-ladder-caption"
          className="data-table-region min-h-0 flex-1 border-b border-[var(--wire-hairline)]"
        >
          <table className="text-[11px] tabular-mono">
            <caption id="l2-ladder-caption" className="sr-only">
              Level 2 fixed-point limit order book ladder for {symbol}
            </caption>
            <thead>
              <tr className="border-b border-[var(--wire-hairline)] text-[10px] text-[var(--ink-tertiary)]">
                <th scope="col" className="px-2.5 py-1 text-left font-medium">
                  Price (USDT)
                </th>
                <th scope="col" className="px-2 py-1 text-right font-medium">
                  Size
                </th>
                <th scope="col" className="px-2.5 py-1 text-right font-medium">
                  Cum. Notional
                </th>
              </tr>
            </thead>
            <tbody>
              {Array.from({ length: 10 }, (_, idx) => {
                const askOffset = 10 - topAsks.length;
                const lvl = idx >= askOffset ? topAsks[idx - askOffset] : undefined;
                if (!lvl) {
                  return (
                    <tr key={`ask-slot-${idx}`} className="h-[22px]">
                      <th scope="row" className="px-2.5 py-0.5 text-left font-medium text-[var(--ink-muted)]">
                        —
                      </th>
                      <td className="px-2 py-0.5 text-right text-[var(--ink-muted)]">—</td>
                      <td className="px-2.5 py-0.5 text-right text-[var(--ink-muted)]">—</td>
                    </tr>
                  );
                }
                const barPct = Math.min(
                  100,
                  Number((lvl.cumulativeQtyFp * 100n) / (maxBookCum > 0n ? maxBookCum : 1n))
                );
                return (
                  <tr
                    key={`ask-slot-${idx}`}
                    style={{
                      background: `linear-gradient(to left, rgba(255, 59, 92, 0.14) ${barPct}%, transparent ${barPct}%)`,
                    }}
                    className="h-[22px] hover:bg-white/[0.04]"
                  >
                    <th scope="row" className="px-2.5 py-0.5 text-left font-medium text-[#ff5470]">
                      {formatFp(lvl.priceFp, meta.quoteDecimals)}
                    </th>
                    <td className="px-2 py-0.5 text-right text-[var(--ink-primary)]">
                      {formatFp(lvl.totalQtyFp, meta.baseDecimals)}
                    </td>
                    <td className="px-2.5 py-0.5 text-right text-[var(--ink-secondary)]">
                      {formatCompactNotionalFp(lvl.cumulativeNotionalFp)}
                    </td>
                  </tr>
                );
              })}

              <tr className="h-[28px] border-y border-[var(--wire-strong)] bg-[#06080c]">
                <td colSpan={3} className="px-2.5 py-1">
                  <div className="flex items-center justify-between">
                    <span className="text-xs font-bold text-white">
                      {formatFp(uiFrame?.midPriceFp ?? meta.basePriceFp, meta.quoteDecimals)}
                    </span>
                    <span className="text-[10px] text-[#00d492]">
                      UNCROSSED: {uiFrame?.chaos.uncrossedBookHolds !== false ? "VERIFIED" : "ALERT"}
                    </span>
                  </div>
                </td>
              </tr>

              {Array.from({ length: 10 }, (_, idx) => {
                const lvl = topBids[idx];
                if (!lvl) {
                  return (
                    <tr key={`bid-slot-${idx}`} className="h-[22px]">
                      <th scope="row" className="px-2.5 py-0.5 text-left font-medium text-[var(--ink-muted)]">
                        —
                      </th>
                      <td className="px-2 py-0.5 text-right text-[var(--ink-muted)]">—</td>
                      <td className="px-2.5 py-0.5 text-right text-[var(--ink-muted)]">—</td>
                    </tr>
                  );
                }
                const barPct = Math.min(
                  100,
                  Number((lvl.cumulativeQtyFp * 100n) / (maxBookCum > 0n ? maxBookCum : 1n))
                );
                return (
                  <tr
                    key={`bid-slot-${idx}`}
                    style={{
                      background: `linear-gradient(to left, rgba(0, 212, 146, 0.14) ${barPct}%, transparent ${barPct}%)`,
                    }}
                    className="h-[22px] hover:bg-white/[0.04]"
                  >
                    <th scope="row" className="px-2.5 py-0.5 text-left font-medium text-[#00d492]">
                      {formatFp(lvl.priceFp, meta.quoteDecimals)}
                    </th>
                    <td className="px-2 py-0.5 text-right text-[var(--ink-primary)]">
                      {formatFp(lvl.totalQtyFp, meta.baseDecimals)}
                    </td>
                    <td className="px-2.5 py-0.5 text-right text-[var(--ink-secondary)]">
                      {formatCompactNotionalFp(lvl.cumulativeNotionalFp)}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {/* Live Execution Tape */}
        <div className="h-36 shrink-0 overflow-y-auto px-2.5 py-1.5">
          <div className="mb-1 flex items-center justify-between text-[10px] font-medium text-[var(--ink-tertiary)]">
            <span>Execution Print Stream</span>
            <span>Maker → Taker</span>
          </div>
          <div className="space-y-0.5 text-[11px] tabular-mono">
            {(uiFrame?.recentFills ?? []).slice(0, 8).map((f) => (
              <div
                key={f.fillId.toString()}
                className={`flex items-center justify-between rounded-[2px] px-1.5 py-0.5 ${
                  f.isWashSuspect ? "bg-[#ff3b5c]/15 ring-1 ring-[#ff3b5c]/40" : ""
                }`}
              >
                <span className={f.side === "BUY" ? "text-[#00d492]" : "text-[#ff5470]"}>
                  {formatFp(f.priceFp, meta.quoteDecimals)}
                </span>
                <span className="text-[var(--ink-primary)]">
                  {formatFp(f.qtyFp, meta.baseDecimals)}
                </span>
                <span className="text-[10px] text-[var(--ink-secondary)]">
                  #{f.makerAccountId.toString()}→#{f.takerAccountId.toString()}
                </span>
              </div>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}
