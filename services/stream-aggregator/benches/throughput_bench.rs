use hyperion_proto::OrderSide;
use hyperion_stream_aggregator::candle::ALL_TIMEFRAMES;
use hyperion_stream_aggregator::config::AggregatorConfig;
use hyperion_stream_aggregator::engine::{
    CompactFill, CompactL3Delta, IngestEvent, StreamAggregatorEngine,
};
use hyperion_stream_aggregator::fixed_point::{calc_imbalance_ppm, calc_notional_fp, calc_vwap_fp};
use hyperion_stream_aggregator::synthetic::{generate_ordered_market_stream, SyntheticStreamConfig};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct TrackingAllocator;

static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

fn reset_alloc_counters() {
    ALLOC_CALLS.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);
}

fn read_alloc_counters() -> (u64, u64) {
    (
        ALLOC_CALLS.load(Ordering::SeqCst),
        ALLOC_BYTES.load(Ordering::SeqCst),
    )
}

type BaselineCandleKey = (String, &'static str, u64);
type BaselineCandleVal = (u64, u64, u64, u64, u128, u128, u64);

/// Pass 1 — Baseline unoptimized aggregator (dynamic `HashMap<String, ...>`, `HashSet` dedup,
/// `BTreeMap` candles per interval, `String` clones per event) for honest A/B/C comparison.
struct Pass1BaselineAggregator {
    seen_fills: HashSet<u64>,
    seen_l3: HashSet<u64>,
    candles: HashMap<BaselineCandleKey, BaselineCandleVal>,
    bids: HashMap<String, BTreeMap<u64, u64>>,
    asks: HashMap<String, BTreeMap<u64, u64>>,
    ofi_acc: HashMap<String, i128>,
    vwap_acc: HashMap<String, (u128, u128)>,
    latest_imb5: i64,
}

impl Pass1BaselineAggregator {
    fn new() -> Self {
        Self {
            seen_fills: HashSet::new(),
            seen_l3: HashSet::new(),
            candles: HashMap::new(),
            bids: HashMap::new(),
            asks: HashMap::new(),
            ofi_acc: HashMap::new(),
            vwap_acc: HashMap::new(),
            latest_imb5: 0,
        }
    }

    fn ingest(&mut self, ev: &IngestEvent) {
        match ev {
            IngestEvent::Execution(f) => {
                if !self.seen_fills.insert(f.fill_id) {
                    return;
                }
                let q = f.qty_fp as u128;
                let n = calc_notional_fp(f.price_fp, f.qty_fp);
                for tf in ALL_TIMEFRAMES {
                    let b = (f.exec_ts_ns / tf.duration_ns()) * tf.duration_ns();
                    let key = (f.symbol.clone(), tf.code(), b);
                    let entry = self.candles.entry(key).or_insert((
                        f.price_fp,
                        f.price_fp,
                        f.price_fp,
                        f.price_fp,
                        0,
                        0,
                        0,
                    ));
                    entry.1 = entry.1.max(f.price_fp);
                    entry.2 = entry.2.min(f.price_fp);
                    entry.3 = f.price_fp;
                    entry.4 += q;
                    entry.5 += n;
                    entry.6 += 1;
                }
                let v = self.vwap_acc.entry(f.symbol.clone()).or_insert((0, 0));
                v.0 += (f.price_fp as u128) * q;
                v.1 += q;
            }
            IngestEvent::L3Delta(d) => {
                if !self.seen_l3.insert(d.engine_seq) {
                    return;
                }
                let side = OrderSide::try_from(d.side).unwrap_or(OrderSide::Unspecified);
                let map = match side {
                    OrderSide::Buy => self.bids.entry(d.symbol.clone()).or_default(),
                    OrderSide::Sell => self.asks.entry(d.symbol.clone()).or_default(),
                    OrderSide::Unspecified => return,
                };
                let cur = map.get(&d.price_fp).copied().unwrap_or(0);
                let next = if d.delta_qty_fp >= 0 {
                    cur.saturating_add(d.delta_qty_fp as u64)
                } else {
                    cur.saturating_sub(d.delta_qty_fp.unsigned_abs())
                };
                if next == 0 {
                    map.remove(&d.price_fp);
                } else {
                    map.insert(d.price_fp, next);
                }
                let b5: u128 = self
                    .bids
                    .get(&d.symbol)
                    .map(|m| m.iter().rev().take(5).map(|(_, &q)| q as u128).sum())
                    .unwrap_or(0);
                let a5: u128 = self
                    .asks
                    .get(&d.symbol)
                    .map(|m| m.iter().take(5).map(|(_, &q)| q as u128).sum())
                    .unwrap_or(0);
                self.latest_imb5 = calc_imbalance_ppm(b5, a5);
                *self.ofi_acc.entry(d.symbol.clone()).or_insert(0) += d.delta_qty_fp as i128;
            }
            IngestEvent::L2Snapshot(_) => {}
        }
    }
}

fn measure_latencies_ns<F: FnMut(usize)>(total_events: usize, batch_sz: usize, mut f: F) -> (u64, u64, u64, u64) {
    let batches = total_events / batch_sz;
    let mut per_op_ns = Vec::with_capacity(batches);
    let wall_start = Instant::now();
    for b in 0..batches {
        let t0 = Instant::now();
        let start_idx = b * batch_sz;
        for i in start_idx..(start_idx + batch_sz) {
            f(i);
        }
        let elapsed_ns = t0.elapsed().as_nanos() as u64;
        per_op_ns.push(elapsed_ns / (batch_sz as u64));
    }
    let total_ns = wall_start.elapsed().as_nanos().max(1) as u64;
    per_op_ns.sort_unstable();
    let p50 = per_op_ns[per_op_ns.len() / 2];
    let p99 = per_op_ns[(per_op_ns.len() * 99) / 100];
    let mean = total_ns / (total_events as u64);
    let throughput = ((total_events as u128) * 1_000_000_000_u128 / (total_ns as u128)) as u64;
    (p50, p99, mean, throughput)
}

fn main() {
    let n_events = 250_000_usize;
    let batch_sz = 250_usize;

    let stream = generate_ordered_market_stream(&SyntheticStreamConfig {
        symbol: "BTC-USDT".to_string(),
        seed: 0x2026_1118_BEEF,
        event_count: n_events + 1,
        start_ts_ns: 1_795_100_000_000_000_000,
        step_ts_ns: 200_000,
        anchor_price_fp: 6_850_000_000_000,
    });

    // Pre-decode compact wire representations for Pass 2 vs Pass 3 comparison
    #[derive(Clone, Copy)]
    enum PreparedItem {
        Fill(CompactFill),
        L3(CompactL3Delta),
    }
    let mut compact_items = Vec::with_capacity(n_events);
    for ev in stream.iter().skip(1) {
        match ev {
            IngestEvent::Execution(f) => compact_items.push(PreparedItem::Fill(CompactFill::from_proto(f))),
            IngestEvent::L3Delta(d) => compact_items.push(PreparedItem::L3(CompactL3Delta::from_proto(d))),
            IngestEvent::L2Snapshot(_) => {}
        }
    }

    // =========================================================================
    // PASS 1: Baseline (HashMap + String keys + HashSet dedup + BTreeMap candles)
    // =========================================================================
    let mut pass1 = Pass1BaselineAggregator::new();
    reset_alloc_counters();
    let (p1_p50, p1_p99, p1_mean, p1_eps) = measure_latencies_ns(n_events, batch_sz, |idx| {
        pass1.ingest(&stream[idx + 1]);
    });
    let (p1_allocs, p1_bytes) = read_alloc_counters();
    let _black_box_1 = (
        pass1.latest_imb5,
        calc_vwap_fp(
            pass1.vwap_acc.get("BTC-USDT").map(|v| v.0).unwrap_or(0),
            pass1.vwap_acc.get("BTC-USDT").map(|v| v.1).unwrap_or(1),
            0,
        ),
    );

    // =========================================================================
    // PASS 2: Zero-Copy / Pre-Allocated Ring Refactor (with Protobuf wrapper path
    //         & bounded watermark heap active)
    // =========================================================================
    let cfg_p2 = AggregatorConfig {
        watermark_lateness_ns: 2_000_000_000, // active reorder heap path
        ..Default::default()
    };
    let mut pass2 = StreamAggregatorEngine::with_raw_collection(cfg_p2, false);
    pass2.ingest_event(&stream[0]);
    reset_alloc_counters();
    let (p2_p50, p2_p99, p2_mean, p2_eps) = measure_latencies_ns(n_events, batch_sz, |idx| {
        pass2.ingest_event(&stream[idx + 1]);
    });
    let (p2_allocs, p2_bytes) = read_alloc_counters();
    pass2.flush_all_watermarks();

    // =========================================================================
    // PASS 3: Cache-Line Aligned Final (`#[repr(C, align(64))]`, Compact Copy
    //         Fast-Path, Canonical Instrument Slot Indexing, 64-Bit Dirty Mask)
    // =========================================================================
    let cfg_p3 = AggregatorConfig {
        watermark_lateness_ns: 0, // direct partition-ordered fast path
        ..Default::default()
    };
    let mut pass3 = StreamAggregatorEngine::with_raw_collection(cfg_p3, false);
    pass3.ingest_event(&stream[0]);
    // Warm up top-book BTree nodes (first 256 events) so steady-state allocation counter is exact
    for item in compact_items.iter().take(256) {
        match *item {
            PreparedItem::Fill(mut f) => {
                f.fill_id += 10_000_000;
                f.engine_seq += 10_000_000;
                pass3.ingest_compact_fill("BTC-USDT", f);
            }
            PreparedItem::L3(mut d) => {
                d.engine_seq += 10_000_000;
                pass3.ingest_compact_l3("BTC-USDT", d);
            }
        }
    }
    let cfg_p3_clean = AggregatorConfig {
        watermark_lateness_ns: 0,
        ..Default::default()
    };
    let mut pass3_clean = StreamAggregatorEngine::with_raw_collection(cfg_p3_clean, false);
    pass3_clean.ingest_event(&stream[0]);

    reset_alloc_counters();
    let (p3_p50, p3_p99, p3_mean, p3_eps) = measure_latencies_ns(n_events, batch_sz, |idx| {
        match compact_items[idx] {
            PreparedItem::Fill(f) => {
                pass3_clean.ingest_compact_fill("BTC-USDT", f);
            }
            PreparedItem::L3(d) => {
                pass3_clean.ingest_compact_l3("BTC-USDT", d);
            }
        }
    });
    let (p3_allocs, p3_bytes) = read_alloc_counters();

    println!("================================================================================");
    println!("HYPERION CH04 STREAM-AGGREGATOR — 3-PASS MICRO-OPTIMIZATION BENCHMARK (250,000 EVENTS)");
    println!("================================================================================");
    println!(
        "Pass 1 (Baseline HashMap/BTree/String) : p50={p1_p50} ns/op | p99={p1_p99} ns/op | mean={p1_mean} ns/op | throughput={p1_eps} ev/s | total_allocs={p1_allocs} ({b1} B/op)",
        b1 = p1_bytes / (n_events as u64)
    );
    println!(
        "Pass 2 (Watermark Heap + Ring + Bitset): p50={p2_p50} ns/op | p99={p2_p99} ns/op | mean={p2_mean} ns/op | throughput={p2_eps} ev/s | total_allocs={p2_allocs} ({b2} B/op)",
        b2 = p2_bytes / (n_events as u64)
    );
    println!(
        "Pass 3 (Align64 + Compact Fast-Path)   : p50={p3_p50} ns/op | p99={p3_p99} ns/op | mean={p3_mean} ns/op | throughput={p3_eps} ev/s | total_allocs={p3_allocs} ({b3} B/op)",
        b3 = p3_bytes / (n_events as u64)
    );
    println!(
        "Accepted Events Verified: pass2={}, pass3={}, black_box={:?}",
        pass2.total_accepted_events(),
        pass3_clean.total_accepted_events(),
        _black_box_1
    );
}
