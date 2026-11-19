use std::collections::HashSet;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::TableProvider;
use datafusion::datasource::file_format::parquet::ParquetFormat;
use datafusion::datasource::listing::{
    ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
};
use datafusion::datasource::ViewTable;
use datafusion::execution::cache::cache_manager::CacheManagerConfig;
use datafusion::execution::cache::cache_unit::{
    DefaultFileStatisticsCache, DefaultListFilesCache,
};
use datafusion::execution::cache::CacheAccessor;
use datafusion::execution::context::{QueryPlanner, SessionConfig, SessionState};
use datafusion::execution::memory_pool::GreedyMemoryPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::execution::SessionStateBuilder;
use datafusion::logical_expr::{col, Extension, LogicalPlan, LogicalPlanBuilder};
use datafusion::physical_plan::display::DisplayableExecutionPlan;
use datafusion::physical_plan::{collect, displayable, ExecutionPlan};
use datafusion::physical_planner::{DefaultPhysicalPlanner, PhysicalPlanner};
use datafusion::prelude::SessionContext;
use parking_lot::RwLock;
use url::Url;

use crate::arrow_writer::{
    ingest_alerts_partitioned, ingest_quotes_partitioned, ingest_trades_partitioned,
    WrittenSegmentMeta,
};
use crate::ast_guard::{
    rewrite_asof_query_to_view_sql, validate_forensic_sql, ExtractedAsofSpec,
};
use crate::compactor::{run_compaction_pass, CompactionReport};
use crate::config::LakehouseConfig;
use crate::error::{LakehouseError, Result};
use crate::schema::{
    fraud_alerts_physical_schema, quotes_physical_schema, trades_physical_schema,
    FraudAlertRecord, QuoteRecord, TradeRecord, TABLE_FRAUD_ALERTS, TABLE_QUOTES, TABLE_TRADES,
};
use crate::udfs::asof_join::{AsofJoinNode, AsofJoinPhysicalPlanner};
use crate::udfs::markout::MarketMidTape;
use crate::udfs::register_financial_udfs;
use crate::wal_ingest::parse_cold_wal_segment;

struct LakehouseQueryPlanner {
    inner: DefaultPhysicalPlanner,
}

impl std::fmt::Debug for LakehouseQueryPlanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LakehouseQueryPlanner").finish()
    }
}

impl Default for LakehouseQueryPlanner {
    fn default() -> Self {
        Self {
            inner: DefaultPhysicalPlanner::with_extension_planners(vec![Arc::new(
                AsofJoinPhysicalPlanner,
            )]),
        }
    }
}

#[async_trait]
impl QueryPlanner for LakehouseQueryPlanner {
    async fn create_physical_plan(
        &self,
        logical_plan: &LogicalPlan,
        session_state: &SessionState,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        self.inner
            .create_physical_plan(logical_plan, session_state)
            .await
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryRequestOptions {
    pub memory_limit_bytes: Option<usize>,
    pub timeout: Option<Duration>,
    pub max_rows: Option<usize>,
    pub capture_plan: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PlanInspectionSummary {
    pub physical_plan_tree: String,
    pub has_parquet_scan: bool,
    pub has_pushed_predicate: bool,
    pub has_asof_join_exec: bool,
    pub total_parquet_files_scheduled: usize,
    pub execution_metrics_text: String,
}

#[derive(Debug)]
pub struct QueryExecutionOutcome {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
    pub row_count: usize,
    pub elapsed_us: u64,
    pub plan_summary: Option<PlanInspectionSummary>,
}

use std::ops::Deref;

/// 64-byte cache-line aligned atomic counter preventing cross-core MESI false sharing.
#[repr(C, align(64))]
#[derive(Debug, Default)]
pub struct CachePaddedAtomicU64(pub AtomicU64);

impl Deref for CachePaddedAtomicU64 {
    type Target = AtomicU64;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Default)]
pub struct EngineTelemetry {
    pub queries_total: CachePaddedAtomicU64,
    pub queries_rejected_ast: CachePaddedAtomicU64,
    pub queries_timed_out: CachePaddedAtomicU64,
    pub queries_oom_blocked: CachePaddedAtomicU64,
    pub ingested_trade_rows: CachePaddedAtomicU64,
    pub ingested_quote_rows: CachePaddedAtomicU64,
    pub ingested_alert_rows: CachePaddedAtomicU64,
    pub compactions_completed: CachePaddedAtomicU64,
}

struct RegisteredProviders {
    trades: Arc<dyn TableProvider>,
    quotes: Arc<dyn TableProvider>,
    fraud_alerts: Arc<dyn TableProvider>,
}

/// Core Channel 17 Columnar Lakehouse OLAP Engine.
pub struct LakehouseEngine {
    cfg: LakehouseConfig,
    mid_tape: Arc<MarketMidTape>,
    providers: RwLock<Option<RegisteredProviders>>,
    list_files_cache: Arc<DefaultListFilesCache>,
    file_stats_cache: Arc<DefaultFileStatisticsCache>,
    pub telemetry: Arc<EngineTelemetry>,
}

impl LakehouseEngine {
    pub async fn try_new(cfg: LakehouseConfig) -> Result<Arc<Self>> {
        fs::create_dir_all(cfg.storage_root.join(TABLE_TRADES))?;
        fs::create_dir_all(cfg.storage_root.join(TABLE_QUOTES))?;
        fs::create_dir_all(cfg.storage_root.join(TABLE_FRAUD_ALERTS))?;

        let engine = Arc::new(Self {
            cfg,
            mid_tape: MarketMidTape::new(),
            providers: RwLock::new(None),
            list_files_cache: Arc::new(DefaultListFilesCache::default()),
            file_stats_cache: Arc::new(DefaultFileStatisticsCache::default()),
            telemetry: Arc::new(EngineTelemetry::default()),
        });
        engine.refresh_table_providers().await?;
        Ok(engine)
    }

    pub fn config(&self) -> &LakehouseConfig {
        &self.cfg
    }

    pub fn mid_tape(&self) -> Arc<MarketMidTape> {
        Arc::clone(&self.mid_tape)
    }

    /// Refreshes cached Hive-partitioned `ListingTable` providers (`symbol`, `hour_bucket`)
    /// and invalidates metadata caches so newly ingested/compacted segments become immediately visible.
    pub async fn refresh_table_providers(&self) -> Result<()> {
        self.list_files_cache.clear();
        self.file_stats_cache.clear();
        let trades = self
            .build_partitioned_listing_table(TABLE_TRADES, trades_physical_schema())
            .await?;
        let quotes = self
            .build_partitioned_listing_table(TABLE_QUOTES, quotes_physical_schema())
            .await?;
        let fraud_alerts = self
            .build_partitioned_listing_table(TABLE_FRAUD_ALERTS, fraud_alerts_physical_schema())
            .await?;

        *self.providers.write() = Some(RegisteredProviders {
            trades,
            quotes,
            fraud_alerts,
        });
        Ok(())
    }

    async fn build_partitioned_listing_table(
        &self,
        table_name: &str,
        physical_schema: SchemaRef,
    ) -> Result<Arc<dyn TableProvider>> {
        let abs_dir = fs::canonicalize(self.cfg.storage_root.join(table_name))?;
        // Strip Windows `\\?\` extended-length prefix so `Url::from_directory_path` stays clean
        let clean_str = abs_dir
            .to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(std::path::PathBuf::from)
            .unwrap_or(abs_dir);

        let dir_url = Url::from_directory_path(&clean_str).map_err(|_| {
            LakehouseError::InvalidInput(format!("Invalid directory path for {table_name}"))
        })?;
        let table_url = ListingTableUrl::parse(dir_url.as_str())?;

        let parquet_format = ParquetFormat::default()
            .with_enable_pruning(true)
            .with_skip_metadata(false);

        let sort_order = vec![vec![
            col("instrument_id").sort(true, false),
            col("timestamp_ns").sort(true, false),
            col("seq_id").sort(true, false),
        ]];

        let listing_opts = ListingOptions::new(Arc::new(parquet_format))
            .with_file_extension(".parquet")
            .with_target_partitions(self.cfg.target_partitions)
            .with_collect_stat(true)
            .with_table_partition_cols(vec![
                ("symbol".to_string(), DataType::Utf8),
                ("hour_bucket".to_string(), DataType::UInt64),
            ])
            .with_file_sort_order(sort_order);

        let cfg = ListingTableConfig::new(table_url)
            .with_listing_options(listing_opts)
            .with_schema(physical_schema);

        let table = ListingTable::try_new(cfg)?;
        Ok(Arc::new(table))
    }

    /// Ingests `TradeRecord`s into partitioned Zstd Parquet segments.
    pub fn ingest_trades(&self, records: &[TradeRecord]) -> Result<Vec<WrittenSegmentMeta>> {
        let segs = ingest_trades_partitioned(&self.cfg, records)?;
        self.telemetry
            .ingested_trade_rows
            .fetch_add(records.len() as u64, Ordering::Relaxed);
        Ok(segs)
    }

    /// Ingests `QuoteRecord`s into partitioned Zstd Parquet segments and updates `MarketMidTape`.
    pub fn ingest_quotes(&self, records: &[QuoteRecord]) -> Result<Vec<WrittenSegmentMeta>> {
        let segs = ingest_quotes_partitioned(&self.cfg, records)?;
        self.mid_tape.ingest_quotes(records);
        self.telemetry
            .ingested_quote_rows
            .fetch_add(records.len() as u64, Ordering::Relaxed);
        Ok(segs)
    }

    /// Ingests `FraudAlertRecord`s into partitioned Zstd Parquet segments.
    pub fn ingest_alerts(&self, records: &[FraudAlertRecord]) -> Result<Vec<WrittenSegmentMeta>> {
        let segs = ingest_alerts_partitioned(&self.cfg, records)?;
        self.telemetry
            .ingested_alert_rows
            .fetch_add(records.len() as u64, Ordering::Relaxed);
        Ok(segs)
    }

    /// Zero-copy parses and ingests a Cold WAL (`HYPWAL01`) binary segment into partitioned Parquet.
    pub fn ingest_cold_wal_segment(
        &self,
        segment_bytes: &[u8],
        expected_initial_prev_hash: Option<[u8; 32]>,
    ) -> Result<(usize, Vec<WrittenSegmentMeta>)> {
        let decoded = parse_cold_wal_segment(segment_bytes, expected_initial_prev_hash)?;
        let mut written = Vec::new();
        if !decoded.trades.is_empty() {
            written.extend(self.ingest_trades(&decoded.trades)?);
        }
        if !decoded.quotes.is_empty() {
            written.extend(self.ingest_quotes(&decoded.quotes)?);
        }
        if !decoded.alerts.is_empty() {
            written.extend(self.ingest_alerts(&decoded.alerts)?);
        }
        Ok((decoded.verified_wal_frames, written))
    }

    /// Triggers an immediate compaction pass across all partitions.
    pub fn compact_now(&self, force: bool) -> Result<CompactionReport> {
        let report = run_compaction_pass(&self.cfg, force)?;
        if report.partitions_compacted > 0 {
            self.telemetry
                .compactions_completed
                .fetch_add(report.partitions_compacted as u64, Ordering::Relaxed);
        }
        Ok(report)
    }

    fn build_isolated_session(&self, mem_limit_bytes: usize) -> Result<SessionContext> {
        let mut session_cfg = SessionConfig::new()
            .with_target_partitions(self.cfg.target_partitions)
            .with_batch_size(self.cfg.execution_batch_size)
            .with_collect_statistics(true)
            .with_parquet_pruning(true);

        {
            let pq = &mut session_cfg.options_mut().execution.parquet;
            pq.pushdown_filters = true;
            pq.reorder_filters = true;
            pq.bloom_filter_on_read = true;
            pq.enable_page_index = true;
        }

        let cache_cfg = CacheManagerConfig::default()
            .with_list_files_cache(Some(Arc::clone(&self.list_files_cache) as _))
            .with_files_statistics_cache(Some(Arc::clone(&self.file_stats_cache) as _));

        let runtime_env = RuntimeEnvBuilder::new()
            .with_memory_pool(Arc::new(GreedyMemoryPool::new(mem_limit_bytes)))
            .with_cache_manager(cache_cfg)
            .build_arc()?;

        let state = SessionStateBuilder::new()
            .with_config(session_cfg)
            .with_runtime_env(runtime_env)
            .with_default_features()
            .with_query_planner(Arc::new(LakehouseQueryPlanner::default()))
            .build();

        let ctx = SessionContext::new_with_state(state);
        register_financial_udfs(&ctx, Arc::clone(&self.mid_tape));

        let guard = self.providers.read();
        if let Some(ref p) = *guard {
            ctx.register_table(TABLE_TRADES, Arc::clone(&p.trades))?;
            ctx.register_table(TABLE_QUOTES, Arc::clone(&p.quotes))?;
            ctx.register_table(TABLE_FRAUD_ALERTS, Arc::clone(&p.fraud_alerts))?;
        }

        Ok(ctx)
    }

    async fn build_logical_plan_for_validated(
        &self,
        ctx: &SessionContext,
        sql: &str,
        asof_opt: Option<&ExtractedAsofSpec>,
    ) -> Result<LogicalPlan> {
        let Some(spec) = asof_opt else {
            let df = ctx.sql(sql).await?;
            return Ok(df.into_optimized_plan()?);
        };

        // Construct Left and Right sub-plans, disambiguating overlapping Right column names
        // so ViewTable has unique field names while preserving original projection aliases!
        let left_df = ctx
            .sql(&format!("SELECT * FROM {}", spec.left_relation_sql))
            .await?;
        let left_plan = left_df.into_unoptimized_plan();
        let left_names: HashSet<String> = left_plan
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().to_ascii_lowercase())
            .collect();

        let right_df = ctx
            .sql(&format!("SELECT * FROM {}", spec.right_relation_sql))
            .await?;
        let right_raw = right_df.into_unoptimized_plan();

        let mut proj_exprs = Vec::with_capacity(right_raw.schema().fields().len());
        let mut right_renames: Vec<(String, String)> = Vec::new();
        let mut effective_right_key = spec.right_key_col.clone();
        let mut effective_right_ts = spec.right_ts_col.clone();

        for f in right_raw.schema().fields() {
            let orig = f.name().clone();
            if left_names.contains(&orig.to_ascii_lowercase()) {
                let disambig = format!("__asof_rhs_{orig}");
                if orig.eq_ignore_ascii_case(&spec.right_key_col) {
                    effective_right_key = disambig.clone();
                }
                if orig.eq_ignore_ascii_case(&spec.right_ts_col) {
                    effective_right_ts = disambig.clone();
                }
                proj_exprs.push(col(&orig).alias(&disambig));
                right_renames.push((orig, disambig));
            } else {
                proj_exprs.push(col(&orig));
            }
        }

        let right_plan = LogicalPlanBuilder::from(right_raw)
            .project(proj_exprs)?
            .build()?;

        let asof_node = AsofJoinNode::try_new(
            left_plan,
            right_plan,
            spec.left_key_col.clone(),
            effective_right_key,
            spec.left_ts_col.clone(),
            effective_right_ts,
            spec.strict_lt,
            spec.is_left_join,
        )?;
        let asof_ext_plan = LogicalPlan::Extension(Extension {
            node: Arc::new(asof_node),
        });

        let view_name = "__hyperion_asof_rel";
        let view_provider = ViewTable::new(asof_ext_plan, None);
        ctx.register_table(view_name, Arc::new(view_provider))?;

        let rewritten_sql =
            rewrite_asof_query_to_view_sql(sql, spec, view_name, &right_renames)?;
        let outer_df = ctx.sql(&rewritten_sql).await?;
        Ok(outer_df.into_optimized_plan()?)
    }

    /// Validates SQL via `SqlAstGuard`, plans with DataFusion, enforces per-query memory pool
    /// and hard wall-clock timeout, and returns Arrow `RecordBatch`es + optional plan diagnostics.
    pub async fn execute_sql(
        &self,
        sql: &str,
        opts: QueryRequestOptions,
    ) -> Result<QueryExecutionOutcome> {
        let start = Instant::now();
        self.telemetry.queries_total.fetch_add(1, Ordering::Relaxed);

        let validated = match validate_forensic_sql(sql, self.cfg.max_ast_depth) {
            Ok(v) => v,
            Err(err) => {
                self.telemetry
                    .queries_rejected_ast
                    .fetch_add(1, Ordering::Relaxed);
                return Err(err);
            }
        };

        let mem_limit = opts
            .memory_limit_bytes
            .unwrap_or(self.cfg.max_query_memory_bytes);
        let timeout_dur = opts.timeout.unwrap_or(self.cfg.query_timeout);
        let timeout_ms = timeout_dur.as_millis() as u64;

        let exec_future = async {
            let ctx = self.build_isolated_session(mem_limit)?;
            let optimized_plan = self
                .build_logical_plan_for_validated(
                    &ctx,
                    &validated.normalized_sql,
                    validated.top_level_asof.as_ref(),
                )
                .await?;

            let physical_plan = ctx
                .state()
                .create_physical_plan(&optimized_plan)
                .await
                .map_err(|e| map_df_err(e, mem_limit))?;

            let schema = physical_plan.schema();
            let task_ctx = ctx.task_ctx();

            let mut batches = collect(Arc::clone(&physical_plan), task_ctx)
                .await
                .map_err(|e| map_df_err(e, mem_limit))?;

            if let Some(limit_rows) = opts.max_rows {
                batches = truncate_batches(batches, limit_rows);
            }

            let row_count: usize = batches.iter().map(RecordBatch::num_rows).sum();
            let plan_summary = if opts.capture_plan || validated.is_explain {
                Some(inspect_physical_plan(physical_plan.as_ref()))
            } else {
                None
            };

            Ok::<QueryExecutionOutcome, LakehouseError>(QueryExecutionOutcome {
                schema,
                batches,
                row_count,
                elapsed_us: start.elapsed().as_micros() as u64,
                plan_summary,
            })
        };

        match tokio::time::timeout(timeout_dur, exec_future).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(err)) => {
                if matches!(err, LakehouseError::MemoryQuotaExceeded { .. }) {
                    self.telemetry
                        .queries_oom_blocked
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(err)
            }
            Err(_) => {
                self.telemetry
                    .queries_timed_out
                    .fetch_add(1, Ordering::Relaxed);
                Err(LakehouseError::QueryTimeout(timeout_ms))
            }
        }
    }

    /// Plans a validated read-only SQL query and inspects its `PhysicalExecutionPlan` without scanning data.
    pub async fn explain_physical_plan(&self, sql: &str) -> Result<PlanInspectionSummary> {
        let validated = match validate_forensic_sql(sql, self.cfg.max_ast_depth) {
            Ok(v) => v,
            Err(err) => {
                self.telemetry
                    .queries_rejected_ast
                    .fetch_add(1, Ordering::Relaxed);
                return Err(err);
            }
        };

        let ctx = self.build_isolated_session(self.cfg.max_query_memory_bytes)?;
        let optimized_plan = self
            .build_logical_plan_for_validated(
                &ctx,
                &validated.normalized_sql,
                validated.top_level_asof.as_ref(),
            )
            .await?;
        let physical_plan = ctx.state().create_physical_plan(&optimized_plan).await?;
        Ok(inspect_physical_plan(physical_plan.as_ref()))
    }
}

fn map_df_err(err: datafusion::error::DataFusionError, limit_bytes: usize) -> LakehouseError {
    let msg = err.to_string();
    if msg.contains("Resources exhausted")
        || msg.contains("Failed to allocate")
        || msg.contains("Not enough memory")
    {
        LakehouseError::MemoryQuotaExceeded {
            limit_bytes,
            detail: msg,
        }
    } else {
        LakehouseError::DataFusion(msg)
    }
}

fn truncate_batches(batches: Vec<RecordBatch>, max_rows: usize) -> Vec<RecordBatch> {
    let mut remaining = max_rows;
    let mut out = Vec::with_capacity(batches.len());
    for b in batches {
        if remaining == 0 {
            break;
        }
        if b.num_rows() <= remaining {
            remaining -= b.num_rows();
            out.push(b);
        } else {
            out.push(b.slice(0, remaining));
            break;
        }
    }
    out
}

/// Inspects DataFusion's `PhysicalExecutionPlan` tree to extract verifiable proof of
/// Parquet scans, Partition Pruning (`file_groups`), Predicate Pushdown (`predicate=`),
/// `AsofJoinExec`, and post-execution RowGroup/Bloom pruning metrics.
pub fn inspect_physical_plan(plan: &dyn ExecutionPlan) -> PlanInspectionSummary {
    let tree = displayable(plan).indent(true).to_string();
    let with_metrics = DisplayableExecutionPlan::with_metrics(plan)
        .indent(true)
        .to_string();

    let has_parquet_scan = tree.contains("DataSourceExec:") || tree.contains("ParquetExec:");
    let has_pushed_predicate = tree.contains("predicate=");
    let has_asof_join_exec = tree.contains("AsofJoinExec:");

    // Count distinct `.parquet` file paths inside `file_groups={...}` (DataFusion may split a single
    // large Parquet file into multiple parallel byte-range slices like `seg.parquet:0..4500000`).
    let mut unique_files: HashSet<&str> = HashSet::new();
    for token in tree.split(['[', ']', '{', '}', ',', ' ', '\n', '\r']) {
        if let Some(end_idx) = token.find(".parquet") {
            let path_part = &token[..end_idx + ".parquet".len()];
            if !path_part.is_empty() {
                unique_files.insert(path_part);
            }
        }
    }
    let total_parquet_files_scheduled = unique_files.len();

    PlanInspectionSummary {
        physical_plan_tree: tree,
        has_parquet_scan,
        has_pushed_predicate,
        has_asof_join_exec,
        total_parquet_files_scheduled,
        execution_metrics_text: with_metrics,
    }
}
