/**
 * HYPERION Channel 06 — Strict Zod Schemas (`.strict()`) for MCP Tools, LLM Structured Output & REST API.
 */

import { z } from "zod";

export const Uint64StringSchema = z
  .string()
  .regex(/^[0-9]{1,20}$/, "Must be an unsigned 64-bit decimal integer string");

export const FixedPointUintSchema = z
  .string()
  .regex(/^[0-9]{1,39}$/, "Must be an unsigned 1e8 fixed-point integer string (no decimals/floats)");

export const FixedPointIntSchema = z
  .string()
  .regex(/^-?[0-9]{1,39}$/, "Must be a signed 1e8 fixed-point integer string (no decimals/floats)");

export const HexDigest32Schema = z
  .string()
  .regex(/^[0-9a-f]{64}$/, "Must be a 64-character lowercase hexadecimal 32-byte Blake3 digest");

export const InstrumentSymbolSchema = z.enum(["BTC-USDT", "ETH-USDT", "SOL-USDT", "HYPE-USDT"]);

export const MitigationActionSchema = z.enum([
  "THROTTLE_RATE_LIMIT",
  "FREEZE_ACCOUNT",
  "CANCEL_OPEN_ORDERS",
  "ENABLE_VOLATILITY_GUARD",
  "RESTORE_NORMAL"
]);

export const ResponseFormatSchema = z
  .enum(["json", "markdown"])
  .default("json")
  .describe("Output format: 'json' for structured machine consumption or 'markdown' for human operators");

// ============================================================================
// 1. MCP TOOL INPUT SCHEMAS (.strict())
// ============================================================================

export const QueryLedgerMerkleAuditInputShape = {
  account_id: Uint64StringSchema.describe("Target trading account ID as uint64 decimal string (e.g. '9001')"),
  asset: z
    .string()
    .min(2)
    .max(16)
    .default("USDT")
    .describe("Asset ticker to verify conservation law against (default: 'USDT')"),
  at_engine_seq: Uint64StringSchema.optional().describe(
    "Optional historical matching engine sequence ID; omit for latest committed state"
  ),
  expected_state_root_blake3: HexDigest32Schema.optional().describe(
    "Optional expected 32-byte Blake3 state root hex to cross-verify against"
  ),
  response_format: ResponseFormatSchema
};

export const QueryLedgerMerkleAuditInputSchema = z.object(QueryLedgerMerkleAuditInputShape).strict();
export type QueryLedgerMerkleAuditInput = z.infer<typeof QueryLedgerMerkleAuditInputSchema>;

export const InspectFraudSubgraphInputShape = {
  center_account_id: Uint64StringSchema.describe("Seed suspect account ID as uint64 decimal string (e.g. '9001')"),
  symbol: InstrumentSymbolSchema.describe("Canonical instrument symbol (e.g. 'BTC-USDT')"),
  max_hops: z
    .number()
    .int()
    .min(1)
    .max(6)
    .default(3)
    .describe("Maximum directed multigraph traversal hops (bounded k <= 6)"),
  lookback_window_ns: Uint64StringSchema.default("60000000000").describe(
    "Temporal sliding window in nanoseconds (default: 60s = '60000000000')"
  ),
  response_format: ResponseFormatSchema
};

export const InspectFraudSubgraphInputSchema = z.object(InspectFraudSubgraphInputShape).strict();
export type InspectFraudSubgraphInput = z.infer<typeof InspectFraudSubgraphInputSchema>;

export const AnalyzeSystemTelemetryInputShape = {
  services: z
    .array(
      z.enum([
        "ledger-rs",
        "gateway-go",
        "stream-aggregator",
        "sentinel-ml",
        "aegis-agent",
        "chaos-sim",
        "chronos-replay"
      ])
    )
    .optional()
    .describe("Optional subset of microservices to inspect; omit to inspect full mesh"),
  include_redpanda_lag: z
    .boolean()
    .default(true)
    .describe("Include Redpanda consumer lag metrics across partitions"),
  include_circuit_breakers: z
    .boolean()
    .default(true)
    .describe("Include gateway-go adaptive gRPC circuit breaker states"),
  window_seconds: z
    .number()
    .int()
    .min(5)
    .max(3600)
    .default(60)
    .describe("Prometheus rate/quantile evaluation window in seconds"),
  response_format: ResponseFormatSchema
};

export const AnalyzeSystemTelemetryInputSchema = z.object(AnalyzeSystemTelemetryInputShape).strict();
export type AnalyzeSystemTelemetryInput = z.infer<typeof AnalyzeSystemTelemetryInputSchema>;

export const ExecuteRiskMitigationInputShape = {
  incident_id: z
    .string()
    .min(4)
    .max(96)
    .regex(/^[A-Za-z0-9._:-]+$/, "Incident ID must be alphanumeric/safe-token")
    .describe("Deterministic incident correlation identifier (e.g. 'INC-20261118-0001')"),
  source_alert_id: z
    .string()
    .min(3)
    .max(96)
    .regex(/^[A-Za-z0-9._:-]+$/, "Source alert ID must be alphanumeric/safe-token")
    .describe("Originating Sentinel-ML alert ID"),
  action: MitigationActionSchema.describe(
    "Protective risk command: THROTTLE_RATE_LIMIT, FREEZE_ACCOUNT, CANCEL_OPEN_ORDERS, ENABLE_VOLATILITY_GUARD, or RESTORE_NORMAL"
  ),
  target_account_ids: z
    .array(Uint64StringSchema)
    .max(50)
    .describe("Target account IDs (empty array allowed only for symbol-wide ENABLE_VOLATILITY_GUARD / RESTORE_NORMAL)"),
  target_symbol: InstrumentSymbolSchema.describe("Target market symbol (e.g. 'BTC-USDT')"),
  rate_limit_override_rps: z
    .number()
    .int()
    .min(1)
    .max(10_000)
    .optional()
    .describe("Token-bucket rate limit in requests/sec when action == THROTTLE_RATE_LIMIT"),
  cooldown_duration_ms: z
    .number()
    .int()
    .min(1_000)
    .max(86_400_000)
    .default(300_000)
    .describe("Mitigation TTL in milliseconds before automatic rollback fires (default: 300000 = 5m)"),
  dry_run: z
    .boolean()
    .default(false)
    .describe("When true, simulates blast radius, policy gate verdict, and signature generation without mutating state"),
  reason: z
    .string()
    .min(10)
    .max(1000)
    .describe("Forensic justification citing verified quantitative evidence"),
  operator_approval_token: z
    .string()
    .min(16)
    .max(512)
    .optional()
    .describe("Cryptographic HITL approval token issued when a human operator approves a high-blast-radius action"),
  response_format: ResponseFormatSchema
};

export const ExecuteRiskMitigationInputSchema = z.object(ExecuteRiskMitigationInputShape).strict();
export type ExecuteRiskMitigationInput = z.infer<typeof ExecuteRiskMitigationInputSchema>;

// ============================================================================
// 2. MCP TOOL STRUCTURED OUTPUT SCHEMAS
// ============================================================================

export const QueryLedgerMerkleAuditOutputShape = {
  account_id: Uint64StringSchema,
  asset: z.string(),
  engine_seq: Uint64StringSchema,
  merkle_proof_valid: z.boolean(),
  computed_root_blake3: HexDigest32Schema,
  state_root_blake3: HexDigest32Schema,
  leaf_hash_verified: z.boolean(),
  double_entry_balanced: z.boolean(),
  account_line_balanced: z.boolean(),
  conservation_delta_fp: FixedPointIntSchema,
  account_balances: z.array(
    z.object({
      asset: z.string(),
      available_fp: FixedPointIntSchema,
      locked_fp: FixedPointIntSchema,
      total_fp: FixedPointIntSchema
    })
  ),
  formatted_balances: z.array(
    z.object({
      asset: z.string(),
      available_decimal: z.string(),
      locked_decimal: z.string(),
      total_decimal: z.string()
    })
  ),
  proof_ts_ns: Uint64StringSchema,
  verdict: z.enum(["VERIFIED_SOLVENT", "CRITICAL_LEDGER_INVARIANT_BREACH", "MERKLE_PROOF_TAMPERED"])
};

export const InspectFraudSubgraphOutputShape = {
  center_account_id: Uint64StringSchema,
  symbol: InstrumentSymbolSchema,
  nodes: z.array(z.record(z.unknown())),
  edges: z.array(z.record(z.unknown())),
  detected_cycles: z.array(z.record(z.unknown())),
  related_alerts: z.array(z.record(z.unknown())),
  top_shap_attributions: z.array(z.record(z.unknown())),
  security_taint_report: z.object({
    has_untrusted_injection_attempt: z.boolean(),
    quarantined_findings: z.array(z.record(z.unknown())),
    verified_offender_account_ids: z.array(Uint64StringSchema),
    protected_or_unverified_account_ids: z.array(Uint64StringSchema)
  }),
  snapshot_ts_ns: Uint64StringSchema
};

export const AnalyzeSystemTelemetryOutputShape = {
  overall_health: z.enum(["HEALTHY", "DEGRADED", "CRITICAL"]),
  service_metrics: z.array(z.record(z.unknown())),
  database_telemetry: z.record(z.unknown()),
  redpanda_consumer_lag: z.array(z.record(z.unknown())),
  circuit_breakers: z.array(z.record(z.unknown())),
  anomalies_detected: z.array(z.string()),
  captured_ts_ns: Uint64StringSchema
};

export const ExecuteRiskMitigationOutputShape = {
  status: z.enum([
    "EXECUTED",
    "DRY_RUN_SIMULATED",
    "QUEUED_HITL_APPROVAL",
    "BLOCKED_POLICY",
    "BLOCKED_PROMPT_INJECTION",
    "BLOCKED_COOLDOWN"
  ]),
  policy_evaluation: z.record(z.unknown()),
  signed_command: z.record(z.unknown()).optional(),
  approval_request: z.record(z.unknown()).optional(),
  auto_rollback_scheduled_at_ns: Uint64StringSchema.optional(),
  audit_seq: z.number().int(),
  audit_entry_blake3: HexDigest32Schema
};

// ============================================================================
// 3. LLM STRUCTURED REASONING OUTPUT SCHEMAS
// ============================================================================

export const LlmHypothesisSynthesisSchema = z
  .object({
    hypotheses: z
      .array(
        z
          .object({
            hypothesis_id: z.string().min(2).max(40),
            title: z.string().min(5).max(200),
            category: z.enum([
              "MARKET_MANIPULATION",
              "INFRASTRUCTURE_DEGRADATION",
              "ADVERSARIAL_PROMPT_INJECTION",
              "LEDGER_INTEGRITY"
            ]),
            prior_confidence_ppm: z.number().int().min(0).max(1_000_000),
            verification_plan: z.array(z.string().min(3).max(240)).min(1).max(6)
          })
          .strict()
      )
      .min(1)
      .max(6),
    accounts_to_audit: z.array(Uint64StringSchema).min(1).max(12)
  })
  .strict();

export type LlmHypothesisSynthesis = z.infer<typeof LlmHypothesisSynthesisSchema>;

export const LlmMitigationSynthesisSchema = z
  .object({
    confirmed_hypothesis_ids: z.array(z.string()),
    refuted_hypothesis_ids: z.array(z.string()),
    root_cause_summary: z.string().min(10).max(1500),
    proposed_mitigations: z
      .array(
        z
          .object({
            action: MitigationActionSchema,
            target_account_ids: z.array(Uint64StringSchema).max(50),
            target_symbol: InstrumentSymbolSchema,
            rate_limit_override_rps: z.number().int().min(1).max(10_000).optional(),
            cooldown_duration_ms: z.number().int().min(1_000).max(86_400_000),
            reason: z.string().min(10).max(1000),
            dry_run_first: z.boolean()
          })
          .strict()
      )
      .max(8)
  })
  .strict();

export type LlmMitigationSynthesis = z.infer<typeof LlmMitigationSynthesisSchema>;

// ============================================================================
// 4. HTTP REST CONTROL PLANE SCHEMAS
// ============================================================================

export const TriggerInvestigationRequestSchema = z
  .object({
    alert: z
      .object({
        alert_id: z.string().min(3).max(96),
        engine_seq: Uint64StringSchema,
        symbol: InstrumentSymbolSchema,
        pattern_type: z.enum([
          "WASH_TRADING_CYCLE",
          "SPOOFING_LAYERING",
          "VELOCITY_SPIKE",
          "MOMENTUM_IGNITION",
          "LATENCY_ARBITRAGE"
        ]),
        severity_score: z.number().int().min(0).max(100),
        confidence_ppm: z.number().int().min(0).max(1_000_000),
        autoencoder_recon_error_ppm: z.number().int().min(0).default(0),
        isolation_forest_score_ppm: z.number().int().default(0),
        offender_account_ids: z.array(Uint64StringSchema).min(1),
        shap_attributions: z
          .array(
            z
              .object({
                feature_name: z.string().min(1),
                feature_value_fp: FixedPointIntSchema,
                baseline_value_fp: FixedPointIntSchema,
                shap_contribution_ppm: z.number().int(),
                importance_rank: z.number().int().min(1)
              })
              .strict()
          )
          .default([]),
        wash_trading_cycle: z
          .object({
            cycle_id: z.string(),
            participant_account_ids: z.array(Uint64StringSchema).min(2),
            cycle_length: z.number().int().min(2).max(6),
            circular_volume_fp: FixedPointUintSchema,
            net_position_delta_fp: FixedPointUintSchema,
            cycle_duration_ns: Uint64StringSchema,
            volume_symmetry_ppm: z.number().int().min(0).max(1_000_000)
          })
          .strict()
          .optional(),
        spoofing_layering: z
          .object({
            suspect_account_id: Uint64StringSchema,
            spoofed_side: z.enum(["BUY", "SELL"]),
            layered_level_count: z.number().int().min(1),
            canceled_spoof_qty_fp: FixedPointUintSchema,
            opposite_side_exec_qty_fp: FixedPointUintSchema,
            cancel_to_fill_latency_ns: Uint64StringSchema,
            order_to_trade_ratio_ppm: z.number().int().min(0),
            best_quote_distance_bps: z.number().int().min(0)
          })
          .strict()
          .optional(),
        detected_ts_ns: Uint64StringSchema,
        inference_latency_us: z.number().int().min(0).default(1200),
        model_version: z.string().default("sentinel-ensemble-v1.onnx")
      })
      .strict(),
    incident_id: z
      .string()
      .min(4)
      .max(96)
      .regex(/^[A-Za-z0-9._:-]+$/)
      .optional()
  })
  .strict();

export const HitlDecisionRequestSchema = z
  .object({
    operator_id: z.string().min(2).max(64),
    note: z.string().min(5).max(500)
  })
  .strict();

export const RollbackRequestSchema = z
  .object({
    rollback_token: z.string().min(20).max(2048),
    reason: z.string().min(5).max(500).default("Operator-initiated rollback via token")
  })
  .strict();
