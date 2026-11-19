pub mod redpanda;

use crate::candle::Timeframe;
use crate::config::{topics, AggregatorConfig};
use crate::engine::{FlushBatch, IngestEvent, StreamAggregatorEngine};
use crate::storage::redis_cache::HotStateCache;
use crate::storage::{AnalyticsStorage, BoxFuture};
use hyperion_proto::{ExecutionFill, L2BookSnapshot, L3BookDelta, MicrostructureMetrics, OhlcvCandle};
use parking_lot::{Mutex, RwLock};
use prost::Message;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("broker transport error: {0}")]
    Transport(String),
    #[error("protobuf decode error: {0}")]
    Decode(String),
    #[error("storage commit failed: {0}")]
    Storage(String),
}

/// Canonical Kafka `murmur2` partitioner (`seed = 0x9747b28c`, positive 31-bit mask)
/// matching `.agents/rules/00-hyperion-contract.md` §4.1 (`murmur2_random` deterministic key hashing).
pub fn kafka_murmur2_partition(key: &[u8], num_partitions: i32) -> i32 {
    if num_partitions <= 1 {
        return 0;
    }
    const SEED: u32 = 0x9747_b28c;
    const M: u32 = 0x5bd1_e995;
    const R: u32 = 24;

    let length = key.len();
    let mut h: u32 = SEED ^ (length as u32);
    let length4 = length / 4;

    for i in 0..length4 {
        let i4 = i * 4;
        let mut k: u32 = (key[i4] as u32)
            | ((key[i4 + 1] as u32) << 8)
            | ((key[i4 + 2] as u32) << 16)
            | ((key[i4 + 3] as u32) << 24);
        k = k.wrapping_mul(M);
        k ^= k >> R;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M);
        h ^= k;
    }

    let rem = length % 4;
    if rem >= 3 {
        h ^= (key[(length & !3) + 2] as u32) << 16;
    }
    if rem >= 2 {
        h ^= (key[(length & !3) + 1] as u32) << 8;
    }
    if rem >= 1 {
        h ^= key[length & !3] as u32;
        h = h.wrapping_mul(M);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^= h >> 15;

    let positive = (h & 0x7fff_ffff) as i32;
    positive % num_partitions
}

/// Real-time aggregate event publisher back to Redpanda (`hyperion.analytics.*`).
pub trait AggregatePublisher: Send + Sync {
    fn publish_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<usize, BrokerError>>;
}

/// Raw broker record stored in a partition log.
#[derive(Debug, Clone)]
pub struct PartitionRecord {
    pub topic: String,
    pub partition_id: i32,
    pub offset: i64,
    pub event_ts_ns: u64,
    pub key: Vec<u8>,
    pub payload: Vec<u8>,
    pub headers: BTreeMap<String, String>,
}

type PartitionLogMap = BTreeMap<(String, i32), Vec<PartitionRecord>>;

/// Deterministic in-memory partitioned Redpanda/Kafka log cluster for crash-recovery,
/// manual offset seek, and duplicate redelivery stress verification.
#[derive(Debug, Clone, Default)]
pub struct InMemoryBrokerCluster {
    /// `(topic, partition_id) -> ordered log of PartitionRecord`
    logs: Arc<RwLock<PartitionLogMap>>,
    published_candles: Arc<Mutex<Vec<OhlcvCandle>>>,
    published_micro: Arc<Mutex<Vec<MicrostructureMetrics>>>,
}

impl InMemoryBrokerCluster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn produce_ingest_event(&self, num_partitions: i32, ev: &IngestEvent) -> (String, i32, i64) {
        let (topic, key_str, payload, schema_type) = match ev {
            IngestEvent::Execution(f) => (
                topics::MARKET_EXECUTIONS.to_string(),
                f.symbol.clone(),
                f.encode_to_vec(),
                "hyperion.v1.ExecutionFill",
            ),
            IngestEvent::L3Delta(d) => (
                topics::MARKET_L3_DELTAS.to_string(),
                d.symbol.clone(),
                d.encode_to_vec(),
                "hyperion.v1.L3BookDelta",
            ),
            IngestEvent::L2Snapshot(s) => (
                topics::MARKET_L2_SNAPSHOTS.to_string(),
                s.symbol.clone(),
                s.encode_to_vec(),
                "hyperion.v1.L2BookSnapshot",
            ),
        };

        let part = kafka_murmur2_partition(key_str.as_bytes(), num_partitions.max(1));
        let mut logs = self.logs.write();
        let vec = logs.entry((topic.clone(), part)).or_default();
        let offset = vec.len() as i64;
        let mut headers = BTreeMap::new();
        headers.insert("x-engine-seq".to_string(), ev.engine_seq().to_string());
        headers.insert("x-schema-type".to_string(), schema_type.to_string());
        headers.insert("x-source-service".to_string(), "ledger-rs".to_string());

        vec.push(PartitionRecord {
            topic: topic.clone(),
            partition_id: part,
            offset,
            event_ts_ns: ev.ts_ns(),
            key: key_str.into_bytes(),
            payload,
            headers,
        });
        (topic, part, offset)
    }

    pub fn fetch_from(
        &self,
        topic: &str,
        partition_id: i32,
        start_offset: i64,
        max_records: usize,
    ) -> Vec<PartitionRecord> {
        let logs = self.logs.read();
        let Some(vec) = logs.get(&(topic.to_string(), partition_id)) else {
            return Vec::new();
        };
        let idx = start_offset.max(0) as usize;
        if idx >= vec.len() {
            return Vec::new();
        }
        let end = (idx + max_records).min(vec.len());
        vec[idx..end].to_vec()
    }

    pub fn partitions_for(&self, topic: &str) -> Vec<i32> {
        let logs = self.logs.read();
        logs.keys()
            .filter(|(t, _)| t == topic)
            .map(|(_, p)| *p)
            .collect()
    }

    pub fn published_candles(&self) -> Vec<OhlcvCandle> {
        self.published_candles.lock().clone()
    }

    pub fn published_microstructure(&self) -> Vec<MicrostructureMetrics> {
        self.published_micro.lock().clone()
    }
}

impl AggregatePublisher for InMemoryBrokerCluster {
    fn publish_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<usize, BrokerError>> {
        Box::pin(async move {
            let mut count = 0_usize;
            if !batch.candles.is_empty() {
                let mut c_guard = self.published_candles.lock();
                for c in &batch.candles {
                    let tf_code = Timeframe::from_proto_i32(c.interval)
                        .map(|t| t.code())
                        .unwrap_or("1s");
                    let key = format!("{}:{tf_code}", c.symbol);
                    let part = kafka_murmur2_partition(key.as_bytes(), 8);
                    let mut logs = self.logs.write();
                    let log = logs
                        .entry((topics::ANALYTICS_CANDLES.to_string(), part))
                        .or_default();
                    let offset = log.len() as i64;
                    log.push(PartitionRecord {
                        topic: topics::ANALYTICS_CANDLES.to_string(),
                        partition_id: part,
                        offset,
                        event_ts_ns: c.bucket_open_ts_ns,
                        key: key.into_bytes(),
                        payload: c.encode_to_vec(),
                        headers: BTreeMap::new(),
                    });
                    c_guard.push(c.clone());
                    count += 1;
                }
            }
            if !batch.microstructure.is_empty() {
                let mut m_guard = self.published_micro.lock();
                for m in &batch.microstructure {
                    let part = kafka_murmur2_partition(m.symbol.as_bytes(), 8);
                    let mut logs = self.logs.write();
                    let log = logs
                        .entry((topics::ANALYTICS_MICROSTRUCTURE.to_string(), part))
                        .or_default();
                    let offset = log.len() as i64;
                    log.push(PartitionRecord {
                        topic: topics::ANALYTICS_MICROSTRUCTURE.to_string(),
                        partition_id: part,
                        offset,
                        event_ts_ns: m.ts_ns,
                        key: m.symbol.as_bytes().to_vec(),
                        payload: m.encode_to_vec(),
                        headers: BTreeMap::new(),
                    });
                    m_guard.push(m.clone());
                    count += 1;
                }
            }
            Ok(count)
        })
    }
}

/// Decodes a raw Protobuf record payload from a canonical market topic.
pub fn decode_market_record(topic: &str, payload: &[u8]) -> Result<IngestEvent, BrokerError> {
    match topic {
        topics::MARKET_EXECUTIONS => {
            let f = ExecutionFill::decode(payload)
                .map_err(|e| BrokerError::Decode(e.to_string()))?;
            Ok(IngestEvent::Execution(f))
        }
        topics::MARKET_L3_DELTAS => {
            let d =
                L3BookDelta::decode(payload).map_err(|e| BrokerError::Decode(e.to_string()))?;
            Ok(IngestEvent::L3Delta(d))
        }
        topics::MARKET_L2_SNAPSHOTS => {
            let s = L2BookSnapshot::decode(payload)
                .map_err(|e| BrokerError::Decode(e.to_string()))?;
            Ok(IngestEvent::L2Snapshot(s))
        }
        other => Err(BrokerError::Decode(format!("unsupported topic: {other}"))),
    }
}

/// Transactional Exactly-Once Consumer Coordinator tying manual partition offset progression
/// to ACID database batch flushes and deterministic checkpoint recovery.
pub struct ExactlyOnceCoordinator<S: AnalyticsStorage, P: AggregatePublisher> {
    engine: Arc<RwLock<StreamAggregatorEngine>>,
    storage: Arc<S>,
    publisher: Arc<P>,
    hot_cache: Option<Arc<dyn HotStateCache>>,
    batch_flush_max_events: usize,
    unflushed_events: usize,
}

impl<S: AnalyticsStorage, P: AggregatePublisher> ExactlyOnceCoordinator<S, P> {
    pub fn new(
        cfg: AggregatorConfig,
        storage: Arc<S>,
        publisher: Arc<P>,
        hot_cache: Option<Arc<dyn HotStateCache>>,
    ) -> Self {
        let max_ev = cfg.batch_flush_max_events.max(1);
        Self {
            engine: Arc::new(RwLock::new(StreamAggregatorEngine::new(cfg))),
            storage,
            publisher,
            hot_cache,
            batch_flush_max_events: max_ev,
            unflushed_events: 0,
        }
    }

    pub fn from_shared_engine(
        engine: Arc<RwLock<StreamAggregatorEngine>>,
        batch_flush_max_events: usize,
        storage: Arc<S>,
        publisher: Arc<P>,
        hot_cache: Option<Arc<dyn HotStateCache>>,
    ) -> Self {
        Self {
            engine,
            storage,
            publisher,
            hot_cache,
            batch_flush_max_events: batch_flush_max_events.max(1),
            unflushed_events: 0,
        }
    }

    pub fn engine(&self) -> Arc<RwLock<StreamAggregatorEngine>> {
        Arc::clone(&self.engine)
    }

    /// Restores committed partition offsets AND exact in-memory accumulator state from `AnalyticsStorage`.
    pub async fn recover_from_storage(&mut self, consumer_group: &str) -> Result<bool, BrokerError> {
        let loaded = self
            .storage
            .load_checkpoint(consumer_group)
            .await
            .map_err(|e| BrokerError::Storage(e.to_string()))?;
        let Some((_cps, blob)) = loaded else {
            return Ok(false);
        };
        let mut guard = self.engine.write();
        guard
            .restore_from_checkpoint(&blob)
            .map_err(|e| BrokerError::Storage(e.to_string()))?;
        self.unflushed_events = 0;
        Ok(true)
    }

    /// Processes a batch of `PartitionRecord`s and triggers a transactional DB + Redis + Redpanda
    /// flush once `unflushed_events >= batch_flush_max_events`.
    pub async fn process_records(&mut self, records: &[PartitionRecord]) -> Result<usize, BrokerError> {
        let mut accepted = 0_usize;
        for rec in records {
            let ev = decode_market_record(&rec.topic, &rec.payload)?;
            let out = {
                let mut guard = self.engine.write();
                guard.ingest_broker_record(&rec.topic, rec.partition_id, rec.offset, &ev)
            };
            if matches!(out, crate::engine::IngestOutcome::Accepted) {
                accepted += 1;
            }
            self.unflushed_events += 1;
            if self.unflushed_events >= self.batch_flush_max_events {
                self.flush_now(false).await?;
            }
        }
        Ok(accepted)
    }

    /// Atomically flushes dirty state + consumer offsets to `AnalyticsStorage`, then updates
    /// `HotStateCache` (Redis) and publishes aggregates to `AggregatePublisher` (Redpanda).
    pub async fn flush_now(&mut self, seal_watermarks: bool) -> Result<(), BrokerError> {
        let batch = {
            let mut guard = self.engine.write();
            if seal_watermarks {
                guard.flush_all_watermarks();
            }
            guard
                .prepare_flush_batch()
                .map_err(|e| BrokerError::Storage(e.to_string()))?
        };

        if batch.is_empty() && batch.checkpoints.is_empty() {
            self.unflushed_events = 0;
            return Ok(());
        }

        self.storage
            .commit_flush_batch(&batch)
            .await
            .map_err(|e| BrokerError::Storage(e.to_string()))?;

        if let Some(cache) = &self.hot_cache {
            if let Err(e) = cache.sync_flush_batch(&batch).await {
                tracing::warn!(error = %e, "Redis hot-cache sync warning (DB checkpoint remains authoritative)");
            }
        }

        self.publisher.publish_flush_batch(&batch).await?;
        self.unflushed_events = 0;
        Ok(())
    }

    /// Consumes up to `max_events_limit` across all partitions in an `InMemoryBrokerCluster`
    /// using Kafka-Streams-style event-time aligned polling (always advancing the unread
    /// `(topic, partition)` with the minimum head `event_ts_ns`) starting strictly at
    /// `(last_committed_offset + 1)`.
    pub async fn drain_in_memory_broker(
        &mut self,
        broker: &InMemoryBrokerCluster,
        topics_to_consume: &[&str],
        max_events_to_read: usize,
        seal_on_complete: bool,
    ) -> Result<usize, BrokerError> {
        let mut total_read = 0_usize;
        while total_read < max_events_to_read {
            // Find the `(topic, partition, next_offset)` with the smallest next event timestamp
            let mut best: Option<(&str, i32, i64, u64)> = None;
            for &topic in topics_to_consume {
                for part in broker.partitions_for(topic) {
                    let next_offset = {
                        let guard = self.engine.read();
                        guard.partition_offset(topic, part) + 1
                    };
                    if let Some(head) = broker.fetch_from(topic, part, next_offset, 1).first() {
                        let ts = head.event_ts_ns;
                        let is_better = match best {
                            None => true,
                            Some((_, _, _, best_ts)) => ts < best_ts,
                        };
                        if is_better {
                            best = Some((topic, part, next_offset, ts));
                        }
                    }
                }
            }

            let Some((topic, part, next_offset, _)) = best else {
                break;
            };
            let remaining = max_events_to_read - total_read;
            let chunk = broker.fetch_from(topic, part, next_offset, remaining.min(32));
            if chunk.is_empty() {
                break;
            }
            total_read += chunk.len();
            self.process_records(&chunk).await?;
        }
        if seal_on_complete {
            self.flush_now(true).await?;
        }
        Ok(total_read)
    }
}
