"use client";

import React from "react";
import { EbpfShieldTelemetry, RaftClusterState } from "@/lib/telemetry-types";

export interface RaftEbpfTopologyProps {
  readonly raft: RaftClusterState;
  readonly ebpf: EbpfShieldTelemetry;
  readonly tickSeq: number;
  readonly onKillLeader: () => void;
  readonly onHealCluster: () => void;
  readonly onTogglePartition: () => void;
  readonly onToggleEbpfFlood: (active: boolean) => void;
  readonly onDetach?: () => void;
}

export function RaftEbpfTopology({
  raft,
  ebpf,
  tickSeq,
  onKillLeader,
  onHealCluster,
  onTogglePartition,
  onToggleEbpfFlood,
  onDetach,
}: RaftEbpfTopologyProps) {
  // Compute 5-node pentagon coordinates in 260x220 SVG viewport
  const cx = 130;
  const cy = 112;
  const ringR = 76;

  const nodeCoords = raft.nodes.map((node, idx) => {
    const angle = -Math.PI / 2 + (idx * 2 * Math.PI) / 5;
    return {
      node,
      x: cx + ringR * Math.cos(angle),
      y: cy + ringR * Math.sin(angle),
    };
  });

  const leaderCoord = nodeCoords.find((n) => n.node.role === "LEADER") ?? null;
  const pulsePhase = (tickSeq % 10) / 10;

  return (
    <section className="panel-card" aria-labelledby="raft-ebpf-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                raft.quorumHealthy ? "bg-emerald-400" : "bg-amber-400"
              }`}
              aria-hidden="true"
            />
            <h2 id="raft-ebpf-heading" className="text-sm font-semibold tracking-tight text-slate-100">
              CH12 &amp; CH16 · 5-Node Raft Quorum Ring &amp; eBPF XDP Packet Shield
            </h2>
          </div>
          <p className="mono-num text-xs text-slate-400">
            Term {raft.term} · Failover {raft.lastFailoverMs}ms (&lt;500ms SLA) · Committed Tx Loss:{" "}
            <span className="text-emerald-400">{raft.committedTxLoss}</span> · XDP_DROP:{" "}
            <span className={ebpf.floodAttackActive ? "text-rose-400 font-semibold" : "text-cyan-300"}>
              {ebpf.xdpDropPps.toLocaleString("en-US")} PPS
            </span>
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <button
            type="button"
            data-testid="kill-raft-leader-btn"
            className="sovereign-btn sovereign-btn-danger"
            onClick={onKillLeader}
          >
            Kill Leader
          </button>
          <button
            type="button"
            data-testid="toggle-partition-btn"
            className="sovereign-btn"
            aria-pressed={raft.partitionActive}
            onClick={onTogglePartition}
          >
            {raft.partitionActive ? "Heal Partition" : "Partition N5"}
          </button>
          <button type="button" className="sovereign-btn" onClick={onHealCluster}>
            Restore All
          </button>
          <button
            type="button"
            data-testid="toggle-ebpf-flood-btn"
            className={`sovereign-btn ${ebpf.floodAttackActive ? "sovereign-btn-danger" : ""}`}
            aria-pressed={ebpf.floodAttackActive}
            onClick={() => onToggleEbpfFlood(!ebpf.floodAttackActive)}
          >
            {ebpf.floodAttackActive ? "Stop SYN Flood" : "Inject XDP Flood"}
          </button>
          {onDetach && (
            <button
              type="button"
              className="sovereign-btn"
              onClick={onDetach}
              aria-label="Pop out Raft and eBPF Topology window"
            >
              Pop-Out
            </button>
          )}
        </div>
      </div>

      {/* Dual Topology Canvas: Left = 5-Node Raft Consensus Ring, Right = eBPF XDP_DROP Polar Radar */}
      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        {/* LEFT: 5-Node Raft Ring */}
        <div className="rounded border border-slate-800/80 bg-[#060a12] p-2.5">
          <div className="mb-1 flex items-center justify-between text-[11px] text-slate-400">
            <span className="font-semibold uppercase tracking-wider text-slate-300">
              Raft PreVote + ReadIndex Ring
            </span>
            <span className="mono-num text-emerald-400">
              AppendEntries: {raft.appendEntriesPerSec.toLocaleString("en-US")}/s
            </span>
          </div>

          <svg
            role="img"
            aria-label={`5-Node Raft consensus topology, Term ${raft.term}, Leader ${
              raft.leaderId ? `Node ${raft.leaderId}` : "Electing"
            }`}
            viewBox="0 0 260 222"
            className="mx-auto h-[210px] w-full max-w-[300px]"
          >
            {/* Quorum ring perimeter */}
            <circle
              cx={cx}
              cy={cy}
              r={ringR}
              fill="none"
              stroke="rgba(148,163,184,0.14)"
              strokeDasharray="3 3"
            />

            {/* Asymmetric Partition Barrier Wall when Node 5 is isolated */}
            {raft.partitionActive && (
              <line
                x1={42}
                y1={56}
                x2={112}
                y2={132}
                stroke="#f59e0b"
                strokeWidth={2}
                strokeDasharray="5 3"
              />
            )}

            {/* AppendEntries Heartbeat Beams + Travelling Packets from Leader to Healthy Followers */}
            {leaderCoord &&
              nodeCoords.map(({ node, x, y }) => {
                if (
                  node.nodeId === leaderCoord.node.nodeId ||
                  node.role === "DOWN" ||
                  node.role === "PARTITIONED"
                ) {
                  return null;
                }
                const px = leaderCoord.x + (x - leaderCoord.x) * pulsePhase;
                const py = leaderCoord.y + (y - leaderCoord.y) * pulsePhase;
                return (
                  <g key={`hb-${node.nodeId}`}>
                    <line
                      x1={leaderCoord.x}
                      y1={leaderCoord.y}
                      x2={x}
                      y2={y}
                      stroke="rgba(34,211,238,0.28)"
                      strokeWidth={1.3}
                    />
                    <circle cx={px} cy={py} r={3.2} fill="#22d3ee" />
                  </g>
                );
              })}

            {/* 5 Raft Nodes */}
            {nodeCoords.map(({ node, x, y }) => {
              const strokeColor =
                node.role === "LEADER"
                  ? "#10b981"
                  : node.role === "DOWN"
                    ? "#f43f5e"
                    : node.role === "PARTITIONED"
                      ? "#f59e0b"
                      : node.role === "CANDIDATE" || node.role === "PRE_CANDIDATE"
                        ? "#facc15"
                        : "#38bdf8";
              return (
                <g key={node.nodeId} transform={`translate(${x}, ${y})`}>
                  <circle
                    r={21}
                    fill="#0b1220"
                    stroke={strokeColor}
                    strokeWidth={node.role === "LEADER" ? 2.6 : 1.6}
                  />
                  <text
                    y={-4}
                    textAnchor="middle"
                    fill="#f1f5f9"
                    fontSize="9.5"
                    fontWeight="700"
                  >
                    N{node.nodeId} · {node.role.slice(0, 4)}
                  </text>
                  <text
                    y={8}
                    textAnchor="middle"
                    fill="#94a3b8"
                    fontSize="8"
                    fontFamily="monospace"
                  >
                    #{node.commitIndex % 10000}
                  </text>
                </g>
              );
            })}

            {/* Center ReadIndex Barrier Readout */}
            <text x={cx} y={cy - 4} textAnchor="middle" fill="#94a3b8" fontSize="8.5">
              ReadIndex
            </text>
            <text
              x={cx}
              y={cy + 9}
              textAnchor="middle"
              fill="#e2e8f0"
              fontSize="10"
              fontWeight="600"
              fontFamily="monospace"
            >
              {raft.readIndexBarrierSeq.toLocaleString("en-US")}
            </text>
          </svg>
        </div>

        {/* RIGHT: eBPF XDP Packet Shield Radar & Kernel RTT Histogram */}
        <div className="rounded border border-slate-800/80 bg-[#060a12] p-2.5">
          <div className="mb-1 flex items-center justify-between text-[11px] text-slate-400">
            <span className="font-semibold uppercase tracking-wider text-slate-300">
              eBPF XDP Zero-Trust Radar
            </span>
            <span className="mono-num text-amber-300">
              LPM Blocked: {ebpf.blockedCidrsCount} CIDRs
            </span>
          </div>

          <div className="grid grid-cols-1 items-center gap-2 sm:grid-cols-2">
            <svg
              role="img"
              aria-label={`eBPF XDP packet filter radar dropping ${ebpf.xdpDropPps} packets per second`}
              viewBox="0 0 180 180"
              className="mx-auto h-[165px] w-[165px]"
            >
              <circle cx={90} cy={90} r={76} fill="#040810" stroke="rgba(34,211,238,0.22)" />
              <circle cx={90} cy={90} r={52} fill="none" stroke="rgba(148,163,184,0.12)" />
              <circle cx={90} cy={90} r={26} fill="none" stroke="rgba(148,163,184,0.12)" />
              <line x1={14} y1={90} x2={166} y2={90} stroke="rgba(148,163,184,0.12)" />
              <line x1={90} y1={14} x2={90} y2={166} stroke="rgba(148,163,184,0.12)" />

              {/* Rotating radar sweep beam */}
              {(() => {
                const sweepAngle = (tickSeq * 0.16) % (Math.PI * 2);
                const sx = 90 + 76 * Math.cos(sweepAngle);
                const sy = 90 + 76 * Math.sin(sweepAngle);
                return (
                  <line
                    x1={90}
                    y1={90}
                    x2={sx}
                    y2={sy}
                    stroke={ebpf.floodAttackActive ? "#f43f5e" : "#22d3ee"}
                    strokeWidth={1.8}
                  />
                );
              })()}

              {/* Packet verdict blips */}
              {ebpf.radarBlips.map((blip, idx) => {
                const bx = 90 + blip.radiusNorm * 72 * Math.cos(blip.angleRad);
                const by = 90 + blip.radiusNorm * 72 * Math.sin(blip.angleRad);
                const fill =
                  blip.verdict === "PASS"
                    ? "#10b981"
                    : blip.verdict === "DROP_MAGIC"
                      ? "#f59e0b"
                      : "#f43f5e";
                return <circle key={idx} cx={bx} cy={by} r={blip.verdict === "PASS" ? 2.4 : 3.4} fill={fill} />;
              })}
            </svg>

            {/* Per-CPU log2 TCP RTT Histogram & Drop Counters */}
            <div className="flex flex-col gap-1.5 text-[11px]">
              <div className="flex justify-between">
                <span className="text-slate-400">XDP_PASS</span>
                <span className="mono-num font-semibold text-emerald-400">
                  {(ebpf.xdpPassPps / 1000).toFixed(1)}k/s
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-slate-400">SYN/Rate Drop</span>
                <span className="mono-num font-semibold text-rose-400">
                  {(ebpf.synFloodDropsPps / 1000).toFixed(1)}k/s
                </span>
              </div>
              <div className="flex justify-between">
                <span className="text-slate-400">Bad 0x4859 Magic</span>
                <span className="mono-num font-semibold text-amber-400">
                  {(ebpf.invalidMagicDropsPps / 1000).toFixed(1)}k/s
                </span>
              </div>

              <div className="mt-1 border-t border-slate-800 pt-1.5">
                <div className="mb-1 text-[10px] uppercase tracking-wider text-slate-400">
                  Kernel TCP RTT (log2)
                </div>
                {ebpf.tcpRttHistogramLog2Us.slice(0, 4).map((b) => {
                  const pct = Math.min(100, Math.round((b.count / 110_000) * 100));
                  return (
                    <div key={b.bucketLabel} className="mb-1 flex items-center gap-1.5">
                      <span className="mono-num w-14 text-[10px] text-slate-400">{b.bucketLabel}</span>
                      <div className="h-1.5 flex-1 rounded bg-slate-800">
                        <div
                          className="h-1.5 rounded bg-cyan-400"
                          style={{ width: `${Math.max(pct, 4)}%` }}
                        />
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* Node WAL Digest Strip */}
      <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-5">
        {raft.nodes.map((n) => (
          <div
            key={n.nodeId}
            className="rounded border border-slate-800/70 bg-[#070b14] px-2 py-1.5 text-[11px]"
          >
            <div className="flex items-center justify-between">
              <span className="font-semibold text-slate-200">N{n.nodeId}</span>
              <span
                className={`mono-num text-[10px] ${
                  n.role === "LEADER"
                    ? "text-emerald-400"
                    : n.role === "DOWN"
                      ? "text-rose-400"
                      : n.role === "PARTITIONED"
                        ? "text-amber-400"
                        : "text-cyan-300"
                }`}
              >
                {n.role}
              </span>
            </div>
            <div className="mono-num truncate text-[10px] text-slate-400">{n.walBlake3Short}</div>
          </div>
        ))}
      </div>
    </section>
  );
}
