use crate::config::instrument_spec_for;
use crate::engine::IngestEvent;
use crate::fixed_point::calc_notional_fp;
use hyperion_proto::{
    ExecutionFill, L2BookSnapshot, L3BookDelta, L3DeltaOp, OrderSide, PriceLevel, U128,
};

/// Deterministic SplitMix64 PRNG (zero external crates, zero floating-point).
#[derive(Debug, Clone, Copy)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[inline(always)]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[inline(always)]
    pub fn range_u64(&mut self, low: u64, high_inclusive: u64) -> u64 {
        if high_inclusive <= low {
            return low;
        }
        let span = (high_inclusive - low) + 1;
        low + (self.next_u64() % span)
    }
}

#[derive(Debug, Clone)]
pub struct SyntheticStreamConfig {
    pub symbol: String,
    pub seed: u64,
    pub event_count: usize,
    pub start_ts_ns: u64,
    /// Mean step between events in nanoseconds (e.g. 200_000 ns = 5,000 events/sec market clock).
    pub step_ts_ns: u64,
    pub anchor_price_fp: u64,
}

impl Default for SyntheticStreamConfig {
    fn default() -> Self {
        Self {
            symbol: "BTC-USDT".to_string(),
            seed: 0x4859_5045_5249_4F4E,
            event_count: 250_000,
            start_ts_ns: 1_795_010_000_000_000_000,
            step_ts_ns: 200_000, // 0.2 ms -> 250k events = 50 seconds of dense HFT flow
            anchor_price_fp: 6_850_000_000_000, // 68,500.00 USDT
        }
    }
}

/// Generates a deterministic ordered stream of `event_count` realistic market events
/// (`L2BookSnapshot` seed + interleaved `L3BookDelta` book mutations and `ExecutionFill` trades).
pub fn generate_ordered_market_stream(cfg: &SyntheticStreamConfig) -> Vec<IngestEvent> {
    let spec = instrument_spec_for(&cfg.symbol);
    let tick = spec.tick_size_fp.max(1_000_000);
    let lot = spec.lot_size_fp.max(1_000);
    let mut rng = SplitMix64::new(cfg.seed);

    let mut out = Vec::with_capacity(cfg.event_count);
    if cfg.event_count == 0 {
        return out;
    }

    let mut ts_ns = cfg.start_ts_ns;
    let mut seq: u64 = 1;
    let mut fill_id: u64 = 1;
    let mut mid_ticks: i64 = (cfg.anchor_price_fp / tick) as i64;

    // Seed initial 20-level L2 book snapshot at event 0
    let mut init_bids = Vec::with_capacity(20);
    let mut init_asks = Vec::with_capacity(20);
    for lvl in 1..=20_i64 {
        let b_px = ((mid_ticks - lvl).max(1) as u64) * tick;
        let a_px = ((mid_ticks + lvl) as u64) * tick;
        let b_qty = rng.range_u64(500, 5_000) * lot;
        let a_qty = rng.range_u64(500, 5_000) * lot;
        init_bids.push(PriceLevel {
            price_fp: b_px,
            total_qty_fp: b_qty,
            order_count: 3,
        });
        init_asks.push(PriceLevel {
            price_fp: a_px,
            total_qty_fp: a_qty,
            order_count: 3,
        });
    }

    out.push(IngestEvent::L2Snapshot(L2BookSnapshot {
        engine_seq: seq,
        symbol: cfg.symbol.clone(),
        instrument_id: spec.instrument_id,
        best_bid_fp: init_bids[0].price_fp,
        best_ask_fp: init_asks[0].price_fp,
        mid_price_fp: cfg.anchor_price_fp,
        spread_fp: tick * 2,
        bids: init_bids,
        asks: init_asks,
        snapshot_ts_ns: ts_ns,
        state_root_blake3: vec![0x11; 32],
    }));

    for idx in 1..cfg.event_count {
        ts_ns += rng.range_u64(cfg.step_ts_ns / 2, cfg.step_ts_ns + (cfg.step_ts_ns / 2));
        seq += 1;

        // Random mean-reverting tick drift
        let drift_roll = rng.next_u64() % 10;
        if drift_roll == 0 {
            mid_ticks += 1;
        } else if drift_roll == 1 && mid_ticks > 100 {
            mid_ticks -= 1;
        }

        // 55% ExecutionFill, 45% L3BookDelta
        if (idx % 20) < 11 {
            let is_buy = (rng.next_u64() & 1) == 0;
            let side = if is_buy {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };
            let px_ticks = if is_buy { mid_ticks + 1 } else { mid_ticks - 1 };
            let price_fp = (px_ticks.max(1) as u64) * tick;
            let qty_fp = rng.range_u64(100, 25_000) * lot;
            let notional_fp = calc_notional_fp(price_fp, qty_fp);
            let cur_fill_id = fill_id;
            fill_id += 1;

            out.push(IngestEvent::Execution(ExecutionFill {
                fill_id: cur_fill_id,
                engine_seq: seq,
                symbol: cfg.symbol.clone(),
                instrument_id: spec.instrument_id,
                maker_order_id: 100_000 + cur_fill_id,
                taker_order_id: 900_000 + cur_fill_id,
                maker_account_id: 1_000 + (cur_fill_id % 64),
                taker_account_id: 2_000 + (cur_fill_id % 64),
                aggressor_side: side as i32,
                price_fp,
                qty_fp,
                notional_fp: Some(U128::from(notional_fp)),
                maker_fee_fp: -((notional_fp / 50_000) as i64),
                taker_fee_fp: (notional_fp / 10_000) as i64,
                exec_ts_ns: ts_ns,
                wal_blake3_hash: vec![0xAB; 32],
                trace: None,
            }));
        } else {
            let is_bid = (rng.next_u64() & 1) == 0;
            let side = if is_bid {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };
            let level_offset = rng.range_u64(1, 15) as i64;
            let px_ticks = if is_bid {
                (mid_ticks - level_offset).max(1)
            } else {
                mid_ticks + level_offset
            };
            let price_fp = (px_ticks as u64) * tick;
            let add_liquidity = (rng.next_u64() % 10) < 6;
            let mag = (rng.range_u64(50, 2_000) * lot) as i64;
            let (op, delta_qty_fp, rem_qty) = if add_liquidity {
                (L3DeltaOp::Add as i32, mag, mag as u64)
            } else {
                (L3DeltaOp::Cancel as i32, -mag, 0)
            };

            out.push(IngestEvent::L3Delta(L3BookDelta {
                engine_seq: seq,
                prev_engine_seq: seq - 1,
                symbol: cfg.symbol.clone(),
                instrument_id: spec.instrument_id,
                op,
                side: side as i32,
                order_id: 5_000_000 + seq,
                account_id: 3_000 + (seq % 64),
                price_fp,
                remaining_qty_fp: rem_qty,
                delta_qty_fp,
                event_ts_ns: ts_ns,
                state_root_blake3: Vec::new(),
                trace: None,
            }));
        }
    }

    out
}

/// Partially shuffles an ordered event stream in-place within local blocks such that
/// for every swapped pair `(i, j)`, `|ts[j] - ts[i]| <= max_out_of_order_ns < watermark_lateness_ns`.
/// Leaves index `0` (initial `L2BookSnapshot`) anchored at the start.
pub fn shuffle_within_watermark_bound(
    events: &[IngestEvent],
    max_out_of_order_ns: u64,
    seed: u64,
) -> Vec<IngestEvent> {
    let mut out = events.to_vec();
    if out.len() <= 2 || max_out_of_order_ns == 0 {
        return out;
    }
    let mut rng = SplitMix64::new(seed);
    let mut block_start = 1_usize;

    while block_start < out.len() {
        let base_ts = out[block_start].ts_ns();
        let mut block_end = block_start + 1;
        while block_end < out.len()
            && out[block_end].ts_ns().saturating_sub(base_ts) <= max_out_of_order_ns
            && (block_end - block_start) < 128
        {
            block_end += 1;
        }

        // Fisher-Yates shuffle inside `[block_start..block_end]`
        let len = block_end - block_start;
        if len > 1 {
            for k in (1..len).rev() {
                let j = (rng.next_u64() as usize) % (k + 1);
                out.swap(block_start + k, block_start + j);
            }
        }
        block_start = block_end;
    }
    out
}
