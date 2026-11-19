pub mod grpc;
pub mod http;

use crate::candle::Timeframe;
use crate::engine::StreamAggregatorEngine;
use crate::storage::{AnalyticsStorage, StorageError};
use hyperion_proto::{
    GetVolumeProfileResponse, LiquidityHeatmapBucket, MicrostructureMetrics, OhlcvCandle,
};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared low-latency query state combining L1 in-memory hot rings with L2 TimescaleDB storage.
#[derive(Clone)]
pub struct QueryServiceState {
    pub engine: Arc<RwLock<StreamAggregatorEngine>>,
    pub storage: Arc<dyn AnalyticsStorage>,
    pub http_requests_total: Arc<AtomicU64>,
    pub grpc_requests_total: Arc<AtomicU64>,
}

impl QueryServiceState {
    pub fn new(
        engine: Arc<RwLock<StreamAggregatorEngine>>,
        storage: Arc<dyn AnalyticsStorage>,
    ) -> Self {
        Self {
            engine,
            storage,
            http_requests_total: Arc::new(AtomicU64::new(0)),
            grpc_requests_total: Arc::new(AtomicU64::new(0)),
        }
    }

    #[inline]
    pub fn now_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    /// Queries historical candles from `AnalyticsStorage` and overlays live in-memory candles
    /// so active un-flushed millisecond ticks are immediately visible to clients.
    pub async fn merged_candles(
        &self,
        symbol: &str,
        tf: Timeframe,
        from_ts_ns: u64,
        to_ts_ns: u64,
        limit: usize,
    ) -> Result<Vec<OhlcvCandle>, StorageError> {
        let eff_limit = if limit == 0 { 300 } else { limit.min(5_000) };
        let db_candles = self
            .storage
            .query_candles(symbol, tf, from_ts_ns, to_ts_ns, eff_limit)
            .await?;
        let hot_candles = {
            let guard = self.engine.read();
            guard.query_candles(symbol, tf, from_ts_ns, to_ts_ns, eff_limit)
        };

        let mut merged: BTreeMap<u64, OhlcvCandle> = BTreeMap::new();
        for c in db_candles {
            merged.insert(c.bucket_open_ts_ns, c);
        }
        for c in hot_candles {
            let replace = match merged.get(&c.bucket_open_ts_ns) {
                None => true,
                Some(prev) => c.last_engine_seq >= prev.last_engine_seq,
            };
            if replace {
                merged.insert(c.bucket_open_ts_ns, c);
            }
        }

        let mut vec: Vec<OhlcvCandle> = merged.into_values().collect();
        if vec.len() > eff_limit {
            vec = vec.split_off(vec.len() - eff_limit);
        }
        Ok(vec)
    }

    pub async fn latest_microstructure(
        &self,
        symbol: &str,
    ) -> Result<Option<MicrostructureMetrics>, StorageError> {
        let hot = {
            let guard = self.engine.read();
            guard.query_latest_microstructure(symbol)
        };
        if let Some(m) = hot {
            if m.engine_seq > 0 {
                return Ok(Some(m));
            }
        }
        self.storage.query_latest_microstructure(symbol).await
    }

    pub async fn volume_profile(
        &self,
        symbol: &str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        bucket_size_fp: u64,
    ) -> Result<GetVolumeProfileResponse, StorageError> {
        let hot = {
            let guard = self.engine.read();
            guard.query_volume_profile(symbol, from_ts_ns, to_ts_ns, bucket_size_fp)
        };
        if !hot.bins.is_empty() {
            return Ok(hot);
        }
        self.storage
            .query_volume_profile(symbol, from_ts_ns, to_ts_ns, bucket_size_fp)
            .await
    }

    pub async fn heatmap(
        &self,
        symbol: &str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        price_bucket_size_fp: u64,
        time_bucket_ns: u64,
    ) -> Result<Vec<LiquidityHeatmapBucket>, StorageError> {
        let db_cells = self
            .storage
            .query_heatmap(symbol, from_ts_ns, to_ts_ns, price_bucket_size_fp, time_bucket_ns)
            .await?;
        let hot_cells = {
            let guard = self.engine.read();
            guard.query_heatmap(symbol, from_ts_ns, to_ts_ns, price_bucket_size_fp, time_bucket_ns)
        };

        let mut merged: BTreeMap<(u64, u64), LiquidityHeatmapBucket> = BTreeMap::new();
        for b in db_cells {
            merged.insert((b.bucket_ts_ns, b.price_bucket_fp), b);
        }
        for b in hot_cells {
            merged.insert((b.bucket_ts_ns, b.price_bucket_fp), b);
        }
        Ok(merged.into_values().collect())
    }

    pub fn render_prometheus_metrics(&self) -> String {
        let (accepted, dup, late, cap) = {
            let g = self.engine.read();
            (
                g.total_accepted_events(),
                g.total_duplicate_events(),
                g.total_late_events(),
                g.steady_state_capacity_units(),
            )
        };
        let http_reqs = self.http_requests_total.load(Ordering::Relaxed);
        let grpc_reqs = self.grpc_requests_total.load(Ordering::Relaxed);

        format!(
            "# HELP hyperion_aggregator_accepted_events_total Total market events accepted by stream-aggregator\n\
             # TYPE hyperion_aggregator_accepted_events_total counter\n\
             hyperion_aggregator_accepted_events_total {accepted}\n\
             # HELP hyperion_aggregator_duplicate_events_total Total duplicate sequence/offset events rejected\n\
             # TYPE hyperion_aggregator_duplicate_events_total counter\n\
             hyperion_aggregator_duplicate_events_total {dup}\n\
             # HELP hyperion_aggregator_late_events_total Total late-arriving events dropped past deterministic watermark\n\
             # TYPE hyperion_aggregator_late_events_total counter\n\
             hyperion_aggregator_late_events_total {late}\n\
             # HELP hyperion_aggregator_ring_capacity_units Pre-allocated steady-state ring capacity units\n\
             # TYPE hyperion_aggregator_ring_capacity_units gauge\n\
             hyperion_aggregator_ring_capacity_units {cap}\n\
             # HELP hyperion_aggregator_http_requests_total Total HTTP analytics API requests served\n\
             # TYPE hyperion_aggregator_http_requests_total counter\n\
             hyperion_aggregator_http_requests_total {http_reqs}\n\
             # HELP hyperion_aggregator_grpc_requests_total Total gRPC StreamAnalyticsService RPCs served\n\
             # TYPE hyperion_aggregator_grpc_requests_total counter\n\
             hyperion_aggregator_grpc_requests_total {grpc_reqs}\n"
        )
    }
}
