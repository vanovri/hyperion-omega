"use client";

import React, { useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { AlertTriangle, Network, ShieldAlert, Sparkles } from "lucide-react";
import { formatCompactNotionalFp, formatFp, formatPpmPercent } from "@/lib/fixed-point";
import type {
  FraudGraphNode,
  MainToWorkerCommand,
  WorkerFramePacket,
} from "@/lib/types";

interface FraudForceGraphWorkspaceProps {
  frameRef: MutableRefObject<WorkerFramePacket | null>;
  uiFrame: WorkerFramePacket | null;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

interface SimNodePos {
  x: number;
  y: number;
  vx: number;
  vy: number;
}

export function FraudForceGraphWorkspace({
  frameRef,
  uiFrame,
  sendCommand,
}: FraudForceGraphWorkspaceProps) {
  const graphCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const nodePositionsRef = useRef<Map<string, SimNodePos>>(new Map());
  const selectedAccountIdRef = useRef<bigint>(9001n);
  const hoveredAccountIdRef = useRef<bigint | null>(null);
  const draggingIdRef = useRef<string | null>(null);

  const [selectedAccountId, setSelectedAccountId] = useState<bigint>(9001n);
  selectedAccountIdRef.current = selectedAccountId;

  useEffect(() => {
    let rafId = 0;
    let phase = 0;

    const ensurePositions = (nodes: FraudGraphNode[], w: number, h: number) => {
      const map = nodePositionsRef.current;
      const cx = w * 0.48;
      const cy = h * 0.5;
      const ringRadius = Math.min(w, h) * 0.21;
      const outerRadius = Math.min(w, h) * 0.37;

      nodes.forEach((node, idx) => {
        const key = node.accountId.toString();
        if (!map.has(key)) {
          if (node.inWashCycle) {
            const ringIdx = Number(node.accountId - 9001n);
            const angle = (ringIdx / 4) * Math.PI * 2 - Math.PI / 4;
            map.set(key, {
              x: cx - w * 0.1 + Math.cos(angle) * ringRadius,
              y: cy + Math.sin(angle) * ringRadius,
              vx: 0,
              vy: 0,
            });
          } else {
            const angle = (idx / Math.max(1, nodes.length)) * Math.PI * 2;
            map.set(key, {
              x: cx + w * 0.08 + Math.cos(angle) * outerRadius,
              y: cy + Math.sin(angle) * outerRadius * 0.85,
              vx: 0,
              vy: 0,
            });
          }
        }
      });
    };

    const stepPhysics = (pkt: WorkerFramePacket, w: number, h: number) => {
      const nodes = pkt.fraudNodes;
      const edges = pkt.fraudEdges;
      ensurePositions(nodes, w, h);
      const map = nodePositionsRef.current;

      // Coulomb repulsion
      for (let i = 0; i < nodes.length; i++) {
        const a = nodes[i]!;
        const pa = map.get(a.accountId.toString());
        if (!pa) continue;
        for (let j = i + 1; j < nodes.length; j++) {
          const b = nodes[j]!;
          const pb = map.get(b.accountId.toString());
          if (!pb) continue;
          const dx = pa.x - pb.x;
          const dy = pa.y - pb.y;
          const distSq = Math.max(220, dx * dx + dy * dy);
          const dist = Math.sqrt(distSq);
          const force = 2600 / distSq;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          pa.vx += fx;
          pa.vy += fy;
          pb.vx -= fx;
          pb.vy -= fy;
        }
      }

      // Spring attraction along transaction edges
      for (const edge of edges) {
        const pm = map.get(edge.makerAccountId.toString());
        const pt = map.get(edge.takerAccountId.toString());
        if (!pm || !pt) continue;
        const dx = pt.x - pm.x;
        const dy = pt.y - pm.y;
        const dist = Math.max(1, Math.sqrt(dx * dx + dy * dy));
        const targetLen = edge.isWashCycleEdge ? Math.min(w, h) * 0.25 : Math.min(w, h) * 0.31;
        const k = edge.isWashCycleEdge ? 0.022 : 0.009;
        const f = (dist - targetLen) * k;
        const fx = (dx / dist) * f;
        const fy = (dy / dist) * f;
        pm.vx += fx;
        pm.vy += fy;
        pt.vx -= fx;
        pt.vy -= fy;
      }

      // Gentle center gravity + wash-ring cluster separation + boundary containment
      const cy = h * 0.52;
      const ringCenterX = w * 0.33;
      const ringExclusionR = Math.min(w, h) * 0.34;
      for (const node of nodes) {
        const key = node.accountId.toString();
        const p = map.get(key);
        if (!p || draggingIdRef.current === key) continue;
        const targetX = node.inWashCycle ? ringCenterX : w * 0.68;
        p.vx += (targetX - p.x) * 0.0034;
        p.vy += (cy - p.y) * 0.003;

        if (!node.inWashCycle) {
          const rdx = p.x - ringCenterX;
          const rdy = p.y - cy;
          const rDist = Math.max(1, Math.sqrt(rdx * rdx + rdy * rdy));
          if (rDist < ringExclusionR) {
            const push = (ringExclusionR - rDist) * 0.025;
            p.vx += (rdx / rDist) * push;
            p.vy += (rdy / rDist) * push;
          }
        }

        p.vx *= 0.84;
        p.vy *= 0.84;
        p.x = Math.max(46, Math.min(w - 46, p.x + p.vx));
        p.y = Math.max(54, Math.min(h - 42, p.y + p.vy));
      }
    };

    const renderGraph = () => {
      const canvas = graphCanvasRef.current;
      const pkt = frameRef.current;
      if (canvas && pkt) {
        phase += 0.014;
        const dpr = Math.min(window.devicePixelRatio || 1, 2);
        const rect = canvas.getBoundingClientRect();
        const w = Math.max(320, Math.floor(rect.width));
        const h = Math.max(260, Math.floor(rect.height));
        if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
          canvas.width = w * dpr;
          canvas.height = h * dpr;
        }

        stepPhysics(pkt, w, h);

        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.save();
          ctx.scale(dpr, dpr);

          // Machined radar grid background
          ctx.fillStyle = "#06080c";
          ctx.fillRect(0, 0, w, h);

          ctx.strokeStyle = "rgba(226, 232, 240, 0.04)";
          ctx.lineWidth = 1;
          for (let gx = 40; gx < w; gx += 48) {
            ctx.beginPath();
            ctx.moveTo(gx + 0.5, 0);
            ctx.lineTo(gx + 0.5, h);
            ctx.stroke();
          }
          for (let gy = 40; gy < h; gy += 48) {
            ctx.beginPath();
            ctx.moveTo(0, gy + 0.5);
            ctx.lineTo(w, gy + 0.5);
            ctx.stroke();
          }

          const map = nodePositionsRef.current;

          // Highlight Crimson Wash-Trading Ring Enclosure
          const cycleNodes = pkt.fraudNodes.filter((n) => n.inWashCycle);
          if (cycleNodes.length >= 3) {
            let rcx = 0;
            let rcy = 0;
            for (const cn of cycleNodes) {
              const p = map.get(cn.accountId.toString());
              if (p) {
                rcx += p.x;
                rcy += p.y;
              }
            }
            rcx /= cycleNodes.length;
            rcy /= cycleNodes.length;

            const pulseR = Math.min(w, h) * 0.24 + Math.sin(phase * 3) * 5;
            const halo = ctx.createRadialGradient(rcx, rcy, 12, rcx, rcy, pulseR);
            halo.addColorStop(0, "rgba(255, 59, 92, 0.18)");
            halo.addColorStop(0.7, "rgba(255, 59, 92, 0.07)");
            halo.addColorStop(1, "rgba(255, 59, 92, 0)");
            ctx.fillStyle = halo;
            ctx.beginPath();
            ctx.arc(rcx, rcy, pulseR, 0, Math.PI * 2);
            ctx.fill();

            // Fixed top-left telemetry HUD badge (prevents overlap with any moving node)
            const badgeText = "WASH CYCLE RING k=4 · 99.4% SYMMETRY";
            ctx.font = "bold 10px ui-monospace, SFMono-Regular, Menlo, monospace";
            const badgeW = Math.ceil(ctx.measureText(badgeText).width) + 24;
            ctx.fillStyle = "rgba(6, 8, 12, 0.92)";
            ctx.strokeStyle = "rgba(255, 59, 92, 0.55)";
            ctx.lineWidth = 1;
            ctx.fillRect(12, 12, badgeW, 22);
            ctx.strokeRect(12.5, 12.5, badgeW - 1, 21);

            ctx.fillStyle = "#ff3b5c";
            ctx.beginPath();
            ctx.arc(22, 23, 3, 0, Math.PI * 2);
            ctx.fill();

            ctx.fillStyle = "#ff8fa3";
            ctx.textAlign = "left";
            ctx.fillText(badgeText, 30, 26);
          }

          // Draw Directed Edges + Animated Value-Flow Particles
          for (let eIdx = 0; eIdx < pkt.fraudEdges.length; eIdx++) {
            const edge = pkt.fraudEdges[eIdx]!;
            const pm = map.get(edge.makerAccountId.toString());
            const pt = map.get(edge.takerAccountId.toString());
            if (!pm || !pt) continue;

            const isWash = edge.isWashCycleEdge;
            ctx.strokeStyle = isWash
              ? "rgba(255, 59, 92, 0.78)"
              : "rgba(148, 163, 184, 0.22)";
            ctx.lineWidth = isWash ? 2.2 : 1.1;

            ctx.beginPath();
            ctx.moveTo(pm.x, pm.y);
            ctx.lineTo(pt.x, pt.y);
            ctx.stroke();

            // Directional arrowhead near target node
            const angle = Math.atan2(pt.y - pm.y, pt.x - pm.x);
            const arrowDist = 20;
            const ax = pt.x - Math.cos(angle) * arrowDist;
            const ay = pt.y - Math.sin(angle) * arrowDist;
            ctx.fillStyle = isWash ? "#ff3b5c" : "rgba(148, 163, 184, 0.45)";
            ctx.beginPath();
            ctx.moveTo(ax, ay);
            ctx.lineTo(ax - Math.cos(angle - 0.45) * 7, ay - Math.sin(angle - 0.45) * 7);
            ctx.lineTo(ax - Math.cos(angle + 0.45) * 7, ay - Math.sin(angle + 0.45) * 7);
            ctx.closePath();
            ctx.fill();

            // Animated value-flow particles
            const particleCount = isWash ? 3 : 1;
            const speed = isWash ? 1.8 : 0.75;
            for (let pIdx = 0; pIdx < particleCount; pIdx++) {
              const t = (phase * speed + pIdx / particleCount + eIdx * 0.17) % 1;
              const px = pm.x + (pt.x - pm.x) * t;
              const py = pm.y + (pt.y - pm.y) * t;
              ctx.beginPath();
              ctx.arc(px, py, isWash ? 3.2 : 2.1, 0, Math.PI * 2);
              ctx.fillStyle = isWash ? "#ff8fa3" : "#38bdf8";
              ctx.fill();
            }
          }

          // Draw Account Nodes with Radial Severity Gauge
          for (const node of pkt.fraudNodes) {
            const p = map.get(node.accountId.toString());
            if (!p) continue;

            const isSelected = node.accountId === selectedAccountIdRef.current;
            const isHovered = node.accountId === hoveredAccountIdRef.current;
            const radius = node.inWashCycle ? 17 : node.role === "SPOOFER_BOT" ? 16 : 13;

            if (isSelected || isHovered) {
              ctx.beginPath();
              ctx.arc(p.x, p.y, radius + 6, 0, Math.PI * 2);
              ctx.strokeStyle = isSelected ? "#38bdf8" : "rgba(241, 245, 249, 0.5)";
              ctx.lineWidth = 1.6;
              ctx.stroke();
            }

            // Node core fill
            ctx.beginPath();
            ctx.arc(p.x, p.y, radius, 0, Math.PI * 2);
            ctx.fillStyle =
              node.status === "FROZEN"
                ? "#1e1b4b"
                : node.inWashCycle
                  ? "#3b0a18"
                  : node.role === "SPOOFER_BOT"
                    ? "#3b2106"
                    : "#0f172a";
            ctx.fill();
            ctx.strokeStyle =
              node.status === "FROZEN"
                ? "#818cf8"
                : node.inWashCycle
                  ? "#ff3b5c"
                  : node.role === "SPOOFER_BOT"
                    ? "#f59e0b"
                    : "#334155";
            ctx.lineWidth = 1.5;
            ctx.stroke();

            // Anomaly severity arc (0..100)
            const scoreAngle = (node.anomalyScore / 100) * Math.PI * 2;
            ctx.beginPath();
            ctx.arc(p.x, p.y, radius + 2.5, -Math.PI / 2, -Math.PI / 2 + scoreAngle);
            ctx.strokeStyle =
              node.anomalyScore >= 80
                ? "#ff3b5c"
                : node.anomalyScore >= 50
                  ? "#f59e0b"
                  : "#00d492";
            ctx.lineWidth = 2.2;
            ctx.stroke();

            // Score inside node
            ctx.fillStyle = "#f8fafc";
            ctx.font = "bold 9px ui-monospace, SFMono-Regular, Menlo, monospace";
            ctx.textAlign = "center";
            ctx.fillText(String(node.anomalyScore), p.x, p.y + 3);

            // Account label below node
            ctx.fillStyle = isSelected ? "#38bdf8" : "#cbd5e1";
            ctx.font = "10px ui-monospace, SFMono-Regular, Menlo, monospace";
            ctx.fillText(`#${node.accountId.toString()}`, p.x, p.y + radius + 13);
            if (node.status !== "ACTIVE") {
              ctx.fillStyle = node.status === "FROZEN" ? "#a5b4fc" : "#fbbf24";
              ctx.font = "bold 9px ui-monospace, SFMono-Regular, Menlo, monospace";
              ctx.fillText(`[${node.status}]`, p.x, p.y + radius + 23);
            }
            ctx.textAlign = "left";
          }

          ctx.restore();
        }
      }
      rafId = requestAnimationFrame(renderGraph);
    };

    rafId = requestAnimationFrame(renderGraph);
    return () => cancelAnimationFrame(rafId);
  }, [frameRef]);

  const findHitNode = (clientX: number, clientY: number): FraudGraphNode | null => {
    const canvas = graphCanvasRef.current;
    const pkt = frameRef.current;
    if (!canvas || !pkt) return null;
    const rect = canvas.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    for (const node of pkt.fraudNodes) {
      const p = nodePositionsRef.current.get(node.accountId.toString());
      if (!p) continue;
      const dx = x - p.x;
      const dy = y - p.y;
      if (dx * dx + dy * dy <= 24 * 24) {
        return node;
      }
    }
    return null;
  };

  const selectedNode =
    uiFrame?.fraudNodes.find((n) => n.accountId === selectedAccountId) ??
    uiFrame?.fraudNodes[0] ??
    null;

  return (
    <div
      data-testid="workspace-fraud-force-graph"
      className="grid h-full min-h-0 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-12"
    >
      {/* Left 8 Cols: Interactive Force-Directed Temporal Transaction Graph */}
      <section className="isolated-pane flex min-h-[380px] flex-col bg-[var(--obsidian-panel)] lg:col-span-8">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-[var(--wire-hairline)] px-3 py-1.5">
          <div className="flex items-center gap-2">
            <Network className="h-3.5 w-3.5 text-[#ff3b5c]" />
            <h2 className="text-xs font-semibold text-white">
              Temporal Multigraph &amp; Circular Wash-Trading Detector (Johnson/Tarjan k≤6)
            </h2>
          </div>

          <div className="flex flex-wrap items-center gap-1.5 text-[11px] tabular-mono">
            {([9001n, 9002n, 6601n, 1001n] as const).map((id) => (
              <button
                key={id.toString()}
                type="button"
                data-testid={`select-node-${id.toString()}`}
                onClick={() => setSelectedAccountId(id)}
                className={`cmd-btn rounded-[2px] border px-2 py-0.5 ${
                  selectedAccountId === id
                    ? "border-[#38bdf8] bg-[#38bdf8]/15 text-white"
                    : "border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] text-[var(--ink-secondary)] hover:text-white"
                }`}
              >
                Inspect #{id.toString()}
              </button>
            ))}
          </div>
        </header>

        <div className="relative min-h-0 flex-1">
          <canvas
            ref={graphCanvasRef}
            data-testid="fraud-force-canvas"
            onPointerDown={(e) => {
              const hit = findHitNode(e.clientX, e.clientY);
              if (hit) {
                setSelectedAccountId(hit.accountId);
                draggingIdRef.current = hit.accountId.toString();
                e.currentTarget.setPointerCapture(e.pointerId);
              }
            }}
            onPointerMove={(e) => {
              if (draggingIdRef.current) {
                const r = e.currentTarget.getBoundingClientRect();
                const pos = nodePositionsRef.current.get(draggingIdRef.current);
                if (pos) {
                  pos.x = Math.max(30, Math.min(r.width - 30, e.clientX - r.left));
                  pos.y = Math.max(30, Math.min(r.height - 30, e.clientY - r.top));
                  pos.vx = 0;
                  pos.vy = 0;
                }
                return;
              }
              const hit = findHitNode(e.clientX, e.clientY);
              hoveredAccountIdRef.current = hit ? hit.accountId : null;
            }}
            onPointerUp={() => {
              draggingIdRef.current = null;
            }}
            className="block h-full w-full cursor-grab active:cursor-grabbing"
          />

          {/* Graph Legend */}
          <div className="pointer-events-none absolute bottom-2.5 left-3 flex flex-wrap items-center gap-3 rounded-[2px] border border-[var(--wire-hairline)] bg-[#06080c]/90 px-2.5 py-1 text-[10px] tabular-mono">
            <span className="inline-flex items-center gap-1.5 text-[#ff5470]">
              <span className="h-2 w-2 rounded-full bg-[#ff3b5c]" />
              Wash Syndicate Ring
            </span>
            <span className="inline-flex items-center gap-1.5 text-[#fbbf24]">
              <span className="h-2 w-2 rounded-full bg-[#f59e0b]" />
              Layering/Spoofer Bot
            </span>
            <span className="inline-flex items-center gap-1.5 text-[#38bdf8]">
              <span className="h-2 w-2 rounded-full bg-[#38bdf8]" />
              Value-Flow Particle (Maker→Taker)
            </span>
          </div>
        </div>
      </section>

      {/* Right 4 Cols: Sentinel-ML ONNX SHAP Attribution Dissection + Live Alert Stream */}
      <section
        data-testid="shap-inspector-panel"
        className="isolated-pane flex min-h-0 flex-col bg-[var(--obsidian-panel)] lg:col-span-4"
      >
        <header className="flex items-center justify-between border-b border-[var(--wire-hairline)] px-3 py-1.5">
          <div className="flex items-center gap-1.5">
            <Sparkles className="h-3.5 w-3.5 text-[#38bdf8]" />
            <h3 className="text-xs font-semibold text-white">
              Sentinel-ML ONNX &amp; SHAP Dissection
            </h3>
          </div>
          <span className="text-[10px] text-[#00d492] tabular-mono">p99 &lt; 2.1ms</span>
        </header>

        {selectedNode && (
          <div className="border-b border-[var(--wire-hairline)] p-3">
            <div className="flex items-start justify-between gap-2">
              <div>
                <div className="flex items-center gap-2">
                  <span
                    data-testid="shap-selected-account"
                    className="text-sm font-bold text-white tabular-mono"
                  >
                    #{selectedNode.accountId.toString()} · {selectedNode.label}
                  </span>
                  <span
                    className={`rounded-[2px] px-1.5 py-0.5 text-[10px] font-semibold tabular-mono ${
                      selectedNode.status === "FROZEN"
                        ? "bg-indigo-500/20 text-indigo-300"
                        : selectedNode.status === "THROTTLED"
                          ? "bg-[#f59e0b]/20 text-[#fbbf24]"
                          : "bg-[#00d492]/15 text-[#00d492]"
                    }`}
                  >
                    {selectedNode.status}
                  </span>
                </div>
                <p className="mt-0.5 text-[11px] text-[var(--ink-secondary)]">
                  Role: {selectedNode.role} {selectedNode.cycleId ? `· ${selectedNode.cycleId}` : ""}
                </p>
              </div>

              <div className="text-right tabular-mono">
                <div
                  className={`text-lg font-bold leading-none ${
                    selectedNode.anomalyScore >= 80
                      ? "text-[#ff3b5c]"
                      : selectedNode.anomalyScore >= 50
                        ? "text-[#fbbf24]"
                        : "text-[#00d492]"
                  }`}
                >
                  {selectedNode.anomalyScore}/100
                </div>
                <span className="text-[10px] text-[var(--ink-tertiary)]">
                  Conf {formatPpmPercent(selectedNode.confidencePpm, 1)}
                </span>
              </div>
            </div>

            {/* Key Account Microstructure Ratios */}
            <div className="mt-2.5 grid grid-cols-3 gap-1.5 rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2 text-[10px] tabular-mono">
              <div>
                <div className="text-[var(--ink-tertiary)]">OTR</div>
                <div className="font-semibold text-white">
                  {(Number(selectedNode.orderToTradeRatioPpm) / 1_000_000).toFixed(1)}x
                </div>
              </div>
              <div>
                <div className="text-[var(--ink-tertiary)]">Cancel Rate</div>
                <div className="font-semibold text-white">
                  {formatPpmPercent(selectedNode.cancelRatePpm, 1)}
                </div>
              </div>
              <div>
                <div className="text-[var(--ink-tertiary)]">Volume</div>
                <div className="font-semibold text-white">
                  {formatCompactNotionalFp(selectedNode.tradedVolumeFp)}
                </div>
              </div>
            </div>

            {/* Bipolar Horizontal SHAP Waterfall */}
            <div className="mt-3">
              <div className="mb-1.5 flex items-center justify-between text-[10px] text-[var(--ink-secondary)]">
                <span className="font-semibold text-white">Local SHAP Feature Attribution</span>
                <span className="tabular-mono">-Benign ← 0 → +Anomalous</span>
              </div>

              <div className="space-y-1.5">
                {selectedNode.shapAttributions.map((shap) => {
                  const pct = Math.min(100, Math.abs(shap.shapContributionPpm) / 5000);
                  const positive = shap.shapContributionPpm >= 0;
                  return (
                    <div key={shap.featureName} className="text-[10px] tabular-mono">
                      <div className="flex items-center justify-between">
                        <span className="truncate text-[var(--ink-primary)]">
                          {shap.importanceRank}. {shap.featureName}
                        </span>
                        <span className={positive ? "text-[#ff5470]" : "text-[#38bdf8]"}>
                          {positive ? "+" : ""}
                          {(shap.shapContributionPpm / 10_000).toFixed(2)}%
                        </span>
                      </div>
                      <div className="mt-0.5 flex h-2 w-full items-center overflow-hidden rounded-[1px] bg-[#06080c]">
                        <div className="relative h-full w-1/2 border-r border-white/20">
                          {!positive && (
                            <div
                              style={{ width: `${pct}%` }}
                              className="ml-auto h-full bg-[#38bdf8]"
                            />
                          )}
                        </div>
                        <div className="relative h-full w-1/2">
                          {positive && (
                            <div
                              style={{ width: `${pct}%` }}
                              className="h-full bg-[#ff3b5c]"
                            />
                          )}
                        </div>
                      </div>
                      <div className="mt-0.5 flex justify-between text-[9px] text-[var(--ink-tertiary)]">
                        <span>obs: {formatFp(shap.featureValueFp, 2)}</span>
                        <span>base: {formatFp(shap.baselineValueFp, 2)}</span>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>

            <button
              type="button"
              data-testid="escalate-account-aegis-btn"
              onClick={() =>
                sendCommand({
                  type: "AEGIS_ESCALATE_ACCOUNT",
                  accountId: selectedNode.accountId,
                  reason: `Manual SHAP forensic escalation (score=${selectedNode.anomalyScore}/100, role=${selectedNode.role})`,
                })
              }
              className="cmd-btn mt-3 inline-flex w-full items-center justify-center gap-1.5 rounded-[2px] border border-[#ff3b5c]/50 bg-[#ff3b5c]/15 px-3 py-1.5 text-[11px] font-semibold text-[#ff6b84] hover:bg-[#ff3b5c]/25"
            >
              <ShieldAlert className="h-3.5 w-3.5" />
              Queue Aegis Quarantine for #{selectedNode.accountId.toString()}
            </button>
          </div>
        )}

        {/* Live Anomaly Alert Stream */}
        <div className="min-h-0 flex-1 overflow-y-auto p-2.5">
          <div className="mb-1.5 flex items-center justify-between text-[10px] font-semibold text-[var(--ink-secondary)]">
            <span>Real-Time Sentinel-ML Alerts</span>
            <span className="tabular-mono">{uiFrame?.fraudAlerts.length ?? 0} active</span>
          </div>
          <div className="space-y-1.5">
            {(uiFrame?.fraudAlerts ?? []).map((alt) => (
              <button
                key={alt.alertId}
                type="button"
                onClick={() => {
                  if (alt.offenderAccountIds[0]) {
                    setSelectedAccountId(alt.offenderAccountIds[0]);
                  }
                }}
                className="cmd-btn w-full rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2 text-left hover:border-[#ff3b5c]/50"
              >
                <div className="flex items-center justify-between text-[11px] tabular-mono">
                  <span className="inline-flex items-center gap-1 font-semibold text-[#ff5470]">
                    <AlertTriangle className="h-3 w-3" />
                    {alt.patternType}
                  </span>
                  <span className="rounded-[2px] bg-[#ff3b5c]/20 px-1.5 py-0.5 text-[10px] font-bold text-[#ff6b84]">
                    SEV {alt.severityScore}
                  </span>
                </div>
                <div className="mt-1 text-[10px] text-[var(--ink-secondary)] tabular-mono">
                  Accounts: {alt.offenderAccountIds.map((x) => `#${x.toString()}`).join(", ")} ·{" "}
                  {alt.inferenceLatencyUs} µs
                </div>
              </button>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}
