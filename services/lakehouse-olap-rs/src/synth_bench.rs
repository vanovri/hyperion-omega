use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use arrow::array::{
    ArrayRef, Int64Array, RecordBatch, StringDictionaryBuilder, UInt32Array, UInt64Array,
};
use arrow::datatypes::Int8Type;

use crate::arrow_writer::{write_sorted_partition_batch, PartitionKey};
use crate::config::{LakehouseConfig, FP_SCALE, FP_SCALE_I128, NS_PER_HOUR};
use crate::engine::LakehouseEngine;
use crate::error::Result;
use crate::schema::{
    hour_bucket_from_ts_ns, quotes_physical_schema, symbol_for_instrument_id,
    trades_physical_schema, TABLE_QUOTES, TABLE_TRADES,
};

/// Summary of the generated multi-million-row synthetic Parquet dataset.
#[derive(Debug, Clone)]
pub struct SyntheticDatasetStats {
    pub total_trade_rows: usize,
    pub total_quote_rows: usize,
    pub total_rows: usize,
    pub partitions_written: usize,
    pub total_parquet_bytes: u64,
    pub generation_elapsed_ms: u64,
    pub base_ts_ns: i64,
    pub toxic_sniper_account_id: u64,
}

#[inline]
fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Generates a deterministic, realistic synthetic market dataset (`>= 2,000,000` rows)
/// partitioned across 4 instruments (`BTC-USDT`, `ETH-USDT`, `SOL-USDT`, `HYPE-USDT`) and
/// 2 hourly buckets (`8` hierarchical Hive partitions), embedding an informed toxic-flow
/// sniper account (`666_001`) whose executions precede upward/downward mid-price jumps!
pub async fn generate_million_row_parquet_dataset(
    engine: &Arc<LakehouseEngine>,
    trade_rows: usize,
    quote_rows: usize,
) -> Result<SyntheticDatasetStats> {
    let start = Instant::now();
    let cfg = engine.config().clone();

    let instruments: [(u32, i64); 4] = [
        (1, 6_850_000_000_000), // BTC-USDT: 68,500.00
        (2, 345_000_000_000),   // ETH-USDT:  3,450.00
        (3, 18_500_000_000),    // SOL-USDT:    185.00
        (4, 2_850_000_000),     // HYPE-USDT:    28.50
    ];
    let num_hours = 2_usize;
    let num_partitions = instruments.len() * num_hours;
    let trades_per_part = trade_rows / num_partitions;
    let quotes_per_part = quote_rows / num_partitions;

    // 2026-11-18T20:00:00Z aligned to exact hour boundary
    let base_hour_idx: u64 = 498_620;
    let base_ts_ns: i64 = (base_hour_idx * NS_PER_HOUR) as i64;
    let toxic_sniper_account_id: u64 = 666_001;

    let mut tasks = Vec::with_capacity(num_partitions);
    for (inst_id, anchor_price_fp) in instruments {
        for hr_off in 0..num_hours {
            let cfg_p = cfg.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                build_and_write_partition(
                    &cfg_p,
                    inst_id,
                    anchor_price_fp,
                    base_ts_ns + (hr_off as i64) * (NS_PER_HOUR as i64),
                    hr_off as u64,
                    trades_per_part,
                    quotes_per_part,
                    toxic_sniper_account_id,
                )
            }));
        }
    }

    let mut total_bytes: u64 = 0;
    let mut written_parts = 0_usize;
    let mut all_quote_series: Vec<(u32, Vec<i64>, Vec<i64>)> = Vec::with_capacity(num_partitions);

    for t in tasks {
        let out = t
            .await
            .map_err(|e| crate::error::LakehouseError::DataFusion(e.to_string()))??;
        total_bytes += out.trade_bytes + out.quote_bytes;
        written_parts += 1;
        all_quote_series.push((out.inst_id, out.quote_ts, out.quote_mids));
    }

    // Merge quote mid-price series per instrument into `MarketMidTape`
    for (inst_id, _) in instruments {
        let mut combined_ts = Vec::with_capacity(quotes_per_part * num_hours);
        let mut combined_mid = Vec::with_capacity(quotes_per_part * num_hours);
        for (id, ts_v, mid_v) in &all_quote_series {
            if *id == inst_id {
                combined_ts.extend_from_slice(ts_v);
                combined_mid.extend_from_slice(mid_v);
            }
        }
        engine
            .mid_tape()
            .set_instrument_series(inst_id, combined_ts, combined_mid);
    }

    let actual_trades = trades_per_part * num_partitions;
    let actual_quotes = quotes_per_part * num_partitions;
    engine
        .telemetry
        .ingested_trade_rows
        .fetch_add(actual_trades as u64, Ordering::Relaxed);
    engine
        .telemetry
        .ingested_quote_rows
        .fetch_add(actual_quotes as u64, Ordering::Relaxed);

    engine.refresh_table_providers().await?;

    Ok(SyntheticDatasetStats {
        total_trade_rows: actual_trades,
        total_quote_rows: actual_quotes,
        total_rows: actual_trades + actual_quotes,
        partitions_written: written_parts,
        total_parquet_bytes: total_bytes,
        generation_elapsed_ms: start.elapsed().as_millis() as u64,
        base_ts_ns,
        toxic_sniper_account_id,
    })
}

struct PartitionBuildOutput {
    trade_bytes: u64,
    quote_bytes: u64,
    inst_id: u32,
    quote_ts: Vec<i64>,
    quote_mids: Vec<i64>,
}

#[allow(clippy::too_many_arguments)]
fn build_and_write_partition(
    cfg: &LakehouseConfig,
    inst_id: u32,
    anchor_price_fp: i64,
    part_start_ts_ns: i64,
    hr_off: u64,
    n_trades: usize,
    n_quotes: usize,
    toxic_sniper_account_id: u64,
) -> Result<PartitionBuildOutput> {
    let symbol = symbol_for_instrument_id(inst_id).to_string();
    let hour_bucket = hour_bucket_from_ts_ns(part_start_ts_ns);
    let part_key = PartitionKey {
        symbol,
        hour_bucket,
    };

    let mut rng_state: u64 =
        0x9E37_79B9_7F4A_7C15_u64 ^ ((inst_id as u64) << 32) ^ (hr_off.wrapping_mul(0xBF58_476D));

    // Step 1: Build monotonically sorted L2 Quote series covering the 3,600s hour
    let q_step_ns = (NS_PER_HOUR as i64) / (n_quotes.max(1) as i64);
    let mut q_inst = Vec::with_capacity(n_quotes);
    let mut q_ts = Vec::with_capacity(n_quotes);
    let mut q_seq = Vec::with_capacity(n_quotes);
    let mut q_bid = Vec::with_capacity(n_quotes);
    let mut q_ask = Vec::with_capacity(n_quotes);
    let mut q_mid = Vec::with_capacity(n_quotes);
    let mut q_spread = Vec::with_capacity(n_quotes);
    let mut q_bid_depth = Vec::with_capacity(n_quotes);
    let mut q_ask_depth = Vec::with_capacity(n_quotes);
    let mut q_regime = StringDictionaryBuilder::<Int8Type>::with_capacity(n_quotes, 2, 32);

    let mut current_mid = anchor_price_fp;
    let base_seq_offset = (inst_id as u64) * 100_000_000 + hr_off * 10_000_000;
    let tick_fp: i64 = 1_000_000; // 0.01 USDT

    for qi in 0..n_quotes {
        let r = xorshift64(&mut rng_state);
        // Zero-mean symmetric Brownian random walk [-7, +7] ticks + periodic informed jump ticks
        let step_ticks = ((r % 15) as i64) - 7;
        let jump_ticks = if qi % 128 == 64 { 60_i64 } else { 0 };
        current_mid = (current_mid + (step_ticks + jump_ticks) * tick_fp).max(10 * FP_SCALE);

        let half_spread = (1 + ((r >> 8) & 0x03) as i64) * tick_fp;
        let bid = current_mid - half_spread;
        let ask = current_mid + half_spread;
        let ts = part_start_ts_ns + (qi as i64) * q_step_ns;

        q_inst.push(inst_id);
        q_ts.push(ts);
        q_seq.push(base_seq_offset + (qi as u64) * 10);
        q_bid.push(bid);
        q_ask.push(ask);
        q_mid.push(current_mid);
        q_spread.push(ask - bid);
        q_bid_depth.push(5 * FP_SCALE + (((r >> 16) & 0xFF) as i64) * 10_000_000);
        q_ask_depth.push(5 * FP_SCALE + (((r >> 24) & 0xFF) as i64) * 10_000_000);
        q_regime.append_value("NORMAL");
    }

    let quote_cols: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(q_inst)),
        Arc::new(Int64Array::from(q_ts.clone())),
        Arc::new(UInt64Array::from(q_seq)),
        Arc::new(Int64Array::from(q_bid)),
        Arc::new(Int64Array::from(q_ask)),
        Arc::new(Int64Array::from(q_mid.clone())),
        Arc::new(Int64Array::from(q_spread)),
        Arc::new(Int64Array::from(q_bid_depth)),
        Arc::new(Int64Array::from(q_ask_depth)),
        Arc::new(q_regime.finish()),
    ];
    let quote_batch = RecordBatch::try_new(quotes_physical_schema(), quote_cols)?;
    let q_written =
        write_sorted_partition_batch(cfg, TABLE_QUOTES, &part_key, &quote_batch, &[], "compacted")?;

    // Step 2: Build monotonically sorted Trades aligned with Quote dynamics + Toxic Sniper orders
    let t_step_ns = (NS_PER_HOUR as i64) / (n_trades.max(1) as i64);
    let mut t_inst = Vec::with_capacity(n_trades);
    let mut t_ts = Vec::with_capacity(n_trades);
    let mut t_seq = Vec::with_capacity(n_trades);
    let mut t_fill = Vec::with_capacity(n_trades);
    let mut t_maker = Vec::with_capacity(n_trades);
    let mut t_taker = Vec::with_capacity(n_trades);
    let mut t_acct = Vec::with_capacity(n_trades);
    let mut t_side_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n_trades, 2, 16);
    let mut t_side_sign = Vec::with_capacity(n_trades);
    let mut t_price = Vec::with_capacity(n_trades);
    let mut t_qty = Vec::with_capacity(n_trades);
    let mut t_notional = Vec::with_capacity(n_trades);
    let mut t_mfee = Vec::with_capacity(n_trades);
    let mut t_tfee = Vec::with_capacity(n_trades);
    let mut t_src_dict = StringDictionaryBuilder::<Int8Type>::with_capacity(n_trades, 2, 24);

    for ti in 0..n_trades {
        let r = xorshift64(&mut rng_state);
        let ts = part_start_ts_ns + (ti as i64) * t_step_ns + 500;
        let q_idx = ((ts - part_start_ts_ns) / q_step_ns.max(1))
            .clamp(0, (n_quotes.saturating_sub(1)) as i64) as usize;
        let ref_mid = q_mid[q_idx];

        // Execute informed toxic sniper buy at the start of every `q_idx % 128 == 63` window
        // (72 ms before the +60-tick upward jump at `q_idx % 128 == 64`)
        let is_toxic_sniper = (q_idx % 128 == 63) && (ti % 5 == 0);
        let (taker_id, maker_id, is_buy) = if is_toxic_sniper {
            (toxic_sniper_account_id, 10_000 + (r % 200), true)
        } else {
            let tk = 20_000 + (r % 4_000);
            let mk = 10_000 + ((r >> 12) % 1_000);
            (tk, mk, (r & 1) == 0)
        };

        let sign = if is_buy { 1_i64 } else { -1_i64 };
        let qty_fp = 10_000_000 + (((r >> 20) & 0x7F) as i64) * 1_000_000; // 0.10 .. 1.37 units
        // Positive price impact correlated with signed volume so Kyle's Lambda is strictly positive!
        let impact_fp = (sign * qty_fp) / 200;
        let exec_price_fp = ref_mid + impact_fp;
        let notional_fp = ((i128::from(exec_price_fp) * i128::from(qty_fp)) / FP_SCALE_I128) as i64;
        let maker_fee = -notional_fp / 50_000; // -0.2 bps rebate
        let taker_fee = notional_fp / 10_000;  // +1.0 bps fee

        let seq = base_seq_offset + 1 + (ti as u64);
        t_inst.push(inst_id);
        t_ts.push(ts);
        t_seq.push(seq);
        t_fill.push(seq);
        t_maker.push(maker_id);
        t_taker.push(taker_id);
        t_acct.push(taker_id);
        if is_buy {
            t_side_dict.append_value("BUY");
        } else {
            t_side_dict.append_value("SELL");
        }
        t_side_sign.push(sign);
        t_price.push(exec_price_fp);
        t_qty.push(qty_fp);
        t_notional.push(notional_fp);
        t_mfee.push(maker_fee);
        t_tfee.push(taker_fee);
        t_src_dict.append_value(if (ti & 3) == 0 { "COLD_WAL" } else { "REDPANDA" });
    }

    let trade_cols: Vec<ArrayRef> = vec![
        Arc::new(UInt32Array::from(t_inst)),
        Arc::new(Int64Array::from(t_ts)),
        Arc::new(UInt64Array::from(t_seq)),
        Arc::new(UInt64Array::from(t_fill)),
        Arc::new(UInt64Array::from(t_maker)),
        Arc::new(UInt64Array::from(t_taker)),
        Arc::new(UInt64Array::from(t_acct)),
        Arc::new(t_side_dict.finish()),
        Arc::new(Int64Array::from(t_side_sign)),
        Arc::new(Int64Array::from(t_price)),
        Arc::new(Int64Array::from(t_qty)),
        Arc::new(Int64Array::from(t_notional)),
        Arc::new(Int64Array::from(t_mfee)),
        Arc::new(Int64Array::from(t_tfee)),
        Arc::new(t_src_dict.finish()),
    ];
    let trade_batch = RecordBatch::try_new(trades_physical_schema(), trade_cols)?;
    let bloom_cols = ["maker_account_id", "taker_account_id", "account_id"];
    let t_written = write_sorted_partition_batch(
        cfg,
        TABLE_TRADES,
        &part_key,
        &trade_batch,
        &bloom_cols,
        "compacted",
    )?;

    Ok(PartitionBuildOutput {
        trade_bytes: t_written.byte_size,
        quote_bytes: q_written.byte_size,
        inst_id,
        quote_ts: q_ts,
        quote_mids: q_mid,
    })
}
