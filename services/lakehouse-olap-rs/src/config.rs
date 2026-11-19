use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Canonical fixed-point scale (1.00000000 = 100_000_000 atomic ticks).
pub const FP_SCALE: i64 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;
pub const NS_PER_MS: i64 = 1_000_000;
pub const NS_PER_SEC: i64 = 1_000_000_000;
pub const NS_PER_HOUR: u64 = 3_600_000_000_000;

/// Configuration for the Channel 17 Columnar Lakehouse OLAP Engine.
#[derive(Debug, Clone)]
pub struct LakehouseConfig {
    /// Root storage directory containing partitioned Parquet tables (`trades`, `quotes`, `fraud_alerts`).
    pub storage_root: PathBuf,
    /// HTTP REST / Arrow IPC / Prometheus / Health endpoint bind address.
    pub http_bind_addr: SocketAddr,
    /// Apache Arrow Flight gRPC streaming bind address.
    pub flight_bind_addr: SocketAddr,
    /// Per-query execution memory pool quota in bytes (default: 256 MiB).
    pub max_query_memory_bytes: usize,
    /// Hard wall-clock execution timeout per query (default: 5,000 ms).
    pub query_timeout: Duration,
    /// Maximum rows returned over JSON HTTP endpoint to protect MCP/LLM callers (default: 50,000).
    pub max_json_rows: usize,
    /// Maximum AST recursion depth allowed by SqlAstGuard.
    pub max_ast_depth: usize,
    /// Vectorized Arrow batch size during DataFusion execution (default: 32,768 rows).
    pub execution_batch_size: usize,
    /// Parallel execution partitions (defaults to logical CPU count).
    pub target_partitions: usize,
    /// Parquet Zstd compression level (1..=22, default: 3 for optimal speed/ratio).
    pub zstd_compression_level: i32,
    /// Target rows per Parquet row group for fine-grained min/max zone-map & Bloom pruning.
    pub parquet_row_group_size: usize,
    /// Split-Block Bloom Filter false-positive probability on account ID columns.
    pub bloom_filter_fpp_ppm: u32,
    /// Expected distinct account IDs per row group for SBBF sizing.
    pub bloom_filter_ndv: u64,
    /// Minimum small L0 Parquet files in a partition before Background Compactor merges them.
    pub compaction_min_files: usize,
    /// Maximum input file size (bytes) eligible as an L0 fragment for compaction.
    pub compaction_max_fragment_bytes: u64,
    /// Background compaction sweep interval.
    pub compaction_interval: Duration,
}

/// Canonical Redpanda topics archived by Channel 17 (`00-hyperion-contract.md` §4.2 & `01-hyperion-omega.md` §3).
pub const ARCHIVED_REDPANDA_TOPICS: &[&str] = &[
    "hyperion.market.executions.v1",
    "hyperion.market.l2-snapshots.v1",
    "hyperion.market.l3-deltas.v1",
    "hyperion.ledger.journals.v1",
    "hyperion.fraud.alerts.v1",
    "hyperion.omega.consensus.events.v1",
    "hyperion.omega.zk.receipts.v1",
    "hyperion.omega.margin.snapshots.v1",
    "hyperion.omega.margin.liquidations.v1",
    "hyperion.omega.wasm.fuel.v1",
    "hyperion.omega.settlement.deposits.v1",
    "hyperion.omega.settlement.batches.v1",
];

impl Default for LakehouseConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .max(4);
        Self {
            storage_root: PathBuf::from("./data/lakehouse"),
            // Locked ports per `.agents/rules/01-hyperion-omega.md` §2:
            // HTTP/REST/Arrow-IPC = 8096, gRPC Arrow Flight = 50166
            http_bind_addr: SocketAddr::from(([0, 0, 0, 0], 8096)),
            flight_bind_addr: SocketAddr::from(([0, 0, 0, 0], 50166)),
            max_query_memory_bytes: 256 * 1024 * 1024,
            query_timeout: Duration::from_millis(5_000),
            max_json_rows: 50_000,
            max_ast_depth: 24,
            execution_batch_size: 65_536,
            target_partitions: cpus,
            zstd_compression_level: 1,
            parquet_row_group_size: 131_072,
            bloom_filter_fpp_ppm: 10_000, // 1.0% FPP (10,000 ppm)
            bloom_filter_ndv: 16_384,
            compaction_min_files: 3,
            compaction_max_fragment_bytes: 32 * 1024 * 1024,
            compaction_interval: Duration::from_secs(10),
        }
    }
}

impl LakehouseConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(root) = std::env::var("HYPERION_LAKEHOUSE_DIR") {
            cfg.storage_root = PathBuf::from(root);
        }
        if let Ok(http_port) = std::env::var("HYPERION_LAKEHOUSE_HTTP_PORT") {
            if let Ok(port) = http_port.parse::<u16>() {
                cfg.http_bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
            }
        }
        if let Ok(flight_port) = std::env::var("HYPERION_LAKEHOUSE_FLIGHT_PORT") {
            if let Ok(port) = flight_port.parse::<u16>() {
                cfg.flight_bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
            }
        }
        if let Ok(mem_mb) = std::env::var("HYPERION_LAKEHOUSE_MAX_QUERY_MEM_MB") {
            if let Ok(mb) = mem_mb.parse::<usize>() {
                cfg.max_query_memory_bytes = mb.saturating_mul(1024 * 1024);
            }
        }
        if let Ok(timeout_ms) = std::env::var("HYPERION_LAKEHOUSE_QUERY_TIMEOUT_MS") {
            if let Ok(ms) = timeout_ms.parse::<u64>() {
                cfg.query_timeout = Duration::from_millis(ms);
            }
        }
        cfg
    }
}
