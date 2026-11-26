use crate::broker::{
    kafka_murmur2_partition, AggregatePublisher, BrokerError, ExactlyOnceCoordinator,
    PartitionRecord,
};
use crate::candle::Timeframe;
use crate::config::{topics, AggregatorConfig};
use crate::engine::FlushBatch;
use crate::storage::{AnalyticsStorage, BoxFuture};
use chrono::Utc;
use prost::Message;
use rskafka::client::partition::{Compression, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{watch, Mutex};
use tokio::time::{Duration, Instant};

type PartitionClientMap = FxHashMap<(String, i32), Arc<PartitionClient>>;

/// Pure-Rust Redpanda (`rskafka`) producer publishing `OhlcvCandle` and `MicrostructureMetrics`
/// to `hyperion.analytics.candles.v1` and `hyperion.analytics.microstructure.v1` with
/// deterministic `murmur2` key partitioning and W3C/Hyperion headers.
#[derive(Clone)]
pub struct RedpandaAggregatePublisher {
    client: Arc<Client>,
    partition_clients: Arc<Mutex<PartitionClientMap>>,
    partition_counts: Arc<Mutex<FxHashMap<String, i32>>>,
}

impl RedpandaAggregatePublisher {
    pub async fn connect(brokers: &[String]) -> Result<Self, BrokerError> {
        let client = ClientBuilder::new(brokers.to_vec())
            .build()
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        Ok(Self {
            client: Arc::new(client),
            partition_clients: Arc::new(Mutex::new(FxHashMap::default())),
            partition_counts: Arc::new(Mutex::new(FxHashMap::default())),
        })
    }

    pub fn from_client(client: Arc<Client>) -> Self {
        Self {
            client,
            partition_clients: Arc::new(Mutex::new(FxHashMap::default())),
            partition_counts: Arc::new(Mutex::new(FxHashMap::default())),
        }
    }

    async fn partition_count_for(&self, topic: &str) -> Result<i32, BrokerError> {
        {
            let guard = self.partition_counts.lock().await;
            if let Some(&cnt) = guard.get(topic) {
                return Ok(cnt);
            }
        }
        let topics_meta = self
            .client
            .list_topics()
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        let cnt = topics_meta
            .into_iter()
            .find(|t| t.name == topic)
            .map(|t| (t.partitions.len() as i32).max(1))
            .unwrap_or(8);
        self.partition_counts
            .lock()
            .await
            .insert(topic.to_string(), cnt);
        Ok(cnt)
    }

    async fn get_partition_client(
        &self,
        topic: &str,
        partition_id: i32,
    ) -> Result<Arc<PartitionClient>, BrokerError> {
        let key = (topic.to_string(), partition_id);
        {
            let guard = self.partition_clients.lock().await;
            if let Some(pc) = guard.get(&key) {
                return Ok(Arc::clone(pc));
            }
        }
        let pc = self
            .client
            .partition_client(topic, partition_id, UnknownTopicHandling::Retry)
            .await
            .map_err(|e| BrokerError::Transport(e.to_string()))?;
        let arc_pc = Arc::new(pc);
        self.partition_clients
            .lock()
            .await
            .insert(key, Arc::clone(&arc_pc));
        Ok(arc_pc)
    }
}

impl AggregatePublisher for RedpandaAggregatePublisher {
    fn publish_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<usize, BrokerError>> {
        Box::pin(async move {
            let mut total_published = 0_usize;

            if !batch.candles.is_empty() {
                let num_parts = self.partition_count_for(topics::ANALYTICS_CANDLES).await?;
                let mut grouped: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
                for c in &batch.candles {
                    let tf_code = Timeframe::from_proto_i32(c.interval)
                        .map(|t| t.code())
                        .unwrap_or("1s");
                    let key_str = format!("{}:{tf_code}", c.symbol);
                    let part = kafka_murmur2_partition(key_str.as_bytes(), num_parts);
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "x-engine-seq".to_string(),
                        c.last_engine_seq.to_string().into_bytes(),
                    );
                    headers.insert(
                        "x-schema-type".to_string(),
                        b"hyperion.v1.OhlcvCandle".to_vec(),
                    );
                    headers.insert(
                        "x-source-service".to_string(),
                        b"stream-aggregator".to_vec(),
                    );
                    headers.insert(
                        "traceparent".to_string(),
                        format!(
                            "00-{:032x}-{:016x}-01",
                            c.bucket_open_ts_ns, c.last_engine_seq
                        )
                        .into_bytes(),
                    );

                    grouped.entry(part).or_default().push(Record {
                        key: Some(key_str.into_bytes()),
                        value: Some(c.encode_to_vec()),
                        headers,
                        timestamp: Utc::now(),
                    });
                }

                for (part, records) in grouped {
                    let count = records.len();
                    let pc = self
                        .get_partition_client(topics::ANALYTICS_CANDLES, part)
                        .await?;
                    pc.produce(records, Compression::NoCompression)
                        .await
                        .map_err(|e| BrokerError::Transport(e.to_string()))?;
                    total_published += count;
                }
            }

            if !batch.microstructure.is_empty() {
                let num_parts = self
                    .partition_count_for(topics::ANALYTICS_MICROSTRUCTURE)
                    .await?;
                let mut grouped: BTreeMap<i32, Vec<Record>> = BTreeMap::new();
                for m in &batch.microstructure {
                    let part = kafka_murmur2_partition(m.symbol.as_bytes(), num_parts);
                    let mut headers = BTreeMap::new();
                    headers.insert(
                        "x-engine-seq".to_string(),
                        m.engine_seq.to_string().into_bytes(),
                    );
                    headers.insert(
                        "x-schema-type".to_string(),
                        b"hyperion.v1.MicrostructureMetrics".to_vec(),
                    );
                    headers.insert(
                        "x-source-service".to_string(),
                        b"stream-aggregator".to_vec(),
                    );
                    headers.insert(
                        "traceparent".to_string(),
                        format!("00-{:032x}-{:016x}-01", m.ts_ns, m.engine_seq).into_bytes(),
                    );

                    grouped.entry(part).or_default().push(Record {
                        key: Some(m.symbol.as_bytes().to_vec()),
                        value: Some(m.encode_to_vec()),
                        headers,
                        timestamp: Utc::now(),
                    });
                }

                for (part, records) in grouped {
                    let count = records.len();
                    let pc = self
                        .get_partition_client(topics::ANALYTICS_MICROSTRUCTURE, part)
                        .await?;
                    pc.produce(records, Compression::NoCompression)
                        .await
                        .map_err(|e| BrokerError::Transport(e.to_string()))?;
                    total_published += count;
                }
            }

            Ok(total_published)
        })
    }
}

/// Runs the manual-offset Redpanda ingestion loop bound transactionally to `ExactlyOnceCoordinator`.
pub async fn run_redpanda_consumer_loop<S: AnalyticsStorage>(
    cfg: AggregatorConfig,
    client: Arc<Client>,
    mut coordinator: ExactlyOnceCoordinator<S, RedpandaAggregatePublisher>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), BrokerError> {
    coordinator
        .recover_from_storage(&cfg.consumer_group)
        .await?;

    let source_topics = [
        topics::MARKET_EXECUTIONS,
        topics::MARKET_L3_DELTAS,
        topics::MARKET_L2_SNAPSHOTS,
    ];

    let topics_meta = client
        .list_topics()
        .await
        .map_err(|e| BrokerError::Transport(e.to_string()))?;

    let mut partition_clients: Vec<(String, i32, Arc<PartitionClient>)> = Vec::new();
    for topic in source_topics {
        if let Some(meta) = topics_meta.iter().find(|t| t.name == topic) {
            for &part in &meta.partitions {
                let pc = client
                    .partition_client(topic, part, UnknownTopicHandling::Retry)
                    .await
                    .map_err(|e| BrokerError::Transport(e.to_string()))?;
                partition_clients.push((topic.to_string(), part, Arc::new(pc)));
            }
        }
    }

    let flush_interval = Duration::from_millis(cfg.batch_flush_interval_ms.max(20));
    let mut last_flush = Instant::now();

    loop {
        if *shutdown_rx.borrow() {
            coordinator.flush_now(true).await?;
            return Ok(());
        }

        let mut fetched_any = false;
        for (topic, part, pc) in &partition_clients {
            let next_offset = {
                let eng = coordinator.engine();
                let guard = eng.read();
                (guard.partition_offset(topic, *part) + 1).max(0)
            };

            match pc.fetch_records(next_offset, 1..1_048_576, 15).await {
                Ok((records, _high_watermark)) if !records.is_empty() => {
                    fetched_any = true;
                    let mut batch_recs = Vec::with_capacity(records.len());
                    for rec_and_off in records {
                        let Some(payload) = rec_and_off.record.value else {
                            continue;
                        };
                        let headers = rec_and_off
                            .record
                            .headers
                            .into_iter()
                            .map(|(k, v)| (k, String::from_utf8_lossy(&v).into_owned()))
                            .collect();
                        batch_recs.push(PartitionRecord {
                            topic: topic.clone(),
                            partition_id: *part,
                            offset: rec_and_off.offset,
                            event_ts_ns: rec_and_off
                                .record
                                .timestamp
                                .timestamp_nanos_opt()
                                .unwrap_or(0)
                                .max(0) as u64,
                            key: rec_and_off.record.key.unwrap_or_default(),
                            payload,
                            headers,
                        });
                    }
                    coordinator.process_records(&batch_recs).await?;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(topic = %topic, partition = part, error = %e, "partition fetch idle/retry");
                }
            }
        }

        if last_flush.elapsed() >= flush_interval {
            coordinator.flush_now(false).await?;
            last_flush = Instant::now();
        }

        if !fetched_any {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                _ = shutdown_rx.changed() => {
                    coordinator.flush_now(true).await?;
                    return Ok(());
                }
            }
        }
    }
}
