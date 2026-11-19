use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Int64Array, RecordBatch, StringArray, StringDictionaryBuilder, UInt32Array,
    UInt64Array,
};
use arrow::compute::{lexsort_to_indices, take_record_batch, SortColumn, SortOptions};
use arrow::datatypes::Int8Type;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::{EnabledStatistics, WriterProperties, WriterVersion};
use parquet::format::SortingColumn;
use parquet::schema::types::ColumnPath;

use crate::config::LakehouseConfig;
use crate::error::{LakehouseError, Result};
use crate::schema::{
    fraud_alerts_physical_schema, hour_bucket_from_ts_ns, quotes_physical_schema,
    trades_physical_schema, FraudAlertRecord, QuoteRecord, TradeRecord, TABLE_FRAUD_ALERTS,
    TABLE_QUOTES, TABLE_TRADES,
};

static SEGMENT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartitionKey {
    pub symbol: String,
    pub hour_bucket: u64,
}

impl PartitionKey {
    pub fn directory_path(&self, storage_root: &Path, table_name: &str) -> PathBuf {
        storage_root
            .join(table_name)
            .join(format!("symbol={}", self.symbol))
            .join(format!("hour_bucket={}", self.hour_bucket))
    }
}

#[derive(Debug, Clone)]
pub struct WrittenSegmentMeta {
    pub table_name: String,
    pub partition: PartitionKey,
    pub file_path: PathBuf,
    pub row_count: usize,
    pub byte_size: u64,
    pub min_ts_ns: i64,
    pub max_ts_ns: i64,
    pub min_seq_id: u64,
    pub max_seq_id: u64,
}

/// Builds Parquet `WriterProperties` with Zstd compression, targeted Dictionary encoding
/// on categorical columns (preserving native Parquet v2 `DELTA_BINARY_PACKED` on numeric columns),
/// Page-level min/max zone-maps, composite `SortingColumn` metadata `(0, 1, 2)`,
/// and Split-Block Bloom Filters on account columns.
pub fn build_parquet_writer_properties(
    cfg: &LakehouseConfig,
    account_bloom_columns: &[&str],
) -> Result<WriterProperties> {
    let zstd_lvl = ZstdLevel::try_new(cfg.zstd_compression_level)
        .map_err(|e| LakehouseError::InvalidInput(format!("Invalid Zstd level: {e}")))?;

    let sorting_cols = vec![
        SortingColumn::new(0, false, false), // instrument_id ASC
        SortingColumn::new(1, false, false), // timestamp_ns ASC
        SortingColumn::new(2, false, false), // seq_id ASC
    ];

    let mut builder = WriterProperties::builder()
        .set_writer_version(WriterVersion::PARQUET_2_0)
        .set_compression(Compression::ZSTD(zstd_lvl))
        .set_dictionary_enabled(false)
        .set_statistics_enabled(EnabledStatistics::Page)
        .set_max_row_group_size(cfg.parquet_row_group_size)
        .set_sorting_columns(Some(sorting_cols));

    // Enable Dictionary encoding exclusively on low-cardinality categorical columns
    for dict_col in [
        "aggressor_side",
        "ingest_source",
        "market_regime",
        "pattern_type",
    ] {
        builder = builder.set_column_dictionary_enabled(ColumnPath::from(dict_col), true);
    }

    for &col_name in account_bloom_columns {
        let col_path = ColumnPath::from(col_name);
        builder = builder
            .set_column_bloom_filter_enabled(col_path.clone(), true)
            .set_column_bloom_filter_fpp(col_path.clone(), 0.01)
            .set_column_bloom_filter_ndv(col_path, cfg.bloom_filter_ndv);
    }

    Ok(builder.build())
}

/// Checks if a `RecordBatch` is already strictly ordered by `(instrument_id, timestamp_ns, seq_id)`.
/// If not, performs a vectorized Arrow `lexsort_to_indices` + `take_record_batch`.
pub fn sort_batch_by_composite_key(batch: &RecordBatch) -> Result<RecordBatch> {
    if batch.num_rows() <= 1 {
        return Ok(batch.clone());
    }

    let inst_col = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(|| LakehouseError::InvalidInput("Column 0 must be UInt32 instrument_id".into()))?;
    let ts_col = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| LakehouseError::InvalidInput("Column 1 must be Int64 timestamp_ns".into()))?;
    let seq_col = batch
        .column(2)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| LakehouseError::InvalidInput("Column 2 must be UInt64 seq_id".into()))?;

    let inst_s = inst_col.values();
    let ts_s = ts_col.values();
    let seq_s = seq_col.values();

    let mut already_sorted = true;
    for i in 1..batch.num_rows() {
        let prev = (inst_s[i - 1], ts_s[i - 1], seq_s[i - 1]);
        let curr = (inst_s[i], ts_s[i], seq_s[i]);
        if curr < prev {
            already_sorted = false;
            break;
        }
    }
    if already_sorted {
        return Ok(batch.clone());
    }

    let opts = Some(SortOptions {
        descending: false,
        nulls_first: false,
    });
    let sort_cols = vec![
        SortColumn {
            values: Arc::clone(batch.column(0)),
            options: opts,
        },
        SortColumn {
            values: Arc::clone(batch.column(1)),
            options: opts,
        },
        SortColumn {
            values: Arc::clone(batch.column(2)),
            options: opts,
        },
    ];

    let indices = lexsort_to_indices(&sort_cols, None)?;
    Ok(take_record_batch(batch, &indices)?)
}

/// Converts a slice of `TradeRecord`s into a composite-sorted Arrow `RecordBatch`
/// using zero-copy `Vec<T> -> Buffer` ownership transfer and `Int8` Dictionary encoding.
pub fn trades_to_sorted_record_batch(records: &[TradeRecord]) -> Result<RecordBatch> {
    let n = records.len();
    let mut instrument_ids = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut seq_ids = Vec::with_capacity(n);
    let mut fill_ids = Vec::with_capacity(n);
    let mut maker_accounts = Vec::with_capacity(n);
    let mut taker_accounts = Vec::with_capacity(n);
    let mut primary_accounts = Vec::with_capacity(n);
    let mut side_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n, 2, 16);
    let mut side_signs = Vec::with_capacity(n);
    let mut prices = Vec::with_capacity(n);
    let mut qtys = Vec::with_capacity(n);
    let mut notionals = Vec::with_capacity(n);
    let mut maker_fees = Vec::with_capacity(n);
    let mut taker_fees = Vec::with_capacity(n);
    let mut source_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n, 2, 24);

    for r in records {
        instrument_ids.push(r.instrument_id);
        timestamps.push(r.timestamp_ns);
        seq_ids.push(r.seq_id);
        fill_ids.push(r.fill_id);
        maker_accounts.push(r.maker_account_id);
        taker_accounts.push(r.taker_account_id);
        primary_accounts.push(r.taker_account_id);
        if r.is_buy_aggressor {
            side_dict.append_value("BUY");
            side_signs.push(1_i64);
        } else {
            side_dict.append_value("SELL");
            side_signs.push(-1_i64);
        }
        prices.push(r.price_fp);
        qtys.push(r.qty_fp);
        notionals.push(r.notional_fp);
        maker_fees.push(r.maker_fee_fp);
        taker_fees.push(r.taker_fee_fp);
        source_dict.append_value(if r.from_wal { "COLD_WAL" } else { "REDPANDA" });
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(instrument_ids)),
        Arc::new(Int64Array::from(timestamps)),
        Arc::new(UInt64Array::from(seq_ids)),
        Arc::new(UInt64Array::from(fill_ids)),
        Arc::new(UInt64Array::from(maker_accounts)),
        Arc::new(UInt64Array::from(taker_accounts)),
        Arc::new(UInt64Array::from(primary_accounts)),
        Arc::new(side_dict.finish()),
        Arc::new(Int64Array::from(side_signs)),
        Arc::new(Int64Array::from(prices)),
        Arc::new(Int64Array::from(qtys)),
        Arc::new(Int64Array::from(notionals)),
        Arc::new(Int64Array::from(maker_fees)),
        Arc::new(Int64Array::from(taker_fees)),
        Arc::new(source_dict.finish()),
    ];

    let raw_batch = RecordBatch::try_new(trades_physical_schema(), columns)?;
    sort_batch_by_composite_key(&raw_batch)
}

/// Converts a slice of `QuoteRecord`s into a composite-sorted Arrow `RecordBatch`.
pub fn quotes_to_sorted_record_batch(records: &[QuoteRecord]) -> Result<RecordBatch> {
    let n = records.len();
    let mut instrument_ids = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut seq_ids = Vec::with_capacity(n);
    let mut best_bids = Vec::with_capacity(n);
    let mut best_asks = Vec::with_capacity(n);
    let mut mids = Vec::with_capacity(n);
    let mut spreads = Vec::with_capacity(n);
    let mut bid_depths = Vec::with_capacity(n);
    let mut ask_depths = Vec::with_capacity(n);
    let mut regime_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n, 4, 64);

    for r in records {
        instrument_ids.push(r.instrument_id);
        timestamps.push(r.timestamp_ns);
        seq_ids.push(r.seq_id);
        best_bids.push(r.best_bid_fp);
        best_asks.push(r.best_ask_fp);
        mids.push(r.mid_price_fp);
        spreads.push(r.spread_fp);
        bid_depths.push(r.bid_depth_fp);
        ask_depths.push(r.ask_depth_fp);
        regime_dict.append_value(r.market_regime);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(instrument_ids)),
        Arc::new(Int64Array::from(timestamps)),
        Arc::new(UInt64Array::from(seq_ids)),
        Arc::new(Int64Array::from(best_bids)),
        Arc::new(Int64Array::from(best_asks)),
        Arc::new(Int64Array::from(mids)),
        Arc::new(Int64Array::from(spreads)),
        Arc::new(Int64Array::from(bid_depths)),
        Arc::new(Int64Array::from(ask_depths)),
        Arc::new(regime_dict.finish()),
    ];

    let raw_batch = RecordBatch::try_new(quotes_physical_schema(), columns)?;
    sort_batch_by_composite_key(&raw_batch)
}

/// Converts a slice of `FraudAlertRecord`s into a composite-sorted Arrow `RecordBatch`.
pub fn alerts_to_sorted_record_batch(records: &[FraudAlertRecord]) -> Result<RecordBatch> {
    let n = records.len();
    let mut instrument_ids = Vec::with_capacity(n);
    let mut timestamps = Vec::with_capacity(n);
    let mut seq_ids = Vec::with_capacity(n);
    let mut alert_ids = Vec::with_capacity(n);
    let mut accounts = Vec::with_capacity(n);
    let mut pattern_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n, 6, 128);
    let mut severities = Vec::with_capacity(n);
    let mut confidences = Vec::with_capacity(n);

    for r in records {
        instrument_ids.push(r.instrument_id);
        timestamps.push(r.timestamp_ns);
        seq_ids.push(r.seq_id);
        alert_ids.push(r.alert_id.as_str());
        accounts.push(r.account_id);
        pattern_dict.append_value(r.pattern_type);
        severities.push(r.severity_score);
        confidences.push(r.confidence_ppm);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(instrument_ids)),
        Arc::new(Int64Array::from(timestamps)),
        Arc::new(UInt64Array::from(seq_ids)),
        Arc::new(StringArray::from(alert_ids)),
        Arc::new(UInt64Array::from(accounts)),
        Arc::new(pattern_dict.finish()),
        Arc::new(UInt32Array::from(severities)),
        Arc::new(UInt32Array::from(confidences)),
    ];

    let raw_batch = RecordBatch::try_new(fraud_alerts_physical_schema(), columns)?;
    sort_batch_by_composite_key(&raw_batch)
}

/// Writes a pre-sorted `RecordBatch` atomically to a Zstd-compressed Parquet segment
/// inside `<storage_root>/<table>/symbol=<symbol>/hour_bucket=<hour_bucket>/`.
pub fn write_sorted_partition_batch(
    cfg: &LakehouseConfig,
    table_name: &str,
    partition: &PartitionKey,
    sorted_batch: &RecordBatch,
    bloom_cols: &[&str],
    file_prefix: &str,
) -> Result<WrittenSegmentMeta> {
    if sorted_batch.num_rows() == 0 {
        return Err(LakehouseError::InvalidInput(
            "Cannot write empty RecordBatch to Parquet".into(),
        ));
    }

    let dir = partition.directory_path(&cfg.storage_root, table_name);
    fs::create_dir_all(&dir)?;

    let ts_arr = sorted_batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| LakehouseError::InvalidInput("Missing Int64 timestamp_ns at col 1".into()))?;
    let seq_arr = sorted_batch
        .column(2)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| LakehouseError::InvalidInput("Missing UInt64 seq_id at col 2".into()))?;

    let min_ts_ns = ts_arr.value(0);
    let max_ts_ns = ts_arr.value(sorted_batch.num_rows() - 1);
    let mut min_seq_id = u64::MAX;
    let mut max_seq_id = 0_u64;
    for &s in seq_arr.values() {
        if s < min_seq_id {
            min_seq_id = s;
        }
        if s > max_seq_id {
            max_seq_id = s;
        }
    }

    let seg_id = SEGMENT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let final_name = format!("{file_prefix}_{seg_id:06}_{min_seq_id}_{max_seq_id}.parquet");
    let tmp_name = format!(".{final_name}.tmp");
    let tmp_path = dir.join(tmp_name);
    let final_path = dir.join(final_name);

    let props = build_parquet_writer_properties(cfg, bloom_cols)?;
    {
        let file = File::create(&tmp_path)?;
        let mut writer = ArrowWriter::try_new(file, sorted_batch.schema(), Some(props))?;
        writer.write(sorted_batch)?;
        writer.close()?;
    }

    fs::rename(&tmp_path, &final_path)?;
    let byte_size = fs::metadata(&final_path)?.len();

    Ok(WrittenSegmentMeta {
        table_name: table_name.to_string(),
        partition: partition.clone(),
        file_path: final_path,
        row_count: sorted_batch.num_rows(),
        byte_size,
        min_ts_ns,
        max_ts_ns,
        min_seq_id,
        max_seq_id,
    })
}

/// Partitions `TradeRecord`s hierarchically by `(symbol, hour_bucket)`, sorts each partition
/// by `(instrument_id, timestamp_ns, seq_id)`, and flushes L0 Zstd Parquet segments.
pub fn ingest_trades_partitioned(
    cfg: &LakehouseConfig,
    records: &[TradeRecord],
) -> Result<Vec<WrittenSegmentMeta>> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let mut grouped: BTreeMap<PartitionKey, Vec<TradeRecord>> = BTreeMap::new();
    for rec in records {
        let key = PartitionKey {
            symbol: rec.symbol.clone(),
            hour_bucket: hour_bucket_from_ts_ns(rec.timestamp_ns),
        };
        grouped.entry(key).or_default().push(rec.clone());
    }

    let bloom_cols = ["maker_account_id", "taker_account_id", "account_id"];
    let mut written = Vec::with_capacity(grouped.len());
    for (part_key, part_records) in grouped {
        let batch = trades_to_sorted_record_batch(&part_records)?;
        let meta =
            write_sorted_partition_batch(cfg, TABLE_TRADES, &part_key, &batch, &bloom_cols, "l0")?;
        written.push(meta);
    }
    Ok(written)
}

/// Partitions `QuoteRecord`s hierarchically by `(symbol, hour_bucket)` and flushes L0 Zstd Parquet segments.
pub fn ingest_quotes_partitioned(
    cfg: &LakehouseConfig,
    records: &[QuoteRecord],
) -> Result<Vec<WrittenSegmentMeta>> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let mut grouped: BTreeMap<PartitionKey, Vec<QuoteRecord>> = BTreeMap::new();
    for rec in records {
        let key = PartitionKey {
            symbol: rec.symbol.clone(),
            hour_bucket: hour_bucket_from_ts_ns(rec.timestamp_ns),
        };
        grouped.entry(key).or_default().push(rec.clone());
    }

    let mut written = Vec::with_capacity(grouped.len());
    for (part_key, part_records) in grouped {
        let batch = quotes_to_sorted_record_batch(&part_records)?;
        let meta = write_sorted_partition_batch(cfg, TABLE_QUOTES, &part_key, &batch, &[], "l0")?;
        written.push(meta);
    }
    Ok(written)
}

/// Partitions `FraudAlertRecord`s hierarchically by `(symbol, hour_bucket)` and flushes L0 Zstd Parquet segments.
pub fn ingest_alerts_partitioned(
    cfg: &LakehouseConfig,
    records: &[FraudAlertRecord],
) -> Result<Vec<WrittenSegmentMeta>> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let mut grouped: BTreeMap<PartitionKey, Vec<FraudAlertRecord>> = BTreeMap::new();
    for rec in records {
        let key = PartitionKey {
            symbol: rec.symbol.clone(),
            hour_bucket: hour_bucket_from_ts_ns(rec.timestamp_ns),
        };
        grouped.entry(key).or_default().push(rec.clone());
    }

    let bloom_cols = ["account_id"];
    let mut written = Vec::with_capacity(grouped.len());
    for (part_key, part_records) in grouped {
        let batch = alerts_to_sorted_record_batch(&part_records)?;
        let meta = write_sorted_partition_batch(
            cfg,
            TABLE_FRAUD_ALERTS,
            &part_key,
            &batch,
            &bloom_cols,
            "l0",
        )?;
        written.push(meta);
    }
    Ok(written)
}
