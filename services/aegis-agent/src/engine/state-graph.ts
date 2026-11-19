/**
 * HYPERION Channel 06 — Autonomous State-Graph Investigation Engine.
 *
 * Orchestrates deterministic state-machine transitions:
 *   OBSERVE -> HYPOTHESIZE -> VERIFY (via real MCP Client <-> Server protocol!)
 *   -> POLICY_GATE (dry-run blast-radius & guardrail verification)
 *   -> REMEDIATE (live execution or AWAITING_HITL_APPROVAL queue)
 *   -> POST_MORTEM -> COMPLETED
 */

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import type { AegisReasoner } from "../adapters/llm-reasoner.js";
import type { HyperionUpstreamBackend } from "../adapters/upstream-clients.js";
import type { ImmutableAuditChain } from "../crypto/audit-chain.js";
import type { NanosecondClock } from "../domain/clock.js";
import type {
  AnomalyAlertDto,
  FraudSubgraphInspectionResult,
  InvestigationHypothesis,
  InvestigationPhase,
  InvestigationRecord,
  LedgerAuditVerificationResult,
  MitigationExecutionResult,
  SseIncidentStreamEvent,
  SystemTelemetryAnalysisResult
} from "../domain/types.js";
import { PromptInjectionFirewall } from "../guardrails/injection-firewall.js";
import type { SafetyPolicyEngine } from "../guardrails/policy-engine.js";
import {
  AegisToolOperations,
  createAegisMcpServer,
  type IncidentRepositoryView
} from "../mcp/server.js";
import { generateIncidentPostMortemMarkdown } from "./post-mortem.js";

export type IncidentStreamSubscriber = (event: SseIncidentStreamEvent) => void;

export class AutonomousStateGraphEngine implements IncidentRepositoryView {
  private readonly clock: NanosecondClock;
  private readonly upstream: HyperionUpstreamBackend;
  private readonly policyEngine: SafetyPolicyEngine;
  private readonly auditChain: ImmutableAuditChain;
  private readonly firewall: PromptInjectionFirewall;
  private readonly reasoner: AegisReasoner;
  private readonly toolOps: AegisToolOperations;

  private readonly incidents = new Map<string, InvestigationRecord>();
  private readonly rootCauseByIncident = new Map<string, string>();
  private readonly subscribers = new Set<IncidentStreamSubscriber>();
  private incidentCounter = 1;
  private sseEventCounter = 1;

  public constructor(params: {
    clock: NanosecondClock;
    upstream: HyperionUpstreamBackend;
    policyEngine: SafetyPolicyEngine;
    auditChain: ImmutableAuditChain;
    firewall?: PromptInjectionFirewall;
    reasoner: AegisReasoner;
  }) {
    this.clock = params.clock;
    this.upstream = params.upstream;
    this.policyEngine = params.policyEngine;
    this.auditChain = params.auditChain;
    this.firewall = params.firewall ?? new PromptInjectionFirewall();
    this.reasoner = params.reasoner;
    this.toolOps = new AegisToolOperations({
      clock: this.clock,
      upstream: this.upstream,
      policyEngine: this.policyEngine,
      auditChain: this.auditChain,
      firewall: this.firewall,
      incidentStore: this,
      onMcpToolExecuted: (toolName, summary) => {
        this.emitStreamEvent("MCP_TOOL_INVOKED", String(summary.incident_id ?? "mcp"), {
          tool: toolName,
          ...summary
        });
      },
      onInjectionQuarantined: (findings) => {
        this.emitStreamEvent("INJECTION_QUARANTINED", "security-firewall", {
          findings
        });
      }
    });
  }

  public getToolOperations(): AegisToolOperations {
    return this.toolOps;
  }

  public subscribeEvents(listener: IncidentStreamSubscriber): () => void {
    this.subscribers.add(listener);
    return () => {
      this.subscribers.delete(listener);
    };
  }

  public emitStreamEvent(
    eventType: SseIncidentStreamEvent["event_type"],
    incidentId: string,
    payload: Record<string, unknown>
  ): void {
    const evt: SseIncidentStreamEvent = {
      event_id: `evt-${String(this.sseEventCounter++).padStart(6, "0")}`,
      event_type: eventType,
      incident_id: incidentId,
      ts_ns: this.clock.nowNs().toString(10),
      payload
    };
    for (const sub of this.subscribers) {
      sub(evt);
    }
  }

  public getIncident(incidentId: string): InvestigationRecord | undefined {
    return this.incidents.get(incidentId);
  }

  public listIncidents(): InvestigationRecord[] {
    return [...this.incidents.values()];
  }

  private recordTimelineStep(
    record: InvestigationRecord,
    phase: InvestigationPhase,
    eventType: string,
    summary: string,
    details: Record<string, unknown>
  ): void {
    const nowNs = this.clock.nowNs();
    const startNs = BigInt(record.started_ts_ns);
    const deltaNs = nowNs >= startNs ? nowNs - startNs : 0n;
    record.current_phase = phase;

    const auditEntry = this.auditChain.append({
      incidentId: record.incident_id,
      eventType: `STATE_${phase}_${eventType}`,
      actor: "aegis-state-graph",
      payload: {
        phase,
        summary,
        ...details
      },
      timestampNsOverride: nowNs
    });

    record.timeline.push({
      step_index: record.timeline.length + 1,
      ts_ns: nowNs.toString(10),
      delta_from_start_ns: deltaNs.toString(10),
      phase,
      event_type: eventType,
      summary,
      details,
      audit_entry_blake3: auditEntry.entry_blake3
    });

    this.emitStreamEvent("PHASE_TRANSITION", record.incident_id, {
      phase,
      event_type: eventType,
      summary,
      ts_ns: nowNs.toString(10)
    });
  }

  private extractStructuredFromMcpCall<T>(callResult: unknown, toolName: string): T {
    const resObj = callResult as {
      isError?: boolean;
      structuredContent?: unknown;
      content?: Array<{ type: string; text?: string }>;
    };
    if (resObj.isError) {
      const errText = resObj.content?.[0]?.text ?? `MCP tool ${toolName} returned error`;
      throw new Error(errText);
    }
    if (resObj.structuredContent !== undefined) {
      return resObj.structuredContent as T;
    }
    const textContent = resObj.content?.[0]?.text;
    if (!textContent) {
      throw new Error(`MCP tool ${toolName} returned empty response`);
    }
    return JSON.parse(textContent) as T;
  }

  /**
   * Runs a full end-to-end autonomous investigation over the state-graph, executing every
   * verification & mitigation tool call through a real MCP Client <-> Server transport pair!
   */
  public async runInvestigation(
    alert: AnomalyAlertDto,
    incidentIdOverride?: string
  ): Promise<InvestigationRecord> {
    const startNs = this.clock.nowNs();
    const incidentId =
      incidentIdOverride ?? `INC-20261118-${String(this.incidentCounter++).padStart(4, "0")}`;

    const record: InvestigationRecord = {
      incident_id: incidentId,
      trigger_alert: alert,
      reasoning_mode_used: "DETERMINISTIC_RULE_ENGINE",
      llm_fallback_triggered: false,
      current_phase: "OBSERVE",
      started_ts_ns: startNs.toString(10),
      hypotheses: [],
      ledger_audits: [],
      dry_run_results: [],
      mitigation_outcomes: [],
      pending_approvals: [],
      timeline: []
    };
    this.incidents.set(incidentId, record);

    this.emitStreamEvent("INCIDENT_STARTED", incidentId, {
      alert_id: alert.alert_id,
      symbol: alert.symbol,
      pattern_type: alert.pattern_type,
      severity_score: alert.severity_score
    });

    // Establish real in-process MCP Client <-> Server protocol session for this investigation
    const mcpServer = createAegisMcpServer(
      {
        clock: this.clock,
        upstream: this.upstream,
        policyEngine: this.policyEngine,
        auditChain: this.auditChain,
        firewall: this.firewall,
        incidentStore: this
      },
      this.toolOps
    );
    const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
    const mcpClient = new Client({
      name: "aegis-state-graph-orchestrator",
      version: "1.0.0"
    });

    await mcpServer.connect(serverTransport);
    await mcpClient.connect(clientTransport);

    try {
      // ======================================================================
      // PHASE 1: OBSERVE
      // ======================================================================
      this.policyEngine.registerSymbolAnomalySeverity(alert.symbol, alert.severity_score);
      this.recordTimelineStep(
        record,
        "OBSERVE",
        "ALERT_INGESTED",
        `Ingested Sentinel-ML alert ${alert.alert_id} (${alert.pattern_type} on ${alert.symbol}, severity=${alert.severity_score}, offenders=[${alert.offender_account_ids.join(", ")}])`,
        {
          alert_id: alert.alert_id,
          engine_seq: alert.engine_seq,
          severity_score: alert.severity_score,
          confidence_ppm: alert.confidence_ppm
        }
      );

      // ======================================================================
      // PHASE 2: HYPOTHESIZE (LLM Structured Output or Deterministic Fallback)
      // ======================================================================
      const hypOutcome = await this.reasoner.synthesizeHypotheses(alert);
      record.reasoning_mode_used = hypOutcome.modeUsed;
      record.llm_fallback_triggered = hypOutcome.fallbackTriggered;
      record.llm_fallback_reason = hypOutcome.fallbackReason;

      const hypotheses: InvestigationHypothesis[] = hypOutcome.result.hypotheses.map((h) => ({
        ...h,
        status: "PROPOSED",
        supporting_evidence: []
      }));
      record.hypotheses = hypotheses;

      this.recordTimelineStep(
        record,
        "HYPOTHESIZE",
        "HYPOTHESES_FORMULATED",
        `Formulated ${hypotheses.length} hypotheses via ${hypOutcome.modeUsed}${hypOutcome.fallbackTriggered ? " (resilient fallback)" : ""}`,
        {
          mode_used: hypOutcome.modeUsed,
          hypothesis_ids: hypotheses.map((h) => h.hypothesis_id),
          accounts_to_audit: hypOutcome.result.accounts_to_audit
        }
      );

      // ======================================================================
      // PHASE 3: VERIFY VIA MCP TOOLS
      // ======================================================================
      const seedAccount = alert.offender_account_ids[0] ?? hypOutcome.result.accounts_to_audit[0] ?? "9001";

      // 3a. MCP Tool: inspect_fraud_subgraph
      const subgraphCall = await mcpClient.callTool({
        name: "inspect_fraud_subgraph",
        arguments: {
          center_account_id: seedAccount,
          symbol: alert.symbol,
          max_hops: 3,
          lookback_window_ns: "60000000000",
          response_format: "json"
        }
      });
      const subgraph = this.extractStructuredFromMcpCall<FraudSubgraphInspectionResult>(
        subgraphCall,
        "inspect_fraud_subgraph"
      );
      record.subgraph_inspection = subgraph;

      this.recordTimelineStep(
        record,
        "VERIFY",
        "MCP_SUBGRAPH_VERIFIED",
        `MCP inspect_fraud_subgraph verified ${subgraph.security_taint_report.verified_offender_account_ids.length} offender(s) and ${subgraph.detected_cycles.length} cycle(s); untrusted injection=${subgraph.security_taint_report.has_untrusted_injection_attempt}`,
        {
          verified_offenders: subgraph.security_taint_report.verified_offender_account_ids,
          quarantined_injections: subgraph.security_taint_report.quarantined_findings.length
        }
      );

      // 3b. MCP Tool: analyze_system_telemetry
      const telemetryCall = await mcpClient.callTool({
        name: "analyze_system_telemetry",
        arguments: {
          include_redpanda_lag: true,
          include_circuit_breakers: true,
          window_seconds: 60,
          response_format: "json"
        }
      });
      const telemetry = this.extractStructuredFromMcpCall<SystemTelemetryAnalysisResult>(
        telemetryCall,
        "analyze_system_telemetry"
      );
      record.telemetry_analysis = telemetry;

      this.recordTimelineStep(
        record,
        "VERIFY",
        "MCP_TELEMETRY_VERIFIED",
        `MCP analyze_system_telemetry reported mesh=${telemetry.overall_health}, db_write_p99=${telemetry.database_telemetry.write_batch_p99_us}us, anomalies=${telemetry.anomalies_detected.length}`,
        {
          overall_health: telemetry.overall_health,
          db_write_batch_p99_us: telemetry.database_telemetry.write_batch_p99_us,
          anomalies: telemetry.anomalies_detected
        }
      );

      // 3c. MCP Tool: query_ledger_merkle_audit for verified offenders / seed accounts
      const accountsToAudit = [
        ...new Set([
          ...subgraph.security_taint_report.verified_offender_account_ids,
          ...hypOutcome.result.accounts_to_audit.slice(0, 3)
        ])
      ].slice(0, 5);

      for (const acctId of accountsToAudit) {
        const merkleCall = await mcpClient.callTool({
          name: "query_ledger_merkle_audit",
          arguments: {
            account_id: acctId,
            asset: "USDT",
            response_format: "json"
          }
        });
        const auditRes = this.extractStructuredFromMcpCall<LedgerAuditVerificationResult>(
          merkleCall,
          "query_ledger_merkle_audit"
        );
        record.ledger_audits.push(auditRes);
      }

      this.recordTimelineStep(
        record,
        "VERIFY",
        "MCP_LEDGER_MERKLE_VERIFIED",
        `MCP query_ledger_merkle_audit verified ${record.ledger_audits.length} account(s); all_solvent=${record.ledger_audits.every((a) => a.verdict === "VERIFIED_SOLVENT")}`,
        {
          audited_accounts: record.ledger_audits.map((a) => a.account_id),
          verdicts: record.ledger_audits.map((a) => a.verdict)
        }
      );

      // ======================================================================
      // PHASE 4: POLICY GATE (Synthesize Plan + Dry-Run Blast-Radius Check)
      // ======================================================================
      const planOutcome = await this.reasoner.synthesizeMitigationPlan({
        alert,
        hypotheses: record.hypotheses,
        ledgerAudits: record.ledger_audits,
        subgraph,
        telemetry
      });
      if (planOutcome.fallbackTriggered) {
        record.llm_fallback_triggered = true;
        record.llm_fallback_reason = planOutcome.fallbackReason;
      }

      for (const h of record.hypotheses) {
        if (planOutcome.result.confirmed_hypothesis_ids.includes(h.hypothesis_id)) {
          h.status = "CONFIRMED";
          h.posterior_confidence_ppm = 980_000;
        } else if (planOutcome.result.refuted_hypothesis_ids.includes(h.hypothesis_id)) {
          h.status = "REFUTED";
          h.posterior_confidence_ppm = 25_000;
        }
      }

      this.rootCauseByIncident.set(incidentId, planOutcome.result.root_cause_summary);

      for (const item of planOutcome.result.proposed_mitigations) {
        if (item.dry_run_first) {
          const dryRunCall = await mcpClient.callTool({
            name: "execute_risk_mitigation",
            arguments: {
              incident_id: incidentId,
              source_alert_id: alert.alert_id,
              action: item.action,
              target_account_ids: item.target_account_ids,
              target_symbol: item.target_symbol,
              ...(item.rate_limit_override_rps !== undefined
                ? { rate_limit_override_rps: item.rate_limit_override_rps }
                : {}),
              cooldown_duration_ms: item.cooldown_duration_ms,
              dry_run: true,
              reason: item.reason,
              response_format: "json"
            }
          });
          const dryRes = this.extractStructuredFromMcpCall<MitigationExecutionResult>(
            dryRunCall,
            "execute_risk_mitigation"
          );
          record.dry_run_results.push(dryRes);
        }
      }

      this.recordTimelineStep(
        record,
        "POLICY_GATE",
        "POLICY_DRY_RUN_EVALUATED",
        `Evaluated ${planOutcome.result.proposed_mitigations.length} proposed mitigation(s) against Safety Policy Gate`,
        {
          dry_run_statuses: record.dry_run_results.map((d) => ({
            status: d.status,
            verdict: d.policy_evaluation.verdict,
            rule: d.policy_evaluation.rule_triggered
          }))
        }
      );

      // ======================================================================
      // PHASE 5: REMEDIATE (Execute Permitted Actions & Queue HITL Actions)
      // ======================================================================
      for (const item of planOutcome.result.proposed_mitigations) {
        const liveCall = await mcpClient.callTool({
          name: "execute_risk_mitigation",
          arguments: {
            incident_id: incidentId,
            source_alert_id: alert.alert_id,
            action: item.action,
            target_account_ids: item.target_account_ids,
            target_symbol: item.target_symbol,
            ...(item.rate_limit_override_rps !== undefined
              ? { rate_limit_override_rps: item.rate_limit_override_rps }
              : {}),
            cooldown_duration_ms: item.cooldown_duration_ms,
            dry_run: false,
            reason: item.reason,
            response_format: "json"
          }
        });
        const liveRes = this.extractStructuredFromMcpCall<MitigationExecutionResult>(
          liveCall,
          "execute_risk_mitigation"
        );
        record.mitigation_outcomes.push(liveRes);
        if (liveRes.approval_request) {
          record.pending_approvals.push(liveRes.approval_request);
        }
      }

      const hasPendingHitl = record.pending_approvals.some((a) => a.status === "PENDING");
      this.recordTimelineStep(
        record,
        hasPendingHitl ? "AWAITING_HITL_APPROVAL" : "REMEDIATE",
        "MITIGATION_DISPATCH_COMPLETED",
        `Processed ${record.mitigation_outcomes.length} live mitigation decision(s): executed=${record.mitigation_outcomes.filter((m) => m.status === "EXECUTED").length}, hitl_queued=${record.pending_approvals.length}, blocked=${record.mitigation_outcomes.filter((m) => m.status.startsWith("BLOCKED")).length}`,
        {
          outcomes: record.mitigation_outcomes.map((m) => m.status)
        }
      );

      // ======================================================================
      // PHASE 6: POST_MORTEM & COMPLETED
      // ======================================================================
      const completedNs = this.clock.nowNs();
      record.completed_ts_ns = completedNs.toString(10);

      this.recordTimelineStep(
        record,
        "POST_MORTEM",
        "POST_MORTEM_GENERATED",
        `Compiled Markdown Post-Mortem with nanosecond timeline, Mermaid causality flowchart, and cryptographic rollback tokens`,
        {
          completed_ts_ns: record.completed_ts_ns
        }
      );

      record.current_phase = hasPendingHitl ? "AWAITING_HITL_APPROVAL" : "COMPLETED";
      record.post_mortem_markdown = generateIncidentPostMortemMarkdown(
        record,
        planOutcome.result.root_cause_summary,
        this.auditChain.getHeadHash()
      );

      await this.upstream.publishIncidentEvent(incidentId, {
        incident_id: incidentId,
        phase: record.current_phase,
        alert_id: alert.alert_id,
        symbol: alert.symbol,
        mitigation_statuses: record.mitigation_outcomes.map((m) => m.status),
        audit_head_blake3: this.auditChain.getHeadHash()
      });

      this.emitStreamEvent("POST_MORTEM_READY", incidentId, {
        phase: record.current_phase,
        post_mortem_length: record.post_mortem_markdown.length
      });

      return record;
    } finally {
      await mcpClient.close();
      await mcpServer.close();
    }
  }

  /**
   * Refreshes an incident's state & Markdown Post-Mortem after a human operator approves or rejects
   * a queued HITL request.
   */
  public refreshIncidentAfterHitlDecision(
    incidentId: string,
    execution?: MitigationExecutionResult
  ): InvestigationRecord | undefined {
    const record = this.incidents.get(incidentId);
    if (!record) {
      return undefined;
    }
    if (execution) {
      record.mitigation_outcomes.push(execution);
    }
    // Synchronize approval states from authoritative SafetyPolicyEngine (since MCP tool responses were serialized)
    const authoritativeApprovals = this.policyEngine
      .getAllApprovals()
      .filter((a) => a.incident_id === incidentId);
    if (authoritativeApprovals.length > 0) {
      record.pending_approvals = authoritativeApprovals;
    }
    const stillPending = record.pending_approvals.some((a) => a.status === "PENDING");
    record.completed_ts_ns = this.clock.nowNs().toString(10);
    record.current_phase = stillPending ? "AWAITING_HITL_APPROVAL" : "COMPLETED";
    const summary =
      this.rootCauseByIncident.get(incidentId) ??
      "Incident updated following operator Human-in-the-Loop review.";
    record.post_mortem_markdown = generateIncidentPostMortemMarkdown(
      record,
      summary,
      this.auditChain.getHeadHash()
    );
    return record;
  }
}
