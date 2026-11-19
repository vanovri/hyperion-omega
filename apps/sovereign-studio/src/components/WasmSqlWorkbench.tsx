"use client";

import React, { useEffect, useId, useMemo, useState } from "react";
import {
  executeLocalForensicSql,
  PRESET_FORENSIC_SQL_QUERIES,
} from "@/lib/sql-ast-guard";
import {
  AlphaEvolutionTelemetry,
  SymbolicAlphaNode,
  WasmStrategyModuleInfo,
} from "@/lib/telemetry-types";

export interface WasmSqlWorkbenchProps {
  readonly wasmModules: ReadonlyArray<WasmStrategyModuleInfo>;
  readonly alpha: AlphaEvolutionTelemetry;
  readonly onTriggerMalwareTrap: () => void;
  readonly onHotSwapModule: (moduleId: string) => void;
  readonly onAnnounce?: (msg: string) => void;
  readonly onDetach?: () => void;
}

function AstTreeBadge({ node, depth = 0 }: { node: SymbolicAlphaNode; depth?: number }) {
  return (
    <div className="pl-2.5 border-l border-slate-700/80 my-0.5">
      <div className="mono-num text-[11px] text-cyan-300">
        {depth > 0 ? "├─ " : ""}
        <span className="font-semibold">{node.op}</span>
        {node.weight ? <span className="text-slate-400"> (w={node.weight})</span> : null}
      </div>
      {node.children?.map((child) => (
        <AstTreeBadge key={child.id} node={child} depth={depth + 1} />
      ))}
    </div>
  );
}

export function WasmSqlWorkbench({
  wasmModules,
  alpha,
  onTriggerMalwareTrap,
  onHotSwapModule,
  onAnnounce,
  onDetach,
}: WasmSqlWorkbenchProps) {
  const [selectedModuleId, setSelectedModuleId] = useState<string>("stat_arb_zscore");
  const [sqlText, setSqlText] = useState<string>(PRESET_FORENSIC_SQL_QUERIES[0]!.sql);
  const [executedSql, setExecutedSql] = useState<string>(PRESET_FORENSIC_SQL_QUERIES[0]!.sql);
  const [hydrated, setHydrated] = useState<boolean>(false);

  useEffect(() => {
    setHydrated(true);
  }, []);

  const moduleSelectId = useId();
  const sqlInputId = useId();
  const sqlHintId = useId();
  const sqlCaptionId = useId();

  const activeModule =
    wasmModules.find((m) => m.moduleId === selectedModuleId) ?? wasmModules[0]!;

  const queryResult = useMemo(() => executeLocalForensicSql(executedSql), [executedSql]);

  const handleRunSql = () => {
    setExecutedSql(sqlText);
    const res = executeLocalForensicSql(sqlText);
    onAnnounce?.(
      res.validation.allowed
        ? `Executed forensic SQL over ${res.scannedRows.toLocaleString("en-US")} rows in ${res.latencyMs}ms.`
        : `SQL AST Guard blocked query: ${res.validation.violationCode}`
    );
  };

  // Build SVG polyline points for DSR Equity Curve
  const svgChart = useMemo(() => {
    const pts = alpha.equityCurve;
    if (pts.length === 0) return { stratPath: "", benchPath: "" };
    const allVals = pts.flatMap((p) => [p.strategyNav, p.benchmarkNav]);
    const minV = Math.min(...allVals) * 0.995;
    const maxV = Math.max(...allVals) * 1.005;
    const span = Math.max(maxV - minV, 1);
    const w = 260;
    const h = 76;

    const toCoord = (idx: number, val: number) => {
      const x = (idx / Math.max(pts.length - 1, 1)) * w;
      const y = h - ((val - minV) / span) * (h - 10) - 5;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    };

    return {
      stratPath: pts.map((p, i) => toCoord(i, p.strategyNav)).join(" "),
      benchPath: pts.map((p, i) => toCoord(i, p.benchmarkNav)).join(" "),
    };
  }, [alpha.equityCurve]);

  return (
    <section className="panel-card" aria-labelledby="wasm-sql-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="inline-block h-2 w-2 rounded-full bg-emerald-400" aria-hidden="true" />
            <h2 id="wasm-sql-heading" className="text-sm font-semibold tracking-tight text-slate-100">
              CH15, CH17 &amp; CH19 · Wasmtime Quant IDE, Symbolic Alpha &amp; DataFusion SQL Workbench
            </h2>
          </div>
          <p className="mono-num text-xs text-slate-400">
            Zero-Syscall Wasmtime Host (16MB Cap · Fuel Metering) · DSR:{" "}
            <span className="text-emerald-400 font-semibold">
              {(alpha.deflatedSharpeRatioProb * 100).toFixed(2)}%
            </span>{" "}
            (PBO {(alpha.pboOverfitProb * 100).toFixed(1)}%)
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <button
            type="button"
            data-testid="wasm-hotswap-btn"
            className="sovereign-btn"
            onClick={() => onHotSwapModule(activeModule.moduleId)}
          >
            Hot-Swap .wasm
          </button>
          <button
            type="button"
            data-testid="wasm-malware-trap-btn"
            className="sovereign-btn sovereign-btn-danger"
            onClick={() => {
              onTriggerMalwareTrap();
              setSelectedModuleId("infinite_loop_bomb");
            }}
          >
            Test Infinite-Loop Trap
          </button>
          {onDetach && (
            <button
              type="button"
              className="sovereign-btn"
              onClick={onDetach}
              aria-label="Pop out Wasm Quant IDE and SQL Workbench window"
            >
              Pop-Out
            </button>
          )}
        </div>
      </div>

      {/* Split Grid: Left = Wasm Quant Sandbox + Symbolic Alpha DSR, Right = DataFusion Lakehouse SQL */}
      <div className="grid grid-cols-1 gap-3 xl:grid-cols-12">
        {/* LEFT (5 cols): Co-Located Wasmtime Inspector + Symbolic Alpha */}
        <div className="xl:col-span-5 flex flex-col gap-2 rounded border border-slate-800/80 bg-[#060a12] p-2.5 min-w-0">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label htmlFor={moduleSelectId} className="text-xs font-semibold text-slate-200">
              Compiled Wasm Guest
            </label>
            <select
              id={moduleSelectId}
              value={activeModule.moduleId}
              onChange={(e) => setSelectedModuleId(e.target.value)}
              className="sovereign-input mono-num"
            >
              {wasmModules.map((m) => (
                <option key={m.moduleId} value={m.moduleId}>
                  {m.filename} ({m.version})
                </option>
              ))}
            </select>
          </div>

          <div className="grid grid-cols-3 gap-1.5 text-[11px]">
            <div className="rounded border border-slate-800 bg-[#080e1a] px-2 py-1">
              <div className="text-[10px] text-slate-400">Fuel / Tick</div>
              <div className="mono-num font-semibold text-cyan-300">
                {activeModule.fuelConsumedPerTick.toLocaleString("en-US")} / 50k
              </div>
            </div>
            <div className="rounded border border-slate-800 bg-[#080e1a] px-2 py-1">
              <div className="text-[10px] text-slate-400">Tick p50 / p99</div>
              <div className="mono-num font-semibold text-emerald-400">
                {activeModule.p50LatencyUs}µs / {activeModule.p99LatencyUs}µs
              </div>
            </div>
            <div className="rounded border border-slate-800 bg-[#080e1a] px-2 py-1">
              <div className="text-[10px] text-slate-400">Sandbox State</div>
              <div
                data-testid="wasm-sandbox-status"
                className={`mono-num font-semibold text-[10.5px] ${
                  activeModule.status === "TRAPPED_QUARANTINED"
                    ? "text-rose-400"
                    : "text-emerald-400"
                }`}
              >
                {activeModule.status === "TRAPPED_QUARANTINED" ? "TRAPPED" : "ACTIVE"}
              </div>
            </div>
          </div>

          {activeModule.lastTrapReason && (
            <div
              data-testid="wasm-trap-banner"
              className="rounded border border-rose-500/50 bg-rose-950/30 px-2 py-1 text-[11px] mono-num text-rose-200"
            >
              {activeModule.lastTrapReason}
            </div>
          )}

          <pre className="scroll-region mono-num m-0 max-h-[115px] rounded border border-slate-800/90 bg-[#04070d] p-2 text-[10.5px] leading-relaxed text-slate-200">
            <code>{activeModule.rustSourceSnippet}</code>
          </pre>

          {/* Symbolic Alpha AST + DSR Equity Curve */}
          <div className="border-t border-slate-800 pt-2">
            <div className="flex items-center justify-between text-[11px]">
              <span className="font-semibold text-slate-300">
                CH19 GP Alpha AST (Gen #{alpha.generation})
              </span>
              <span className="mono-num text-emerald-400">
                SR {alpha.rawSharpe} · DSR {(alpha.deflatedSharpeRatioProb * 100).toFixed(1)}%
              </span>
            </div>
            <div className="mt-1 grid grid-cols-1 gap-2 sm:grid-cols-2 items-center">
              <div className="rounded border border-slate-800/70 bg-[#050811] p-1.5">
                <AstTreeBadge node={alpha.astRoot} />
              </div>
              <svg
                role="img"
                aria-label="Deflated Sharpe Ratio Strategy versus Benchmark Equity Curve"
                viewBox="0 0 260 76"
                className="h-[76px] w-full rounded border border-slate-800/70 bg-[#050811]"
              >
                <polyline
                  fill="none"
                  stroke="#64748b"
                  strokeWidth="1.3"
                  strokeDasharray="3 2"
                  points={svgChart.benchPath}
                />
                <polyline
                  fill="none"
                  stroke="#10b981"
                  strokeWidth="2"
                  points={svgChart.stratPath}
                />
              </svg>
            </div>
          </div>
        </div>

        {/* RIGHT (7 cols): Lakehouse Arrow/DataFusion SQL Forensics Workbench */}
        <div className="xl:col-span-7 flex flex-col gap-2 rounded border border-slate-800/80 bg-[#060a12] p-2.5 min-w-0">
          <div className="flex flex-wrap items-center justify-between gap-1.5">
            <label htmlFor={sqlInputId} className="text-xs font-semibold text-slate-200">
              CH17 Arrow / DataFusion Forensics SQL
            </label>
            <div className="flex flex-wrap gap-1">
              {PRESET_FORENSIC_SQL_QUERIES.map((preset) => (
                <button
                  key={preset.id}
                  type="button"
                  data-testid={`sql-preset-${preset.id}`}
                  className="sovereign-btn text-[11px]"
                  onClick={() => {
                    setSqlText(preset.sql);
                    setExecutedSql(preset.sql);
                  }}
                >
                  {preset.label}
                </button>
              ))}
            </div>
          </div>

          <span id={sqlHintId} className="text-[11px] text-slate-400">
            AST Guard enforces read-only SELECT, blocks DDL/DML/CROSS JOIN, and supports native{" "}
            <code className="mono-num text-cyan-300">ASOF JOIN</code> &amp;{" "}
            <code className="mono-num text-cyan-300">markout_pnl()</code>.
          </span>

          <textarea
            id={sqlInputId}
            aria-describedby={sqlHintId}
            rows={5}
            value={sqlText}
            onChange={(e) => setSqlText(e.target.value)}
            className="sovereign-input mono-num scroll-region w-full text-[11.5px]"
          />

          <div className="flex flex-wrap items-center justify-between gap-2">
            <div
              data-testid="sql-ast-guard-badge"
              className={`mono-num text-[11px] font-semibold ${
                queryResult.validation.allowed ? "text-emerald-400" : "text-rose-400"
              }`}
            >
              {queryResult.validation.allowed
                ? `AST GUARD PASS · ${queryResult.scannedRows.toLocaleString("en-US")} Parquet rows (${queryResult.parquetPartitionsPruned} partitions pruned) in ${hydrated ? `${queryResult.latencyMs}ms` : "<30ms"}`
                : `AST GUARD BLOCKED [${queryResult.validation.violationCode}]: ${queryResult.validation.reason}`}
            </div>

            <button
              type="button"
              data-testid="run-forensic-sql-btn"
              className="sovereign-btn"
              onClick={handleRunSql}
            >
              Execute Arrow SQL
            </button>
          </div>

          {queryResult.validation.allowed && queryResult.rows.length > 0 && (
            <div tabIndex={0} role="region" aria-labelledby={sqlCaptionId} className="table-wrapper">
              <table className="sovereign-table mono-num">
                <caption id={sqlCaptionId} className="visually-hidden">
                  Lakehouse DataFusion Forensic Query Results
                </caption>
                <thead>
                  <tr>
                    {queryResult.columns.map((col) => (
                      <th key={col.name} scope="col">
                        {col.name}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {queryResult.rows.map((row, rIdx) => (
                    <tr key={rIdx}>
                      {queryResult.columns.map((col, cIdx) =>
                        cIdx === 0 ? (
                          <th key={col.name} scope="row">
                            {row[col.name] ?? ""}
                          </th>
                        ) : (
                          <td key={col.name} data-col-label={col.name}>
                            {row[col.name] ?? ""}
                          </td>
                        )
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
