/**
 * HYPERION Channel 06 — Hard Deterministic Safety Policy Engine, HITL Queue & Anti-Flapping Cooldown.
 *
 * Enforces mandatory guardrails across BOTH autonomous state-graph runs and direct MCP tool calls:
 * 1. Prompt-Injection & Metadata-Hijack Firewall (100% block of spoofed/unverified targets)
 * 2. Protected System Account Invariant (System Treasury & Insurance Fund immutable)
 * 3. Quantitative Forensic Provenance Verification
 * 4. Anti-Flapping Cooldown & Rolling Rate Cap
 * 5. Blast-Radius & Fixed-Point Notional Thresholds -> Human-in-the-Loop (HITL) Approval Queue
 * 6. Cryptographic Signing, Hash-Chained Auditing, Dry-Run Simulation & Deterministic Auto-Rollback
 */

import type { ImmutableAuditChain } from "../crypto/audit-chain.js";
import type { AegisCryptographicSigner } from "../crypto/signer.js";
import type { NanosecondClock } from "../domain/clock.js";
import {
  checkedAddI128,
  formatFixedPoint1e8,
  parseFixedPointUint
} from "../domain/fixed-point.js";
import type {
  AccountProfileMetadata,
  HitlApprovalRequest,
  InstrumentSymbol,
  MitigationActionKind,
  MitigationExecutionResult,
  PolicyEvaluationResult,
  RollbackExecutionResult,
  SafetyPolicyConfig,
  SignedMitigationEnvelope
} from "../domain/types.js";
import { PromptInjectionFirewall } from "./injection-firewall.js";

export interface VerifiedAccountForensics {
  accountId: string;
  symbol: InstrumentSymbol;
  severityScore: number;
  confidencePpm: number;
  inVerifiedCycleOrSpoof: boolean;
  tier: AccountProfileMetadata["tier"];
  estimatedEquityFp: bigint;
  openOrdersCount: number;
  recordedTsNs: bigint;
}

export interface ActiveMitigationRecord {
  commandEnvelope: SignedMitigationEnvelope;
  executedTsNs: bigint;
  expiresTsNs: bigint;
  rolledBack: boolean;
  rolledBackTsNs?: bigint;
}

export interface MitigationSinkDispatcher {
  dispatchMitigation(envelope: SignedMitigationEnvelope): Promise<void>;
}

export const DEFAULT_SAFETY_POLICY_CONFIG: SafetyPolicyConfig = {
  policy_version: "hyperion-aegis-policy-v1.0",
  max_auto_freeze_accounts: 3,
  max_auto_freeze_notional_fp: "5000000000000", // 50,000.00000000 USDT (scale 1e8)
  min_auto_mitigation_severity: 70,
  min_auto_mitigation_confidence_ppm: 750_000, // 75.00%
  min_volatility_guard_severity: 85,
  allow_autonomous_volatility_guard: false, // Market-wide regime shifts require HITL by default
  anti_flapping_cooldown_ms: 60_000, // 60s anti-flapping per (action, target)
  max_actions_per_rolling_minute: 10,
  protected_system_account_ids: ["1", "2"] // 1 = System Treasury, 2 = Insurance Fund
};

export class SafetyPolicyEngine {
  private readonly config: SafetyPolicyConfig;
  private readonly clock: NanosecondClock;
  private readonly signer: AegisCryptographicSigner;
  private readonly auditChain: ImmutableAuditChain;
  private readonly firewall: PromptInjectionFirewall;
  private readonly sink?: MitigationSinkDispatcher;
  private readonly onEventCallback?: (
    eventType:
      | "POLICY_VERDICT"
      | "HITL_APPROVAL_REQUIRED"
      | "HITL_DECISION_RECORDED"
      | "MITIGATION_EXECUTED"
      | "MITIGATION_ROLLED_BACK",
    incidentId: string,
    payload: Record<string, unknown>
  ) => void;

  private readonly verifiedEvidence = new Map<string, VerifiedAccountForensics>();
  private readonly symbolAlertSeverity = new Map<InstrumentSymbol, number>();
  private readonly hijackPoisonedAccountIds = new Set<string>();
  private readonly knownAccountProfiles = new Map<
    string,
    { tier: AccountProfileMetadata["tier"]; equityFp: bigint; openOrdersCount: number }
  >();

  private readonly cooldownLedger = new Map<string, bigint>();
  private readonly rollingExecutionTimestampsNs: bigint[] = [];
  private readonly hitlQueue = new Map<string, HitlApprovalRequest>();
  private readonly activeMitigations = new Map<string, ActiveMitigationRecord>();

  private commandSeq = 1;
  private approvalSeq = 1;

  public constructor(params: {
    config?: Partial<SafetyPolicyConfig>;
    clock: NanosecondClock;
    signer: AegisCryptographicSigner;
    auditChain: ImmutableAuditChain;
    firewall?: PromptInjectionFirewall;
    sink?: MitigationSinkDispatcher;
    onEvent?: (
      eventType:
        | "POLICY_VERDICT"
        | "HITL_APPROVAL_REQUIRED"
        | "HITL_DECISION_RECORDED"
        | "MITIGATION_EXECUTED"
        | "MITIGATION_ROLLED_BACK",
      incidentId: string,
      payload: Record<string, unknown>
    ) => void;
  }) {
    this.config = { ...DEFAULT_SAFETY_POLICY_CONFIG, ...params.config };
    this.clock = params.clock;
    this.signer = params.signer;
    this.auditChain = params.auditChain;
    this.firewall = params.firewall ?? new PromptInjectionFirewall();
    this.sink = params.sink;
    this.onEventCallback = params.onEvent;
  }

  public getConfig(): SafetyPolicyConfig {
    return {
      ...this.config,
      protected_system_account_ids: [...this.config.protected_system_account_ids]
    };
  }

  /**
   * Registers quantitative forensic evidence derived strictly from structured Sentinel-ML / Ledger data.
   */
  public registerVerifiedForensicEvidence(entry: VerifiedAccountForensics): void {
    this.verifiedEvidence.set(entry.accountId, entry);
    this.knownAccountProfiles.set(entry.accountId, {
      tier: entry.tier,
      equityFp: entry.estimatedEquityFp,
      openOrdersCount: entry.openOrdersCount
    });
    const currentSymSev = this.symbolAlertSeverity.get(entry.symbol) ?? 0;
    if (entry.severityScore > currentSymSev) {
      this.symbolAlertSeverity.set(entry.symbol, entry.severityScore);
    }
  }

  public registerSymbolAnomalySeverity(symbol: InstrumentSymbol, severityScore: number): void {
    const prev = this.symbolAlertSeverity.get(symbol) ?? 0;
    if (severityScore > prev) {
      this.symbolAlertSeverity.set(symbol, severityScore);
    }
  }

  public registerKnownAccountProfile(params: {
    accountId: string;
    tier: AccountProfileMetadata["tier"];
    equityFp: bigint;
    openOrdersCount: number;
  }): void {
    this.knownAccountProfiles.set(params.accountId, {
      tier: params.tier,
      equityFp: params.equityFp,
      openOrdersCount: params.openOrdersCount
    });
  }

  /**
   * Flags account IDs extracted from adversarial metadata prompt-injection strings.
   */
  public flagHijackPoisonedTargets(accountIds: readonly string[]): void {
    for (const id of accountIds) {
      this.hijackPoisonedAccountIds.add(id);
    }
  }

  public getPendingApprovals(): HitlApprovalRequest[] {
    return [...this.hitlQueue.values()].filter((req) => req.status === "PENDING");
  }

  public getAllApprovals(): HitlApprovalRequest[] {
    return [...this.hitlQueue.values()];
  }

  public getActiveMitigations(): ActiveMitigationRecord[] {
    return [...this.activeMitigations.values()].filter((m) => !m.rolledBack);
  }

  private computeBlastRadius(
    action: MitigationActionKind,
    targetAccountIds: readonly string[]
  ): PolicyEvaluationResult["estimated_blast_radius"] {
    let totalNotionalFp = 0n;
    let totalOpenOrders = 0;
    for (const acctId of targetAccountIds) {
      const profile = this.knownAccountProfiles.get(acctId);
      if (profile) {
        totalNotionalFp = checkedAddI128(totalNotionalFp, profile.equityFp);
        totalOpenOrders += profile.openOrdersCount;
      }
    }
    return {
      target_account_count: targetAccountIds.length,
      estimated_locked_notional_fp: totalNotionalFp.toString(10),
      estimated_locked_notional_decimal: formatFixedPoint1e8(totalNotionalFp),
      open_orders_impacted: totalOpenOrders,
      market_wide_impact: action === "ENABLE_VOLATILITY_GUARD" || action === "RESTORE_NORMAL"
    };
  }

  /**
   * Evaluates a proposed risk mitigation command against all hard safety invariants.
   */
  public evaluatePolicy(params: {
    incidentId: string;
    action: MitigationActionKind;
    targetAccountIds: string[];
    targetSymbol: InstrumentSymbol;
    reason: string;
    operatorApprovalToken?: string;
    nowNs?: bigint;
  }): PolicyEvaluationResult {
    const nowNs = params.nowNs ?? this.clock.nowNs();
    const blastRadius = this.computeBlastRadius(params.action, params.targetAccountIds);

    // Gate 1: Scan `reason` for embedded prompt-injection / bypass directives
    const directiveScan = this.firewall.containsAdversarialDirective(params.reason);
    if (directiveScan.adversarial) {
      return {
        verdict: "BLOCKED_PROMPT_INJECTION",
        allowed: false,
        requires_human_approval: false,
        rule_triggered: `GATE1_ADVERSARIAL_REASON:${directiveScan.matchedRuleIds.join(",")}`,
        rationale: `Mitigation rejected: reason field contains untrusted adversarial directive (${directiveScan.matchedRuleIds.join(", ")})`,
        estimated_blast_radius: blastRadius
      };
    }

    // Gate 2: Protected System Accounts (Treasury / Insurance Fund) can NEVER be targeted
    for (const acctId of params.targetAccountIds) {
      const profile = this.knownAccountProfiles.get(acctId);
      if (
        this.config.protected_system_account_ids.includes(acctId) ||
        profile?.tier === "SYSTEM_TREASURY" ||
        profile?.tier === "INSURANCE_FUND"
      ) {
        return {
          verdict: "BLOCKED_POLICY_VIOLATION",
          allowed: false,
          requires_human_approval: false,
          rule_triggered: "GATE2_PROTECTED_SYSTEM_ACCOUNT",
          rationale: `Mitigation hard-blocked: account ${acctId} is a protected system treasury/insurance account`,
          estimated_blast_radius: blastRadius
        };
      }
    }

    // Gate 3: Target Provenance & Anti-Hijack Verification
    const isAccountTargetedAction =
      params.action === "FREEZE_ACCOUNT" ||
      params.action === "THROTTLE_RATE_LIMIT" ||
      params.action === "CANCEL_OPEN_ORDERS";

    if (isAccountTargetedAction) {
      if (params.targetAccountIds.length === 0) {
        return {
          verdict: "BLOCKED_POLICY_VIOLATION",
          allowed: false,
          requires_human_approval: false,
          rule_triggered: "GATE3_EMPTY_ACCOUNT_TARGETS",
          rationale: `Action ${params.action} requires at least one verified target_account_id`,
          estimated_blast_radius: blastRadius
        };
      }

      for (const acctId of params.targetAccountIds) {
        const evidence = this.verifiedEvidence.get(acctId);
        const hasQuantitativeProof =
          evidence !== undefined &&
          evidence.inVerifiedCycleOrSpoof &&
          evidence.severityScore >= this.config.min_auto_mitigation_severity &&
          evidence.confidencePpm >= this.config.min_auto_mitigation_confidence_ppm;

        if (!hasQuantitativeProof && this.hijackPoisonedAccountIds.has(acctId)) {
          return {
            verdict: "BLOCKED_PROMPT_INJECTION",
            allowed: false,
            requires_human_approval: false,
            rule_triggered: "GATE3_METADATA_HIJACK_TARGET_BLOCKED",
            rationale: `Prompt-injection hijack blocked: target account ${acctId} originated from untrusted metadata injection and has zero verified quantitative fraud evidence`,
            estimated_blast_radius: blastRadius
          };
        }

        if (!hasQuantitativeProof) {
          return {
            verdict: "BLOCKED_POLICY_VIOLATION",
            allowed: false,
            requires_human_approval: false,
            rule_triggered: "GATE3_UNVERIFIED_FORENSIC_PROVENANCE",
            rationale: `Policy gate blocked ${params.action} on account ${acctId}: insufficient quantitative Sentinel-ML anomaly evidence (min severity=${this.config.min_auto_mitigation_severity}, min confidence=${this.config.min_auto_mitigation_confidence_ppm} ppm)`,
            estimated_blast_radius: blastRadius
          };
        }
      }
    }

    // Gate 4: Anti-Flapping Cooldown & Rolling Rate Limit (exempting RESTORE_NORMAL rollbacks)
    if (params.action !== "RESTORE_NORMAL") {
      const cooldownNs = BigInt(this.config.anti_flapping_cooldown_ms) * 1_000_000n;
      const targetKey =
        params.targetAccountIds.length > 0
          ? [...params.targetAccountIds].sort().join(",")
          : `SYMBOL:${params.targetSymbol}`;
      const compositeKey = `${params.action}:${params.targetSymbol}:${targetKey}`;
      const prevTsNs = this.cooldownLedger.get(compositeKey);
      if (prevTsNs !== undefined && nowNs - prevTsNs < cooldownNs) {
        const remainingNs = cooldownNs - (nowNs - prevTsNs);
        return {
          verdict: "BLOCKED_COOLDOWN",
          allowed: false,
          requires_human_approval: false,
          rule_triggered: "GATE4_ANTI_FLAPPING_COOLDOWN",
          rationale: `Anti-flapping cooldown active for ${compositeKey}; ${remainingNs.toString(10)} ns remaining`,
          estimated_blast_radius: blastRadius,
          cooldown_remaining_ns: remainingNs.toString(10)
        };
      }

      const oneMinuteAgoNs = nowNs - 60_000_000_000n;
      const recentCount = this.rollingExecutionTimestampsNs.filter((t) => t >= oneMinuteAgoNs).length;
      if (recentCount >= this.config.max_actions_per_rolling_minute) {
        return {
          verdict: "BLOCKED_COOLDOWN",
          allowed: false,
          requires_human_approval: false,
          rule_triggered: "GATE4_ROLLING_RATE_BUDGET_EXCEEDED",
          rationale: `Rolling 60s autonomous mitigation budget exhausted (${recentCount}/${this.config.max_actions_per_rolling_minute})`,
          estimated_blast_radius: blastRadius,
          cooldown_remaining_ns: "60000000000"
        };
      }
    }

    // Gate 5: Blast-Radius & Market-Wide Risk Thresholds -> HITL Approval Check
    const maxAutoNotionalFp = parseFixedPointUint(
      this.config.max_auto_freeze_notional_fp,
      "max_auto_freeze_notional_fp"
    );
    const currentBlastNotionalFp = parseFixedPointUint(
      blastRadius.estimated_locked_notional_fp,
      "estimated_locked_notional_fp"
    );
    const touchesDesignatedMarketMaker = params.targetAccountIds.some(
      (id) => this.knownAccountProfiles.get(id)?.tier === "DESIGNATED_MARKET_MAKER"
    );

    const hitlTriggers: string[] = [];
    if (
      params.action === "FREEZE_ACCOUNT" &&
      params.targetAccountIds.length > this.config.max_auto_freeze_accounts
    ) {
      hitlTriggers.push(
        `FREEZE_COUNT_EXCEEDS_MAX(${params.targetAccountIds.length}>${this.config.max_auto_freeze_accounts})`
      );
    }
    if (
      (params.action === "FREEZE_ACCOUNT" || params.action === "CANCEL_OPEN_ORDERS") &&
      currentBlastNotionalFp > maxAutoNotionalFp
    ) {
      hitlTriggers.push(
        `NOTIONAL_EXCEEDS_MAX(${formatFixedPoint1e8(currentBlastNotionalFp)}>${formatFixedPoint1e8(maxAutoNotionalFp)})`
      );
    }
    if (touchesDesignatedMarketMaker) {
      hitlTriggers.push("TARGET_IS_DESIGNATED_MARKET_MAKER");
    }
    if (params.action === "ENABLE_VOLATILITY_GUARD") {
      const symSev = this.symbolAlertSeverity.get(params.targetSymbol) ?? 0;
      if (!this.config.allow_autonomous_volatility_guard || symSev < this.config.min_volatility_guard_severity) {
        hitlTriggers.push(
          `MARKET_WIDE_VOLATILITY_GUARD_REQUIRES_HITL(severity=${symSev})`
        );
      }
    }

    if (hitlTriggers.length > 0) {
      if (params.operatorApprovalToken) {
        const verifiedClaims = this.signer.verifyHitlApprovalToken(params.operatorApprovalToken);
        const sortedExpected = [...params.targetAccountIds].sort().join(",");
        const sortedClaimed = verifiedClaims ? [...verifiedClaims.target_account_ids].sort().join(",") : "";
        if (
          verifiedClaims &&
          verifiedClaims.incident_id === params.incidentId &&
          verifiedClaims.action === params.action &&
          verifiedClaims.target_symbol === params.targetSymbol &&
          sortedClaimed === sortedExpected
        ) {
          return {
            verdict: "ALLOW_WITH_HITL_TOKEN",
            allowed: true,
            requires_human_approval: true,
            rule_triggered: `GATE5_HITL_TOKEN_VERIFIED:${verifiedClaims.approval_id}`,
            rationale: `High-impact mitigation authorized by cryptographic HITL token from operator ${verifiedClaims.approved_by}`,
            estimated_blast_radius: blastRadius
          };
        }
        return {
          verdict: "BLOCKED_POLICY_VIOLATION",
          allowed: false,
          requires_human_approval: true,
          rule_triggered: "GATE5_INVALID_HITL_APPROVAL_TOKEN",
          rationale: "Provided operator_approval_token failed Ed25519 verification or claim binding mismatch",
          estimated_blast_radius: blastRadius
        };
      }

      return {
        verdict: "REQUIRE_HITL_APPROVAL",
        allowed: false,
        requires_human_approval: true,
        rule_triggered: `GATE5_HITL_REQUIRED:${hitlTriggers.join("|")}`,
        rationale: `Destructive blast radius exceeds autonomous envelope (${hitlTriggers.join(", ")}); queued for Human-in-the-Loop approval`,
        estimated_blast_radius: blastRadius
      };
    }

    // Gate 6: Autonomous execution permitted
    return {
      verdict: "ALLOW_AUTONOMOUS",
      allowed: true,
      requires_human_approval: false,
      rule_triggered: "GATE6_AUTONOMOUS_ENVELOPE_VERIFIED",
      rationale: "Target provenance, blast radius, fixed-point notional cap, and cooldown checks passed",
      estimated_blast_radius: blastRadius
    };
  }

  /**
   * Evaluates policy, handles dry-run simulation or HITL queueing, signs canonical Protobuf
   * `RiskMitigationCommand`, appends to the BLAKE3 hash chain, and registers auto-rollback TTL.
   */
  public async executeOrQueueMitigation(params: {
    incidentId: string;
    sourceAlertId: string;
    action: MitigationActionKind;
    targetAccountIds: string[];
    targetSymbol: InstrumentSymbol;
    rateLimitOverrideRps?: number;
    cooldownDurationMs: number;
    dryRun: boolean;
    reason: string;
    operatorApprovalToken?: string;
    issuedBy?: string;
  }): Promise<MitigationExecutionResult> {
    // Opportunistically sweep any expired TTL mitigations before evaluating
    await this.processDueAutoRollbacks();

    const nowNs = this.clock.nowNs();
    const policyEval = this.evaluatePolicy({
      incidentId: params.incidentId,
      action: params.action,
      targetAccountIds: params.targetAccountIds,
      targetSymbol: params.targetSymbol,
      reason: params.reason,
      operatorApprovalToken: params.operatorApprovalToken,
      nowNs
    });

    this.onEventCallback?.("POLICY_VERDICT", params.incidentId, {
      action: params.action,
      target_account_ids: params.targetAccountIds,
      verdict: policyEval.verdict,
      rule_triggered: policyEval.rule_triggered
    });

    // Case A: Hard Blocked (Prompt Injection, Protected Account, Unverified Target, or Cooldown)
    if (
      policyEval.verdict === "BLOCKED_PROMPT_INJECTION" ||
      policyEval.verdict === "BLOCKED_POLICY_VIOLATION" ||
      policyEval.verdict === "BLOCKED_COOLDOWN"
    ) {
      const statusMap = {
        BLOCKED_PROMPT_INJECTION: "BLOCKED_PROMPT_INJECTION",
        BLOCKED_POLICY_VIOLATION: "BLOCKED_POLICY",
        BLOCKED_COOLDOWN: "BLOCKED_COOLDOWN"
      } as const;
      const mappedStatus = statusMap[policyEval.verdict];
      const auditEntry = this.auditChain.append({
        incidentId: params.incidentId,
        eventType: `MITIGATION_${mappedStatus}`,
        actor: params.issuedBy ?? "aegis-policy-gate",
        payload: {
          action: params.action,
          target_account_ids: params.targetAccountIds,
          target_symbol: params.targetSymbol,
          dry_run: params.dryRun,
          rule_triggered: policyEval.rule_triggered,
          rationale: policyEval.rationale
        },
        timestampNsOverride: nowNs
      });

      return {
        status: mappedStatus,
        policy_evaluation: policyEval,
        audit_seq: auditEntry.seq,
        audit_entry_blake3: auditEntry.entry_blake3
      };
    }

    // Case B: Dry-Run Simulation (even if HITL would be required on live execution, dry-run previews command & policy)
    if (params.dryRun) {
      const dryCmdId = `mit-dry-${String(this.commandSeq++).padStart(6, "0")}`;
      const expiresTsNs = nowNs + BigInt(params.cooldownDurationMs) * 1_000_000n;
      const auditEntry = this.auditChain.append({
        incidentId: params.incidentId,
        eventType: "MITIGATION_DRY_RUN_SIMULATED",
        actor: params.issuedBy ?? "aegis-autonomous-sre",
        payload: {
          command_id: dryCmdId,
          action: params.action,
          target_account_ids: params.targetAccountIds,
          target_symbol: params.targetSymbol,
          verdict: policyEval.verdict,
          blast_radius: policyEval.estimated_blast_radius
        },
        timestampNsOverride: nowNs
      });

      const previewEnvelope = this.signer.signRiskMitigationCommand({
        commandId: dryCmdId,
        sourceAlertId: params.sourceAlertId,
        incidentId: params.incidentId,
        action: params.action,
        targetAccountIds: params.targetAccountIds,
        targetSymbol: params.targetSymbol,
        rateLimitOverrideRps: params.rateLimitOverrideRps ?? (params.action === "THROTTLE_RATE_LIMIT" ? 5 : 0),
        cooldownDurationMs: params.cooldownDurationMs,
        dryRun: true,
        requiresHumanApproval: policyEval.requires_human_approval,
        reason: params.reason,
        issuedBy: params.issuedBy ?? "aegis-autonomous-sre",
        issuedTsNs: nowNs,
        expiresTsNs,
        auditChainBlake3Hex: auditEntry.entry_blake3,
        traceId: auditEntry.entry_blake3.slice(0, 32)
      });

      return {
        status: "DRY_RUN_SIMULATED",
        policy_evaluation: policyEval,
        signed_command: previewEnvelope,
        audit_seq: auditEntry.seq,
        audit_entry_blake3: auditEntry.entry_blake3
      };
    }

    // Case C: Live Execution Exceeds Blast Radius -> Enqueue Human-in-the-Loop Approval
    if (policyEval.verdict === "REQUIRE_HITL_APPROVAL") {
      const approvalId = `hitl-${String(this.approvalSeq++).padStart(5, "0")}`;
      const approvalReq: HitlApprovalRequest = {
        approval_id: approvalId,
        incident_id: params.incidentId,
        source_alert_id: params.sourceAlertId,
        proposed_action: params.action,
        target_account_ids: [...params.targetAccountIds],
        target_symbol: params.targetSymbol,
        rate_limit_override_rps: params.rateLimitOverrideRps,
        cooldown_duration_ms: params.cooldownDurationMs,
        reason: params.reason,
        policy_evaluation: policyEval,
        status: "PENDING",
        created_ts_ns: nowNs.toString(10)
      };
      this.hitlQueue.set(approvalId, approvalReq);

      const auditEntry = this.auditChain.append({
        incidentId: params.incidentId,
        eventType: "MITIGATION_QUEUED_HITL_APPROVAL",
        actor: params.issuedBy ?? "aegis-policy-gate",
        payload: {
          approval_id: approvalId,
          action: params.action,
          target_account_ids: params.targetAccountIds,
          target_symbol: params.targetSymbol,
          rule_triggered: policyEval.rule_triggered,
          blast_radius: policyEval.estimated_blast_radius
        },
        timestampNsOverride: nowNs
      });

      this.onEventCallback?.("HITL_APPROVAL_REQUIRED", params.incidentId, {
        approval_request: approvalReq
      });

      return {
        status: "QUEUED_HITL_APPROVAL",
        policy_evaluation: policyEval,
        approval_request: approvalReq,
        audit_seq: auditEntry.seq,
        audit_entry_blake3: auditEntry.entry_blake3
      };
    }

    // Case D: Live Execution Permitted (ALLOW_AUTONOMOUS or ALLOW_WITH_HITL_TOKEN)
    const cmdId = `mit-live-${String(this.commandSeq++).padStart(6, "0")}`;
    const expiresTsNs = nowNs + BigInt(params.cooldownDurationMs) * 1_000_000n;

    const auditEntry = this.auditChain.append({
      incidentId: params.incidentId,
      eventType: "MITIGATION_EXECUTED",
      actor: params.issuedBy ?? "aegis-autonomous-sre",
      payload: {
        command_id: cmdId,
        action: params.action,
        target_account_ids: params.targetAccountIds,
        target_symbol: params.targetSymbol,
        cooldown_duration_ms: params.cooldownDurationMs,
        verdict: policyEval.verdict
      },
      timestampNsOverride: nowNs
    });

    const signedEnvelope = this.signer.signRiskMitigationCommand({
      commandId: cmdId,
      sourceAlertId: params.sourceAlertId,
      incidentId: params.incidentId,
      action: params.action,
      targetAccountIds: params.targetAccountIds,
      targetSymbol: params.targetSymbol,
      rateLimitOverrideRps: params.rateLimitOverrideRps ?? (params.action === "THROTTLE_RATE_LIMIT" ? 5 : 0),
      cooldownDurationMs: params.cooldownDurationMs,
      dryRun: false,
      requiresHumanApproval: policyEval.requires_human_approval,
      reason: params.reason,
      issuedBy: params.issuedBy ?? "aegis-autonomous-sre",
      issuedTsNs: nowNs,
      expiresTsNs,
      auditChainBlake3Hex: auditEntry.entry_blake3,
      traceId: auditEntry.entry_blake3.slice(0, 32)
    });

    // Update anti-flapping cooldown state
    if (params.action !== "RESTORE_NORMAL") {
      const targetKey =
        params.targetAccountIds.length > 0
          ? [...params.targetAccountIds].sort().join(",")
          : `SYMBOL:${params.targetSymbol}`;
      const compositeKey = `${params.action}:${params.targetSymbol}:${targetKey}`;
      this.cooldownLedger.set(compositeKey, nowNs);
      this.rollingExecutionTimestampsNs.push(nowNs);

      this.activeMitigations.set(cmdId, {
        commandEnvelope: signedEnvelope,
        executedTsNs: nowNs,
        expiresTsNs,
        rolledBack: false
      });
    }

    if (this.sink) {
      await this.sink.dispatchMitigation(signedEnvelope);
    }

    this.onEventCallback?.("MITIGATION_EXECUTED", params.incidentId, {
      signed_command: signedEnvelope
    });

    return {
      status: "EXECUTED",
      policy_evaluation: policyEval,
      signed_command: signedEnvelope,
      auto_rollback_scheduled_at_ns:
        params.action !== "RESTORE_NORMAL" ? expiresTsNs.toString(10) : undefined,
      audit_seq: auditEntry.seq,
      audit_entry_blake3: auditEntry.entry_blake3
    };
  }

  /**
   * Approves a pending HITL request, issues a cryptographic `hitl1.*` token,
   * and immediately executes the approved mitigation.
   */
  public async approveHitlRequest(params: {
    approvalId: string;
    operatorId: string;
    note: string;
  }): Promise<{
    approval: HitlApprovalRequest;
    execution: MitigationExecutionResult;
  }> {
    const req = this.hitlQueue.get(params.approvalId);
    if (!req) {
      throw new Error(`HITL approval request not found: ${params.approvalId}`);
    }
    if (req.status !== "PENDING") {
      throw new Error(`HITL approval request ${params.approvalId} is already ${req.status}`);
    }

    const decidedTsNs = this.clock.nowNs();
    const token = this.signer.issueHitlApprovalToken({
      approval_id: req.approval_id,
      incident_id: req.incident_id,
      action: req.proposed_action,
      target_account_ids: req.target_account_ids,
      target_symbol: req.target_symbol,
      approved_by: params.operatorId,
      approved_ts_ns: decidedTsNs.toString(10)
    });

    req.status = "APPROVED";
    req.decided_ts_ns = decidedTsNs.toString(10);
    req.decided_by = params.operatorId;
    req.decision_note = params.note;
    req.approval_execution_token = token;

    this.auditChain.append({
      incidentId: req.incident_id,
      eventType: "HITL_REQUEST_APPROVED",
      actor: params.operatorId,
      payload: {
        approval_id: req.approval_id,
        note: params.note
      },
      timestampNsOverride: decidedTsNs
    });

    this.onEventCallback?.("HITL_DECISION_RECORDED", req.incident_id, {
      approval_id: req.approval_id,
      status: "APPROVED",
      decided_by: params.operatorId
    });

    const execution = await this.executeOrQueueMitigation({
      incidentId: req.incident_id,
      sourceAlertId: req.source_alert_id,
      action: req.proposed_action,
      targetAccountIds: req.target_account_ids,
      targetSymbol: req.target_symbol,
      rateLimitOverrideRps: req.rate_limit_override_rps,
      cooldownDurationMs: req.cooldown_duration_ms,
      dryRun: false,
      reason: `${req.reason} [HITL Approved by ${params.operatorId}: ${params.note}]`,
      operatorApprovalToken: token,
      issuedBy: `operator:${params.operatorId}`
    });

    return { approval: req, execution };
  }

  /**
   * Rejects a pending HITL request and records an immutable audit trail entry.
   */
  public rejectHitlRequest(params: {
    approvalId: string;
    operatorId: string;
    note: string;
  }): HitlApprovalRequest {
    const req = this.hitlQueue.get(params.approvalId);
    if (!req) {
      throw new Error(`HITL approval request not found: ${params.approvalId}`);
    }
    if (req.status !== "PENDING") {
      throw new Error(`HITL approval request ${params.approvalId} is already ${req.status}`);
    }

    const decidedTsNs = this.clock.nowNs();
    req.status = "REJECTED";
    req.decided_ts_ns = decidedTsNs.toString(10);
    req.decided_by = params.operatorId;
    req.decision_note = params.note;

    this.auditChain.append({
      incidentId: req.incident_id,
      eventType: "HITL_REQUEST_REJECTED",
      actor: params.operatorId,
      payload: {
        approval_id: req.approval_id,
        note: params.note
      },
      timestampNsOverride: decidedTsNs
    });

    this.onEventCallback?.("HITL_DECISION_RECORDED", req.incident_id, {
      approval_id: req.approval_id,
      status: "REJECTED",
      decided_by: params.operatorId
    });

    return req;
  }

  /**
   * Verifies a cryptographic `rbk1.*` rollback token and issues a signed `RESTORE_NORMAL` command.
   */
  public async rollbackByToken(
    rollbackToken: string,
    reason = "Manual rollback via cryptographic rollback token",
    trigger: "MANUAL_TOKEN" | "AUTO_TTL_EXPIRY" = "MANUAL_TOKEN"
  ): Promise<RollbackExecutionResult> {
    const claims = this.signer.verifyRollbackToken(rollbackToken);
    if (!claims) {
      throw new Error("Invalid or tampered rollback_token: Ed25519 signature verification failed");
    }

    const active = this.activeMitigations.get(claims.command_id);
    if (active?.rolledBack) {
      throw new Error(`Mitigation ${claims.command_id} has already been rolled back`);
    }

    const nowNs = this.clock.nowNs();
    const restoreCmdId = `mit-restore-${String(this.commandSeq++).padStart(6, "0")}`;

    const auditEntry = this.auditChain.append({
      incidentId: claims.incident_id,
      eventType: trigger === "AUTO_TTL_EXPIRY" ? "MITIGATION_AUTO_ROLLED_BACK" : "MITIGATION_MANUAL_ROLLED_BACK",
      actor: trigger === "AUTO_TTL_EXPIRY" ? "aegis-ttl-scheduler" : "aegis-rollback-controller",
      payload: {
        original_command_id: claims.command_id,
        restoration_command_id: restoreCmdId,
        restored_account_ids: claims.target_account_ids,
        target_symbol: claims.target_symbol,
        trigger,
        reason
      },
      timestampNsOverride: nowNs
    });

    const restoreEnvelope = this.signer.signRiskMitigationCommand({
      commandId: restoreCmdId,
      sourceAlertId: claims.source_alert_id,
      incidentId: claims.incident_id,
      action: "RESTORE_NORMAL",
      targetAccountIds: claims.target_account_ids,
      targetSymbol: claims.target_symbol,
      rateLimitOverrideRps: 0,
      cooldownDurationMs: 1_000,
      dryRun: false,
      requiresHumanApproval: false,
      reason,
      issuedBy: trigger === "AUTO_TTL_EXPIRY" ? "aegis-ttl-scheduler" : "aegis-rollback-controller",
      issuedTsNs: nowNs,
      expiresTsNs: nowNs + 60_000_000_000n,
      auditChainBlake3Hex: auditEntry.entry_blake3,
      traceId: auditEntry.entry_blake3.slice(0, 32)
    });

    if (active) {
      active.rolledBack = true;
      active.rolledBackTsNs = nowNs;
    }

    if (this.sink) {
      await this.sink.dispatchMitigation(restoreEnvelope);
    }

    this.onEventCallback?.("MITIGATION_ROLLED_BACK", claims.incident_id, {
      original_command_id: claims.command_id,
      restoration_command_id: restoreCmdId,
      trigger
    });

    return {
      rolled_back: true,
      original_command_id: claims.command_id,
      restoration_command_id: restoreCmdId,
      incident_id: claims.incident_id,
      restored_account_ids: [...claims.target_account_ids],
      target_symbol: claims.target_symbol,
      rolled_back_ts_ns: nowNs.toString(10),
      trigger,
      audit_seq: auditEntry.seq,
      audit_entry_blake3: auditEntry.entry_blake3
    };
  }

  /**
   * Deterministically evaluates all active mitigations against `clock.nowNs()` and executes
   * automatic `RESTORE_NORMAL` rollbacks for any expired TTLs.
   */
  public async processDueAutoRollbacks(): Promise<RollbackExecutionResult[]> {
    const nowNs = this.clock.nowNs();
    const rolledBackResults: RollbackExecutionResult[] = [];

    for (const active of this.activeMitigations.values()) {
      if (!active.rolledBack && nowNs >= active.expiresTsNs) {
        const res = await this.rollbackByToken(
          active.commandEnvelope.rollback_token,
          `Automatic TTL rollback after ${active.commandEnvelope.cooldown_duration_ms}ms cooldown`,
          "AUTO_TTL_EXPIRY"
        );
        rolledBackResults.push(res);
      }
    }

    return rolledBackResults;
  }
}
