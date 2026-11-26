use crate::candle::{Timeframe, ALL_TIMEFRAMES, TWAP_SUB_SLICES};
use crate::engine::{canonical_sub_seq, IngestEvent};
use crate::fixed_point::{calc_imbalance_ppm, calc_notional_fp, calc_vwap_fp};
use hyperion_proto::{L3DeltaOp, OhlcvCandle, OrderSide, U128};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceSymbolSummary {
    pub symbol: String,
    /// `(timeframe, bucket_open_ts_ns) -> OhlcvCandle`
    pub candles: BTreeMap<(Timeframe, u64), OhlcvCandle>,
    pub session_vwap_fp: u64,
    pub rolling_vwap_fp: u64,
    pub cumulative_ofi_fp: i128,
    pub rolling_ofi_fp: i128,
    pub book_imbalance_top5_ppm: i64,
    pub book_imbalance_top10_ppm: i64,
    pub book_imbalance_top20_ppm: i64,
    pub total_trades: u64,
    pub total_base_volume_fp: u128,
}

/// Naive, unoptimized `O(N log N)` mathematical reference oracle for formal verification.
///
/// Algorithm:
/// 1. Simulates exact arrival-order deduplication + deterministic watermark admission (`ts >= W`).
/// 2. Globally sorts all accepted events by `(ts_ns, engine_seq, canonical_sub_seq)`.
/// 3. Computes OHLCV (`1s`, `5s`, `1m`, `5m`, `15m`, `1h`), VWAP, 16-slice TWAP,
///    Cont-Kukanov-Stoikov BBO OFI, and Top-5/10/20 Book Imbalances from first principles
///    using brute-force exact `u128`/`i128` integer passes.
pub fn compute_naive_reference(
    events: &[IngestEvent],
    symbol: &str,
    allowed_lateness_ns: u64,
) -> ReferenceSymbolSummary {
    let mut seen_fills: HashSet<u64> = HashSet::new();
    let mut seen_l3: HashSet<u64> = HashSet::new();
    let mut seen_l2: HashSet<u64> = HashSet::new();
    let mut max_ts_ns: u64 = 0;
    let mut watermark_ts_ns: u64 = 0;

    let mut accepted: Vec<(u64, u64, u64, IngestEvent)> = Vec::new();

    for ev in events {
        if ev.symbol() != symbol {
            continue;
        }
        match ev {
            IngestEvent::Execution(f) => {
                if f.price_fp == 0 || f.qty_fp == 0 || f.exec_ts_ns == 0 {
                    continue;
                }
                let id = if f.fill_id > 0 {
                    f.fill_id
                } else {
                    f.engine_seq
                };
                if id == 0 || !seen_fills.insert(id) {
                    continue;
                }
                if f.exec_ts_ns < watermark_ts_ns {
                    continue;
                }
                if f.exec_ts_ns > max_ts_ns {
                    max_ts_ns = f.exec_ts_ns;
                    watermark_ts_ns =
                        watermark_ts_ns.max(max_ts_ns.saturating_sub(allowed_lateness_ns));
                }
                accepted.push((
                    f.exec_ts_ns,
                    f.engine_seq,
                    canonical_sub_seq(3, id),
                    ev.clone(),
                ));
            }
            IngestEvent::L3Delta(d) => {
                if d.event_ts_ns == 0 || d.engine_seq == 0 || !seen_l3.insert(d.engine_seq) {
                    continue;
                }
                if d.event_ts_ns < watermark_ts_ns {
                    continue;
                }
                if d.event_ts_ns > max_ts_ns {
                    max_ts_ns = d.event_ts_ns;
                    watermark_ts_ns =
                        watermark_ts_ns.max(max_ts_ns.saturating_sub(allowed_lateness_ns));
                }
                accepted.push((
                    d.event_ts_ns,
                    d.engine_seq,
                    canonical_sub_seq(2, d.order_id),
                    ev.clone(),
                ));
            }
            IngestEvent::L2Snapshot(s) => {
                if s.snapshot_ts_ns == 0 || s.engine_seq == 0 || !seen_l2.insert(s.engine_seq) {
                    continue;
                }
                if s.snapshot_ts_ns < watermark_ts_ns {
                    continue;
                }
                if s.snapshot_ts_ns > max_ts_ns {
                    max_ts_ns = s.snapshot_ts_ns;
                    watermark_ts_ns =
                        watermark_ts_ns.max(max_ts_ns.saturating_sub(allowed_lateness_ns));
                }
                accepted.push((
                    s.snapshot_ts_ns,
                    s.engine_seq,
                    canonical_sub_seq(1, s.engine_seq),
                    ev.clone(),
                ));
            }
        }
    }

    // Global canonical sort O(N log N)
    accepted.sort_by_key(|a| (a.0, a.1, a.2));

    // 1. Naive multi-timeframe candle computation from sorted fills
    let mut fills_only = Vec::new();
    for (_, _, _, ev) in &accepted {
        if let IngestEvent::Execution(f) = ev {
            fills_only.push(f.clone());
        }
    }

    let mut candles: BTreeMap<(Timeframe, u64), OhlcvCandle> = BTreeMap::new();
    for tf in ALL_TIMEFRAMES {
        let dur = tf.duration_ns();
        let mut grouped: BTreeMap<u64, Vec<&hyperion_proto::ExecutionFill>> = BTreeMap::new();
        for f in &fills_only {
            let b_open = (f.exec_ts_ns / dur) * dur;
            grouped.entry(b_open).or_default().push(f);
        }

        for (b_open, group) in grouped {
            let b_close = b_open + dur;
            let open_fp = group.first().map(|f| f.price_fp).unwrap_or(0);
            let close_fp = group.last().map(|f| f.price_fp).unwrap_or(0);
            let mut high_fp = 0_u64;
            let mut low_fp = u64::MAX;
            let mut base_vol: u128 = 0;
            let mut quote_vol: u128 = 0;
            let mut raw_pq: u128 = 0;
            let mut buy_vol: u128 = 0;
            let mut sell_vol: u128 = 0;
            let mut last_seq = 0_u64;
            let mut sub_px = [0_u64; TWAP_SUB_SLICES];

            for f in &group {
                high_fp = high_fp.max(f.price_fp);
                low_fp = low_fp.min(f.price_fp);
                let q = f.qty_fp as u128;
                let n = f
                    .notional_fp
                    .map(u128::from)
                    .filter(|&v| v > 0)
                    .unwrap_or_else(|| calc_notional_fp(f.price_fp, f.qty_fp));
                base_vol += q;
                quote_vol += n;
                raw_pq += (f.price_fp as u128) * q;
                match OrderSide::try_from(f.aggressor_side).unwrap_or(OrderSide::Unspecified) {
                    OrderSide::Buy => buy_vol += q,
                    OrderSide::Sell | OrderSide::Unspecified => sell_vol += q,
                }
                last_seq = last_seq.max(f.engine_seq);

                let rel = f.exec_ts_ns - b_open;
                let sub_idx = (((rel as u128) * (TWAP_SUB_SLICES as u128)) / (dur as u128))
                    .min((TWAP_SUB_SLICES - 1) as u128) as usize;
                sub_px[sub_idx] = f.price_fp;
            }

            let vwap_fp = calc_vwap_fp(raw_pq, base_vol, close_fp);
            let mut carry = open_fp;
            let mut twap_sum: u128 = 0;
            for &sp in &sub_px {
                if sp > 0 {
                    carry = sp;
                }
                twap_sum += carry as u128;
            }
            let twap_fp = (twap_sum / (TWAP_SUB_SLICES as u128)) as u64;
            let is_closed = b_close <= max_ts_ns;

            candles.insert(
                (tf, b_open),
                OhlcvCandle {
                    symbol: symbol.to_string(),
                    interval: tf.proto_interval() as i32,
                    bucket_open_ts_ns: b_open,
                    bucket_close_ts_ns: b_close,
                    open_fp,
                    high_fp,
                    low_fp,
                    close_fp,
                    base_volume_fp: Some(U128::from(base_vol)),
                    quote_volume_fp: Some(U128::from(quote_vol)),
                    taker_buy_base_volume_fp: Some(U128::from(buy_vol)),
                    taker_sell_base_volume_fp: Some(U128::from(sell_vol)),
                    vwap_fp,
                    twap_fp,
                    trade_count: group.len() as u64,
                    last_engine_seq: last_seq,
                    is_closed,
                },
            );
        }
    }

    // 2. Naive session & 64-second rolling VWAP
    let mut session_raw_pq: u128 = 0;
    let mut session_qty: u128 = 0;
    let mut last_trade_px: u64 = 0;
    let max_trade_sec = fills_only
        .last()
        .map(|f| f.exec_ts_ns / 1_000_000_000)
        .unwrap_or(0);
    let mut rolling_raw_pq: u128 = 0;
    let mut rolling_qty: u128 = 0;

    for f in &fills_only {
        let q = f.qty_fp as u128;
        let pq = (f.price_fp as u128) * q;
        session_raw_pq += pq;
        session_qty += q;
        last_trade_px = f.price_fp;
        let sec = f.exec_ts_ns / 1_000_000_000;
        if max_trade_sec.saturating_sub(sec) < 64 {
            rolling_raw_pq += pq;
            rolling_qty += q;
        }
    }

    // 3. Naive L2/L3 order-book replay & Cont-Kukanov-Stoikov OFI
    let mut bids: BTreeMap<u64, u64> = BTreeMap::new();
    let mut asks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut has_prev_bbo = false;
    let mut prev_b = (0_u64, 0_u64);
    let mut prev_a = (0_u64, 0_u64);
    let mut ofi_events: Vec<(u64, i128)> = Vec::new();

    for (ts_ns, _, _, ev) in &accepted {
        match ev {
            IngestEvent::L2Snapshot(s) => {
                bids.clear();
                asks.clear();
                for l in &s.bids {
                    if l.price_fp > 0 && l.total_qty_fp > 0 {
                        bids.insert(l.price_fp, l.total_qty_fp);
                    }
                }
                for l in &s.asks {
                    if l.price_fp > 0 && l.total_qty_fp > 0 {
                        asks.insert(l.price_fp, l.total_qty_fp);
                    }
                }
                let cur_b = bids
                    .iter()
                    .next_back()
                    .map(|(&p, &q)| (p, q))
                    .unwrap_or((0, 0));
                let cur_a = asks.iter().next().map(|(&p, &q)| (p, q)).unwrap_or((0, 0));
                if !has_prev_bbo {
                    if cur_b.0 > 0 || cur_a.0 > 0 {
                        has_prev_bbo = true;
                        prev_b = cur_b;
                        prev_a = cur_a;
                    }
                } else {
                    let d = naive_bbo_ofi_step(prev_b, prev_a, cur_b, cur_a);
                    prev_b = cur_b;
                    prev_a = cur_a;
                    if d != 0 {
                        ofi_events.push((*ts_ns / 1_000_000_000, d));
                    }
                }
            }
            IngestEvent::L3Delta(d) => {
                let op = L3DeltaOp::try_from(d.op).unwrap_or(L3DeltaOp::Unspecified);
                let side = OrderSide::try_from(d.side).unwrap_or(OrderSide::Unspecified);
                if matches!(op, L3DeltaOp::Clear) {
                    bids.clear();
                    asks.clear();
                } else if d.price_fp > 0 && d.delta_qty_fp != 0 {
                    let book = match side {
                        OrderSide::Buy => &mut bids,
                        OrderSide::Sell => &mut asks,
                        OrderSide::Unspecified => continue,
                    };
                    let cur = book.get(&d.price_fp).copied().unwrap_or(0);
                    let next = if d.delta_qty_fp >= 0 {
                        cur.saturating_add(d.delta_qty_fp as u64)
                    } else {
                        cur.saturating_sub(d.delta_qty_fp.unsigned_abs())
                    };
                    if next == 0 {
                        book.remove(&d.price_fp);
                    } else {
                        book.insert(d.price_fp, next);
                    }
                }
                let cur_b = bids
                    .iter()
                    .next_back()
                    .map(|(&p, &q)| (p, q))
                    .unwrap_or((0, 0));
                let cur_a = asks.iter().next().map(|(&p, &q)| (p, q)).unwrap_or((0, 0));
                if !has_prev_bbo {
                    if cur_b.0 > 0 || cur_a.0 > 0 {
                        has_prev_bbo = true;
                        prev_b = cur_b;
                        prev_a = cur_a;
                    }
                } else {
                    let step = naive_bbo_ofi_step(prev_b, prev_a, cur_b, cur_a);
                    prev_b = cur_b;
                    prev_a = cur_a;
                    if step != 0 {
                        ofi_events.push((*ts_ns / 1_000_000_000, step));
                    }
                }
            }
            IngestEvent::Execution(f) => {
                if !has_prev_bbo && bids.is_empty() && asks.is_empty() {
                    let signed = match OrderSide::try_from(f.aggressor_side)
                        .unwrap_or(OrderSide::Unspecified)
                    {
                        OrderSide::Buy => f.qty_fp as i128,
                        OrderSide::Sell | OrderSide::Unspecified => -(f.qty_fp as i128),
                    };
                    ofi_events.push((*ts_ns / 1_000_000_000, signed));
                }
            }
        }
    }

    let cumulative_ofi_fp: i128 = ofi_events.iter().map(|&(_, d)| d).sum();
    let max_ofi_sec = ofi_events.last().map(|&(s, _)| s).unwrap_or(0);
    let rolling_ofi_fp: i128 = ofi_events
        .iter()
        .filter(|&&(s, _)| max_ofi_sec.saturating_sub(s) < 64)
        .map(|&(_, d)| d)
        .sum();

    let sum_top_bids =
        |k: usize| -> u128 { bids.iter().rev().take(k).map(|(_, &q)| q as u128).sum() };
    let sum_top_asks = |k: usize| -> u128 { asks.iter().take(k).map(|(_, &q)| q as u128).sum() };

    let imb5 = calc_imbalance_ppm(sum_top_bids(5), sum_top_asks(5));
    let imb10 = calc_imbalance_ppm(sum_top_bids(10), sum_top_asks(10));
    let imb20 = calc_imbalance_ppm(sum_top_bids(20), sum_top_asks(20));

    ReferenceSymbolSummary {
        symbol: symbol.to_string(),
        candles,
        session_vwap_fp: calc_vwap_fp(session_raw_pq, session_qty, last_trade_px),
        rolling_vwap_fp: calc_vwap_fp(rolling_raw_pq, rolling_qty, last_trade_px),
        cumulative_ofi_fp,
        rolling_ofi_fp,
        book_imbalance_top5_ppm: imb5,
        book_imbalance_top10_ppm: imb10,
        book_imbalance_top20_ppm: imb20,
        total_trades: fills_only.len() as u64,
        total_base_volume_fp: session_qty,
    }
}

fn naive_bbo_ofi_step(
    prev_b: (u64, u64),
    prev_a: (u64, u64),
    cur_b: (u64, u64),
    cur_a: (u64, u64),
) -> i128 {
    let e_bid = (if cur_b.0 > 0 && cur_b.0 >= prev_b.0 {
        cur_b.1 as i128
    } else {
        0
    }) - (if prev_b.0 > 0 && (cur_b.0 == 0 || cur_b.0 <= prev_b.0) {
        prev_b.1 as i128
    } else {
        0
    });

    let e_ask = (if cur_a.0 > 0 && (prev_a.0 == 0 || cur_a.0 <= prev_a.0) {
        cur_a.1 as i128
    } else {
        0
    }) - (if prev_a.0 > 0 && (cur_a.0 == 0 || cur_a.0 >= prev_a.0) {
        prev_a.1 as i128
    } else {
        0
    });

    e_bid - e_ask
}
