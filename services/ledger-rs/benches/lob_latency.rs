use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ledger_rs::arena_lob::{InstrumentOrderBook, OrderKind, OrderRecord, Side, StpPolicy};
use ledger_rs::bench_passes::{BaselineHeapBook, HeapOrder, Pass2MonolithicSlabBook};
use ledger_rs::engine::{
    CancelOrderCmd, DeterministicExchangeEngine, EngineSizing, SubmitOrderCmd,
};
use ledger_rs::fixed::{AssetSymbol, ClientOrderId, InstrumentSymbol, FP_SCALE_U64};
use ledger_rs::merkle::LobStateAccumulator;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

struct BenchTrackingAlloc;

thread_local! {
    static BENCH_ALLOC_CALLS: Cell<u64> = const { Cell::new(0) };
    static BENCH_ALLOC_BYTES: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for BenchTrackingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            BENCH_ALLOC_CALLS.with(|c| c.set(c.get().wrapping_add(1)));
            BENCH_ALLOC_BYTES.with(|b| b.set(b.get().wrapping_add(layout.size() as u64)));
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            BENCH_ALLOC_CALLS.with(|c| c.set(c.get().wrapping_add(1)));
            if new_size > layout.size() {
                BENCH_ALLOC_BYTES
                    .with(|b| b.set(b.get().wrapping_add((new_size - layout.size()) as u64)));
            }
        }
        p
    }
}

#[global_allocator]
static GLOBAL_BENCH_ALLOC: BenchTrackingAlloc = BenchTrackingAlloc;

fn reset_bench_allocs() {
    BENCH_ALLOC_CALLS.with(|c| c.set(0));
    BENCH_ALLOC_BYTES.with(|b| b.set(0));
}

fn read_bench_allocs() -> (u64, u64) {
    (
        BENCH_ALLOC_CALLS.with(|c| c.get()),
        BENCH_ALLOC_BYTES.with(|b| b.get()),
    )
}

fn percentiles_ns(samples: &mut [u64]) -> (u64, u64, u64, u64) {
    samples.sort_unstable();
    let n = samples.len();
    let p50 = samples[(n * 50) / 100];
    let p95 = samples[(n * 95) / 100];
    let p99 = samples[(n * 99) / 100];
    let p999 = samples[(n * 999) / 1_000];
    (p50, p95, p99, p999)
}

/// Prints exact empirical p50 / p95 / p99 / p99.9 latency (`ns/op`) and memory allocation (`B/op`, `allocs/op`)
/// tables for both the full LMAX Engine (Insertion, Cancellation, Crossing Match, 5-Level Sweep) AND the
/// 3-Pass Micro-Optimization Loop (`Baseline` vs `Pass 2 Monolithic Slab` vs `Pass 3 64B Cache-Aligned Hot/Cold Arena`)!
fn run_percentile_and_3pass_telemetry_report() {
    const N: usize = 50_000;
    println!("\n================================================================================");
    println!(" HYPERION CH02 LEDGER-RS -- EMPIRICAL LATENCY (p50/p99/p99.9) & 3-PASS OVERCLOCK ");
    println!("================================================================================");

    // --- 3-PASS MICRO-OPTIMIZATION LOOP (50,000 mixed Insert + Crossing Match ops across 256 price ticks) ---
    // Pass 1: Baseline Heap BTreeMap + VecDeque<Box<HeapOrder>> + naive full-book rehash
    {
        let mut book = BaselineHeapBook::new();
        let mut samples = Vec::with_capacity(N);
        reset_bench_allocs();
        for i in 0..N as u64 {
            let side = if (i & 1) == 0 { Side::Sell } else { Side::Buy };
            let px = if side == Side::Sell {
                (50_000 + (i % 256)) * FP_SCALE_U64
            } else {
                (50_000 + ((i + 64) % 256)) * FP_SCALE_U64
            };
            let t0 = Instant::now();
            let res = book.submit_limit(HeapOrder {
                order_id: i + 1,
                account_id: (i & 15) + 1,
                client_order_id: "ord-baseline-0001".to_string(),
                symbol: "BTC-USDT".to_string(),
                side,
                price_fp: px,
                remaining_qty_fp: 10_000_000,
            });
            black_box(res);
            samples.push(t0.elapsed().as_nanos() as u64);
        }
        let (alloc_calls, alloc_bytes) = read_bench_allocs();
        let (p50, p95, p99, p999) = percentiles_ns(&mut samples);
        println!(
            "[3-PASS | Pass 1 Baseline Heap BTreeMap]   p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
            p50,
            p95,
            p99,
            p999,
            alloc_bytes as f64 / N as f64,
            alloc_calls as f64 / N as f64
        );
    }

    // Pass 2: Monolithic Unaligned Slab + BTreeMap Level Index (BTreeMap dynamically allocates/frees level nodes)
    {
        let mut book = Pass2MonolithicSlabBook::with_capacity(N + 1024);
        let mut samples = Vec::with_capacity(N);
        reset_bench_allocs();
        for i in 0..N as u64 {
            let side = if (i & 1) == 0 { Side::Sell } else { Side::Buy };
            let px = if side == Side::Sell {
                (50_000 + (i % 256)) * FP_SCALE_U64
            } else {
                (50_000 + ((i + 64) % 256)) * FP_SCALE_U64
            };
            let t0 = Instant::now();
            let res = book.submit_limit(i + 1, (i & 15) + 1, side, px, 10_000_000);
            black_box(res);
            samples.push(t0.elapsed().as_nanos() as u64);
        }
        let (alloc_calls, alloc_bytes) = read_bench_allocs();
        let (p50, p95, p99, p999) = percentiles_ns(&mut samples);
        println!(
            "[3-PASS | Pass 2 Monolithic Slab Refactor] p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
            p50,
            p95,
            p99,
            p999,
            alloc_bytes as f64 / N as f64,
            alloc_calls as f64 / N as f64
        );
    }

    // Pass 3: 64-Byte Cache-Line Aligned Hot/Cold Arena + Intrusive AA-Tree + Threaded Levels + Single-Block LtHash
    {
        let mut book = InstrumentOrderBook::new(1, "BTC-USDT", "BTC", "USDT", N + 1024, 4096);
        let mut lob_acc = LobStateAccumulator::new();
        let cid = ClientOrderId::from_str_trunc("ord-aligned-0001");
        let mut samples = Vec::with_capacity(N);
        reset_bench_allocs();
        for i in 0..N as u64 {
            let side = if (i & 1) == 0 { Side::Sell } else { Side::Buy };
            let px = if side == Side::Sell {
                (50_000 + (i % 256)) * FP_SCALE_U64
            } else {
                (50_000 + ((i + 64) % 256)) * FP_SCALE_U64
            };
            let t0 = Instant::now();
            let mut rem = 10_000_000u64;
            let opp = side.opposite();
            while rem > 0 {
                let best = book.side_book(opp).best_level_idx();
                if best == ledger_rs::arena_lob::NULL_IDX {
                    break;
                }
                let bpx = book.side_book(opp).get(best).price_fp;
                let crosses = match side {
                    Side::Buy => bpx <= px,
                    Side::Sell => bpx >= px,
                };
                if !crosses {
                    break;
                }
                let head = book.side_book(opp).get(best).head_order;
                let m_rem = book.orders.hot(head).remaining_qty_fp;
                let step = rem.min(m_rem);
                rem -= step;
                if m_rem == step {
                    book.remove_resting_by_slot(head, &mut lob_acc);
                } else {
                    book.reduce_resting_qty_in_place(head, m_rem - step, 0, &mut lob_acc);
                }
            }
            if rem > 0 {
                let _ = book.insert_resting(
                    OrderRecord {
                        order_id: i + 1,
                        account_id: (i & 15) + 1,
                        instrument_id: 1,
                        side,
                        kind: OrderKind::Limit,
                        stp_mode: StpPolicy::None,
                        price_fp: px,
                        initial_qty_fp: 10_000_000,
                        remaining_qty_fp: rem,
                        locked_quote_fp: 0,
                        created_ts_ns: i,
                        priority_seq: i + 1,
                        client_order_id: cid,
                    },
                    &mut lob_acc,
                );
            }
            black_box(lob_acc.lanes());
            samples.push(t0.elapsed().as_nanos() as u64);
        }
        let (alloc_calls, alloc_bytes) = read_bench_allocs();
        let (p50, p95, p99, p999) = percentiles_ns(&mut samples);
        println!(
            "[3-PASS | Pass 3 64B Cache-Aligned Final]  p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
            p50,
            p95,
            p99,
            p999,
            alloc_bytes as f64 / N as f64,
            alloc_calls as f64 / N as f64
        );
    }

    // --- FULL ENGINE END-TO-END HOT PATH PERCENTILES (LOB + Double-Entry Ledger + Blake3 Merkle Root) ---
    let sizing = EngineSizing {
        max_accounts: 256,
        max_orders_per_book: 131_072,
        max_levels_per_side: 8_192,
        snapshot_every_n_events: 0,
    };
    let mut eng = DeterministicExchangeEngine::new_in_memory(sizing);
    let sym = InstrumentSymbol::from_str_trunc("BTC-USDT");
    let usdt = AssetSymbol::from_str_trunc("USDT");
    let btc = AssetSymbol::from_str_trunc("BTC");
    let cid = ClientOrderId::from_str_trunc("bench-cid");

    for acc in 1..=32u64 {
        eng.credit_deposit_fast(acc, usdt, 1_000_000_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
        eng.credit_deposit_fast(acc, btc, 10_000_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
    }

    // 1. Full Engine Order Insertion + Double-Entry Hold + Incremental Merkle Root
    let mut ins_samples = Vec::with_capacity(N);
    let mut inserted_ids = Vec::with_capacity(N);
    reset_bench_allocs();
    for i in 0..N as u64 {
        let acc = (i & 15) + 1;
        let px = (60_000 + (i & 255)) * FP_SCALE_U64;
        let t0 = Instant::now();
        let ack = eng.submit_order_fast(SubmitOrderCmd {
            client_order_id: cid,
            account_id: acc,
            instrument_id: 1,
            symbol: sym,
            side: Side::Sell,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            price_fp: px,
            qty_fp: 10_000_000,
            max_slippage_bps: 0,
            ts_ns: 0,
            idempotency_hash: None,
        });
        black_box(ack.state_root_blake3);
        ins_samples.push(t0.elapsed().as_nanos() as u64);
        inserted_ids.push((ack.order_id, acc));
    }
    let (ins_calls, ins_bytes) = read_bench_allocs();
    let (ip50, ip95, ip99, ip999) = percentiles_ns(&mut ins_samples);
    println!(
        "[FULL ENGINE | Order Insertion + Merkle]   p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
        ip50,
        ip95,
        ip99,
        ip999,
        ins_bytes as f64 / N as f64,
        ins_calls as f64 / N as f64
    );

    // 2. Full Engine O(1) Order Cancellation + Double-Entry Release + Incremental Merkle Root
    let mut can_samples = Vec::with_capacity(N);
    reset_bench_allocs();
    for &(ord_id, acc) in &inserted_ids {
        let t0 = Instant::now();
        let ack = eng.cancel_order_fast(CancelOrderCmd {
            order_id: ord_id,
            client_order_id: cid,
            account_id: acc,
            instrument_id: 1,
            symbol: sym,
            ts_ns: 0,
            idempotency_hash: None,
        });
        black_box(ack.state_root_blake3);
        can_samples.push(t0.elapsed().as_nanos() as u64);
    }
    let (can_calls, can_bytes) = read_bench_allocs();
    let (cp50, cp95, cp99, cp999) = percentiles_ns(&mut can_samples);
    println!(
        "[FULL ENGINE | Order Cancel + Merkle]      p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
        cp50,
        cp95,
        cp99,
        cp999,
        can_bytes as f64 / N as f64,
        can_calls as f64 / N as f64
    );

    // 3. Full Engine Single Crossing Match + Double-Entry Settlement + Execution MMR + Merkle Root
    let mut cross_samples = Vec::with_capacity(N);
    let mut cross_alloc_calls = 0u64;
    let mut cross_alloc_bytes = 0u64;
    for i in 0..N as u64 {
        let maker = (i & 7) + 1;
        let taker = (i & 7) + 17;
        let _ = eng.submit_order_fast(SubmitOrderCmd {
            client_order_id: cid,
            account_id: maker,
            instrument_id: 1,
            symbol: sym,
            side: Side::Sell,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            price_fp: 60_500 * FP_SCALE_U64,
            qty_fp: 10_000_000,
            max_slippage_bps: 0,
            ts_ns: 0,
            idempotency_hash: None,
        });
        reset_bench_allocs();
        let t0 = Instant::now();
        let ack = eng.submit_order_fast(SubmitOrderCmd {
            client_order_id: cid,
            account_id: taker,
            instrument_id: 1,
            symbol: sym,
            side: Side::Buy,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            price_fp: 60_500 * FP_SCALE_U64,
            qty_fp: 10_000_000,
            max_slippage_bps: 0,
            ts_ns: 0,
            idempotency_hash: None,
        });
        black_box(ack.state_root_blake3);
        cross_samples.push(t0.elapsed().as_nanos() as u64);
        let (cc, cb) = read_bench_allocs();
        cross_alloc_calls += cc;
        cross_alloc_bytes += cb;
    }
    let (mp50, mp95, mp99, mp999) = percentiles_ns(&mut cross_samples);
    println!(
        "[FULL ENGINE | Crossing Match + Settle]    p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
        mp50,
        mp95,
        mp99,
        mp999,
        cross_alloc_bytes as f64 / N as f64,
        cross_alloc_calls as f64 / N as f64
    );

    // 4. Full Engine 5-Level Multi-Level Sweep + 5 Double-Entry Trade Settlements + Merkle Root
    const SWEEP_N: usize = 10_000;
    let mut sweep_samples = Vec::with_capacity(SWEEP_N);
    let mut sweep_alloc_calls = 0u64;
    let mut sweep_alloc_bytes = 0u64;
    for s in 0..SWEEP_N as u64 {
        // Seed 5 distinct ask price levels (not timed)
        for lvl in 0..5u64 {
            eng.submit_order_fast(SubmitOrderCmd {
                client_order_id: cid,
                account_id: lvl + 1,
                instrument_id: 1,
                symbol: sym,
                side: Side::Sell,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: (61_000 + lvl) * FP_SCALE_U64,
                qty_fp: 10_000_000,
                max_slippage_bps: 0,
                ts_ns: 0,
                idempotency_hash: None,
            });
        }
        reset_bench_allocs();
        let t0 = Instant::now();
        let ack = eng.submit_order_fast(SubmitOrderCmd {
            client_order_id: cid,
            account_id: (s & 7) + 17,
            instrument_id: 1,
            symbol: sym,
            side: Side::Buy,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            price_fp: 61_010 * FP_SCALE_U64,
            qty_fp: 50_000_000, // Sweeps all 5 price levels completely!
            max_slippage_bps: 0,
            ts_ns: 0,
            idempotency_hash: None,
        });
        black_box(ack.state_root_blake3);
        sweep_samples.push(t0.elapsed().as_nanos() as u64);
        let (sc, sb) = read_bench_allocs();
        sweep_alloc_calls += sc;
        sweep_alloc_bytes += sb;
    }
    let (sp50, sp95, sp99, sp999) = percentiles_ns(&mut sweep_samples);
    println!(
        "[FULL ENGINE | 5-Level Sweep + Settlement] p50={:>6} ns | p95={:>7} ns | p99={:>7} ns | p99.9={:>7} ns | B/op={:>6.2} | allocs/op={:.2}",
        sp50,
        sp95,
        sp99,
        sp999,
        sweep_alloc_bytes as f64 / SWEEP_N as f64,
        sweep_alloc_calls as f64 / SWEEP_N as f64
    );
    println!("================================================================================\n");
}

fn criterion_benchmarks(c: &mut Criterion) {
    run_percentile_and_3pass_telemetry_report();

    let sizing = EngineSizing {
        max_accounts: 256,
        max_orders_per_book: 262_144,
        max_levels_per_side: 8_192,
        snapshot_every_n_events: 0,
    };
    let mut eng = DeterministicExchangeEngine::new_in_memory(sizing);
    let sym = InstrumentSymbol::from_str_trunc("BTC-USDT");
    let usdt = AssetSymbol::from_str_trunc("USDT");
    let btc = AssetSymbol::from_str_trunc("BTC");
    let cid = ClientOrderId::from_str_trunc("crit-cid");

    for acc in 1..=32u64 {
        eng.credit_deposit_fast(acc, usdt, 100_000_000_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
        eng.credit_deposit_fast(acc, btc, 100_000_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
    }

    let mut tick_seq = 0u64;
    c.bench_function("full_engine_insert_and_cancel_pair", |b| {
        b.iter(|| {
            tick_seq = tick_seq.wrapping_add(1);
            let acc = (tick_seq & 15) + 1;
            let ack = eng.submit_order_fast(SubmitOrderCmd {
                client_order_id: cid,
                account_id: acc,
                instrument_id: 1,
                symbol: sym,
                side: Side::Sell,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: (62_000 + (tick_seq & 127)) * FP_SCALE_U64,
                qty_fp: 10_000_000,
                max_slippage_bps: 0,
                ts_ns: 0,
                idempotency_hash: None,
            });
            let c_ack = eng.cancel_order_fast(CancelOrderCmd {
                order_id: ack.order_id,
                client_order_id: cid,
                account_id: acc,
                instrument_id: 1,
                symbol: sym,
                ts_ns: 0,
                idempotency_hash: None,
            });
            black_box(c_ack.state_root_blake3)
        });
    });

    c.bench_function("full_engine_crossing_match_settlement", |b| {
        b.iter(|| {
            tick_seq = tick_seq.wrapping_add(1);
            // Alternate maker/taker accounts so net BTC/USDT inventory remains stationary across 1M+ iterations
            let (seller, buyer) = if (tick_seq & 1) == 0 { (1, 2) } else { (2, 1) };
            let _ = eng.submit_order_fast(SubmitOrderCmd {
                client_order_id: cid,
                account_id: seller,
                instrument_id: 1,
                symbol: sym,
                side: Side::Sell,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 63_000 * FP_SCALE_U64,
                qty_fp: 10_000_000,
                max_slippage_bps: 0,
                ts_ns: 0,
                idempotency_hash: None,
            });
            let fill_ack = eng.submit_order_fast(SubmitOrderCmd {
                client_order_id: cid,
                account_id: buyer,
                instrument_id: 1,
                symbol: sym,
                side: Side::Buy,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 63_000 * FP_SCALE_U64,
                qty_fp: 10_000_000,
                max_slippage_bps: 0,
                ts_ns: 0,
                idempotency_hash: None,
            });
            black_box(fill_ack.state_root_blake3)
        });
    });

    c.bench_function("full_engine_5level_sweep_settlement", |b| {
        b.iter(|| {
            tick_seq = tick_seq.wrapping_add(1);
            let flip = (tick_seq & 1) == 0;
            for lvl in 0..5u64 {
                let maker = if flip { lvl + 1 } else { lvl + 17 };
                let _ = eng.submit_order_fast(SubmitOrderCmd {
                    client_order_id: cid,
                    account_id: maker,
                    instrument_id: 1,
                    symbol: sym,
                    side: Side::Sell,
                    kind: OrderKind::Limit,
                    stp_mode: StpPolicy::None,
                    price_fp: (64_000 + lvl) * FP_SCALE_U64,
                    qty_fp: 10_000_000,
                    max_slippage_bps: 0,
                    ts_ns: 0,
                    idempotency_hash: None,
                });
            }
            let taker = if flip { 25 } else { 10 };
            let sweep_ack = eng.submit_order_fast(SubmitOrderCmd {
                client_order_id: cid,
                account_id: taker,
                instrument_id: 1,
                symbol: sym,
                side: Side::Buy,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 64_010 * FP_SCALE_U64,
                qty_fp: 50_000_000,
                max_slippage_bps: 0,
                ts_ns: 0,
                idempotency_hash: None,
            });
            black_box(sweep_ack.state_root_blake3)
        });
    });
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
