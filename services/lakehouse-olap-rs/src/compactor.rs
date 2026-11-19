use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use arrow::array::{BooleanArray, Int64Array, RecordBatch, UInt32Array, UInt64Array};
use arrow::compute::{concat_batches, filter_record_batch};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::file::statistics::Statistics;
use tokio::task::JoinHandle;

use crate::arrow_writer::{
    sort_batch_by_composite_key, write_sorted_partition_batch, PartitionKey, WrittenSegmentMeta,
};
use crate::config::LakehouseConfig;
use crate::error::Result;
use crate::schema::{TABLE_FRAUD_ALERTS, TABLE_QUOTES, TABLE_TRADES};

#[derive(Debug, Clone, Default)]
pub struct CompactionReport {
    pub partitions_compacted: usize,
    pub input_files_merged: usize,
    pub output_segments_written: usize,
    pub rows_before_dedup: usize,
    pub rows_after_dedup: usize,
    pub output_bytes_total: u64,
}

#[derive(Debug, Clone)]
pub struct ParquetSegmentInspection {
    pub num_rows: i64,
    pub num_row_groups: usize,
    pub all_zstd_compressed: bool,
    pub sorting_column_indices: Vec<i32>,
    pub bloom_filter_columns: Vec<String>,
    pub dictionary_encoded_columns: Vec<String>,
    pub row_group_ts_bounds: Vec<(i64, i64)>,
}

/// Inspects physical Parquet metadata to verify Zstd compression, `SortingColumn`s,
/// Split-Block Bloom Filter offsets, Dictionary page offsets, and RowGroup min/max zone-maps.
pub fn inspect_parquet_segment(path: &Path) -> Result<ParquetSegmentInspection> {
    let file = File::open(path)?;
    let reader = SerializedFileReader::new(file)?;
    let meta = reader.metadata();
    let file_meta = meta.file_metadata();

    let mut all_zstd = true;
    let mut sorting_indices = Vec::new();
    let mut bloom_cols = Vec::new();
    let mut dict_cols = Vec::new();
    let mut ts_bounds = Vec::new();

    if let Some(rg0) = meta.row_groups().first() {
        if let Some(sorting) = rg0.sorting_columns() {
            sorting_indices = sorting.iter().map(|s| s.column_idx).collect();
        }
        for col in rg0.columns() {
            if !matches!(col.compression(), Compression::ZSTD(_)) {
                all_zstd = false;
            }
            let col_name = col.column_descr().name().to_string();
            if col.bloom_filter_offset().is_some() {
                bloom_cols.push(col_name.clone());
            }
            if col.dictionary_page_offset().is_some() {
                dict_cols.push(col_name);
            }
        }
    }

    for rg in meta.row_groups() {
        // Column 1 is always `timestamp_ns` (Int64)
        if let Some(ts_col) = rg.columns().get(1) {
            if let Some(Statistics::Int64(stats)) = ts_col.statistics() {
                if let (Some(&min_v), Some(&max_v)) = (stats.min_opt(), stats.max_opt()) {
                    ts_bounds.push((min_v, max_v));
                }
            }
        }
    }

    Ok(ParquetSegmentInspection {
        num_rows: file_meta.num_rows(),
        num_row_groups: meta.num_row_groups(),
        all_zstd_compressed: all_zstd,
        sorting_column_indices: sorting_indices,
        bloom_filter_columns: bloom_cols,
        dictionary_encoded_columns: dict_cols,
        row_group_ts_bounds: ts_bounds,
    })
}

/// Deduplicates adjacent identical `(instrument_id, timestamp_ns, seq_id)` rows in a pre-sorted batch.
fn deduplicate_sorted_batch(sorted: &RecordBatch) -> Result<RecordBatch> {
    let n = sorted.num_rows();
    if n <= 1 {
        return Ok(sorted.clone());
    }
    let inst = sorted
        .column(0)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .expect("col 0 UInt32");
    let ts = sorted
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("col 1 Int64");
    let seq = sorted
        .column(2)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .expect("col 2 UInt64");

    let inst_s = inst.values();
    let ts_s = ts.values();
    let seq_s = seq.values();

    let mut has_dup = false;
    for i in 1..n {
        if inst_s[i] == inst_s[i - 1] && ts_s[i] == ts_s[i - 1] && seq_s[i] == seq_s[i - 1] {
            has_dup = true;
            break;
        }
    }
    if !has_dup {
        return Ok(sorted.clone());
    }

    let mut keep = Vec::with_capacity(n);
    keep.push(true);
    for i in 1..n {
        let is_same =
            inst_s[i] == inst_s[i - 1] && ts_s[i] == ts_s[i - 1] && seq_s[i] == seq_s[i - 1];
        keep.push(!is_same);
    }
    let mask = BooleanArray::from(keep);
    Ok(filter_record_batch(sorted, &mask)?)
}

/// Compacts all eligible partitions across `trades`, `quotes`, and `fraud_alerts`.
/// When `force` is `true`, compacts any partition with `>= 2` Parquet files regardless of size.
pub fn run_compaction_pass(cfg: &LakehouseConfig, force: bool) -> Result<CompactionReport> {
    let mut report = CompactionReport::default();

    let tables: [(&str, &[&str]); 3] = [
        (
            TABLE_TRADES,
            &["maker_account_id", "taker_account_id", "account_id"],
        ),
        (TABLE_QUOTES, &[]),
        (TABLE_FRAUD_ALERTS, &["account_id"]),
    ];

    for (table_name, bloom_cols) in tables {
        let table_dir = cfg.storage_root.join(table_name);
        if !table_dir.exists() {
            continue;
        }
        for sym_entry in fs::read_dir(&table_dir)? {
            let sym_entry = sym_entry?;
            let sym_name = sym_entry.file_name().to_string_lossy().to_string();
            let Some(symbol) = sym_name.strip_prefix("symbol=") else {
                continue;
            };
            if !sym_entry.file_type()?.is_dir() {
                continue;
            }

            for hr_entry in fs::read_dir(sym_entry.path())? {
                let hr_entry = hr_entry?;
                let hr_name = hr_entry.file_name().to_string_lossy().to_string();
                let Some(hr_str) = hr_name.strip_prefix("hour_bucket=") else {
                    continue;
                };
                let Ok(hour_bucket) = hr_str.parse::<u64>() else {
                    continue;
                };
                if !hr_entry.file_type()?.is_dir() {
                    continue;
                }

                let part_key = PartitionKey {
                    symbol: symbol.to_string(),
                    hour_bucket,
                };

                if let Some((merged_count, rows_in, seg_meta)) =
                    compact_single_partition(cfg, table_name, &part_key, &hr_entry.path(), bloom_cols, force)?
                {
                    report.partitions_compacted += 1;
                    report.input_files_merged += merged_count;
                    report.output_segments_written += 1;
                    report.rows_before_dedup += rows_in;
                    report.rows_after_dedup += seg_meta.row_count;
                    report.output_bytes_total += seg_meta.byte_size;
                }
            }
        }
    }

    Ok(report)
}

fn compact_single_partition(
    cfg: &LakehouseConfig,
    table_name: &str,
    part_key: &PartitionKey,
    part_dir: &Path,
    bloom_cols: &[&str],
    force: bool,
) -> Result<Option<(usize, usize, WrittenSegmentMeta)>> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(part_dir)? {
        let entry = entry?;
        let path = entry.path();
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.starts_with('.') || !fname.ends_with(".parquet") {
            continue;
        }
        let meta = entry.metadata()?;
        if force || meta.len() <= cfg.compaction_max_fragment_bytes || fname.starts_with("l0_") {
            candidates.push(path);
        }
    }

    let min_needed = if force { 2 } else { cfg.compaction_min_files.max(2) };
    if candidates.len() < min_needed {
        return Ok(None);
    }
    candidates.sort();

    let mut batches: Vec<RecordBatch> = Vec::new();
    let mut schema_opt = None;
    let mut rows_in = 0_usize;

    for file_path in &candidates {
        let file = File::open(file_path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?
            .with_batch_size(cfg.execution_batch_size);
        if schema_opt.is_none() {
            schema_opt = Some(builder.schema().clone());
        }
        let reader = builder.build()?;
        for b_res in reader {
            let b = b_res?;
            rows_in += b.num_rows();
            batches.push(b);
        }
    }

    let Some(schema) = schema_opt else {
        return Ok(None);
    };
    if batches.is_empty() || rows_in == 0 {
        return Ok(None);
    }

    let combined = concat_batches(&schema, &batches)?;
    let sorted = sort_batch_by_composite_key(&combined)?;
    let deduped = deduplicate_sorted_batch(&sorted)?;

    let written = write_sorted_partition_batch(
        cfg,
        table_name,
        part_key,
        &deduped,
        bloom_cols,
        "compacted",
    )?;

    // Atomically unlink merged L0 fragments only after the compacted L1 segment is durable on disk.
    for old_path in &candidates {
        if old_path != &written.file_path {
            let _ = fs::remove_file(old_path);
        }
    }

    Ok(Some((candidates.len(), rows_in, written)))
}

pub type CompactionCallback = Arc<dyn Fn(&CompactionReport) + Send + Sync>;

/// Spawns an asynchronous background compactor loop with graceful shutdown flag.
pub struct BackgroundCompactorHandle {
    stop_flag: Arc<AtomicBool>,
    join_handle: JoinHandle<()>,
}

impl BackgroundCompactorHandle {
    pub fn spawn(cfg: LakehouseConfig, on_compacted: Option<CompactionCallback>) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);

        let join_handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(cfg.compaction_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            while !flag_clone.load(Ordering::Relaxed) {
                ticker.tick().await;
                if flag_clone.load(Ordering::Relaxed) {
                    break;
                }
                let cfg_step = cfg.clone();
                if let Ok(Ok(report)) =
                    tokio::task::spawn_blocking(move || run_compaction_pass(&cfg_step, false)).await
                {
                    if report.partitions_compacted > 0 {
                        if let Some(ref cb) = on_compacted {
                            cb(&report);
                        }
                    }
                }
            }
        });

        Self {
            stop_flag,
            join_handle,
        }
    }

    pub async fn shutdown(self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.join_handle.abort();
        let _ = self.join_handle.await;
    }
}
