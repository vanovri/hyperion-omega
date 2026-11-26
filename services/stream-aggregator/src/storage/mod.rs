pub mod redis_cache;
pub mod timescale;

use crate::candle::Timeframe;
use crate::dedup::PartitionCheckpoint;
use crate::engine::FlushBatch;
use crate::fixed_point::quantize_price_bucket;
use hyperion_proto::{
    ExecutionFill, GetVolumeProfileResponse, LiquidityHeatmapBucket, MicrostructureMetrics,
    OhlcvCandle, OrderSide, VolumeProfileBin, U128,
};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type LoadedCheckpoint = (Vec<PartitionCheckpoint>, Vec<u8>);

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(String),
    #[error("redis cache error: {0}")]
    Cache(String),
    #[error("simulated crash / injected storage fault: {0}")]
    SimulatedCrash(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushReceipt {
    pub candles_upserted: usize,
    pub executions_inserted: usize,
    pub l3_events_inserted: usize,
    pub microstructure_inserted: usize,
    pub heatmap_upserted: usize,
    pub checkpoints_committed: usize,
}

/// Unified async storage & query contract implemented by both `TimescaleFlusher` (PostgreSQL 16+)
/// and `InMemoryTransactionalStore` (deterministic ACID crash-recovery test harness).
pub trait AnalyticsStorage: Send + Sync {
    fn commit_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<FlushReceipt, StorageError>>;

    fn load_checkpoint<'a>(
        &'a self,
        consumer_group: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedCheckpoint>, StorageError>>;

    fn query_candles<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        from_ts_ns: u64,
        to_ts_ns: u64,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<OhlcvCandle>, StorageError>>;

    fn query_volume_profile<'a>(
        &'a self,
        symbol: &'a str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        bucket_size_fp: u64,
    ) -> BoxFuture<'a, Result<GetVolumeProfileResponse, StorageError>>;

    fn query_heatmap<'a>(
        &'a self,
        symbol: &'a str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        price_bucket_size_fp: u64,
        time_bucket_ns: u64,
    ) -> BoxFuture<'a, Result<Vec<LiquidityHeatmapBucket>, StorageError>>;

    fn query_latest_microstructure<'a>(
        &'a self,
        symbol: &'a str,
    ) -> BoxFuture<'a, Result<Option<MicrostructureMetrics>, StorageError>>;
}

#[derive(Debug, Clone, Default)]
struct CommittedDbState {
    /// `(consumer_group, topic, partition_id) -> PartitionCheckpoint`
    checkpoints: BTreeMap<(String, String, i32), PartitionCheckpoint>,
    /// `consumer_group -> HYPAGG01 binary checkpoint blob`
    state_blobs: BTreeMap<String, Vec<u8>>,
    /// `(symbol, interval_code, bucket_open_ts_ns) -> OhlcvCandle`
    candles: BTreeMap<(String, String, u64), OhlcvCandle>,
    /// `(symbol, exec_ts_ns, engine_seq, fill_id) -> ExecutionFill`
    executions: BTreeMap<(String, u64, u64, u64), ExecutionFill>,
    /// `symbol -> latest MicrostructureMetrics`
    latest_micro: BTreeMap<String, MicrostructureMetrics>,
    /// `(symbol, bucket_ts_ns, price_bucket_fp) -> LiquidityHeatmapBucket`
    heatmap: BTreeMap<(String, u64, u64), LiquidityHeatmapBucket>,
}

/// Deterministic ACID in-memory transactional store mirroring TimescaleDB's exact primary keys,
/// `ON CONFLICT` idempotency guards, and atomic offset+state commits.
#[derive(Debug, Clone, Default)]
pub struct InMemoryTransactionalStore {
    inner: Arc<RwLock<CommittedDbState>>,
    fail_next_commit: Arc<AtomicBool>,
}

impl InMemoryTransactionalStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms a one-shot simulated database crash on the next `commit_flush_batch` call.
    pub fn arm_fail_next_commit(&self) {
        self.fail_next_commit.store(true, Ordering::SeqCst);
    }

    pub fn committed_candle_count(&self, symbol: &str, tf: Timeframe) -> usize {
        let guard = self.inner.read();
        let code = tf.code().to_string();
        guard
            .candles
            .range((symbol.to_string(), code.clone(), 0)..=(symbol.to_string(), code, u64::MAX))
            .count()
    }

    pub fn committed_execution_count(&self, symbol: &str) -> usize {
        let guard = self.inner.read();
        guard
            .executions
            .iter()
            .filter(|((s, _, _, _), _)| s == symbol)
            .count()
    }

    pub fn total_committed_candle_volume_fp(&self, symbol: &str, tf: Timeframe) -> u128 {
        let guard = self.inner.read();
        let code = tf.code().to_string();
        guard
            .candles
            .range((symbol.to_string(), code.clone(), 0)..=(symbol.to_string(), code, u64::MAX))
            .map(|(_, c)| c.base_volume_fp.map(u128::from).unwrap_or(0))
            .sum()
    }
}

impl AnalyticsStorage for InMemoryTransactionalStore {
    fn commit_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<FlushReceipt, StorageError>> {
        Box::pin(async move {
            if self.fail_next_commit.swap(false, Ordering::SeqCst) {
                return Err(StorageError::SimulatedCrash(
                    "injected mid-transaction crash before COMMIT".to_string(),
                ));
            }

            let mut db = self.inner.write();
            let mut receipt = FlushReceipt::default();

            for f in &batch.executions {
                let key = (f.symbol.clone(), f.exec_ts_ns, f.engine_seq, f.fill_id);
                if db.executions.insert(key, f.clone()).is_none() {
                    receipt.executions_inserted += 1;
                }
            }
            receipt.l3_events_inserted = batch.l3_deltas.len();

            for c in &batch.candles {
                let Some(tf) = Timeframe::from_proto_i32(c.interval) else {
                    continue;
                };
                let key = (c.symbol.clone(), tf.code().to_string(), c.bucket_open_ts_ns);
                let should_write = match db.candles.get(&key) {
                    None => true,
                    Some(existing) => c.last_engine_seq >= existing.last_engine_seq,
                };
                if should_write {
                    db.candles.insert(key, c.clone());
                    receipt.candles_upserted += 1;
                }
            }

            for m in &batch.microstructure {
                let should_write = match db.latest_micro.get(&m.symbol) {
                    None => true,
                    Some(existing) => m.engine_seq >= existing.engine_seq,
                };
                if should_write {
                    db.latest_micro.insert(m.symbol.clone(), m.clone());
                    receipt.microstructure_inserted += 1;
                }
            }

            for (sym, h) in &batch.heatmap_buckets {
                let key = (sym.clone(), h.bucket_ts_ns, h.price_bucket_fp);
                db.heatmap.insert(key, *h);
                receipt.heatmap_upserted += 1;
            }

            for cp in &batch.checkpoints {
                let key = (cp.consumer_group.clone(), cp.topic.clone(), cp.partition_id);
                db.checkpoints.insert(key, cp.clone());
                receipt.checkpoints_committed += 1;
            }

            if !batch.engine_checkpoint_blob.is_empty() {
                db.state_blobs.insert(
                    batch.consumer_group.clone(),
                    batch.engine_checkpoint_blob.clone(),
                );
            }

            Ok(receipt)
        })
    }

    fn load_checkpoint<'a>(
        &'a self,
        consumer_group: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedCheckpoint>, StorageError>> {
        Box::pin(async move {
            let db = self.inner.read();
            let Some(blob) = db.state_blobs.get(consumer_group).cloned() else {
                return Ok(None);
            };
            let cps: Vec<PartitionCheckpoint> = db
                .checkpoints
                .iter()
                .filter(|((g, _, _), _)| g == consumer_group)
                .map(|(_, cp)| cp.clone())
                .collect();
            Ok(Some((cps, blob)))
        })
    }

    fn query_candles<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        from_ts_ns: u64,
        to_ts_ns: u64,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<OhlcvCandle>, StorageError>> {
        Box::pin(async move {
            let db = self.inner.read();
            let code = tf.code().to_string();
            let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
            let mut out: Vec<OhlcvCandle> = db
                .candles
                .range(
                    (symbol.to_string(), code.clone(), from_ts_ns)
                        ..=(symbol.to_string(), code, upper),
                )
                .map(|(_, c)| c.clone())
                .collect();
            if limit > 0 && out.len() > limit {
                out = out.split_off(out.len() - limit);
            }
            Ok(out)
        })
    }

    fn query_volume_profile<'a>(
        &'a self,
        symbol: &'a str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        bucket_size_fp: u64,
    ) -> BoxFuture<'a, Result<GetVolumeProfileResponse, StorageError>> {
        Box::pin(async move {
            let db = self.inner.read();
            let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
            let step = bucket_size_fp.max(10_000_000);
            let mut hist: BTreeMap<u64, (u128, u128)> = BTreeMap::new();

            for ((s, ts, _, _), f) in &db.executions {
                if s != symbol || *ts < from_ts_ns || *ts > upper {
                    continue;
                }
                let b_px = quantize_price_bucket(f.price_fp, step);
                let q = f.qty_fp as u128;
                let e = hist.entry(b_px).or_insert((0, 0));
                match OrderSide::try_from(f.aggressor_side).unwrap_or(OrderSide::Unspecified) {
                    OrderSide::Buy => e.0 += q,
                    OrderSide::Sell | OrderSide::Unspecified => e.1 += q,
                }
            }

            let mut poc_price_fp = 0_u64;
            let mut max_vol = 0_u128;
            let mut bins = Vec::with_capacity(hist.len());
            for (px, (bv, sv)) in &hist {
                let tv = bv + sv;
                if tv >= max_vol {
                    max_vol = tv;
                    poc_price_fp = *px;
                }
                bins.push(VolumeProfileBin {
                    price_bucket_fp: *px,
                    buy_volume_fp: Some(U128::from(*bv)),
                    sell_volume_fp: Some(U128::from(*sv)),
                    total_volume_fp: Some(U128::from(tv)),
                });
            }
            let val = bins.first().map(|b| b.price_bucket_fp).unwrap_or(0);
            let vah = bins.last().map(|b| b.price_bucket_fp).unwrap_or(0);

            Ok(GetVolumeProfileResponse {
                symbol: symbol.to_string(),
                poc_price_fp,
                value_area_high_fp: vah,
                value_area_low_fp: val,
                bins,
            })
        })
    }

    fn query_heatmap<'a>(
        &'a self,
        symbol: &'a str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        price_bucket_size_fp: u64,
        time_bucket_ns: u64,
    ) -> BoxFuture<'a, Result<Vec<LiquidityHeatmapBucket>, StorageError>> {
        Box::pin(async move {
            let db = self.inner.read();
            let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
            let px_step = price_bucket_size_fp.max(1);
            let t_step = time_bucket_ns.max(1);

            let mut agg: BTreeMap<(u64, u64), (u64, u64)> = BTreeMap::new();
            for ((s, ts, px), b) in &db.heatmap {
                if s != symbol || *ts < from_ts_ns || *ts > upper {
                    continue;
                }
                let tk = (*ts / t_step) * t_step;
                let pk = quantize_price_bucket(*px, px_step);
                let e = agg.entry((tk, pk)).or_insert((0, 0));
                e.0 = e.0.saturating_add(b.resting_bid_qty_fp);
                e.1 = e.1.saturating_add(b.resting_ask_qty_fp);
            }

            Ok(agg
                .into_iter()
                .map(|((t, p), (bid_q, ask_q))| LiquidityHeatmapBucket {
                    bucket_ts_ns: t,
                    price_bucket_fp: p,
                    resting_bid_qty_fp: bid_q,
                    resting_ask_qty_fp: ask_q,
                })
                .collect())
        })
    }

    fn query_latest_microstructure<'a>(
        &'a self,
        symbol: &'a str,
    ) -> BoxFuture<'a, Result<Option<MicrostructureMetrics>, StorageError>> {
        Box::pin(async move {
            let db = self.inner.read();
            Ok(db.latest_micro.get(symbol).cloned())
        })
    }
}
