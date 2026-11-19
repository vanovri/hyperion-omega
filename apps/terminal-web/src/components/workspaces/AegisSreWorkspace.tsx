"use client";

import React, { useEffect, useRef, useState } from "react";
import {
  Bot,
  Check,
  FileText,
  Lock,
  RotateCcw,
  ShieldAlert,
  Terminal,
  X,
} from "lucide-react";
import { formatCompactNotionalFp } from "@/lib/fixed-point";
import type {
  AegisPostMortemReport,
  AegisStage,
  MainToWorkerCommand,
  WorkerFramePacket,
} from "@/lib/types";

interface AegisSreWorkspaceProps {
  uiFrame: WorkerFramePacket | null;
  sendCommand: (cmd: MainToWorkerCommand) => void;
}

const STAGES: AegisStage[] = [
  "OBSERVE",
  "HYPOTHESIZE",
  "VERIFY_MCP",
  "POLICY_GATE",
  "REMEDIATE",
  "POST_MORTEM",
];

export function AegisSreWorkspace({ uiFrame, sendCommand }: AegisSreWorkspaceProps) {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const [selectedReport, setSelectedReport] = useState<AegisPostMortemReport | null>(null);

  // Modern Web Guidance (light-dismiss-a-dialog & dialog):
  // Fallback for browsers without native HTMLDialogElement.prototype.closedBy,
  // and reset dialog.returnValue = '' on close to prevent stale values.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;

    const handleClose = () => {
      dialog.returnValue = "";
    };
    dialog.addEventListener("close", handleClose);

    let handleBackdropClick: ((event: MouseEvent) => void) | null = null;
    if (typeof HTMLDialogElement !== "undefined" && !("closedBy" in HTMLDialogElement.prototype)) {
      handleBackdropClick = (event: MouseEvent) => {
        if (event.target !== dialog) return;
        const rect = dialog.getBoundingClientRect();
        const inside =
          rect.top <= event.clientY &&
          event.clientY <= rect.top + rect.height &&
          rect.left <= event.clientX &&
          event.clientX <= rect.left + rect.width;
        if (!inside) {
          dialog.close();
        }
      };
      dialog.addEventListener("click", handleBackdropClick);
    }

    return () => {
      dialog.removeEventListener("close", handleClose);
      if (handleBackdropClick) {
        dialog.removeEventListener("click", handleBackdropClick);
      }
    };
  }, []);

  const openPostMortemModal = (report: AegisPostMortemReport) => {
    setSelectedReport(report);
    const dlg = dialogRef.current;
    if (dlg && !dlg.open) {
      dlg.showModal();
    }
  };

  const aegis = uiFrame?.aegis;
  const activeStage = aegis?.currentStage ?? "POLICY_GATE";

  return (
    <div
      data-testid="workspace-aegis-sre"
      className="flex h-full min-h-0 flex-col gap-px bg-[var(--wire-hairline)]"
    >
      {/* Top Autonomous State-Graph Pipeline Bar */}
      <section className="isolated-pane shrink-0 bg-[var(--obsidian-panel)] px-3 py-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2">
            <Bot className="h-4 w-4 text-[#38bdf8]" />
            <h2 className="text-xs font-semibold text-white">
              Aegis Autonomous Incident-Response &amp; Guardrailed MCP Orchestrator
            </h2>
            <span className="rounded-[2px] border border-[#00d492]/40 bg-[#00d492]/12 px-2 py-0.5 text-[10px] font-semibold text-[#00d492] tabular-mono">
              ENGINE: {aegis?.reasoningMode ?? "DETERMINISTIC_RULE"}
            </span>
          </div>

          <div className="flex items-center gap-2">
            <span
              data-testid="aegis-blocked-injections"
              className="rounded-[2px] border border-[#ff3b5c]/40 bg-[#ff3b5c]/12 px-2 py-0.5 text-[10px] font-semibold text-[#ff6b84] tabular-mono"
            >
              Prompt Injections Blocked: {aegis?.promptInjectionsBlocked ?? 0}
            </span>
            <button
              type="button"
              data-testid="aegis-test-injection-btn"
              onClick={() => sendCommand({ type: "AEGIS_SIMULATE_PROMPT_INJECTION" })}
              className="cmd-btn rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] px-2.5 py-1 text-[11px] font-medium text-[var(--ink-secondary)] hover:text-white"
            >
              Simulate Prompt-Injection Attack
            </button>
          </div>
        </div>

        {/* 6-Node State Machine Stepper */}
        <div className="mt-2 grid grid-cols-3 gap-1.5 sm:grid-cols-6">
          {STAGES.map((st, idx) => {
            const isCurrent = st === activeStage;
            return (
              <div
                key={st}
                className={`flex items-center justify-between rounded-[2px] border px-2 py-1 text-[10px] tabular-mono ${
                  isCurrent
                    ? "border-[#38bdf8] bg-[#38bdf8]/15 font-bold text-white"
                    : "border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] text-[var(--ink-tertiary)]"
                }`}
              >
                <span>
                  {idx + 1}. {st}
                </span>
                {isCurrent && <span className="h-1.5 w-1.5 rounded-full bg-[#38bdf8]" />}
              </div>
            );
          })}
        </div>
      </section>

      {/* Main Split: Left Investigation & MCP Stream | Right HITL Approvals & Post-Mortems */}
      <div className="grid min-h-0 flex-1 grid-cols-1 gap-px bg-[var(--wire-hairline)] lg:grid-cols-12">
        {/* Left 7 Cols: Live Investigation Thought & MCP Execution Stream */}
        <section className="isolated-pane flex min-h-0 flex-col bg-[var(--obsidian-panel)] lg:col-span-7">
          <header className="flex items-center justify-between border-b border-[var(--wire-hairline)] px-3 py-1.5">
            <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-white">
              <Terminal className="h-3.5 w-3.5 text-[#38bdf8]" />
              Live Investigation Reasoning &amp; Zod-Validated MCP Trace
            </span>
            <span className="text-[10px] text-[var(--ink-tertiary)] tabular-mono">
              stdio + Streamable HTTP/SSE (:8085/mcp)
            </span>
          </header>

          <div className="min-h-0 flex-1 space-y-2 overflow-y-auto p-3">
            {(aegis?.thoughts ?? []).map((th) => (
              <article
                key={th.id}
                className="offscreen-log-region rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2.5"
              >
                <div className="flex items-center justify-between gap-2 text-[10px] tabular-mono">
                  <span className="rounded-[2px] bg-[#38bdf8]/15 px-1.5 py-0.5 font-semibold text-[#38bdf8]">
                    {th.stage}
                  </span>
                  <span className="text-[var(--ink-tertiary)]">
                    ns:{th.tsNs.toString().slice(-9)}
                  </span>
                </div>

                <p className="mt-1.5 text-xs leading-relaxed text-[var(--ink-primary)]">
                  {th.summary}
                </p>

                {th.mcpTool && (
                  <div className="mt-2 rounded-[2px] border border-[#38bdf8]/25 bg-[#06080c] p-2 text-[10px] tabular-mono">
                    <div className="flex items-center justify-between text-[#38bdf8]">
                      <span className="font-bold">MCP TOOL :: {th.mcpTool.name}</span>
                      <span>{th.mcpTool.latencyMs.toFixed(1)} ms</span>
                    </div>
                    <div className="mt-1 truncate text-[var(--ink-secondary)]">
                      ARGS: {th.mcpTool.argsJson}
                    </div>
                    <div className="mt-0.5 text-[#00d492]">OUT: {th.mcpTool.resultSummary}</div>
                  </div>
                )}

                {th.guardrailBlocked && (
                  <div className="mt-2 rounded-[2px] border border-[#ff3b5c]/45 bg-[#ff3b5c]/10 p-2 text-[10px] tabular-mono">
                    <div className="flex items-center gap-1 font-bold text-[#ff6b84]">
                      <ShieldAlert className="h-3.5 w-3.5" />
                      POLICY GATE BLOCKED ADVERSARIAL PAYLOAD
                    </div>
                    <div className="mt-1 break-all text-white">{th.guardrailBlocked.attackVector}</div>
                    <div className="mt-0.5 text-[var(--ink-secondary)]">
                      {th.guardrailBlocked.reason}
                    </div>
                  </div>
                )}
              </article>
            ))}
          </div>
        </section>

        {/* Right 5 Cols: Human-in-the-Loop Approval Queue + Post-Mortem Archive */}
        <section className="isolated-pane flex min-h-0 flex-col bg-[var(--obsidian-panel)] lg:col-span-5">
          <div className="border-b border-[var(--wire-hairline)] px-3 py-1.5">
            <div className="flex items-center justify-between">
              <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-white">
                <Lock className="h-3.5 w-3.5 text-[#f59e0b]" />
                Operator Mitigation Approval Queue (HITL)
              </span>
              <span className="text-[10px] text-[var(--ink-secondary)] tabular-mono">
                Ed25519 + Blake3 Audit
              </span>
            </div>
          </div>

          <div className="max-h-[55%] space-y-2 overflow-y-auto border-b border-[var(--wire-hairline)] p-3">
            {(aegis?.approvals ?? []).map((item) => (
              <div
                key={item.commandId}
                data-testid={`approval-card-${item.commandId}`}
                className="rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] p-2.5"
              >
                <div className="flex items-center justify-between text-[11px] tabular-mono">
                  <span className="font-bold text-white">
                    {item.commandId} · {item.action}
                  </span>
                  <span
                    data-testid={`approval-status-${item.commandId}`}
                    className={`rounded-[2px] px-1.5 py-0.5 text-[10px] font-semibold ${
                      item.status === "PENDING_APPROVAL"
                        ? "bg-[#f59e0b]/20 text-[#fbbf24]"
                        : item.status === "APPROVED_EXECUTED"
                          ? "bg-[#00d492]/20 text-[#00d492]"
                          : "bg-slate-700/50 text-slate-300"
                    }`}
                  >
                    {item.status}
                  </span>
                </div>

                <p className="mt-1 text-[11px] text-[var(--ink-secondary)]">{item.reason}</p>

                <div className="mt-1.5 space-y-0.5 text-[10px] text-[var(--ink-tertiary)] tabular-mono">
                  <div>
                    Targets: {item.targetAccountIds.map((id) => `#${id.toString()}`).join(", ")} ·{" "}
                    Cooldown {(item.cooldownMs / 1000).toFixed(0)}s
                  </div>
                  <div className="truncate">ED25519: {item.ed25519SigHex.slice(0, 32)}…</div>
                  <div className="truncate">AUDIT: {item.auditChainBlake3Hex.slice(0, 32)}…</div>
                </div>

                <div className="mt-2 flex items-center gap-2">
                  {item.status === "PENDING_APPROVAL" ? (
                    <>
                      <button
                        type="button"
                        data-testid={`approve-btn-${item.commandId}`}
                        onClick={() =>
                          sendCommand({
                            type: "AEGIS_DECIDE_APPROVAL",
                            commandId: item.commandId,
                            decision: "APPROVE",
                          })
                        }
                        className="cmd-btn inline-flex flex-1 items-center justify-center gap-1 rounded-[2px] bg-[#00d492] py-1 text-[11px] font-bold text-[#040609] hover:bg-[#10e8a4]"
                      >
                        <Check className="h-3.5 w-3.5" />
                        Approve &amp; Sign
                      </button>
                      <button
                        type="button"
                        data-testid={`reject-btn-${item.commandId}`}
                        onClick={() =>
                          sendCommand({
                            type: "AEGIS_DECIDE_APPROVAL",
                            commandId: item.commandId,
                            decision: "REJECT",
                          })
                        }
                        className="cmd-btn inline-flex flex-1 items-center justify-center gap-1 rounded-[2px] border border-[#ff3b5c]/50 bg-[#ff3b5c]/15 py-1 text-[11px] font-semibold text-[#ff6b84] hover:bg-[#ff3b5c]/25"
                      >
                        <X className="h-3.5 w-3.5" />
                        Reject
                      </button>
                    </>
                  ) : item.status === "APPROVED_EXECUTED" ? (
                    <button
                      type="button"
                      data-testid={`rollback-btn-${item.commandId}`}
                      onClick={() =>
                        sendCommand({
                          type: "AEGIS_DECIDE_APPROVAL",
                          commandId: item.commandId,
                          decision: "ROLLBACK",
                        })
                      }
                      className="cmd-btn inline-flex w-full items-center justify-center gap-1 rounded-[2px] border border-[var(--wire-strong)] bg-[#111722] py-1 text-[11px] font-medium text-[var(--ink-secondary)] hover:text-white"
                    >
                      <RotateCcw className="h-3 w-3" />
                      Execute Rollback ({item.rollbackToken})
                    </button>
                  ) : null}
                </div>
              </div>
            ))}
          </div>

          {/* Post-Mortem Reports List */}
          <div className="min-h-0 flex-1 overflow-y-auto p-3">
            <div className="mb-2 flex items-center justify-between text-xs font-semibold text-white">
              <span className="inline-flex items-center gap-1.5">
                <FileText className="h-3.5 w-3.5 text-[#00d492]" />
                Automated Post-Mortem Reports
              </span>
              <span className="text-[10px] text-[var(--ink-tertiary)] tabular-mono">
                {aegis?.postMortems.length ?? 0} generated
              </span>
            </div>

            <div className="space-y-2">
              {(aegis?.postMortems ?? []).map((pm) => (
                <div
                  key={pm.incidentId}
                  className="rounded-[2px] border border-[var(--wire-hairline)] bg-[var(--obsidian-inset)] p-2.5"
                >
                  <div className="flex items-center justify-between text-[11px] tabular-mono">
                    <span className="font-bold text-[#38bdf8]">{pm.incidentId}</span>
                    <span className="rounded-[2px] bg-[#ff3b5c]/20 px-1.5 py-0.5 text-[10px] font-bold text-[#ff6b84]">
                      {pm.severity}
                    </span>
                  </div>
                  <div className="mt-1 text-xs font-medium text-white">{pm.title}</div>
                  <div className="mt-1 flex items-center justify-between text-[10px] text-[var(--ink-secondary)] tabular-mono">
                    <span>Saved: {formatCompactNotionalFp(pm.preventedLossFp)} USDT</span>
                    <button
                      type="button"
                      data-testid={`open-postmortem-${pm.incidentId}`}
                      onClick={() => openPostMortemModal(pm)}
                      className="cmd-btn rounded-[2px] border border-[#38bdf8]/40 bg-[#38bdf8]/12 px-2 py-0.5 font-semibold text-[#38bdf8] hover:bg-[#38bdf8]/20"
                    >
                      Inspect Report
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </section>
      </div>

      {/* Native Accessible Modal <dialog closedby="any"> for Post-Mortem Report Viewer */}
      <dialog
        ref={dialogRef}
        aria-labelledby="postmortem-dialog-title"
        {...({ closedby: "any" } as React.DialogHTMLAttributes<HTMLDialogElement>)}
        data-testid="postmortem-modal-dialog"
        className="m-auto w-full text-left shadow-2xl"
      >
        {selectedReport && (
          <div className="flex max-h-[82vh] flex-col">
            <header className="flex items-center justify-between border-b border-[var(--wire-strong)] bg-[#0b0f17] px-4 py-3">
              <div>
                <h2 id="postmortem-dialog-title" className="text-sm font-bold text-white">
                  {selectedReport.incidentId} — {selectedReport.title}
                </h2>
                <p className="text-[11px] text-[var(--ink-secondary)] tabular-mono">
                  Command {selectedReport.mitigationCommandId} · Rollback Token{" "}
                  {selectedReport.rollbackToken}
                </p>
              </div>
              <form method="dialog">
                <button
                  type="submit"
                  autoFocus
                  data-testid="close-postmortem-modal"
                  className="cmd-btn rounded-[2px] border border-[var(--wire-strong)] bg-[var(--obsidian-inset)] px-2.5 py-1 text-xs font-semibold text-white hover:border-[#38bdf8]"
                >
                  Close [Esc]
                </button>
              </form>
            </header>

            <div className="space-y-4 overflow-y-auto p-4 text-xs">
              <pre className="whitespace-pre-wrap rounded-[2px] border border-[var(--wire-hairline)] bg-[#06080c] p-3 text-[11px] leading-relaxed text-[var(--ink-primary)] tabular-mono">
                {selectedReport.markdownBody}
              </pre>

              <div>
                <h3 className="mb-1.5 text-xs font-semibold text-white">
                  Millisecond Incident Chronology
                </h3>
                <div className="space-y-1 tabular-mono">
                  {selectedReport.timelineEvents.map((ev, i) => (
                    <div
                      key={i}
                      className="flex items-center gap-3 rounded-[2px] border border-[var(--wire-hairline)] bg-[#070a10] px-2.5 py-1.5 text-[11px]"
                    >
                      <span className="w-14 font-bold text-[#38bdf8]">+{ev.tsOffsetMs}ms</span>
                      <span className="w-28 font-semibold text-[#fbbf24]">[{ev.actor}]</span>
                      <span className="text-[var(--ink-primary)]">{ev.detail}</span>
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </div>
        )}
      </dialog>
    </div>
  );
}
