/**
 * HYPERION Channel 06 — Strict Domain Models & Invariant Contracts.
 *
 * All monetary & asset quantities on JSON/DTO boundaries are stringified 1e8 fixed-point integers.
 * All timestamps are stringified unsigned 64-bit epoch nanoseconds (`ts_ns`).
 */

export type InstrumentSymbol = "BTC-USDT" | "ETH-USDT" | "SOL-USDT" | "HYPE-USDT";

export type AnomalyPatternKind =
  | "WASH_TRADING_CYCLE"
  | "SPOOFING_LAYERING"
  | "VELOCITY_SPIKE"
  | "MOMENTUM_IGNITION"
  | "LATENCY_ARBITRAGE";

export type MitigationActionKind =
  | "THROTTLE_RATE_LIMIT"
  | "FREEZE_ACCOUNT"
  | "CANCEL_OPEN_ORDERS"
  | "ENABLE_VOLATILITY_GUARD"
  | "RESTORE_NORMAL";

export type CircuitBreakerState = "CLOSED" | "HALF_OPEN" | "OPEN";

export type InvestigationPhase =
  | "OBSERVE"
  | "HYPOTHESIZE"
  | "VERIFY"
  | "POLICY_GATE"
  | "AWAITING_HITL_APPROVAL"
  | "REMEDIATE"
  | "POST_MORTEM"
  | "COMPLETED"
  | "FAILED";

export interface AccountAssetBalanceDto {
  asset: string;
  available_fp: string;
  locked_fp: string;
  total_fp: string;
}

export interface MerkleProofNodeDto {
  sibling_hash: string;
  is_left_sibling: boolean;
  depth: number;
}

export interface MerkleLedgerProofDto {
  engine_seq: string;
  account_id: string;
  balances: AccountAssetBalanceDto[];
  leaf_blake3_hash: string;
  state_root_blake3: string;
  proof_path: MerkleProofNodeDto[];
  system_total_deposits_fp: string;
  system_total_withdrawals_fp: string;
  system_total_user_equity_fp: string;
  treasury_fee_pool_fp: string;
  double_entry_balanced: boolean;
  proof_ts_ns: string;
}

export interface LedgerAuditVerificationResult {
  account_id: string;
  asset: string;
  engine_seq: string;
  merkle_proof_valid: boolean;
  computed_root_blake3: string;
  state_root_blake3: string;
  leaf_hash_verified: boolean;
  double_entry_balanced: boolean;
  account_line_balanced: boolean;
  conservation_delta_fp: string;
  account_balances: AccountAssetBalanceDto[];
  formatted_balances: Array<{
    asset: string;
    available_decimal: string;
    locked_decimal: string;
    total_decimal: string;
  }>;
  proof_ts_ns: string;
  verdict: "VERIFIED_SOLVENT" | "CRITICAL_LEDGER_INVARIANT_BREACH" | "MERKLE_PROOF_TAMPERED";
}

export interface TransactionGraphEdgeDto {
  edge_id: string;
  fill_id: string;
  engine_seq: string;
  symbol: InstrumentSymbol;
  maker_account_id: string;
  taker_account_id: string;
  aggressor_side: "BUY" | "SELL";
  price_fp: string;
  qty_fp: string;
  notional_fp: string;
  exec_ts_ns: string;
  prev_interaction_delta_ns: string;
  temporal_decay_weight_ppm: number;
}

export interface WashTradingCycleDto {
  cycle_id: string;
  participant_account_ids: string[];
  cycle_length: number;
  circular_volume_fp: string;
  net_position_delta_fp: string;
  cycle_duration_ns: string;
  volume_symmetry_ppm: number;
}

export interface SpoofingLayeringDto {
  suspect_account_id: string;
  spoofed_side: "BUY" | "SELL";
  layered_level_count: number;
  canceled_spoof_qty_fp: string;
  opposite_side_exec_qty_fp: string;
  cancel_to_fill_latency_ns: string;
  order_to_trade_ratio_ppm: number;
  best_quote_distance_bps: number;
}

export interface ShapFeatureAttributionDto {
  feature_name: string;
  feature_value_fp: string;
  baseline_value_fp: string;
  shap_contribution_ppm: number;
  importance_rank: number;
}

export interface AnomalyAlertDto {
  alert_id: string;
  engine_seq: string;
  symbol: InstrumentSymbol;
  pattern_type: AnomalyPatternKind;
  severity_score: number;
  confidence_ppm: number;
  autoencoder_recon_error_ppm: number;
  isolation_forest_score_ppm: number;
  offender_account_ids: string[];
  shap_attributions: ShapFeatureAttributionDto[];
  wash_trading_cycle?: WashTradingCycleDto;
  spoofing_layering?: SpoofingLayeringDto;
  detected_ts_ns: string;
  inference_latency_us: number;
  model_version: string;
}

export interface AccountProfileMetadata {
  account_id: string;
  tier: "RETAIL" | "PRO" | "DESIGNATED_MARKET_MAKER" | "SYSTEM_TREASURY" | "INSURANCE_FUND";
  raw_alias: string;
  raw_client_memo: string;
  open_orders_count: number;
}

export interface AccountSubgraphNodeDto {
  account_id: string;
  tier: AccountProfileMetadata["tier"];
  sanitized_alias: string;
  sanitized_client_memo: string;
  max_severity_score: number;
  order_to_trade_ratio_ppm: number;
  cancel_rate_ppm: number;
  traded_volume_fp: string;
  active_alert_count: number;
  open_orders_count: number;
  metadata_tainted: boolean;
}

export interface QuarantinedInjectionFinding {
  account_id: string;
  field_name: "raw_alias" | "raw_client_memo" | "reason" | "external_note";
  rule_id: string;
  severity: "HIGH" | "CRITICAL";
  matched_excerpt: string;
  raw_blake3_digest: string;
  hijack_target_accounts_extracted: string[];
}

export interface SecurityTaintReport {
  has_untrusted_injection_attempt: boolean;
  quarantined_findings: QuarantinedInjectionFinding[];
  verified_offender_account_ids: string[];
  protected_or_unverified_account_ids: string[];
}

export interface FraudSubgraphInspectionResult {
  center_account_id: string;
  symbol: InstrumentSymbol;
  nodes: AccountSubgraphNodeDto[];
  edges: TransactionGraphEdgeDto[];
  detected_cycles: WashTradingCycleDto[];
  related_alerts: AnomalyAlertDto[];
  top_shap_attributions: ShapFeatureAttributionDto[];
  security_taint_report: SecurityTaintReport;
  snapshot_ts_ns: string;
}

export interface ServiceLatencySnapshot {
  service: string;
  healthy: boolean;
  p50_latency_us: number;
  p99_latency_us: number;
  p999_latency_us: number;
  error_rate_bps: number;
  throughput_rps: number;
}

export interface DatabaseTelemetrySnapshot {
  db_cluster: string;
  write_batch_p50_us: number;
  write_batch_p99_us: number;
  pool_saturation_bps: number;
  active_connections: number;
  hypertable_chunk_healthy: boolean;
}

export interface RedpandaTopicLagSnapshot {
  topic: string;
  consumer_group: string;
  total_lag_messages: string;
  max_partition_lag: string;
  oldest_unconsumed_age_ms: number;
}

export interface CircuitBreakerSnapshot {
  upstream_target: string;
  state: CircuitBreakerState;
  trip_count: number;
  recent_error_rate_bps: number;
  last_state_change_ts_ns: string;
}

export interface SystemTelemetryAnalysisResult {
  overall_health: "HEALTHY" | "DEGRADED" | "CRITICAL";
  service_metrics: ServiceLatencySnapshot[];
  database_telemetry: DatabaseTelemetrySnapshot;
  redpanda_consumer_lag: RedpandaTopicLagSnapshot[];
  circuit_breakers: CircuitBreakerSnapshot[];
  anomalies_detected: string[];
  captured_ts_ns: string;
}

export interface SafetyPolicyConfig {
  policy_version: string;
  max_auto_freeze_accounts: number;
  max_auto_freeze_notional_fp: string;
  min_auto_mitigation_severity: number;
  min_auto_mitigation_confidence_ppm: number;
  min_volatility_guard_severity: number;
  allow_autonomous_volatility_guard: boolean;
  anti_flapping_cooldown_ms: number;
  max_actions_per_rolling_minute: number;
  protected_system_account_ids: string[];
}

export type PolicyGateVerdictKind =
  | "ALLOW_AUTONOMOUS"
  | "ALLOW_WITH_HITL_TOKEN"
  | "REQUIRE_HITL_APPROVAL"
  | "BLOCKED_POLICY_VIOLATION"
  | "BLOCKED_PROMPT_INJECTION"
  | "BLOCKED_COOLDOWN";

export interface PolicyEvaluationResult {
  verdict: PolicyGateVerdictKind;
  allowed: boolean;
  requires_human_approval: boolean;
  rule_triggered: string;
  rationale: string;
  estimated_blast_radius: {
    target_account_count: number;
    estimated_locked_notional_fp: string;
    estimated_locked_notional_decimal: string;
    open_orders_impacted: number;
    market_wide_impact: boolean;
  };
  cooldown_remaining_ns?: string;
}

export interface HitlApprovalRequest {
  approval_id: string;
  incident_id: string;
  source_alert_id: string;
  proposed_action: MitigationActionKind;
  target_account_ids: string[];
  target_symbol: InstrumentSymbol;
  rate_limit_override_rps?: number;
  cooldown_duration_ms: number;
  reason: string;
  policy_evaluation: PolicyEvaluationResult;
  status: "PENDING" | "APPROVED" | "REJECTED" | "EXPIRED";
  created_ts_ns: string;
  decided_ts_ns?: string;
  decided_by?: string;
  decision_note?: string;
  approval_execution_token?: string;
}

export interface SignedMitigationEnvelope {
  command_id: string;
  source_alert_id: string;
  incident_id: string;
  action: MitigationActionKind;
  target_account_ids: string[];
  target_symbol: InstrumentSymbol;
  rate_limit_override_rps: number;
  cooldown_duration_ms: number;
  dry_run: boolean;
  requires_human_approval: boolean;
  rollback_token: string;
  reason: string;
  issued_by: string;
  issued_ts_ns: string;
  expires_ts_ns: string;
  ed25519_signature_hex: string;
  ed25519_public_key_hex: string;
  audit_chain_blake3_hex: string;
  protobuf_wire_base64: string;
  gateway_auth_headers: Record<string, string>;
}

export interface MitigationExecutionResult {
  status:
    | "EXECUTED"
    | "DRY_RUN_SIMULATED"
    | "QUEUED_HITL_APPROVAL"
    | "BLOCKED_POLICY"
    | "BLOCKED_PROMPT_INJECTION"
    | "BLOCKED_COOLDOWN";
  policy_evaluation: PolicyEvaluationResult;
  signed_command?: SignedMitigationEnvelope;
  approval_request?: HitlApprovalRequest;
  auto_rollback_scheduled_at_ns?: string;
  audit_seq: number;
  audit_entry_blake3: string;
}

export interface RollbackExecutionResult {
  rolled_back: boolean;
  original_command_id: string;
  restoration_command_id: string;
  incident_id: string;
  restored_account_ids: string[];
  target_symbol: InstrumentSymbol;
  rolled_back_ts_ns: string;
  trigger: "MANUAL_TOKEN" | "AUTO_TTL_EXPIRY";
  audit_seq: number;
  audit_entry_blake3: string;
}

export interface InvestigationHypothesis {
  hypothesis_id: string;
  title: string;
  category: "MARKET_MANIPULATION" | "INFRASTRUCTURE_DEGRADATION" | "ADVERSARIAL_PROMPT_INJECTION" | "LEDGER_INTEGRITY";
  prior_confidence_ppm: number;
  posterior_confidence_ppm?: number;
  status: "PROPOSED" | "CONFIRMED" | "REFUTED";
  verification_plan: string[];
  supporting_evidence: string[];
}

export interface InvestigationTimelineEvent {
  step_index: number;
  ts_ns: string;
  delta_from_start_ns: string;
  phase: InvestigationPhase;
  event_type: string;
  summary: string;
  details: Record<string, unknown>;
  audit_entry_blake3: string;
}

export interface ProposedMitigationPlanItem {
  action: MitigationActionKind;
  target_account_ids: string[];
  target_symbol: InstrumentSymbol;
  rate_limit_override_rps?: number;
  cooldown_duration_ms: number;
  reason: string;
  dry_run_first: boolean;
}

export interface InvestigationRecord {
  incident_id: string;
  trigger_alert: AnomalyAlertDto;
  reasoning_mode_used: "LLM_GEMINI" | "LLM_OPENAI_COMPATIBLE" | "DETERMINISTIC_RULE_ENGINE";
  llm_fallback_triggered: boolean;
  llm_fallback_reason?: string;
  current_phase: InvestigationPhase;
  started_ts_ns: string;
  completed_ts_ns?: string;
  hypotheses: InvestigationHypothesis[];
  ledger_audits: LedgerAuditVerificationResult[];
  subgraph_inspection?: FraudSubgraphInspectionResult;
  telemetry_analysis?: SystemTelemetryAnalysisResult;
  dry_run_results: MitigationExecutionResult[];
  mitigation_outcomes: MitigationExecutionResult[];
  pending_approvals: HitlApprovalRequest[];
  timeline: InvestigationTimelineEvent[];
  post_mortem_markdown?: string;
}

export interface SseIncidentStreamEvent {
  event_id: string;
  event_type:
    | "INCIDENT_STARTED"
    | "PHASE_TRANSITION"
    | "MCP_TOOL_INVOKED"
    | "INJECTION_QUARANTINED"
    | "POLICY_VERDICT"
    | "HITL_APPROVAL_REQUIRED"
    | "HITL_DECISION_RECORDED"
    | "MITIGATION_EXECUTED"
    | "MITIGATION_ROLLED_BACK"
    | "POST_MORTEM_READY";
  incident_id: string;
  ts_ns: string;
  payload: Record<string, unknown>;
}
