/**
 * HYPERION Channel 06 — Upstream Service Adapters & Deterministic Simulation Backend.
 *
 * Bridges Aegis MCP tools to:
 * - `ledger-rs` / `gateway-go` (`/api/v1/accounts/{account_id}/merkle-proof`)
 * - `sentinel-ml` (`/api/v1/fraud/subgraph` & `/api/v1/fraud/alerts`)
 * - `prometheus` & microservice `/healthz` + `/metrics` + Redpanda Admin API
 * - `hyperion.risk.mitigations.v1` & `hyperion.sre.incidents.v1` topics via Pandaproxy / HTTP
 *
 * When running in offline CI, unit tests, or standalone demo mode, `DeterministicSimulationBackend`
 * provides cryptographically valid BLAKE3 Merkle proofs, exact 1e8 double-entry ledgers, temporal
 * wash-trading multigraphs, and configurable fault scenarios.
 */

import { createHmac } from "node:crypto";
import type { AegisRuntimeConfig } from "../config.js";
import { buildDeterministicMerkleTree } from "../crypto/merkle.js";
import type { NanosecondClock } from "../domain/clock.js";
import { checkedAddI128, checkedSubI128, decimalToFixedPoint1e8 } from "../domain/fixed-point.js";
import type {
  AccountAssetBalanceDto,
  AccountProfileMetadata,
  AnomalyAlertDto,
  CircuitBreakerSnapshot,
  DatabaseTelemetrySnapshot,
  InstrumentSymbol,
  MerkleLedgerProofDto,
  RedpandaTopicLagSnapshot,
  ServiceLatencySnapshot,
  ShapFeatureAttributionDto,
  SignedMitigationEnvelope,
  TransactionGraphEdgeDto,
  WashTradingCycleDto
} from "../domain/types.js";
import type { MitigationSinkDispatcher } from "../guardrails/policy-engine.js";

export interface RawSubgraphPayload {
  center_account_id: string;
  symbol: InstrumentSymbol;
  profiles: AccountProfileMetadata[];
  node_metrics: Array<{
    account_id: string;
    max_severity_score: number;
    order_to_trade_ratio_ppm: number;
    cancel_rate_ppm: number;
    traded_volume_fp: string;
    active_alert_count: number;
  }>;
  edges: TransactionGraphEdgeDto[];
  detected_cycles: WashTradingCycleDto[];
  related_alerts: AnomalyAlertDto[];
  top_shap_attributions: ShapFeatureAttributionDto[];
  snapshot_ts_ns: string;
}

export interface RawSystemTelemetryPayload {
  service_metrics: ServiceLatencySnapshot[];
  database_telemetry: DatabaseTelemetrySnapshot;
  redpanda_consumer_lag: RedpandaTopicLagSnapshot[];
  circuit_breakers: CircuitBreakerSnapshot[];
  captured_ts_ns: string;
}

export interface HyperionUpstreamBackend extends MitigationSinkDispatcher {
  fetchMerkleLedgerProof(params: {
    accountId: string;
    asset: string;
    atEngineSeq?: string;
  }): Promise<MerkleLedgerProofDto>;

  fetchFraudSubgraph(params: {
    centerAccountId: string;
    symbol: InstrumentSymbol;
    maxHops: number;
    lookbackWindowNs: string;
  }): Promise<RawSubgraphPayload>;

  fetchSystemTelemetry(params: {
    services?: string[];
    includeRedpandaLag: boolean;
    includeCircuitBreakers: boolean;
    windowSeconds: number;
  }): Promise<RawSystemTelemetryPayload>;

  publishIncidentEvent(incidentId: string, eventPayload: Record<string, unknown>): Promise<void>;
}

export interface SimulatedAccountSeed {
  account_id: string;
  tier: AccountProfileMetadata["tier"];
  raw_alias: string;
  raw_client_memo: string;
  open_orders_count: number;
  usdt_available_fp: bigint;
  usdt_locked_fp: bigint;
  btc_available_fp: bigint;
  btc_locked_fp: bigint;
}

/**
 * High-precision deterministic simulation backend maintaining exact 1e8 double-entry conservation
 * and authentic BLAKE3 Merkle inclusion trees for offline/CI verification and standalone mode.
 */
export class DeterministicSimulationBackend implements HyperionUpstreamBackend {
  private readonly clock: NanosecondClock;
  private engineSeq = 42_500n;
  private treasuryUsdtFeePoolFp = decimalToFixedPoint1e8("125.50000000");
  private treasuryBtcFeePoolFp = 0n;
  private readonly accounts = new Map<string, SimulatedAccountSeed>();
  private customSubgraphOverride?: RawSubgraphPayload;
  private customTelemetryOverride?: RawSystemTelemetryPayload;
  private tamperMerkleAccountSet = new Set<string>();
  private corruptConservationDeltaFp = 0n;

  public readonly dispatchedMitigations: SignedMitigationEnvelope[] = [];
  public readonly publishedIncidentEvents: Array<{
    incident_id: string;
    payload: Record<string, unknown>;
  }> = [];

  public constructor(clock: NanosecondClock) {
    this.clock = clock;
    this.seedDefaultAccounts();
  }

  private seedDefaultAccounts(): void {
    const defaults: SimulatedAccountSeed[] = [
      {
        account_id: "1",
        tier: "SYSTEM_TREASURY",
        raw_alias: "Hyperion System Treasury",
        raw_client_memo: "Protocol reserve account",
        open_orders_count: 0,
        usdt_available_fp: decimalToFixedPoint1e8("1000000.00"),
        usdt_locked_fp: 0n,
        btc_available_fp: decimalToFixedPoint1e8("50.00"),
        btc_locked_fp: 0n
      },
      {
        account_id: "2",
        tier: "INSURANCE_FUND",
        raw_alias: "Hyperion Insurance Fund",
        raw_client_memo: "Liquidation backstop reserve",
        open_orders_count: 0,
        usdt_available_fp: decimalToFixedPoint1e8("500000.00"),
        usdt_locked_fp: 0n,
        btc_available_fp: decimalToFixedPoint1e8("20.00"),
        btc_locked_fp: 0n
      },
      {
        account_id: "1001",
        tier: "DESIGNATED_MARKET_MAKER",
        raw_alias: "Wintermute-DMM-Alpha",
        raw_client_memo: "Avellaneda-Stoikov quoting engine",
        open_orders_count: 48,
        usdt_available_fp: decimalToFixedPoint1e8("250000.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("120000.00"),
        btc_available_fp: decimalToFixedPoint1e8("12.50"),
        btc_locked_fp: decimalToFixedPoint1e8("4.00")
      },
      {
        account_id: "2042",
        tier: "RETAIL",
        raw_alias: "HonestTrader_Jakarta",
        raw_client_memo: "Spot DCA portfolio",
        open_orders_count: 2,
        usdt_available_fp: decimalToFixedPoint1e8("8500.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("1500.00"),
        btc_available_fp: decimalToFixedPoint1e8("0.25"),
        btc_locked_fp: 0n
      },
      {
        account_id: "9001",
        tier: "PRO",
        raw_alias: "SyndicateNode_A",
        raw_client_memo: "High-turnover ring leg A",
        open_orders_count: 14,
        usdt_available_fp: decimalToFixedPoint1e8("9000.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("3000.00"),
        btc_available_fp: decimalToFixedPoint1e8("0.50"),
        btc_locked_fp: decimalToFixedPoint1e8("0.20")
      },
      {
        account_id: "9002",
        tier: "PRO",
        raw_alias: "SyndicateNode_B",
        raw_client_memo: "High-turnover ring leg B",
        open_orders_count: 16,
        usdt_available_fp: decimalToFixedPoint1e8("8200.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("2800.00"),
        btc_available_fp: decimalToFixedPoint1e8("0.45"),
        btc_locked_fp: decimalToFixedPoint1e8("0.25")
      },
      {
        account_id: "9003",
        tier: "PRO",
        raw_alias: "SyndicateNode_C",
        raw_client_memo: "High-turnover ring leg C",
        open_orders_count: 12,
        usdt_available_fp: decimalToFixedPoint1e8("7500.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("2500.00"),
        btc_available_fp: decimalToFixedPoint1e8("0.40"),
        btc_locked_fp: decimalToFixedPoint1e8("0.30")
      },
      {
        account_id: "9099",
        tier: "PRO",
        raw_alias: "WhaleSpoofer_X",
        raw_client_memo: "Layered spoof bot with oversized notional",
        open_orders_count: 95,
        usdt_available_fp: decimalToFixedPoint1e8("40000.00"),
        usdt_locked_fp: decimalToFixedPoint1e8("35000.00"), // 75,000 USDT > 50,000 auto cap -> triggers HITL!
        btc_available_fp: decimalToFixedPoint1e8("2.00"),
        btc_locked_fp: decimalToFixedPoint1e8("1.50")
      }
    ];

    for (const seed of defaults) {
      this.accounts.set(seed.account_id, seed);
    }
  }

  public upsertSimulatedAccount(seed: SimulatedAccountSeed): void {
    this.accounts.set(seed.account_id, seed);
  }

  public getSimulatedAccount(accountId: string): SimulatedAccountSeed | undefined {
    return this.accounts.get(accountId);
  }

  public setSubgraphScenario(payload: RawSubgraphPayload | undefined): void {
    this.customSubgraphOverride = payload;
  }

  public setTelemetryScenario(payload: RawSystemTelemetryPayload | undefined): void {
    this.customTelemetryOverride = payload;
  }

  public setMerkleTamperForAccount(accountId: string, tamper: boolean): void {
    if (tamper) {
      this.tamperMerkleAccountSet.add(accountId);
    } else {
      this.tamperMerkleAccountSet.delete(accountId);
    }
  }

  public setConservationCorruptionFp(deltaFp: bigint): void {
    this.corruptConservationDeltaFp = deltaFp;
  }

  private buildAccountBalanceDtos(seed: SimulatedAccountSeed): AccountAssetBalanceDto[] {
    const usdtTotal = checkedAddI128(seed.usdt_available_fp, seed.usdt_locked_fp);
    const btcTotal = checkedAddI128(seed.btc_available_fp, seed.btc_locked_fp);
    return [
      {
        asset: "BTC",
        available_fp: seed.btc_available_fp.toString(10),
        locked_fp: seed.btc_locked_fp.toString(10),
        total_fp: btcTotal.toString(10)
      },
      {
        asset: "USDT",
        available_fp: seed.usdt_available_fp.toString(10),
        locked_fp: seed.usdt_locked_fp.toString(10),
        total_fp: usdtTotal.toString(10)
      }
    ];
  }

  public async fetchMerkleLedgerProof(params: {
    accountId: string;
    asset: string;
    atEngineSeq?: string;
  }): Promise<MerkleLedgerProofDto> {
    const seqStr = params.atEngineSeq ?? this.engineSeq.toString(10);
    const targetSeed = this.accounts.get(params.accountId);
    if (!targetSeed) {
      throw new Error(`Account ${params.accountId} not found in Hyperion double-entry ledger`);
    }

    const allLeaves: Array<{ account_id: string; balances: AccountAssetBalanceDto[] }> = [];
    let totalUserUsdtFp = 0n;
    let totalUserBtcFp = 0n;

    for (const seed of this.accounts.values()) {
      allLeaves.push({
        account_id: seed.account_id,
        balances: this.buildAccountBalanceDtos(seed)
      });
      totalUserUsdtFp = checkedAddI128(
        totalUserUsdtFp,
        checkedAddI128(seed.usdt_available_fp, seed.usdt_locked_fp)
      );
      totalUserBtcFp = checkedAddI128(
        totalUserBtcFp,
        checkedAddI128(seed.btc_available_fp, seed.btc_locked_fp)
      );
    }

    const tree = buildDeterministicMerkleTree(seqStr, allLeaves);
    const isBtc = params.asset.toUpperCase() === "BTC";
    const userEquityFp = isBtc ? totalUserBtcFp : totalUserUsdtFp;
    const feePoolFp = isBtc ? this.treasuryBtcFeePoolFp : this.treasuryUsdtFeePoolFp;
    const exactDepositsFp = checkedSubI128(
      checkedAddI128(userEquityFp, feePoolFp),
      this.corruptConservationDeltaFp
    );

    let leafHash = tree.leafHashes.get(params.accountId)!;
    if (this.tamperMerkleAccountSet.has(params.accountId)) {
      // Flip last hex nibble to simulate tampered Merkle leaf
      const lastChar = leafHash.slice(-1) === "0" ? "1" : "0";
      leafHash = `${leafHash.slice(0, -1)}${lastChar}`;
    }

    return {
      engine_seq: seqStr,
      account_id: params.accountId,
      balances: this.buildAccountBalanceDtos(targetSeed),
      leaf_blake3_hash: leafHash,
      state_root_blake3: tree.stateRootBlake3,
      proof_path: tree.proofPaths.get(params.accountId) ?? [],
      system_total_deposits_fp: exactDepositsFp.toString(10),
      system_total_withdrawals_fp: "0",
      system_total_user_equity_fp: userEquityFp.toString(10),
      treasury_fee_pool_fp: feePoolFp.toString(10),
      double_entry_balanced: this.corruptConservationDeltaFp === 0n,
      proof_ts_ns: this.clock.nowNs().toString(10)
    };
  }

  public async fetchFraudSubgraph(params: {
    centerAccountId: string;
    symbol: InstrumentSymbol;
    maxHops: number;
    lookbackWindowNs: string;
  }): Promise<RawSubgraphPayload> {
    if (this.customSubgraphOverride) {
      return this.customSubgraphOverride;
    }

    const nowNs = this.clock.nowNs();
    const cycleParticipants = ["9001", "9002", "9003"];
    const involvedIds = new Set<string>([params.centerAccountId, ...cycleParticipants, "1001", "1"]);
    const profiles: AccountProfileMetadata[] = [];
    for (const id of involvedIds) {
      const seed = this.accounts.get(id);
      if (seed) {
        profiles.push({
          account_id: seed.account_id,
          tier: seed.tier,
          raw_alias: seed.raw_alias,
          raw_client_memo: seed.raw_client_memo,
          open_orders_count: seed.open_orders_count
        });
      }
    }

    const legPriceFp = decimalToFixedPoint1e8("68450.00").toString(10);
    const legQtyFp = decimalToFixedPoint1e8("1.25").toString(10);
    const legNotionalFp = decimalToFixedPoint1e8("85562.50").toString(10);
    const totalCycleVolFp = decimalToFixedPoint1e8("256687.50").toString(10);

    const edges: TransactionGraphEdgeDto[] = [
      {
        edge_id: "5001",
        fill_id: "88001",
        engine_seq: "42490",
        symbol: params.symbol,
        maker_account_id: "9001",
        taker_account_id: "9002",
        aggressor_side: "BUY",
        price_fp: legPriceFp,
        qty_fp: legQtyFp,
        notional_fp: legNotionalFp,
        exec_ts_ns: (nowNs - 12_000_000n).toString(10),
        prev_interaction_delta_ns: "4000000",
        temporal_decay_weight_ppm: 992_000
      },
      {
        edge_id: "5002",
        fill_id: "88002",
        engine_seq: "42494",
        symbol: params.symbol,
        maker_account_id: "9002",
        taker_account_id: "9003",
        aggressor_side: "BUY",
        price_fp: legPriceFp,
        qty_fp: legQtyFp,
        notional_fp: legNotionalFp,
        exec_ts_ns: (nowNs - 8_000_000n).toString(10),
        prev_interaction_delta_ns: "4000000",
        temporal_decay_weight_ppm: 995_000
      },
      {
        edge_id: "5003",
        fill_id: "88003",
        engine_seq: "42498",
        symbol: params.symbol,
        maker_account_id: "9003",
        taker_account_id: "9001",
        aggressor_side: "BUY",
        price_fp: legPriceFp,
        qty_fp: legQtyFp,
        notional_fp: legNotionalFp,
        exec_ts_ns: (nowNs - 3_000_000n).toString(10),
        prev_interaction_delta_ns: "5000000",
        temporal_decay_weight_ppm: 998_000
      }
    ];

    const washCycle: WashTradingCycleDto = {
      cycle_id: "cycle-ring-9001-9002-9003",
      participant_account_ids: ["9001", "9002", "9003", "9001"],
      cycle_length: 3,
      circular_volume_fp: totalCycleVolFp,
      net_position_delta_fp: "0",
      cycle_duration_ns: "9000000",
      volume_symmetry_ppm: 998_500
    };

    const shap: ShapFeatureAttributionDto[] = [
      {
        feature_name: "tarjan_cycle_symmetry_score",
        feature_value_fp: "99850000",
        baseline_value_fp: "1200000",
        shap_contribution_ppm: 540_000,
        importance_rank: 1
      },
      {
        feature_name: "zero_net_inventory_retention",
        feature_value_fp: "100000000",
        baseline_value_fp: "15000000",
        shap_contribution_ppm: 310_000,
        importance_rank: 2
      },
      {
        feature_name: "inter_leg_latency_compression",
        feature_value_fp: "96000000",
        baseline_value_fp: "8000000",
        shap_contribution_ppm: 125_000,
        importance_rank: 3
      }
    ];

    const alert: AnomalyAlertDto = {
      alert_id: "alert-wash-default-01",
      engine_seq: "42498",
      symbol: params.symbol,
      pattern_type: "WASH_TRADING_CYCLE",
      severity_score: 94,
      confidence_ppm: 975_000,
      autoencoder_recon_error_ppm: 842_000,
      isolation_forest_score_ppm: -780_000,
      offender_account_ids: ["9001", "9002", "9003"],
      shap_attributions: shap,
      wash_trading_cycle: washCycle,
      detected_ts_ns: nowNs.toString(10),
      inference_latency_us: 1_420,
      model_version: "sentinel-ensemble-v1.onnx"
    };

    return {
      center_account_id: params.centerAccountId,
      symbol: params.symbol,
      profiles,
      node_metrics: [
        {
          account_id: "9001",
          max_severity_score: 94,
          order_to_trade_ratio_ppm: 3_200_000,
          cancel_rate_ppm: 180_000,
          traded_volume_fp: legNotionalFp,
          active_alert_count: 1
        },
        {
          account_id: "9002",
          max_severity_score: 94,
          order_to_trade_ratio_ppm: 3_100_000,
          cancel_rate_ppm: 175_000,
          traded_volume_fp: legNotionalFp,
          active_alert_count: 1
        },
        {
          account_id: "9003",
          max_severity_score: 94,
          order_to_trade_ratio_ppm: 3_050_000,
          cancel_rate_ppm: 160_000,
          traded_volume_fp: legNotionalFp,
          active_alert_count: 1
        },
        {
          account_id: "1001",
          max_severity_score: 8,
          order_to_trade_ratio_ppm: 6_500_000,
          cancel_rate_ppm: 620_000,
          traded_volume_fp: decimalToFixedPoint1e8("1450000.00").toString(10),
          active_alert_count: 0
        },
        {
          account_id: "1",
          max_severity_score: 0,
          order_to_trade_ratio_ppm: 0,
          cancel_rate_ppm: 0,
          traded_volume_fp: "0",
          active_alert_count: 0
        }
      ],
      edges,
      detected_cycles: [washCycle],
      related_alerts: [alert],
      top_shap_attributions: shap,
      snapshot_ts_ns: nowNs.toString(10)
    };
  }

  public async fetchSystemTelemetry(params: {
    services?: string[];
    includeRedpandaLag: boolean;
    includeCircuitBreakers: boolean;
    windowSeconds: number;
  }): Promise<RawSystemTelemetryPayload> {
    if (this.customTelemetryOverride) {
      const base = this.customTelemetryOverride;
      return {
        service_metrics: params.services
          ? base.service_metrics.filter((s) => params.services?.includes(s.service))
          : base.service_metrics,
        database_telemetry: base.database_telemetry,
        redpanda_consumer_lag: params.includeRedpandaLag ? base.redpanda_consumer_lag : [],
        circuit_breakers: params.includeCircuitBreakers ? base.circuit_breakers : [],
        captured_ts_ns: this.clock.nowNs().toString(10)
      };
    }

    const allServices: ServiceLatencySnapshot[] = [
      {
        service: "ledger-rs",
        healthy: true,
        p50_latency_us: 2,
        p99_latency_us: 6,
        p999_latency_us: 11,
        error_rate_bps: 0,
        throughput_rps: 48_500
      },
      {
        service: "gateway-go",
        healthy: true,
        p50_latency_us: 180,
        p99_latency_us: 890,
        p999_latency_us: 1_950,
        error_rate_bps: 2,
        throughput_rps: 46_200
      },
      {
        service: "stream-aggregator",
        healthy: true,
        p50_latency_us: 420,
        p99_latency_us: 2_100,
        p999_latency_us: 4_800,
        error_rate_bps: 0,
        throughput_rps: 92_000
      },
      {
        service: "sentinel-ml",
        healthy: true,
        p50_latency_us: 1_100,
        p99_latency_us: 4_600,
        p999_latency_us: 7_900,
        error_rate_bps: 0,
        throughput_rps: 18_400
      },
      {
        service: "aegis-agent",
        healthy: true,
        p50_latency_us: 650,
        p99_latency_us: 3_200,
        p999_latency_us: 6_100,
        error_rate_bps: 0,
        throughput_rps: 120
      },
      {
        service: "chronos-replay",
        healthy: true,
        p50_latency_us: 900,
        p99_latency_us: 3_800,
        p999_latency_us: 7_200,
        error_rate_bps: 0,
        throughput_rps: 45
      }
    ];

    const filteredServices = params.services
      ? allServices.filter((s) => params.services?.includes(s.service))
      : allServices;

    return {
      service_metrics: filteredServices,
      database_telemetry: {
        db_cluster: "timescaledb-pg16-primary",
        write_batch_p50_us: 1_400,
        write_batch_p99_us: 6_800,
        pool_saturation_bps: 2_200,
        active_connections: 18,
        hypertable_chunk_healthy: true
      },
      redpanda_consumer_lag: params.includeRedpandaLag
        ? [
            {
              topic: "hyperion.market.executions.v1",
              consumer_group: "stream-aggregator-cg",
              total_lag_messages: "14",
              max_partition_lag: "4",
              oldest_unconsumed_age_ms: 6
            },
            {
              topic: "hyperion.fraud.alerts.v1",
              consumer_group: "aegis-sre-cg",
              total_lag_messages: "0",
              max_partition_lag: "0",
              oldest_unconsumed_age_ms: 0
            }
          ]
        : [],
      circuit_breakers: params.includeCircuitBreakers
        ? [
            {
              upstream_target: "ledger-rs:50151",
              state: "CLOSED",
              trip_count: 0,
              recent_error_rate_bps: 0,
              last_state_change_ts_ns: "1795013500000000000"
            },
            {
              upstream_target: "chronos-replay:50154",
              state: "CLOSED",
              trip_count: 0,
              recent_error_rate_bps: 0,
              last_state_change_ts_ns: "1795013500000000000"
            }
          ]
        : [],
      captured_ts_ns: this.clock.nowNs().toString(10)
    };
  }

  public async dispatchMitigation(envelope: SignedMitigationEnvelope): Promise<void> {
    this.dispatchedMitigations.push(envelope);
    this.engineSeq += 1n;
  }

  public async publishIncidentEvent(
    incidentId: string,
    eventPayload: Record<string, unknown>
  ): Promise<void> {
    this.publishedIncidentEvents.push({ incident_id: incidentId, payload: eventPayload });
  }
}

/**
 * Production Hybrid HTTP/Pandaproxy/Prometheus Backend that attempts live microservice calls
 * within bounded `upstreamTimeoutMs` and gracefully falls back to `DeterministicSimulationBackend`
 * when local containers are offline.
 */
export class ResilientHttpUpstreamBackend implements HyperionUpstreamBackend {
  private readonly config: AegisRuntimeConfig;
  private readonly fallback: DeterministicSimulationBackend;

  public constructor(config: AegisRuntimeConfig, fallback: DeterministicSimulationBackend) {
    this.config = config;
    this.fallback = fallback;
  }

  private mintServiceJwt(): string {
    const nowSec = Math.floor(Date.now() / 1000);
    const header = Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url");
    const payload = Buffer.from(
      JSON.stringify({
        sub: "aegis-sre-agent",
        account_id: 1,
        roles: ["sre", "auditor"],
        iat: nowSec,
        exp: nowSec + 120
      })
    ).toString("base64url");
    const signingInput = `${header}.${payload}`;
    const sig = createHmac("sha256", this.config.jwtSecret)
      .update(signingInput)
      .digest("base64url");
    return `${signingInput}.${sig}`;
  }

  private buildInternalHeaders(extra?: Record<string, string>): Record<string, string> {
    const headers: Record<string, string> = { Accept: "application/json", ...extra };
    if (this.config.internalAuthToken) {
      headers["X-Hyperion-Internal-Token"] = this.config.internalAuthToken;
    }
    return headers;
  }

  private async fetchJsonWithTimeout<T>(url: string, init?: RequestInit): Promise<T> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.config.upstreamTimeoutMs);
    try {
      const res = await fetch(url, { ...init, signal: controller.signal });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} from ${url}`);
      }
      return (await res.json()) as T;
    } finally {
      clearTimeout(timer);
    }
  }

  public async fetchMerkleLedgerProof(params: {
    accountId: string;
    asset: string;
    atEngineSeq?: string;
  }): Promise<MerkleLedgerProofDto> {
    try {
      const qs = params.atEngineSeq ? `?at_engine_seq=${encodeURIComponent(params.atEngineSeq)}` : "";
      const dto = await this.fetchJsonWithTimeout<MerkleLedgerProofDto>(
        `${this.config.gatewayHttpUrl}/api/v1/accounts/${encodeURIComponent(params.accountId)}/merkle-proof${qs}`,
        {
          headers: this.buildInternalHeaders({
            Authorization: `Bearer ${this.mintServiceJwt()}`
          })
        }
      );
      return {
        ...dto,
        system_total_withdrawals_fp: dto.system_total_withdrawals_fp ?? "0"
      };
    } catch {
      return this.fallback.fetchMerkleLedgerProof(params);
    }
  }

  public async fetchFraudSubgraph(params: {
    centerAccountId: string;
    symbol: InstrumentSymbol;
    maxHops: number;
    lookbackWindowNs: string;
  }): Promise<RawSubgraphPayload> {
    try {
      const qs = new URLSearchParams({
        symbol: params.symbol,
        max_hops: String(params.maxHops),
        lookback_window_ns: params.lookbackWindowNs
      });
      const raw = await this.fetchJsonWithTimeout<Record<string, unknown>>(
        `${this.config.sentinelMlHttpUrl}/api/v1/fraud/subgraph/${encodeURIComponent(params.centerAccountId)}?${qs.toString()}`,
        {
          headers: this.buildInternalHeaders()
        }
      );
      if (Array.isArray(raw.profiles) && Array.isArray(raw.node_metrics)) {
        return raw as unknown as RawSubgraphPayload;
      }

      // INT-06 Adapter: normalize live CH05 sentinel-ml REST JSON (`nodes`, `top_attributions`,
      // full enum names & numeric timestamps) into Aegis `RawSubgraphPayload`.
      const rawNodes = Array.isArray(raw.nodes)
        ? (raw.nodes as Array<Record<string, unknown>>)
        : [];
      const node_metrics = rawNodes.map((n) => ({
        account_id: String(n.account_id ?? params.centerAccountId),
        max_severity_score: Number(n.max_severity_score ?? 0),
        order_to_trade_ratio_ppm: Number(n.order_to_trade_ratio_ppm ?? 0),
        cancel_rate_ppm: Number(n.cancel_rate_ppm ?? 0),
        traded_volume_fp: String(n.traded_volume_fp ?? "0"),
        active_alert_count: Number(n.active_alert_count ?? 0)
      }));

      const profiles: AccountProfileMetadata[] = node_metrics.map((nm) => ({
        account_id: nm.account_id,
        tier:
          nm.account_id === "1"
            ? "SYSTEM_TREASURY"
            : nm.account_id === "2"
              ? "INSURANCE_FUND"
              : "RETAIL",
        raw_alias: `acct-${nm.account_id}`,
        raw_client_memo: "",
        open_orders_count: 0
      }));

      const rawEdges = Array.isArray(raw.edges)
        ? (raw.edges as Array<Record<string, unknown>>)
        : [];
      const edges: TransactionGraphEdgeDto[] = rawEdges.map((e, idx) => ({
        edge_id: String(e.edge_id ?? idx + 1),
        fill_id: String(e.fill_id ?? idx + 1),
        engine_seq: String(e.engine_seq ?? idx + 1),
        symbol: (String(e.symbol ?? params.symbol) as InstrumentSymbol) || params.symbol,
        maker_account_id: String(e.maker_account_id ?? "0"),
        taker_account_id: String(e.taker_account_id ?? "0"),
        aggressor_side: String(e.aggressor_side ?? "BUY").toUpperCase() === "SELL" ? "SELL" : "BUY",
        price_fp: String(e.price_fp ?? "0"),
        qty_fp: String(e.qty_fp ?? "0"),
        notional_fp: String(e.notional_fp ?? "0"),
        exec_ts_ns: String(e.exec_ts_ns ?? "0"),
        prev_interaction_delta_ns: String(e.prev_interaction_delta_ns ?? "0"),
        temporal_decay_weight_ppm: Number(e.temporal_decay_weight_ppm ?? 1_000_000)
      }));

      const rawAlerts = Array.isArray(raw.related_alerts)
        ? (raw.related_alerts as Array<Record<string, unknown>>)
        : [];
      const normalizePattern = (p: unknown): AnomalyAlertDto["pattern_type"] => {
        const cleaned = String(p ?? "WASH_TRADING_CYCLE").replace(/^ANOMALY_PATTERN_TYPE_/, "");
        if (
          cleaned === "WASH_TRADING_CYCLE" ||
          cleaned === "SPOOFING_LAYERING" ||
          cleaned === "VELOCITY_SPIKE" ||
          cleaned === "MOMENTUM_IGNITION" ||
          cleaned === "LATENCY_ARBITRAGE"
        ) {
          return cleaned;
        }
        return "WASH_TRADING_CYCLE";
      };

      const related_alerts: AnomalyAlertDto[] = rawAlerts.map((a, idx) => ({
        alert_id: String(a.alert_id ?? `live-alert-${idx + 1}`),
        engine_seq: String(a.engine_seq ?? "1"),
        symbol: (String(a.symbol ?? params.symbol) as InstrumentSymbol) || params.symbol,
        pattern_type: normalizePattern(a.pattern_type),
        severity_score: Number(a.severity_score ?? 0),
        confidence_ppm: Number(a.confidence_ppm ?? 0),
        autoencoder_recon_error_ppm: Number(a.autoencoder_recon_error_ppm ?? 0),
        isolation_forest_score_ppm: Number(a.isolation_forest_score_ppm ?? 0),
        offender_account_ids: Array.isArray(a.offender_account_ids)
          ? a.offender_account_ids.map((id) => String(id))
          : [params.centerAccountId],
        shap_attributions: Array.isArray(a.shap_attributions)
          ? (a.shap_attributions as ShapFeatureAttributionDto[])
          : [],
        detected_ts_ns: String(a.detected_ts_ns ?? "0"),
        inference_latency_us: Number(a.inference_latency_us ?? 1000),
        model_version: String(a.model_version ?? "sentinel-ensemble-v1.0.0-int8")
      }));

      const detected_cycles: WashTradingCycleDto[] = [];
      for (const al of related_alerts) {
        if (al.pattern_type === "WASH_TRADING_CYCLE" && al.offender_account_ids.length >= 2) {
          detected_cycles.push({
            cycle_id: `cycle-${al.alert_id}`,
            participant_account_ids: al.offender_account_ids,
            cycle_length: Math.min(6, Math.max(2, al.offender_account_ids.length)),
            circular_volume_fp:
              edges.reduce((acc, ed) => acc + BigInt(ed.notional_fp || "0"), 0n).toString(10),
            net_position_delta_fp: "0",
            cycle_duration_ns: "100000000",
            volume_symmetry_ppm: 990_000
          });
        }
      }

      const top_shap_attributions: ShapFeatureAttributionDto[] = Array.isArray(raw.top_attributions)
        ? (raw.top_attributions as ShapFeatureAttributionDto[])
        : Array.isArray(raw.top_shap_attributions)
          ? (raw.top_shap_attributions as ShapFeatureAttributionDto[])
          : [];

      return {
        center_account_id: params.centerAccountId,
        symbol: params.symbol,
        profiles,
        node_metrics,
        edges,
        detected_cycles,
        related_alerts,
        top_shap_attributions,
        snapshot_ts_ns: String(raw.snapshot_ts_ns ?? Date.now() * 1_000_000)
      };
    } catch {
      return this.fallback.fetchFraudSubgraph(params);
    }
  }

  public async fetchSystemTelemetry(params: {
    services?: string[];
    includeRedpandaLag: boolean;
    includeCircuitBreakers: boolean;
    windowSeconds: number;
  }): Promise<RawSystemTelemetryPayload> {
    try {
      return await this.fetchJsonWithTimeout<RawSystemTelemetryPayload>(
        `${this.config.gatewayMetricsUrl}/api/v1/telemetry/summary?window_s=${params.windowSeconds}`
      );
    } catch {
      return this.fallback.fetchSystemTelemetry(params);
    }
  }

  public async dispatchMitigation(envelope: SignedMitigationEnvelope): Promise<void> {
    await this.fallback.dispatchMitigation(envelope);
    // Best-effort publish to Redpanda Pandaproxy (`hyperion.risk.mitigations.v1` per 00-hyperion-contract.md §4.2)
    const partitionKey =
      envelope.target_account_ids.length > 0
        ? `account:${envelope.target_account_ids[0]}`
        : `symbol:${envelope.target_symbol}`;
    try {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), this.config.upstreamTimeoutMs);
      try {
        await fetch(
          `${this.config.redpandaPandaproxyUrl}/topics/hyperion.risk.mitigations.v1`,
          {
            method: "POST",
            headers: {
              "Content-Type": "application/vnd.kafka.binary.v2+json",
              ...envelope.gateway_auth_headers
            },
            body: JSON.stringify({
              records: [
                {
                  key: Buffer.from(partitionKey, "utf8").toString("base64"),
                  value: envelope.protobuf_wire_base64
                }
              ]
            }),
            signal: controller.signal
          }
        );
      } finally {
        clearTimeout(timer);
      }
    } catch {
      // Offline environment: fallback sink already persisted envelope deterministically
    }
  }

  public async publishIncidentEvent(
    incidentId: string,
    eventPayload: Record<string, unknown>
  ): Promise<void> {
    await this.fallback.publishIncidentEvent(incidentId, eventPayload);
    try {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), this.config.upstreamTimeoutMs);
      try {
        await fetch(
          `${this.config.redpandaPandaproxyUrl}/topics/hyperion.sre.incidents.v1`,
          {
            method: "POST",
            headers: {
              "Content-Type": "application/vnd.kafka.json.v2+json"
            },
            body: JSON.stringify({
              records: [
                {
                  key: `incident:${incidentId}`,
                  value: eventPayload
                }
              ]
            }),
            signal: controller.signal
          }
        );
      } finally {
        clearTimeout(timer);
      }
    } catch {
      // Offline environment: fallback event log already captured
    }
  }
}
