use crate::candle::Timeframe;
use crate::engine::FlushBatch;
use crate::storage::{BoxFuture, StorageError};
use hyperion_proto::{GetLiquidityHeatmapResponse, MicrostructureMetrics, OhlcvCandle};
use prost::Message;
use redis::aio::ConnectionManager;

/// Hot-state cache contract for sub-millisecond active candle, microstructure, and heatmap retrieval.
pub trait HotStateCache: Send + Sync {
    fn sync_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<usize, StorageError>>;
}

/// Pipelined Redis 7 hot cache storing Protobuf-encoded active candles, microstructure metrics,
/// and 60fps latest liquidity depth matrices.
#[derive(Clone)]
pub struct RedisHotCache {
    conn: ConnectionManager,
    ttl_seconds: u64,
}

impl RedisHotCache {
    pub async fn connect(redis_url: &str) -> Result<Self, StorageError> {
        let client =
            redis::Client::open(redis_url).map_err(|e| StorageError::Cache(e.to_string()))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| StorageError::Cache(e.to_string()))?;
        Ok(Self {
            conn,
            ttl_seconds: 3_600,
        })
    }

    pub async fn get_latest_microstructure(
        &self,
        symbol: &str,
    ) -> Result<Option<MicrostructureMetrics>, StorageError> {
        let mut conn = self.conn.clone();
        let key = format!("hyperion:analytics:micro:{symbol}");
        let raw: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| StorageError::Cache(e.to_string()))?;
        match raw {
            Some(bytes) => MicrostructureMetrics::decode(bytes.as_slice())
                .map(Some)
                .map_err(|e| StorageError::Cache(e.to_string())),
            None => Ok(None),
        }
    }

    pub async fn get_running_candle(
        &self,
        symbol: &str,
        tf: Timeframe,
    ) -> Result<Option<OhlcvCandle>, StorageError> {
        let mut conn = self.conn.clone();
        let key = format!("hyperion:analytics:candle:{symbol}:{}", tf.code());
        let raw: Option<Vec<u8>> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .map_err(|e| StorageError::Cache(e.to_string()))?;
        match raw {
            Some(bytes) => OhlcvCandle::decode(bytes.as_slice())
                .map(Some)
                .map_err(|e| StorageError::Cache(e.to_string())),
            None => Ok(None),
        }
    }
}

impl HotStateCache for RedisHotCache {
    fn sync_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<usize, StorageError>> {
        Box::pin(async move {
            let mut pipe = redis::pipe();
            let mut ops = 0_usize;

            for c in &batch.candles {
                if let Some(tf) = Timeframe::from_proto_i32(c.interval) {
                    let key = format!("hyperion:analytics:candle:{}:{}", c.symbol, tf.code());
                    pipe.cmd("SETEX")
                        .arg(key)
                        .arg(self.ttl_seconds)
                        .arg(c.encode_to_vec())
                        .ignore();
                    ops += 1;
                }
            }

            for m in &batch.microstructure {
                let key = format!("hyperion:analytics:micro:{}", m.symbol);
                pipe.cmd("SETEX")
                    .arg(key)
                    .arg(self.ttl_seconds)
                    .arg(m.encode_to_vec())
                    .ignore();
                ops += 1;
            }

            if !batch.heatmap_buckets.is_empty() {
                let mut grouped: std::collections::BTreeMap<&str, Vec<_>> =
                    std::collections::BTreeMap::new();
                for (sym, cell) in &batch.heatmap_buckets {
                    grouped.entry(sym.as_str()).or_default().push(*cell);
                }
                for (sym, buckets) in grouped {
                    let msg = GetLiquidityHeatmapResponse {
                        symbol: sym.to_string(),
                        buckets,
                    };
                    let key = format!("hyperion:analytics:heatmap:{sym}");
                    pipe.cmd("SETEX")
                        .arg(key)
                        .arg(self.ttl_seconds)
                        .arg(msg.encode_to_vec())
                        .ignore();
                    ops += 1;
                }
            }

            if ops > 0 {
                let mut conn = self.conn.clone();
                pipe.query_async::<()>(&mut conn)
                    .await
                    .map_err(|e| StorageError::Cache(e.to_string()))?;
            }
            Ok(ops)
        })
    }
}
