use crate::candle::Timeframe;
use crate::dedup::PartitionCheckpoint;
use crate::engine::FlushBatch;
use crate::fixed_point::{quantize_price_bucket, PgNumericI128};
use crate::storage::{AnalyticsStorage, BoxFuture, FlushReceipt, LoadedCheckpoint, StorageError};
use bytes::{BufMut, BytesMut};
use chrono::{DateTime, TimeZone, Utc};
use futures_util::pin_mut;
use hyperion_proto::{
    GetVolumeProfileResponse, LiquidityHeatmapBucket, MicrostructureMetrics, OhlcvCandle,
    VolumeProfileBin, I128, U128,
};
use postgres_types::{ToSql, Type};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio_postgres::{Client, NoTls};

/// PostgreSQL Binary `COPY` file signature (`PGCOPY\n\xff\r\n\0` + 4B flags + 4B ext len).
pub const PGCOPY_HEADER_MAGIC: &[u8; 11] = b"PGCOPY\n\xff\r\n\0";

/// Additional covering indexes & snapshot state DDL applied idempotently by `TimescaleFlusher`
/// on connection startup without mutating `/deploy/sql/001_init_timescaledb.sql`.
pub const AGGREGATOR_DDL_BOOTSTRAP: &str = r#"
CREATE TABLE IF NOT EXISTS stream_aggregator_state_snapshots (
    consumer_group TEXT        PRIMARY KEY,
    state_blob     BYTEA       NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_ohlcv_candles_lookup_covering
    ON ohlcv_candles (symbol, interval_code, bucket_open_ts_ns DESC)
    INCLUDE (open_fp, high_fp, low_fp, close_fp, base_volume_fp, quote_volume_fp, vwap_fp, twap_fp, trade_count, is_closed);

CREATE INDEX IF NOT EXISTS idx_heatmap_lookup_covering
    ON liquidity_heatmap_buckets (symbol, bucket_ts_ns DESC, price_bucket_fp ASC)
    INCLUDE (resting_bid_qty_fp, resting_ask_qty_fp);

CREATE INDEX IF NOT EXISTS idx_executions_profile_covering
    ON market_executions (symbol, exec_ts_ns DESC)
    INCLUDE (price_fp, qty_fp, aggressor_side);
"#;

#[inline]
fn ns_to_timestamptz(ts_ns: u64) -> DateTime<Utc> {
    let secs = (ts_ns / 1_000_000_000) as i64;
    let nanos = (ts_ns % 1_000_000_000) as u32;
    Utc.timestamp_opt(secs, nanos)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
}

/// Encodes a slice of `OhlcvCandle` records into exact PostgreSQL `COPY ... BINARY` wire bytes
/// (`PGCOPY\n\xff\r\n\0` + big-endian tuple headers + `PgNumericI128` 128-bit volumes + `0xFFFF` trailer).
/// Used both for offline binary wire verification and diagnostic replay inspection.
pub fn encode_candles_pg_binary_copy(candles: &[OhlcvCandle]) -> Result<Vec<u8>, StorageError> {
    let mut out = BytesMut::with_capacity(32 + candles.len() * 192);
    out.put_slice(PGCOPY_HEADER_MAGIC);
    out.put_u32(0); // flags
    out.put_u32(0); // header extension area length

    let mut field_buf = BytesMut::with_capacity(64);
    for c in candles {
        let Some(tf) = Timeframe::from_proto_i32(c.interval) else {
            continue;
        };
        let bucket_time = ns_to_timestamptz(c.bucket_open_ts_ns);
        let open_ts = c.bucket_open_ts_ns as i64;
        let close_ts = c.bucket_close_ts_ns as i64;
        let code = tf.code();
        let open_fp = c.open_fp as i64;
        let high_fp = c.high_fp as i64;
        let low_fp = c.low_fp as i64;
        let close_fp = c.close_fp as i64;
        let base_vol = PgNumericI128::from(c.base_volume_fp.map(u128::from).unwrap_or(0));
        let quote_vol = PgNumericI128::from(c.quote_volume_fp.map(u128::from).unwrap_or(0));
        let buy_vol = PgNumericI128::from(c.taker_buy_base_volume_fp.map(u128::from).unwrap_or(0));
        let sell_vol =
            PgNumericI128::from(c.taker_sell_base_volume_fp.map(u128::from).unwrap_or(0));
        let vwap_fp = c.vwap_fp as i64;
        let twap_fp = c.twap_fp as i64;
        let trade_count = c.trade_count as i64;
        let last_seq = c.last_engine_seq as i64;

        let cols: [(&dyn ToSql, Type); 18] = [
            (&bucket_time, Type::TIMESTAMPTZ),
            (&open_ts, Type::INT8),
            (&close_ts, Type::INT8),
            (&c.symbol, Type::TEXT),
            (&code, Type::TEXT),
            (&open_fp, Type::INT8),
            (&high_fp, Type::INT8),
            (&low_fp, Type::INT8),
            (&close_fp, Type::INT8),
            (&base_vol, Type::NUMERIC),
            (&quote_vol, Type::NUMERIC),
            (&buy_vol, Type::NUMERIC),
            (&sell_vol, Type::NUMERIC),
            (&vwap_fp, Type::INT8),
            (&twap_fp, Type::INT8),
            (&trade_count, Type::INT8),
            (&last_seq, Type::INT8),
            (&c.is_closed, Type::BOOL),
        ];

        out.put_i16(cols.len() as i16);
        for (val, ty) in cols {
            field_buf.clear();
            val.to_sql_checked(&ty, &mut field_buf)
                .map_err(|e| StorageError::Database(e.to_string()))?;
            out.put_i32(field_buf.len() as i32);
            out.extend_from_slice(&field_buf);
        }
    }

    out.put_i16(-1); // PGCOPY EOF trailer (0xFFFF)
    Ok(out.to_vec())
}

/// High-throughput TimescaleDB (PostgreSQL 16+) transactional batch flusher & analytical query client.
///
/// Combines:
/// 1. Session-reused temporary staging tables populated via **`BinaryCopyInWriter` (`COPY ... FROM STDIN BINARY`)**.
/// 2. Idempotent hypertable bulk upserts (`ON CONFLICT DO UPDATE ... WHERE last_engine_seq <= EXCLUDED.last_engine_seq`).
/// 3. Transactional consumer offset checkpoints (`stream_consumer_checkpoints`) + serialized `HYPAGG01` engine snapshot
///    committed atomically in the **exact same ACID transaction**.
#[derive(Clone)]
pub struct TimescaleFlusher {
    client: Arc<Mutex<Client>>,
}

impl TimescaleFlusher {
    pub async fn connect(dsn: &str) -> Result<Self, StorageError> {
        let (client, connection) = tokio_postgres::connect(dsn, NoTls)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!(error = %e, "TimescaleDB background connection error");
            }
        });

        client
            .batch_execute(AGGREGATOR_DDL_BOOTSTRAP)
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        // Prepare session-scoped staging tables once to eliminate catalog churn on every flush!
        client
            .batch_execute(
                r#"
                CREATE TEMP TABLE IF NOT EXISTS _stg_executions (LIKE market_executions INCLUDING DEFAULTS);
                CREATE TEMP TABLE IF NOT EXISTS _stg_l3_events  (LIKE l3_book_events INCLUDING DEFAULTS);
                CREATE TEMP TABLE IF NOT EXISTS _stg_candles    (LIKE ohlcv_candles INCLUDING DEFAULTS);
                CREATE TEMP TABLE IF NOT EXISTS _stg_micro      (LIKE microstructure_metrics INCLUDING DEFAULTS);
                CREATE TEMP TABLE IF NOT EXISTS _stg_heatmap    (LIKE liquidity_heatmap_buckets INCLUDING DEFAULTS);
                "#,
            )
            .await
            .map_err(|e| StorageError::Database(e.to_string()))?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
        })
    }
}

impl AnalyticsStorage for TimescaleFlusher {
    fn commit_flush_batch<'a>(
        &'a self,
        batch: &'a FlushBatch,
    ) -> BoxFuture<'a, Result<FlushReceipt, StorageError>> {
        Box::pin(async move {
            let mut guard = self.client.lock().await;
            let tx = guard
                .transaction()
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            let mut receipt = FlushReceipt::default();

            // 1. Binary COPY -> market_executions
            if !batch.executions.is_empty() {
                tx.batch_execute("TRUNCATE _stg_executions;")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let sink = tx
                    .copy_in("COPY _stg_executions FROM STDIN BINARY")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                let col_types = [
                    Type::TIMESTAMPTZ,
                    Type::INT8,
                    Type::TEXT,
                    Type::INT4,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT2,
                    Type::INT8,
                    Type::INT8,
                    Type::NUMERIC,
                    Type::INT8,
                    Type::INT8,
                    Type::BYTEA,
                ];
                let writer = BinaryCopyInWriter::new(sink, &col_types);
                pin_mut!(writer);

                for f in &batch.executions {
                    let exec_time = ns_to_timestamptz(f.exec_ts_ns);
                    let exec_ts_ns = f.exec_ts_ns as i64;
                    let inst_id = f.instrument_id as i32;
                    let engine_seq = f.engine_seq as i64;
                    let fill_id = f.fill_id as i64;
                    let maker_oid = f.maker_order_id as i64;
                    let taker_oid = f.taker_order_id as i64;
                    let maker_aid = f.maker_account_id as i64;
                    let taker_aid = f.taker_account_id as i64;
                    let side = (f.aggressor_side.clamp(1, 2)) as i16;
                    let price_fp = f.price_fp as i64;
                    let qty_fp = f.qty_fp as i64;
                    let notional = PgNumericI128::from(f.notional_fp.map(u128::from).unwrap_or(0));
                    let hash_bytes: &[u8] = if f.wal_blake3_hash.is_empty() {
                        &[0_u8; 32]
                    } else {
                        &f.wal_blake3_hash
                    };

                    writer
                        .as_mut()
                        .write(&[
                            &exec_time,
                            &exec_ts_ns,
                            &f.symbol,
                            &inst_id,
                            &engine_seq,
                            &fill_id,
                            &maker_oid,
                            &taker_oid,
                            &maker_aid,
                            &taker_aid,
                            &side,
                            &price_fp,
                            &qty_fp,
                            &notional,
                            &f.maker_fee_fp,
                            &f.taker_fee_fp,
                            &hash_bytes,
                        ])
                        .await
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
                writer
                    .finish()
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let inserted = tx
                    .execute(
                        r#"
                        INSERT INTO market_executions
                        SELECT * FROM _stg_executions
                        ON CONFLICT (symbol, exec_time, engine_seq, fill_id) DO NOTHING
                        "#,
                        &[],
                    )
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.executions_inserted = inserted as usize;
            }

            // 2. Binary COPY -> l3_book_events
            if !batch.l3_deltas.is_empty() {
                tx.batch_execute("TRUNCATE _stg_l3_events;")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let sink = tx
                    .copy_in("COPY _stg_l3_events FROM STDIN BINARY")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                let col_types = [
                    Type::TIMESTAMPTZ,
                    Type::INT8,
                    Type::TEXT,
                    Type::INT8,
                    Type::INT8,
                    Type::INT2,
                    Type::INT2,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                ];
                let writer = BinaryCopyInWriter::new(sink, &col_types);
                pin_mut!(writer);

                for d in &batch.l3_deltas {
                    let ev_time = ns_to_timestamptz(d.event_ts_ns);
                    let ev_ts_ns = d.event_ts_ns as i64;
                    let seq = d.engine_seq as i64;
                    let prev_seq = d.prev_engine_seq as i64;
                    let op = (d.op.clamp(1, 5)) as i16;
                    let side = (d.side.clamp(1, 2)) as i16;
                    let oid = d.order_id as i64;
                    let aid = d.account_id as i64;
                    let px = d.price_fp as i64;
                    let rem = d.remaining_qty_fp as i64;

                    writer
                        .as_mut()
                        .write(&[
                            &ev_time,
                            &ev_ts_ns,
                            &d.symbol,
                            &seq,
                            &prev_seq,
                            &op,
                            &side,
                            &oid,
                            &aid,
                            &px,
                            &rem,
                            &d.delta_qty_fp,
                        ])
                        .await
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
                writer
                    .finish()
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let inserted = tx
                    .execute(
                        r#"
                        INSERT INTO l3_book_events
                        SELECT * FROM _stg_l3_events
                        ON CONFLICT (symbol, event_time, engine_seq) DO NOTHING
                        "#,
                        &[],
                    )
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.l3_events_inserted = inserted as usize;
            }

            // 3. Binary COPY -> ohlcv_candles (Idempotent Full-State Upsert guarded by last_engine_seq)
            if !batch.candles.is_empty() {
                tx.batch_execute("TRUNCATE _stg_candles;")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let sink = tx
                    .copy_in("COPY _stg_candles FROM STDIN BINARY")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                let col_types = [
                    Type::TIMESTAMPTZ,
                    Type::INT8,
                    Type::INT8,
                    Type::TEXT,
                    Type::TEXT,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::NUMERIC,
                    Type::NUMERIC,
                    Type::NUMERIC,
                    Type::NUMERIC,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::BOOL,
                ];
                let writer = BinaryCopyInWriter::new(sink, &col_types);
                pin_mut!(writer);

                for c in &batch.candles {
                    let Some(tf) = Timeframe::from_proto_i32(c.interval) else {
                        continue;
                    };
                    let b_time = ns_to_timestamptz(c.bucket_open_ts_ns);
                    let open_ts = c.bucket_open_ts_ns as i64;
                    let close_ts = c.bucket_close_ts_ns as i64;
                    let code = tf.code();
                    let open_fp = c.open_fp as i64;
                    let high_fp = c.high_fp as i64;
                    let low_fp = c.low_fp as i64;
                    let close_fp = c.close_fp as i64;
                    let base_vol =
                        PgNumericI128::from(c.base_volume_fp.map(u128::from).unwrap_or(0));
                    let quote_vol =
                        PgNumericI128::from(c.quote_volume_fp.map(u128::from).unwrap_or(0));
                    let buy_vol = PgNumericI128::from(
                        c.taker_buy_base_volume_fp.map(u128::from).unwrap_or(0),
                    );
                    let sell_vol = PgNumericI128::from(
                        c.taker_sell_base_volume_fp.map(u128::from).unwrap_or(0),
                    );
                    let vwap_fp = c.vwap_fp as i64;
                    let twap_fp = c.twap_fp as i64;
                    let t_count = c.trade_count as i64;
                    let last_seq = c.last_engine_seq as i64;

                    writer
                        .as_mut()
                        .write(&[
                            &b_time,
                            &open_ts,
                            &close_ts,
                            &c.symbol,
                            &code,
                            &open_fp,
                            &high_fp,
                            &low_fp,
                            &close_fp,
                            &base_vol,
                            &quote_vol,
                            &buy_vol,
                            &sell_vol,
                            &vwap_fp,
                            &twap_fp,
                            &t_count,
                            &last_seq,
                            &c.is_closed,
                        ])
                        .await
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
                writer
                    .finish()
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let upserted = tx
                    .execute(
                        r#"
                        INSERT INTO ohlcv_candles
                        SELECT * FROM _stg_candles
                        ON CONFLICT (symbol, interval_code, bucket_time) DO UPDATE SET
                            bucket_open_ts_ns      = EXCLUDED.bucket_open_ts_ns,
                            bucket_close_ts_ns     = EXCLUDED.bucket_close_ts_ns,
                            open_fp                = EXCLUDED.open_fp,
                            high_fp                = EXCLUDED.high_fp,
                            low_fp                 = EXCLUDED.low_fp,
                            close_fp               = EXCLUDED.close_fp,
                            base_volume_fp         = EXCLUDED.base_volume_fp,
                            quote_volume_fp        = EXCLUDED.quote_volume_fp,
                            taker_buy_base_vol_fp  = EXCLUDED.taker_buy_base_vol_fp,
                            taker_sell_base_vol_fp = EXCLUDED.taker_sell_base_vol_fp,
                            vwap_fp                = EXCLUDED.vwap_fp,
                            twap_fp                = EXCLUDED.twap_fp,
                            trade_count            = EXCLUDED.trade_count,
                            last_engine_seq        = EXCLUDED.last_engine_seq,
                            is_closed              = EXCLUDED.is_closed
                        WHERE ohlcv_candles.last_engine_seq <= EXCLUDED.last_engine_seq
                        "#,
                        &[],
                    )
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.candles_upserted = upserted as usize;
            }

            // 4. Binary COPY -> microstructure_metrics
            if !batch.microstructure.is_empty() {
                tx.batch_execute("TRUNCATE _stg_micro;")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let sink = tx
                    .copy_in("COPY _stg_micro FROM STDIN BINARY")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                let col_types = [
                    Type::TIMESTAMPTZ,
                    Type::INT8,
                    Type::TEXT,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT4,
                    Type::INT8,
                    Type::INT8,
                    Type::NUMERIC,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                ];
                let writer = BinaryCopyInWriter::new(sink, &col_types);
                pin_mut!(writer);

                for m in &batch.microstructure {
                    let m_time = ns_to_timestamptz(m.ts_ns);
                    let ts_ns = m.ts_ns as i64;
                    let seq = m.engine_seq as i64;
                    let mid = m.mid_price_fp as i64;
                    let sp = m.spread_fp as i64;
                    let bps = m.spread_bps as i32;
                    let r_vwap = m.rolling_vwap_fp as i64;
                    let r_twap = m.rolling_twap_fp as i64;
                    let ofi = PgNumericI128::from(m.ofi_fp.map(i128::from).unwrap_or(0));

                    writer
                        .as_mut()
                        .write(&[
                            &m_time,
                            &ts_ns,
                            &m.symbol,
                            &seq,
                            &mid,
                            &sp,
                            &bps,
                            &r_vwap,
                            &r_twap,
                            &ofi,
                            &m.book_imbalance_top5_ppm,
                            &m.book_imbalance_top10_ppm,
                            &m.book_imbalance_top20_ppm,
                            &m.kyles_lambda_fp,
                        ])
                        .await
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
                writer
                    .finish()
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let inserted = tx
                    .execute(
                        r#"
                        INSERT INTO microstructure_metrics
                        SELECT * FROM _stg_micro
                        ON CONFLICT (symbol, metric_time, engine_seq) DO NOTHING
                        "#,
                        &[],
                    )
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.microstructure_inserted = inserted as usize;
            }

            // 5. Binary COPY -> liquidity_heatmap_buckets
            if !batch.heatmap_buckets.is_empty() {
                tx.batch_execute("TRUNCATE _stg_heatmap;")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let sink = tx
                    .copy_in("COPY _stg_heatmap FROM STDIN BINARY")
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                let col_types = [
                    Type::TIMESTAMPTZ,
                    Type::INT8,
                    Type::TEXT,
                    Type::INT8,
                    Type::INT8,
                    Type::INT8,
                ];
                let writer = BinaryCopyInWriter::new(sink, &col_types);
                pin_mut!(writer);

                for (sym, h) in &batch.heatmap_buckets {
                    let b_time = ns_to_timestamptz(h.bucket_ts_ns);
                    let b_ts = h.bucket_ts_ns as i64;
                    let p_bucket = h.price_bucket_fp as i64;
                    let bid_q = h.resting_bid_qty_fp as i64;
                    let ask_q = h.resting_ask_qty_fp as i64;

                    writer
                        .as_mut()
                        .write(&[&b_time, &b_ts, sym, &p_bucket, &bid_q, &ask_q])
                        .await
                        .map_err(|e| StorageError::Database(e.to_string()))?;
                }
                writer
                    .finish()
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;

                let upserted = tx
                    .execute(
                        r#"
                        INSERT INTO liquidity_heatmap_buckets
                        SELECT * FROM _stg_heatmap
                        ON CONFLICT (symbol, bucket_time, price_bucket_fp) DO UPDATE SET
                            resting_bid_qty_fp = EXCLUDED.resting_bid_qty_fp,
                            resting_ask_qty_fp = EXCLUDED.resting_ask_qty_fp
                        "#,
                        &[],
                    )
                    .await
                    .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.heatmap_upserted = upserted as usize;
            }

            // 6. Atomic Transactional Consumer Checkpoints + Engine State Snapshot
            for cp in &batch.checkpoints {
                let last_seq = cp.last_engine_seq as i64;
                let wm_ts = cp.watermark_ts_ns as i64;
                tx.execute(
                    r#"
                    INSERT INTO stream_consumer_checkpoints
                        (consumer_group, topic, partition_id, last_kafka_offset, last_engine_seq, watermark_ts_ns, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, NOW())
                    ON CONFLICT (consumer_group, topic, partition_id) DO UPDATE SET
                        last_kafka_offset = EXCLUDED.last_kafka_offset,
                        last_engine_seq   = EXCLUDED.last_engine_seq,
                        watermark_ts_ns   = EXCLUDED.watermark_ts_ns,
                        updated_at        = NOW()
                    WHERE stream_consumer_checkpoints.last_kafka_offset <= EXCLUDED.last_kafka_offset
                    "#,
                    &[
                        &cp.consumer_group,
                        &cp.topic,
                        &cp.partition_id,
                        &cp.last_kafka_offset,
                        &last_seq,
                        &wm_ts,
                    ],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;
                receipt.checkpoints_committed += 1;
            }

            if !batch.engine_checkpoint_blob.is_empty() {
                tx.execute(
                    r#"
                    INSERT INTO stream_aggregator_state_snapshots (consumer_group, state_blob, updated_at)
                    VALUES ($1, $2, NOW())
                    ON CONFLICT (consumer_group) DO UPDATE SET
                        state_blob = EXCLUDED.state_blob,
                        updated_at = NOW()
                    "#,
                    &[&batch.consumer_group, &batch.engine_checkpoint_blob],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;
            }

            tx.commit()
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            Ok(receipt)
        })
    }

    fn load_checkpoint<'a>(
        &'a self,
        consumer_group: &'a str,
    ) -> BoxFuture<'a, Result<Option<LoadedCheckpoint>, StorageError>> {
        Box::pin(async move {
            let guard = self.client.lock().await;
            let snap_row = guard
                .query_opt(
                    "SELECT state_blob FROM stream_aggregator_state_snapshots WHERE consumer_group = $1",
                    &[&consumer_group],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            let Some(row) = snap_row else {
                return Ok(None);
            };
            let blob: Vec<u8> = row.get(0);

            let cp_rows = guard
                .query(
                    r#"
                    SELECT consumer_group, topic, partition_id, last_kafka_offset, last_engine_seq, watermark_ts_ns
                    FROM stream_consumer_checkpoints
                    WHERE consumer_group = $1
                    "#,
                    &[&consumer_group],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            let mut cps = Vec::with_capacity(cp_rows.len());
            for r in cp_rows {
                cps.push(PartitionCheckpoint {
                    consumer_group: r.get(0),
                    topic: r.get(1),
                    partition_id: r.get(2),
                    last_kafka_offset: r.get(3),
                    last_engine_seq: r.get::<_, i64>(4) as u64,
                    watermark_ts_ns: r.get::<_, i64>(5) as u64,
                });
            }

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
            let guard = self.client.lock().await;
            let code = tf.code();
            let from_i64 = from_ts_ns as i64;
            let to_i64 = if to_ts_ns == 0 {
                i64::MAX
            } else {
                to_ts_ns as i64
            };
            let lim_i64 = (limit.clamp(1, 5_000)) as i64;

            let rows = guard
                .query(
                    r#"
                    SELECT bucket_open_ts_ns, bucket_close_ts_ns, open_fp, high_fp, low_fp, close_fp,
                           base_volume_fp, quote_volume_fp, taker_buy_base_vol_fp, taker_sell_base_vol_fp,
                           vwap_fp, twap_fp, trade_count, last_engine_seq, is_closed
                    FROM ohlcv_candles
                    WHERE symbol = $1 AND interval_code = $2
                      AND bucket_open_ts_ns >= $3 AND bucket_open_ts_ns <= $4
                    ORDER BY bucket_open_ts_ns DESC
                    LIMIT $5
                    "#,
                    &[&symbol, &code, &from_i64, &to_i64, &lim_i64],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            let mut out: Vec<OhlcvCandle> = rows
                .into_iter()
                .map(|r| {
                    let base_v: PgNumericI128 = r.get(6);
                    let quote_v: PgNumericI128 = r.get(7);
                    let buy_v: PgNumericI128 = r.get(8);
                    let sell_v: PgNumericI128 = r.get(9);
                    OhlcvCandle {
                        symbol: symbol.to_string(),
                        interval: tf.proto_interval() as i32,
                        bucket_open_ts_ns: r.get::<_, i64>(0) as u64,
                        bucket_close_ts_ns: r.get::<_, i64>(1) as u64,
                        open_fp: r.get::<_, i64>(2) as u64,
                        high_fp: r.get::<_, i64>(3) as u64,
                        low_fp: r.get::<_, i64>(4) as u64,
                        close_fp: r.get::<_, i64>(5) as u64,
                        base_volume_fp: Some(U128::from(base_v.0.max(0) as u128)),
                        quote_volume_fp: Some(U128::from(quote_v.0.max(0) as u128)),
                        taker_buy_base_volume_fp: Some(U128::from(buy_v.0.max(0) as u128)),
                        taker_sell_base_volume_fp: Some(U128::from(sell_v.0.max(0) as u128)),
                        vwap_fp: r.get::<_, i64>(10) as u64,
                        twap_fp: r.get::<_, i64>(11) as u64,
                        trade_count: r.get::<_, i64>(12) as u64,
                        last_engine_seq: r.get::<_, i64>(13) as u64,
                        is_closed: r.get(14),
                    }
                })
                .collect();
            out.reverse();
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
            let guard = self.client.lock().await;
            let from_i64 = from_ts_ns as i64;
            let to_i64 = if to_ts_ns == 0 {
                i64::MAX
            } else {
                to_ts_ns as i64
            };
            let step = (bucket_size_fp.max(10_000_000)) as i64;

            let rows = guard
                .query(
                    r#"
                    SELECT ((price_fp / $4) * $4) AS price_bucket_fp,
                           SUM(CASE WHEN aggressor_side = 1 THEN qty_fp ELSE 0 END)::NUMERIC(38,0) AS buy_vol,
                           SUM(CASE WHEN aggressor_side = 2 THEN qty_fp ELSE 0 END)::NUMERIC(38,0) AS sell_vol
                    FROM market_executions
                    WHERE symbol = $1 AND exec_ts_ns >= $2 AND exec_ts_ns <= $3
                    GROUP BY price_bucket_fp
                    ORDER BY price_bucket_fp ASC
                    "#,
                    &[&symbol, &from_i64, &to_i64, &step],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            let mut poc_price_fp = 0_u64;
            let mut max_vol = 0_u128;
            let mut bins = Vec::with_capacity(rows.len());
            for r in rows {
                let raw_px = r.get::<_, i64>(0).max(1) as u64;
                let px = quantize_price_bucket(raw_px, step as u64);
                let bv = r.get::<_, PgNumericI128>(1).0.max(0) as u128;
                let sv = r.get::<_, PgNumericI128>(2).0.max(0) as u128;
                let tv = bv + sv;
                if tv >= max_vol {
                    max_vol = tv;
                    poc_price_fp = px;
                }
                bins.push(VolumeProfileBin {
                    price_bucket_fp: px,
                    buy_volume_fp: Some(U128::from(bv)),
                    sell_volume_fp: Some(U128::from(sv)),
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
            let guard = self.client.lock().await;
            let from_i64 = from_ts_ns as i64;
            let to_i64 = if to_ts_ns == 0 {
                i64::MAX
            } else {
                to_ts_ns as i64
            };
            let px_step = (price_bucket_size_fp.max(1)) as i64;
            let t_step = (time_bucket_ns.max(1)) as i64;

            let rows = guard
                .query(
                    r#"
                    SELECT ((bucket_ts_ns / $5) * $5) AS t_bucket,
                           GREATEST(((price_bucket_fp / $4) * $4), $4) AS p_bucket,
                           SUM(resting_bid_qty_fp)::BIGINT AS bid_qty,
                           SUM(resting_ask_qty_fp)::BIGINT AS ask_qty
                    FROM liquidity_heatmap_buckets
                    WHERE symbol = $1 AND bucket_ts_ns >= $2 AND bucket_ts_ns <= $3
                    GROUP BY t_bucket, p_bucket
                    ORDER BY t_bucket ASC, p_bucket ASC
                    LIMIT 20000
                    "#,
                    &[&symbol, &from_i64, &to_i64, &px_step, &t_step],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            Ok(rows
                .into_iter()
                .map(|r| LiquidityHeatmapBucket {
                    bucket_ts_ns: r.get::<_, i64>(0) as u64,
                    price_bucket_fp: r.get::<_, i64>(1) as u64,
                    resting_bid_qty_fp: r.get::<_, i64>(2).max(0) as u64,
                    resting_ask_qty_fp: r.get::<_, i64>(3).max(0) as u64,
                })
                .collect())
        })
    }

    fn query_latest_microstructure<'a>(
        &'a self,
        symbol: &'a str,
    ) -> BoxFuture<'a, Result<Option<MicrostructureMetrics>, StorageError>> {
        Box::pin(async move {
            let guard = self.client.lock().await;
            let row = guard
                .query_opt(
                    r#"
                    SELECT engine_seq, ts_ns, mid_price_fp, spread_fp, spread_bps,
                           rolling_vwap_fp, rolling_twap_fp, ofi_fp,
                           book_imbalance_top5_ppm, book_imbalance_top10_ppm, book_imbalance_top20_ppm,
                           kyles_lambda_fp
                    FROM microstructure_metrics
                    WHERE symbol = $1
                    ORDER BY metric_time DESC, engine_seq DESC
                    LIMIT 1
                    "#,
                    &[&symbol],
                )
                .await
                .map_err(|e| StorageError::Database(e.to_string()))?;

            Ok(row.map(|r| {
                let ofi: PgNumericI128 = r.get(7);
                MicrostructureMetrics {
                    symbol: symbol.to_string(),
                    engine_seq: r.get::<_, i64>(0) as u64,
                    ts_ns: r.get::<_, i64>(1) as u64,
                    mid_price_fp: r.get::<_, i64>(2) as u64,
                    spread_fp: r.get::<_, i64>(3) as u64,
                    spread_bps: r.get::<_, i32>(4) as u32,
                    rolling_vwap_fp: r.get::<_, i64>(5) as u64,
                    rolling_twap_fp: r.get::<_, i64>(6) as u64,
                    ofi_fp: Some(I128::from(ofi.0)),
                    book_imbalance_top5_ppm: r.get(8),
                    book_imbalance_top10_ppm: r.get(9),
                    book_imbalance_top20_ppm: r.get(10),
                    kyles_lambda_fp: r.get(11),
                }
            }))
        })
    }
}
