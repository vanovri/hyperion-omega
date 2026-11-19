/**
 * HYPERION Channel 06 — Automated Incident Post-Mortem Markdown Generator.
 *
 * Produces an institutional-grade Markdown Post-Mortem featuring:
 * - Nanosecond-precision chronological timeline (`ts_ns`, `+Δns`, ISO-8601 ns, BLAKE3 audit hash)
 * - Valid Mermaid causality diagram (`flowchart TD` with quoted labels)
 * - Quantitative forensic & system telemetry proof tables (exact 1e8 fixed-point formatting)
 * - Cryptographic Rollback Tokens (`rbk1.*`) & executable CLI/REST/UI cancellation controls
 */

import { formatEpochNsIso } from "../domain/clock.js";
import {
  formatBpsAsPercent,
  formatFixedPoint1e8,
  formatPpmAsPercent
} from "../domain/fixed-point.js";
import type { InvestigationRecord } from "../domain/types.js";

function sanitizeMermaidLabel(raw: string): string {
  return raw.replace(/"/g, "'").replace(/[\r\n]+/g, " ").trim();
}

export function generateIncidentPostMortemMarkdown(
  record: InvestigationRecord,
  rootCauseSummary: string,
  auditHeadBlake3: string
): string {
  const startNs = BigInt(record.started_ts_ns);
  const endNs = record.completed_ts_ns ? BigInt(record.completed_ts_ns) : startNs;
  const totalDurationNs = endNs >= startNs ? endNs - startNs : 0n;
  const totalDurationUs = totalDurationNs / 1_000n;

  const lines: string[] = [];

  lines.push(`# HYPERION Autonomous Incident Post-Mortem — \`${record.incident_id}\``);
  lines.push("");
  lines.push("## 1. Executive Summary");
  lines.push("");
  lines.push(`| Field | Value |`);
  lines.push(`|---|---|`);
  lines.push(`| **Incident ID** | \`${record.incident_id}\` |`);
  lines.push(`| **Trigger Alert** | \`${record.trigger_alert.alert_id}\` (\`${record.trigger_alert.pattern_type}\` on \`${record.trigger_alert.symbol}\`) |`);
  lines.push(`| **Severity / Confidence** | \`${record.trigger_alert.severity_score}/100\` · \`${formatPpmAsPercent(record.trigger_alert.confidence_ppm)}\` (\`${record.trigger_alert.confidence_ppm} ppm\`) |`);
  lines.push(`| **Reasoning Engine** | \`${record.reasoning_mode_used}\` (fallback=\`${record.llm_fallback_triggered}\`) |`);
  lines.push(`| **Terminal State** | \`${record.current_phase}\` |`);
  lines.push(`| **Start Timestamp (ns)** | \`${record.started_ts_ns}\` (\`${formatEpochNsIso(startNs)}\`) |`);
  lines.push(`| **End Timestamp (ns)** | \`${record.completed_ts_ns ?? record.started_ts_ns}\` (\`${formatEpochNsIso(endNs)}\`) |`);
  lines.push(`| **End-to-End Duration** | \`${totalDurationNs.toString(10)} ns\` (\`${totalDurationUs.toString(10)} us\`) |`);
  lines.push(`| **Audit Chain Head (BLAKE3)** | \`${auditHeadBlake3}\` |`);
  lines.push("");
  lines.push(`**Root Cause Determination**: ${rootCauseSummary}`);
  lines.push("");

  // 2. Mermaid Causality Flowchart (strict quoted labels, zero HTML tags)
  lines.push("## 2. Causality & Guardrail Decision Graph");
  lines.push("");
  lines.push("```mermaid");
  lines.push("flowchart TD");
  lines.push(
    `  ALERT["Trigger Alert: ${sanitizeMermaidLabel(record.trigger_alert.alert_id)} (${record.trigger_alert.pattern_type} sev=${record.trigger_alert.severity_score})"]`
  );
  lines.push(
    `  OBS["Observe & Hypothesize (${record.hypotheses.length} Hypotheses via ${record.reasoning_mode_used})"]`
  );
  lines.push(`  ALERT --> OBS`);

  if (record.subgraph_inspection) {
    const verifiedList =
      record.subgraph_inspection.security_taint_report.verified_offender_account_ids.join(", ") || "none";
    lines.push(
      `  SUBGRAPH["MCP inspect_fraud_subgraph: Verified=[${sanitizeMermaidLabel(verifiedList)}] Cycles=${record.subgraph_inspection.detected_cycles.length}"]`
    );
    lines.push(`  OBS --> SUBGRAPH`);

    if (record.subgraph_inspection.security_taint_report.has_untrusted_injection_attempt) {
      const qCount = record.subgraph_inspection.security_taint_report.quarantined_findings.length;
      lines.push(
        `  FW["Prompt-Injection Firewall: Quarantined ${qCount} Untrusted Metadata Payload(s)"]`
      );
      lines.push(`  SUBGRAPH --> FW`);
      lines.push(`  FW --> GATE["Hard Safety Policy Gate (Zero-Trust Provenance & Blast Radius)"]`);
    } else {
      lines.push(`  SUBGRAPH --> GATE["Hard Safety Policy Gate (Zero-Trust Provenance & Blast Radius)"]`);
    }
  } else {
    lines.push(`  OBS --> GATE["Hard Safety Policy Gate (Zero-Trust Provenance & Blast Radius)"]`);
  }

  if (record.telemetry_analysis) {
    lines.push(
      `  TELEM["MCP analyze_system_telemetry: Mesh=${record.telemetry_analysis.overall_health} DB_p99=${record.telemetry_analysis.database_telemetry.write_batch_p99_us}us"]`
    );
    lines.push(`  OBS --> TELEM`);
    lines.push(`  TELEM --> GATE`);
  }

  if (record.ledger_audits.length > 0) {
    const allSolvent = record.ledger_audits.every((a) => a.verdict === "VERIFIED_SOLVENT");
    lines.push(
      `  MERKLE["MCP query_ledger_merkle_audit: ${record.ledger_audits.length} Account(s) Solvent=${allSolvent}"]`
    );
    lines.push(`  OBS --> MERKLE`);
    lines.push(`  MERKLE --> GATE`);
  }

  record.mitigation_outcomes.forEach((outcome, idx) => {
    const nodeId = `MIT_${idx + 1}`;
    const targetStr =
      outcome.signed_command?.target_account_ids.join(",") ||
      outcome.approval_request?.target_account_ids.join(",") ||
      record.trigger_alert.symbol;
    lines.push(
      `  ${nodeId}["Outcome #${idx + 1}: ${outcome.status} (${outcome.policy_evaluation.verdict} targets=${sanitizeMermaidLabel(targetStr)})"]`
    );
    lines.push(`  GATE --> ${nodeId}`);
  });
  lines.push("```");
  lines.push("");

  // 3. Nanosecond Chronological Timeline
  lines.push("## 3. Nanosecond Chronological Investigation Timeline");
  lines.push("");
  lines.push("| Step | Timestamp (`ts_ns`) | `+Δns` | Phase | Event Type | Summary | Audit BLAKE3 (Prefix) |");
  lines.push("|---:|---:|---:|---|---|---|---|");
  for (const ev of record.timeline) {
    lines.push(
      `| ${ev.step_index} | \`${ev.ts_ns}\` | \`+${ev.delta_from_start_ns}\` | \`${ev.phase}\` | \`${ev.event_type}\` | ${ev.summary.replace(/\|/g, "\\|")} | \`${ev.audit_entry_blake3.slice(0, 16)}\` |`
    );
  }
  lines.push("");

  // 4. Quantitative Evidence & Metrics Proof
  lines.push("## 4. Quantitative Forensic, Telemetry & Merkle Ledger Evidence");
  lines.push("");

  if (record.subgraph_inspection) {
    lines.push("### 4.1 Sentinel-ML Subgraph & SHAP Feature Attributions");
    lines.push("");
    for (const cycle of record.subgraph_inspection.detected_cycles) {
      lines.push(
        `- **Cycle \`${cycle.cycle_id}\`**: Ring \`${cycle.participant_account_ids.join(" -> ")}\` · Circular Volume = \`${formatFixedPoint1e8(BigInt(cycle.circular_volume_fp))} USDT\` (\`${cycle.circular_volume_fp}\` fp) · Net Inventory Delta = \`${formatFixedPoint1e8(BigInt(cycle.net_position_delta_fp))}\` · Leg Symmetry = \`${formatPpmAsPercent(cycle.volume_symmetry_ppm)}\``
      );
    }
    if (record.subgraph_inspection.top_shap_attributions.length > 0) {
      lines.push("");
      lines.push("| Rank | Feature Name | Observed (`1e8` fp) | Baseline (`1e8` fp) | SHAP Contribution |");
      lines.push("|---:|---|---:|---:|---:|");
      for (const s of record.subgraph_inspection.top_shap_attributions) {
        lines.push(
          `| ${s.importance_rank} | \`${s.feature_name}\` | \`${s.feature_value_fp}\` | \`${s.baseline_value_fp}\` | \`${formatPpmAsPercent(s.shap_contribution_ppm)}\` (\`${s.shap_contribution_ppm} ppm\`) |`
        );
      }
    }
    if (record.subgraph_inspection.security_taint_report.has_untrusted_injection_attempt) {
      lines.push("");
      lines.push("### 4.2 Adversarial Metadata Prompt-Injection Quarantine Report");
      lines.push("");
      for (const q of record.subgraph_inspection.security_taint_report.quarantined_findings) {
        lines.push(
          `- **Quarantined on Account \`${q.account_id}\` (\`${q.field_name}\`)**: Rule \`${q.rule_id}\` (\`${q.severity}\`), Raw BLAKE3=\`${q.raw_blake3_digest}\`, Extracted Spoofed Targets=\`[${q.hijack_target_accounts_extracted.join(", ") || "none"}]\``
        );
      }
    }
    lines.push("");
  }

  if (record.telemetry_analysis) {
    const t = record.telemetry_analysis;
    lines.push("### 4.3 System Telemetry, TimescaleDB & Redpanda Lag Snapshot");
    lines.push("");
    lines.push(
      `- **Overall Mesh Health**: \`${t.overall_health}\` · **TimescaleDB Write Batch p99**: \`${t.database_telemetry.write_batch_p99_us} us\` · **DB Pool Saturation**: \`${formatBpsAsPercent(t.database_telemetry.pool_saturation_bps)}\``
    );
    lines.push("");
    lines.push("| Service | Healthy | p50 (`us`) | p99 (`us`) | p99.9 (`us`) | Error Rate | Throughput (`rps`) |");
    lines.push("|---|---|---:|---:|---:|---:|---:|");
    for (const svc of t.service_metrics) {
      lines.push(
        `| \`${svc.service}\` | \`${svc.healthy}\` | ${svc.p50_latency_us} | ${svc.p99_latency_us} | ${svc.p999_latency_us} | ${formatBpsAsPercent(svc.error_rate_bps)} | ${svc.throughput_rps} |`
      );
    }
    lines.push("");
  }

  if (record.ledger_audits.length > 0) {
    lines.push("### 4.4 Double-Entry Conservation & BLAKE3 Merkle Proofs");
    lines.push("");
    lines.push("| Account ID | Engine Seq | Verdict | Merkle Valid | Conservation Delta (`1e-8` ticks) | State Root BLAKE3 |");
    lines.push("|---|---:|---|---|---:|---|");
    for (const audit of record.ledger_audits) {
      lines.push(
        `| \`${audit.account_id}\` | \`${audit.engine_seq}\` | \`${audit.verdict}\` | \`${audit.merkle_proof_valid}\` | \`${audit.conservation_delta_fp}\` | \`${audit.state_root_blake3.slice(0, 24)}...\` |`
      );
    }
    lines.push("");
  }

  // 5. Executed Mitigations, Pending HITL Queue & Cryptographic Rollback Controls
  lines.push("## 5. Mitigation Actions & Cryptographic Rollback Controls");
  lines.push("");

  const executedCommands = record.mitigation_outcomes
    .filter((m) => m.status === "EXECUTED" && m.signed_command !== undefined)
    .map((m) => m.signed_command!);

  if (executedCommands.length > 0) {
    for (const cmd of executedCommands) {
      lines.push(`### Active Mitigation \`${cmd.command_id}\` (\`${cmd.action}\`)`);
      lines.push(`- **Target Accounts**: \`[${cmd.target_account_ids.join(", ")}]\` on \`${cmd.target_symbol}\``);
      lines.push(`- **Issued / Auto-Rollback Expiry (` + "`ts_ns`" + `)**: \`${cmd.issued_ts_ns}\` -> \`${cmd.expires_ts_ns}\` (TTL \`${cmd.cooldown_duration_ms} ms\`)`);
      lines.push(`- **Ed25519 Signature**: \`${cmd.ed25519_signature_hex}\``);
      lines.push(`- **Rollback Token**: \`${cmd.rollback_token}\``);
      lines.push("");
      lines.push("```bash");
      lines.push("# Instant Operator Rollback Command:");
      lines.push(`curl -sS -X POST http://localhost:8085/api/v1/aegis/mitigations/rollback \\`);
      lines.push(`  -H "Content-Type: application/json" \\`);
      lines.push(
        `  -d '{"rollback_token":"${cmd.rollback_token}","reason":"Operator post-incident manual restoration"}'`
      );
      lines.push("```");
      lines.push("");
    }
  } else {
    lines.push("_No autonomous live mitigations executed without human sign-off._");
    lines.push("");
  }

  if (record.pending_approvals.length > 0) {
    lines.push("### Pending Human-in-the-Loop (HITL) Approval Queue");
    lines.push("");
    for (const pending of record.pending_approvals) {
      lines.push(
        `- **Approval ID \`${pending.approval_id}\` (\`${pending.status}\`)**: Proposed \`${pending.proposed_action}\` on \`[${pending.target_account_ids.join(", ")}]\` (\`${pending.target_symbol}\`) — Trigger: \`${pending.policy_evaluation.rule_triggered}\` (Estimated Locked Notional: \`${pending.policy_evaluation.estimated_blast_radius.estimated_locked_notional_decimal} USDT\`)`
      );
    }
    lines.push("");
  }

  return lines.join("\n");
}
