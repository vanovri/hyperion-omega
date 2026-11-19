use std::env;

/// Canonical runtime configuration for `services/stream-aggregator` (Channel 04).
/// Aligned strictly with `.agents/rules/00-hyperion-contract.md`.
#[derive(Debug, Clone)]
pub struct AggregatorConfig {
    pub service_name: String,
    pub consumer_group: String,
    pub grpc_bind_addr: String,
    pub http_bind_addr: String,
    pub redpanda_brokers: Vec<String>,
    pub postgres_dsn: String,
    pub redis_url: String,
    /// Deterministic bounded-out-of-orderness watermark lateness in nanoseconds.
    pub watermark_lateness_ns: u64,
    /// Maximum buffered reorder events per symbol within watermark horizon.
    pub max_reorder_buffer: usize,
    /// Batch flush threshold (events) before triggering transactional DB flush.
    pub batch_flush_max_events: usize,
    /// Periodic batch flush interval in milliseconds.
    pub batch_flush_interval_ms: u64,
    /// Liquidity heatmap time-column resolution in nanoseconds (default 100ms for 10Hz / 60fps interpolation).
    pub heatmap_time_bucket_ns: u64,
    /// Maximum historical candles retained in hot memory ring per `(symbol, interval)`.
    pub hot_candle_ring_capacity: usize,
    /// Maximum heatmap columns retained in hot memory ring per symbol.
    pub hot_heatmap_columns: usize,
    /// Enable live background Redpanda + TimescaleDB + Redis loops on startup.
    pub enable_external_io: bool,
}

impl Default for AggregatorConfig {
    fn default() -> Self {
        Self {
            service_name: "stream-aggregator".to_string(),
            consumer_group: "hyperion-stream-aggregator-v1".to_string(),
            grpc_bind_addr: "0.0.0.0:50152".to_string(),
            http_bind_addr: "0.0.0.0:8083".to_string(),
            redpanda_brokers: vec!["localhost:19092".to_string()],
            postgres_dsn: "postgres://hyperion:hyperion_dev_secret@localhost:5432/hyperion?sslmode=disable"
                .to_string(),
            redis_url: "redis://localhost:6379/0".to_string(),
            watermark_lateness_ns: 2_000_000_000, // 2 seconds bounded out-of-orderness
            max_reorder_buffer: 4096,
            batch_flush_max_events: 2048,
            batch_flush_interval_ms: 100,
            heatmap_time_bucket_ns: 1_000_000_000, // 1s default historical column granularity
            hot_candle_ring_capacity: 1024,
            hot_heatmap_columns: 256,
            enable_external_io: true,
        }
    }
}

impl AggregatorConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = env::var("HYPERION_CONSUMER_GROUP") {
            if !v.trim().is_empty() {
                cfg.consumer_group = v;
            }
        }
        if let Ok(v) = env::var("HYPERION_AGGREGATOR_GRPC_ADDR") {
            if !v.trim().is_empty() {
                cfg.grpc_bind_addr = v;
            }
        }
        if let Ok(v) = env::var("HYPERION_AGGREGATOR_HTTP_ADDR") {
            if !v.trim().is_empty() {
                cfg.http_bind_addr = v;
            }
        }
        if let Ok(v) = env::var("REDPANDA_BROKERS") {
            let parsed: Vec<String> = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            if !parsed.is_empty() {
                cfg.redpanda_brokers = parsed;
            }
        }
        if let Ok(v) = env::var("DATABASE_URL").or_else(|_| env::var("TIMESCALEDB_URL")) {
            if !v.trim().is_empty() {
                cfg.postgres_dsn = v;
            }
        }
        if let Ok(v) = env::var("REDIS_URL") {
            if !v.trim().is_empty() {
                cfg.redis_url = v;
            }
        }
        if let Ok(v) = env::var("WATERMARK_LATENESS_NS") {
            if let Ok(parsed) = v.parse::<u64>() {
                cfg.watermark_lateness_ns = parsed;
            }
        }
        if let Ok(v) = env::var("BATCH_FLUSH_MAX_EVENTS") {
            if let Ok(parsed) = v.parse::<usize>() {
                if parsed > 0 {
                    cfg.batch_flush_max_events = parsed;
                }
            }
        }
        if let Ok(v) = env::var("BATCH_FLUSH_INTERVAL_MS") {
            if let Ok(parsed) = v.parse::<u64>() {
                if parsed > 0 {
                    cfg.batch_flush_interval_ms = parsed;
                }
            }
        }
        if let Ok(v) = env::var("ENABLE_EXTERNAL_IO") {
            cfg.enable_external_io = !matches!(v.as_str(), "0" | "false" | "FALSE");
        }
        cfg
    }

    /// SEC-05: Refuse production startup with dev default PostgreSQL credentials or plaintext `sslmode=disable`.
    pub fn validate_production(&self) -> Result<(), String> {
        let is_prod = env::var("HYPERION_ENV")
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("production");
        if !is_prod || !self.enable_external_io {
            return Ok(());
        }
        if self.postgres_dsn.contains("hyperion_dev_secret") {
            return Err(
                "SECURITY_FAIL_CLOSED: DATABASE_URL must not use default dev password in production"
                    .to_string(),
            );
        }
        if self.postgres_dsn.to_ascii_lowercase().contains("sslmode=disable") {
            return Err(
                "SECURITY_FAIL_CLOSED: DATABASE_URL must enforce TLS (sslmode=require/verify-full) in production"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Canonical Redpanda topics defined in `.agents/rules/00-hyperion-contract.md` §4.2.
pub mod topics {
    pub const MARKET_L3_DELTAS: &str = "hyperion.market.l3-deltas.v1";
    pub const MARKET_L2_SNAPSHOTS: &str = "hyperion.market.l2-snapshots.v1";
    pub const MARKET_EXECUTIONS: &str = "hyperion.market.executions.v1";
    pub const LEDGER_JOURNALS: &str = "hyperion.ledger.journals.v1";
    pub const ANALYTICS_CANDLES: &str = "hyperion.analytics.candles.v1";
    pub const ANALYTICS_MICROSTRUCTURE: &str = "hyperion.analytics.microstructure.v1";
    pub const FRAUD_ALERTS: &str = "hyperion.fraud.alerts.v1";
}

/// Deterministic canonical instrument metadata (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstrumentSpec {
    pub instrument_id: u32,
    pub symbol: &'static str,
    pub tick_size_fp: u64,
    pub lot_size_fp: u64,
    pub default_heatmap_bucket_fp: u64,
}

pub const CANONICAL_INSTRUMENTS: [InstrumentSpec; 4] = [
    InstrumentSpec {
        instrument_id: 1,
        symbol: "BTC-USDT",
        tick_size_fp: 1_000_000, // 0.01 USDT
        lot_size_fp: 1_000,      // 0.00001 BTC
        default_heatmap_bucket_fp: 100_000_000, // 1.00 USDT bucket
    },
    InstrumentSpec {
        instrument_id: 2,
        symbol: "ETH-USDT",
        tick_size_fp: 1_000_000, // 0.01 USDT
        lot_size_fp: 10_000,     // 0.0001 ETH
        default_heatmap_bucket_fp: 50_000_000, // 0.50 USDT bucket
    },
    InstrumentSpec {
        instrument_id: 3,
        symbol: "SOL-USDT",
        tick_size_fp: 100_000, // 0.001 USDT
        lot_size_fp: 100_000,  // 0.001 SOL
        default_heatmap_bucket_fp: 10_000_000, // 0.10 USDT bucket
    },
    InstrumentSpec {
        instrument_id: 4,
        symbol: "HYPE-USDT",
        tick_size_fp: 10_000,    // 0.0001 USDT
        lot_size_fp: 1_000_000,  // 0.01 HYPE
        default_heatmap_bucket_fp: 1_000_000, // 0.01 USDT bucket
    },
];

#[inline]
pub fn instrument_spec_for(symbol: &str) -> InstrumentSpec {
    for spec in CANONICAL_INSTRUMENTS {
        if spec.symbol == symbol {
            return spec;
        }
    }
    InstrumentSpec {
        instrument_id: 0,
        symbol: "UNKNOWN",
        tick_size_fp: 1_000_000,
        lot_size_fp: 1_000,
        default_heatmap_bucket_fp: 100_000_000,
    }
}
