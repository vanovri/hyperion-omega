use crate::engine::OutboundRingEvent;
use crate::proto_bridge::{delta_to_proto, fill_to_proto, journal_to_proto};
use crate::ring::SpscConsumer;
use hyperion_proto::{market_event_envelope, MarketEventEnvelope};
use prost::Message;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

pub const TOPIC_L3_DELTAS: &str = "hyperion.market.l3-deltas.v1";
pub const TOPIC_EXECUTIONS: &str = "hyperion.market.executions.v1";
pub const TOPIC_JOURNALS: &str = "hyperion.ledger.journals.v1";
pub const DEFAULT_TOPIC_PARTITIONS: i32 = 8;

/// Standard Apache Kafka `murmur2` 32-bit hash (`org.apache.kafka.common.utils.Utils.murmur2`)
/// mandated by `00-hyperion-contract.md` Section 4.1 for deterministic per-symbol & per-account partitioning.
pub fn kafka_murmur2(data: &[u8]) -> u32 {
    let length = data.len() as u32;
    let seed: u32 = 0x9747_b28c;
    let m: u32 = 0x5bd1_e995;
    let r: u32 = 24;

    let mut h: u32 = seed ^ length;
    let length4 = data.len() / 4;

    for i in 0..length4 {
        let i4 = i * 4;
        let mut k: u32 = (data[i4] as u32)
            | ((data[i4 + 1] as u32) << 8)
            | ((data[i4 + 2] as u32) << 16)
            | ((data[i4 + 3] as u32) << 24);
        k = k.wrapping_mul(m);
        k ^= k >> r;
        k = k.wrapping_mul(m);
        h = h.wrapping_mul(m);
        h ^= k;
    }

    let rem = data.len() & 3;
    let tail_base = length4 * 4;
    if rem >= 3 {
        h ^= (data[tail_base + 2] as u32) << 16;
    }
    if rem >= 2 {
        h ^= (data[tail_base + 1] as u32) << 8;
    }
    if rem >= 1 {
        h ^= data[tail_base] as u32;
        h = h.wrapping_mul(m);
    }

    h ^= h >> 13;
    h = h.wrapping_mul(m);
    h ^= h >> 15;
    h
}

/// Deterministic Kafka positive partition selector `(murmur2(key) & 0x7fffffff) % num_partitions`.
#[inline]
pub fn deterministic_partition(key: &[u8], num_partitions: i32) -> i32 {
    debug_assert!(num_partitions > 0);
    ((kafka_murmur2(key) & 0x7fff_ffff) as i32) % num_partitions
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedKafkaRecord {
    pub topic: &'static str,
    pub partition: i32,
    pub key: Vec<u8>,
    pub payload: Vec<u8>,
    pub engine_seq: u64,
    pub ts_ms: i64,
    pub schema_type: &'static str,
    pub traceparent: String,
}

#[inline]
fn write_signed_varint(out: &mut Vec<u8>, val: i64) {
    let mut u = ((val << 1) ^ (val >> 63)) as u64;
    while u >= 0x80 {
        out.push(((u as u8) & 0x7F) | 0x80);
        u >>= 7;
    }
    out.push(u as u8);
}

/// Encodes a valid Kafka RecordBatch v2 (`magic = 2`) with hardware Castagnoli CRC-32C and W3C headers.
pub fn encode_kafka_record_batch_v2(record: &PublishedKafkaRecord) -> Vec<u8> {
    let seq_str = record.engine_seq.to_string();
    let headers: [(&[u8], &[u8]); 4] = [
        (b"traceparent", record.traceparent.as_bytes()),
        (b"x-engine-seq", seq_str.as_bytes()),
        (b"x-schema-type", record.schema_type.as_bytes()),
        (b"x-source-service", b"ledger-rs"),
    ];

    // Encode inner Kafka Record v2 body
    let mut rec_body = Vec::with_capacity(record.payload.len() + 128);
    rec_body.push(0u8); // attributes = 0
    write_signed_varint(&mut rec_body, 0); // timestampDelta = 0
    write_signed_varint(&mut rec_body, 0); // offsetDelta = 0
    write_signed_varint(&mut rec_body, record.key.len() as i64);
    rec_body.extend_from_slice(&record.key);
    write_signed_varint(&mut rec_body, record.payload.len() as i64);
    rec_body.extend_from_slice(&record.payload);
    write_signed_varint(&mut rec_body, headers.len() as i64);
    for (hk, hv) in &headers {
        write_signed_varint(&mut rec_body, hk.len() as i64);
        rec_body.extend_from_slice(hk);
        write_signed_varint(&mut rec_body, hv.len() as i64);
        rec_body.extend_from_slice(hv);
    }

    let mut rec_framed = Vec::with_capacity(rec_body.len() + 8);
    write_signed_varint(&mut rec_framed, rec_body.len() as i64);
    rec_framed.extend_from_slice(&rec_body);

    // RecordBatch v2 CRC-covered section: [attributes(2) .. records]
    let mut crc_region = Vec::with_capacity(40 + rec_framed.len());
    crc_region.extend_from_slice(&0i16.to_be_bytes()); // attributes (none, uncompressed)
    crc_region.extend_from_slice(&0i32.to_be_bytes()); // lastOffsetDelta = 0
    crc_region.extend_from_slice(&record.ts_ms.to_be_bytes()); // firstTimestamp
    crc_region.extend_from_slice(&record.ts_ms.to_be_bytes()); // maxTimestamp
    crc_region.extend_from_slice(&(-1i64).to_be_bytes()); // producerId = -1
    crc_region.extend_from_slice(&(-1i16).to_be_bytes()); // producerEpoch = -1
    crc_region.extend_from_slice(&(-1i32).to_be_bytes()); // baseSequence = -1
    crc_region.extend_from_slice(&1i32.to_be_bytes()); // records count = 1
    crc_region.extend_from_slice(&rec_framed);

    let batch_crc = crc32c::crc32c(&crc_region);
    // batchLength = partitionLeaderEpoch(4) + magic(1) + crc(4) + crc_region.len()
    let batch_length = (4 + 1 + 4 + crc_region.len()) as i32;

    let mut batch = Vec::with_capacity(12 + batch_length as usize);
    batch.extend_from_slice(&0i64.to_be_bytes()); // baseOffset = 0
    batch.extend_from_slice(&batch_length.to_be_bytes());
    batch.extend_from_slice(&0i32.to_be_bytes()); // partitionLeaderEpoch = 0
    batch.push(2u8); // magic = 2 (RecordBatch v2)
    batch.extend_from_slice(&batch_crc.to_be_bytes());
    batch.extend_from_slice(&crc_region);
    batch
}

/// Encodes a pure-Rust Kafka Produce Request (`ApiKey = 0, ApiVersion = 3`) over TCP wire protocol.
pub fn encode_kafka_produce_request_v3(
    correlation_id: i32,
    client_id: &str,
    record: &PublishedKafkaRecord,
) -> Vec<u8> {
    let record_batch = encode_kafka_record_batch_v2(record);
    let client_bytes = client_id.as_bytes();
    let topic_bytes = record.topic.as_bytes();

    let mut body = Vec::with_capacity(64 + record_batch.len());
    // RequestHeader v1: api_key(0), api_version(3), correlation_id, client_id
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&3i16.to_be_bytes());
    body.extend_from_slice(&correlation_id.to_be_bytes());
    body.extend_from_slice(&(client_bytes.len() as i16).to_be_bytes());
    body.extend_from_slice(client_bytes);

    // ProduceRequest v3 body: transactional_id(null = -1), acks(1 = leader), timeout_ms(1500), [topic_data]
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&1i16.to_be_bytes()); // acks = 1
    body.extend_from_slice(&1500i32.to_be_bytes());
    body.extend_from_slice(&1i32.to_be_bytes()); // 1 topic
    body.extend_from_slice(&(topic_bytes.len() as i16).to_be_bytes());
    body.extend_from_slice(topic_bytes);
    body.extend_from_slice(&1i32.to_be_bytes()); // 1 partition
    body.extend_from_slice(&record.partition.to_be_bytes());
    body.extend_from_slice(&(record_batch.len() as i32).to_be_bytes());
    body.extend_from_slice(&record_batch);

    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as i32).to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Captures outbound Redpanda records in memory for deterministic inspection in unit/integration tests.
#[derive(Default, Clone)]
pub struct CapturedRedpandaSink {
    records: Arc<Mutex<Vec<PublishedKafkaRecord>>>,
}

impl CapturedRedpandaSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, rec: PublishedKafkaRecord) {
        if let Ok(mut guard) = self.records.lock() {
            guard.push(rec);
        }
    }

    pub fn snapshot(&self) -> Vec<PublishedKafkaRecord> {
        self.records.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.records.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct PublisherMetrics {
    pub published_l3_total: AtomicU64,
    pub published_fills_total: AtomicU64,
    pub published_journals_total: AtomicU64,
    pub redpanda_tcp_sent_total: AtomicU64,
    pub redpanda_tcp_errors_total: AtomicU64,
}

impl Default for PublisherMetrics {
    fn default() -> Self {
        Self {
            published_l3_total: AtomicU64::new(0),
            published_fills_total: AtomicU64::new(0),
            published_journals_total: AtomicU64::new(0),
            redpanda_tcp_sent_total: AtomicU64::new(0),
            redpanda_tcp_errors_total: AtomicU64::new(0),
        }
    }
}

/// Converts a raw `OutboundRingEvent` popped from the wait-free SPSC ring into both a
/// `MarketEventEnvelope` (for gRPC `StreamMarketEvents`) and a `PublishedKafkaRecord` (for Redpanda).
pub fn convert_outbound_event(
    ev: &OutboundRingEvent,
) -> (MarketEventEnvelope, PublishedKafkaRecord) {
    let trace_id_hex = |seq: u64| format!("00-{:032x}-{:016x}-01", seq as u128, seq);

    match ev {
        OutboundRingEvent::L3Delta(d) => {
            let pb = delta_to_proto(d, None);
            let mut payload = Vec::with_capacity(96);
            pb.encode(&mut payload).expect("encode L3BookDelta");
            let key = d.symbol.as_bytes().to_vec();
            let partition = deterministic_partition(&key, DEFAULT_TOPIC_PARTITIONS);
            let env = MarketEventEnvelope {
                engine_seq: d.engine_seq,
                symbol: d.symbol.as_str().to_string(),
                ts_ns: d.event_ts_ns,
                payload: Some(market_event_envelope::Payload::L3Delta(pb)),
            };
            let rec = PublishedKafkaRecord {
                topic: TOPIC_L3_DELTAS,
                partition,
                key,
                payload,
                engine_seq: d.engine_seq,
                ts_ms: (d.event_ts_ns / 1_000_000) as i64,
                schema_type: "hyperion.v1.L3BookDelta",
                traceparent: trace_id_hex(d.engine_seq),
            };
            (env, rec)
        }
        OutboundRingEvent::Fill(f) => {
            let pb = fill_to_proto(f, None);
            let mut payload = Vec::with_capacity(128);
            pb.encode(&mut payload).expect("encode ExecutionFill");
            let key = f.symbol.as_bytes().to_vec();
            let partition = deterministic_partition(&key, DEFAULT_TOPIC_PARTITIONS);
            let env = MarketEventEnvelope {
                engine_seq: f.engine_seq,
                symbol: f.symbol.as_str().to_string(),
                ts_ns: f.exec_ts_ns,
                payload: Some(market_event_envelope::Payload::Fill(pb)),
            };
            let rec = PublishedKafkaRecord {
                topic: TOPIC_EXECUTIONS,
                partition,
                key,
                payload,
                engine_seq: f.engine_seq,
                ts_ms: (f.exec_ts_ns / 1_000_000) as i64,
                schema_type: "hyperion.v1.ExecutionFill",
                traceparent: trace_id_hex(f.engine_seq),
            };
            (env, rec)
        }
        OutboundRingEvent::Journal { symbol, entry } => {
            let pb = journal_to_proto(entry);
            let mut payload = Vec::with_capacity(160);
            pb.encode(&mut payload).expect("encode LedgerJournalEntry");
            let key = if symbol.is_empty() {
                let primary_acc = entry
                    .legs_slice()
                    .first()
                    .map(|l| l.account_id)
                    .unwrap_or(0);
                format!("account:{primary_acc}").into_bytes()
            } else {
                symbol.as_bytes().to_vec()
            };
            let partition = deterministic_partition(&key, DEFAULT_TOPIC_PARTITIONS);
            let env = MarketEventEnvelope {
                engine_seq: entry.engine_seq,
                symbol: symbol.as_str().to_string(),
                ts_ns: entry.ts_ns,
                payload: Some(market_event_envelope::Payload::JournalEntry(pb)),
            };
            let rec = PublishedKafkaRecord {
                topic: TOPIC_JOURNALS,
                partition,
                key,
                payload,
                engine_seq: entry.engine_seq,
                ts_ms: (entry.ts_ns / 1_000_000) as i64,
                schema_type: "hyperion.v1.LedgerJournalEntry",
                traceparent: trace_id_hex(entry.engine_seq),
            };
            (env, rec)
        }
    }
}

/// Spawns the non-blocking asynchronous publisher draining the SPSC ring buffer into Redpanda TCP
/// and the `StreamMarketEvents` broadcast channel without ever blocking the matching thread.
pub fn spawn_async_redpanda_publisher(
    mut consumer: SpscConsumer<OutboundRingEvent>,
    broker_addr: Option<String>,
    captured_sink: Option<CapturedRedpandaSink>,
    stream_tx: broadcast::Sender<MarketEventEnvelope>,
    metrics: Arc<PublisherMetrics>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut batch = Vec::with_capacity(512);
        let mut tcp_conn: Option<TcpStream> = None;
        let mut corr_id: i32 = 1;
        let mut read_hdr = [0u8; 4];
        let mut read_discard = Vec::with_capacity(512);

        loop {
            batch.clear();
            let drained = consumer.drain_batch(&mut batch, 512);
            if drained == 0 {
                if shutdown.load(Ordering::Acquire) && consumer.pending_len() == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_micros(250)).await;
                continue;
            }

            for ev in &batch {
                let (env, kafka_rec) = convert_outbound_event(ev);
                match ev {
                    OutboundRingEvent::L3Delta(_) => {
                        metrics.published_l3_total.fetch_add(1, Ordering::Relaxed);
                    }
                    OutboundRingEvent::Fill(_) => {
                        metrics
                            .published_fills_total
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    OutboundRingEvent::Journal { .. } => {
                        metrics
                            .published_journals_total
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }

                let _ = stream_tx.send(env);
                if let Some(cap) = &captured_sink {
                    cap.push(kafka_rec.clone());
                }

                if let Some(addr) = &broker_addr {
                    if tcp_conn.is_none() {
                        if let Ok(Ok(stream)) = tokio::time::timeout(
                            Duration::from_millis(75),
                            TcpStream::connect(addr),
                        )
                        .await
                        {
                            let _ = stream.set_nodelay(true);
                            tcp_conn = Some(stream);
                        }
                    }
                    if let Some(stream) = &mut tcp_conn {
                        let wire = encode_kafka_produce_request_v3(
                            corr_id,
                            "hyperion-ledger-rs",
                            &kafka_rec,
                        );
                        corr_id = corr_id.wrapping_add(1).max(1);
                        let send_ok = match stream.write_all(&wire).await {
                            Ok(()) => match tokio::time::timeout(
                                Duration::from_millis(50),
                                stream.read_exact(&mut read_hdr),
                            )
                            .await
                            {
                                Ok(Ok(_)) => {
                                    let resp_len = i32::from_be_bytes(read_hdr).max(0) as usize;
                                    if resp_len <= 65_536 {
                                        read_discard.resize(resp_len, 0);
                                        stream.read_exact(&mut read_discard).await.is_ok()
                                    } else {
                                        false
                                    }
                                }
                                _ => false,
                            },
                            Err(_) => false,
                        };
                        if send_ok {
                            metrics
                                .redpanda_tcp_sent_total
                                .fetch_add(1, Ordering::Relaxed);
                        } else {
                            metrics
                                .redpanda_tcp_errors_total
                                .fetch_add(1, Ordering::Relaxed);
                            tcp_conn = None;
                        }
                    }
                }
            }
        }
    })
}
