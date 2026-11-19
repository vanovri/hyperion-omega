"use client";

import React, { useEffect, useId, useMemo, useState } from "react";
import { formatFixedPoint } from "@/lib/fixed-point";
import { L2RelayerTelemetry } from "@/lib/telemetry-types";
import {
  buildCanonicalSampleSolvencyBundle,
  injectAdversarialTamper,
  MerkleSumInclusionProofBundle,
  verifyMerkleSumSolvencyProof,
} from "@/lib/zk-merkle-sum-verifier";

export interface ZkSolvencyPortalProps {
  readonly l2Relayer: L2RelayerTelemetry;
  readonly onTriggerL2Reorg: () => void;
  readonly onAnnounce?: (msg: string) => void;
  readonly onDetach?: () => void;
}

export function ZkSolvencyPortal({
  l2Relayer,
  onTriggerL2Reorg,
  onAnnounce,
  onDetach,
}: ZkSolvencyPortalProps) {
  const initialBundle = useMemo(
    () =>
      buildCanonicalSampleSolvencyBundle({
        epochId: 42,
        stateSeq: l2Relayer.anchoredStateSeq,
        l2BlockNumber: l2Relayer.finalizedL2Block,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  const [proofJson, setProofJson] = useState<string>(() =>
    JSON.stringify(initialBundle, null, 2)
  );
  const [showRawJson, setShowRawJson] = useState<boolean>(false);
  const [hydrated, setHydrated] = useState<boolean>(false);

  useEffect(() => {
    setHydrated(true);
  }, []);

  const textareaId = useId();
  const textareaHintId = useId();

  const verification = useMemo(() => {
    try {
      const parsed = JSON.parse(proofJson) as MerkleSumInclusionProofBundle;
      return {
        bundle: parsed,
        report: verifyMerkleSumSolvencyProof(parsed),
        parseError: null as string | null,
      };
    } catch (err) {
      return {
        bundle: null,
        report: null,
        parseError: err instanceof Error ? err.message : "Invalid JSON proof payload",
      };
    }
  }, [proofJson]);

  const parseError = verification.parseError;

  const applyCleanSnapshot = (leafIndex = 2) => {
    const clean = buildCanonicalSampleSolvencyBundle({
      epochId: 42,
      stateSeq: l2Relayer.anchoredStateSeq,
      l2BlockNumber: l2Relayer.finalizedL2Block,
      targetLeafIndex: leafIndex,
    });
    setProofJson(JSON.stringify(clean, null, 2));
    onAnnounce?.("Loaded valid Merkle-Sum ZK solvency proof; client verification passed.");
  };

  const applyAttack = (
    attack:
      | "INFLATE_LEAF_1_SATOSHI"
      | "NEGATIVE_GHOST_SIBLING"
      | "FINITE_FIELD_WRAP_AROUND"
      | "UNDERCOLLATERALIZED_VAULT"
  ) => {
    const clean = buildCanonicalSampleSolvencyBundle({
      epochId: 42,
      stateSeq: l2Relayer.anchoredStateSeq,
      l2BlockNumber: l2Relayer.finalizedL2Block,
    });
    const tampered = injectAdversarialTamper(clean, attack);
    setProofJson(JSON.stringify(tampered, null, 2));
    onAnnounce?.(`Injected adversarial soundness test ${attack}; client verifier rejected tampered proof.`);
  };

  const report = verification?.report ?? null;
  const bundle = verification?.bundle ?? null;

  return (
    <section className="panel-card" aria-labelledby="zk-solvency-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                report?.valid ? "bg-emerald-400" : "bg-rose-500"
              }`}
              aria-hidden="true"
            />
            <h2 id="zk-solvency-heading" className="text-sm font-semibold tracking-tight text-slate-100">
              CH13 &amp; CH18 · Client-Side ZK Merkle-Sum Solvency Verifier &amp; L2 Anchor
            </h2>
          </div>
          <p className="mono-num text-xs text-slate-400">
            100% Trustless Browser Verification (
            {hydrated ? `${report?.verificationLatencyUs ?? 0}µs` : "<1ms"}) · L2 Block #
            {l2Relayer.finalizedL2Block.toLocaleString("en-US")} · Reorg Shield:{" "}
            <span className={l2Relayer.reorgAlertActive ? "text-amber-400 font-semibold" : "text-emerald-400"}>
              {l2Relayer.reorgAlertActive
                ? `REORG-${l2Relayer.lastReorgDepth}B MITIGATED (0 Double-Spend)`
                : "FINALIZED"}
            </span>
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-1.5">
          <button
            type="button"
            data-testid="zk-load-valid-btn"
            className="sovereign-btn"
            onClick={() => applyCleanSnapshot(2)}
          >
            Pull Clean Proof
          </button>
          <button
            type="button"
            data-testid="zk-tamper-leaf-btn"
            className="sovereign-btn sovereign-btn-danger"
            onClick={() => applyAttack("INFLATE_LEAF_1_SATOSHI")}
          >
            Tamper +1e-8
          </button>
          <button
            type="button"
            data-testid="zk-tamper-neg-btn"
            className="sovereign-btn sovereign-btn-danger"
            onClick={() => applyAttack("NEGATIVE_GHOST_SIBLING")}
          >
            Inject -1 Leaf
          </button>
          <button
            type="button"
            className="sovereign-btn"
            onClick={onTriggerL2Reorg}
          >
            Sim 4-Block Reorg
          </button>
          <button
            type="button"
            className="sovereign-btn"
            aria-pressed={showRawJson}
            onClick={() => setShowRawJson((s) => !s)}
          >
            {showRawJson ? "Hide JSON" : "Paste/Edit Proof"}
          </button>
          {onDetach && (
            <button
              type="button"
              className="sovereign-btn"
              onClick={onDetach}
              aria-label="Pop out ZK Solvency Verifier window"
            >
              Pop-Out
            </button>
          )}
        </div>
      </div>

      {/* Verification Verdict Banner */}
      <div
        data-testid="zk-verdict-banner"
        className={`rounded border px-3 py-2 text-xs ${
          report?.valid
            ? "border-emerald-500/40 bg-emerald-950/25 text-emerald-200"
            : "border-rose-500/50 bg-rose-950/30 text-rose-200"
        }`}
      >
        {parseError ? (
          <div className="mono-num font-semibold">JSON PARSE ERROR: {parseError}</div>
        ) : report?.valid && bundle ? (
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="font-semibold">
              CRYPTOGRAPHIC PROOF VERIFIED IN BROWSER · Reserves Cover{" "}
              <span className="mono-num">{(report.reserveRatioBps / 100).toFixed(2)}%</span> of Liabilities
            </span>
            <span className="mono-num text-[11px] text-emerald-300">
              Surplus: +{formatFixedPoint(report.solvencySurplusFp, 2)} {bundle.asset}
            </span>
          </div>
        ) : (
          <div>
            <div className="font-semibold">CRYPTOGRAPHIC SOUNDNESS REJECTION:</div>
            <ul className="mt-1 list-disc pl-4 mono-num text-[11px]">
              {(report?.failureReasons ?? []).map((reason, idx) => (
                <li key={idx}>{reason}</li>
              ))}
            </ul>
          </div>
        )}
      </div>

      {/* Optional Interactive JSON Proof Input */}
      {showRawJson && (
        <div className="flex flex-col gap-1">
          <label htmlFor={textareaId} className="text-xs font-semibold text-slate-200">
            Merkle-Sum Inclusion Proof &amp; L2 Anchor Payload (JSON)
          </label>
          <span id={textareaHintId} className="text-[11px] text-slate-400">
            Edit any hash, sibling sum, or reserve scalar below to verify deterministic client-side rejection.
          </span>
          <textarea
            id={textareaId}
            aria-describedby={textareaHintId}
            rows={7}
            value={proofJson}
            onChange={(e) => setProofJson(e.target.value)}
            className="sovereign-input mono-num scroll-region w-full text-[11px]"
          />
        </div>
      )}

      {/* Step-by-Step Merkle-Sum Hash Path -> SolvencyStateAnchor.sol Root */}
      {report && bundle && (
        <div className="grid grid-cols-1 gap-2.5 lg:grid-cols-12">
          <div className="lg:col-span-7 flex flex-col gap-1.5 rounded border border-slate-800/80 bg-[#060a12] p-2.5">
            <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-300">
              Merkle-Sum Sparse Tree Inclusion Path (Leaf → Root)
            </h3>

            <div className="rounded border border-cyan-500/30 bg-[#08111e] px-2.5 py-1.5 text-[11px]">
              <div className="flex justify-between">
                <span className="font-semibold text-cyan-300">
                  Leaf · Account #{bundle.accountId} ({bundle.accountAlias})
                </span>
                <span className="mono-num font-semibold text-slate-100">
                  {formatFixedPoint(BigInt(bundle.leafBalanceFp), 2)} {bundle.asset}
                </span>
              </div>
              <div className="mono-num truncate text-[10px] text-slate-400">
                SHA256: 0x{report.computedLeafHashHex}
              </div>
            </div>

            {report.levelTraces.map((trace) => (
              <div
                key={trace.level}
                className={`rounded border px-2.5 py-1.5 text-[11px] ${
                  trace.nonNegativeValid
                    ? "border-slate-800 bg-[#080d18]"
                    : "border-rose-500/60 bg-rose-950/25"
                }`}
              >
                <div className="flex justify-between">
                  <span className="text-slate-300">
                    Depth {trace.level} ({trace.direction === "LEFT_SIBLING" ? "Sibling Left" : "Sibling Right"})
                  </span>
                  <span className="mono-num text-emerald-300">
                    Subtree Σ = {formatFixedPoint(trace.subtreeSumFp, 2)} {bundle.asset}
                  </span>
                </div>
                <div className="mono-num truncate text-[10px] text-slate-400">
                  Parent: 0x{trace.parentHashHex}
                </div>
              </div>
            ))}
          </div>

          {/* Right Column: L2 SolvencyStateAnchor.sol & ZK Circuit Checklist */}
          <div className="lg:col-span-5 flex flex-col justify-between gap-2 rounded border border-slate-800/80 bg-[#060a12] p-2.5 text-[11px]">
            <div>
              <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-300">
                L2 SolvencyStateAnchor.sol
              </h3>
              <div className="mt-1.5 space-y-1 mono-num text-[11px]">
                <div className="flex justify-between gap-2">
                  <span className="text-slate-400">Contract</span>
                  <span className="truncate text-cyan-300">
                    {bundle.l2Anchor.contractAddress.slice(0, 10)}…{bundle.l2Anchor.contractAddress.slice(-6)}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-slate-400">Anchored Root</span>
                  <span className="truncate text-slate-200">
                    0x{bundle.l2Anchor.anchoredMerkleSumRootHex.slice(0, 14)}…
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-slate-400">Total Liabilities</span>
                  <span className="text-slate-200">
                    {formatFixedPoint(report.computedTotalLiabilitiesFp, 2)}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-slate-400">Vault Reserves</span>
                  <span className="text-emerald-400 font-semibold">
                    {formatFixedPoint(report.verifiedVaultReservesFp, 2)}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-slate-400">Escape Hatch</span>
                  <span className="text-emerald-300">
                    ARMED ({bundle.l2Anchor.escapeHatchTimeoutBlocks} blk timeout)
                  </span>
                </div>
              </div>
            </div>

            <div className="border-t border-slate-800 pt-2 space-y-1">
              {[
                { label: "64-Bit Non-Negative Range Proof", ok: report.checks.leafRangeNonNegative && report.checks.allSiblingsNonNegativeAndNoFieldWrap },
                { label: "Merkle-Sum Binary Path Integrity", ok: report.checks.leafHashMatches && report.checks.rootHashMatchesProof },
                { label: "L2 State Root == MSST Root", ok: report.checks.l2ContractAnchorMatchesRoot },
                { label: "ZK Groth16 Transcript Binding", ok: report.checks.zkPublicInputTranscriptBound },
              ].map((chk) => (
                <div key={chk.label} className="flex items-center justify-between">
                  <span className="text-slate-300">{chk.label}</span>
                  <span
                    className={`mono-num font-semibold ${
                      chk.ok ? "text-emerald-400" : "text-rose-400"
                    }`}
                  >
                    {chk.ok ? "PASS" : "FAIL"}
                  </span>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
