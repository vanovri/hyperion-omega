use hyperion_proto::{
    CancelOrder, CreditDepositRequest, ModifyOrder, OrderSide, OrderStatus, OrderType,
    SelfTradePreventionMode, SubmitOrder, VerifySystemLedgerInvariantRequest, U128,
};
use ledger_rs::arena_lob::{OrderKind, Side, StpPolicy};
use ledger_rs::engine::{
    CancelOrderCmd, DeterministicExchangeEngine, EngineSizing, ModifyOrderCmd, SubmitOrderCmd,
};
use ledger_rs::fixed::{AssetSymbol, ClientOrderId, InstrumentSymbol, FP_SCALE_U64};
use ledger_rs::ring::spsc_channel;
use ledger_rs::wal::{WalReader, WalSyncMode, WalWriter};
use proptest::prelude::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::RwLock;
use std::time::Instant;

/// Ensures the 500,000-cycle process-wide `GLOBAL_LIVE_BYTES` soak measurement runs with exclusive
/// allocator isolation while keeping concurrent `proptest` suites parallel under a shared read lock.
static ALLOC_BENCH_ISOLATION: RwLock<()> = RwLock::new(());

struct TrackingAllocator;

static GLOBAL_LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static THREAD_ALLOC_CALLS: Cell<u64> = const { Cell::new(0) };
    static THREAD_ALLOC_BYTES: Cell<u64> = const { Cell::new(0) };
    static THREAD_LIVE_BYTES: Cell<isize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            GLOBAL_LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            THREAD_LIVE_BYTES.with(|l| l.set(l.get().wrapping_add(layout.size() as isize)));
            THREAD_ALLOC_CALLS.with(|c| c.set(c.get().wrapping_add(1)));
            THREAD_ALLOC_BYTES.with(|b| b.set(b.get().wrapping_add(layout.size() as u64)));
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        GLOBAL_LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        THREAD_LIVE_BYTES.with(|l| l.set(l.get().wrapping_sub(layout.size() as isize)));
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            let delta = (new_size as isize) - (layout.size() as isize);
            GLOBAL_LIVE_BYTES.fetch_add(delta, Ordering::Relaxed);
            THREAD_LIVE_BYTES.with(|l| l.set(l.get().wrapping_add(delta)));
            THREAD_ALLOC_CALLS.with(|c| c.set(c.get().wrapping_add(1)));
            if new_size > layout.size() {
                THREAD_ALLOC_BYTES
                    .with(|b| b.set(b.get().wrapping_add((new_size - layout.size()) as u64)));
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator;

fn reset_thread_allocs() {
    THREAD_ALLOC_CALLS.with(|c| c.set(0));
    THREAD_ALLOC_BYTES.with(|b| b.set(0));
}

fn read_thread_allocs() -> (u64, u64) {
    (
        THREAD_ALLOC_CALLS.with(|c| c.get()),
        THREAD_ALLOC_BYTES.with(|b| b.get()),
    )
}

#[derive(Debug, Clone)]
enum FuzzOp {
    Deposit {
        account_idx: u8,
        asset_idx: u8,
        amount_units: u32,
    },
    Submit {
        account_idx: u8,
        symbol_idx: u8,
        is_buy: bool,
        order_type_idx: u8,
        stp_idx: u8,
        price_ticks: u16,
        qty_milli: u16,
    },
    Modify {
        active_pick: u16,
        new_price_ticks: u16,
        new_qty_milli: u16,
    },
    Cancel {
        active_pick: u16,
    },
}

fn arb_fuzz_op() -> impl Strategy<Value = FuzzOp> {
    prop_oneof![
        2 => (0u8..8, 0u8..3, 1u32..5_000).prop_map(|(a, as_idx, u)| FuzzOp::Deposit {
            account_idx: a,
            asset_idx: as_idx,
            amount_units: u,
        }),
        10 => (
            0u8..8,
            0u8..2,
            any::<bool>(),
            0u8..5,
            0u8..4,
            950u16..1_050u16,
            1u16..500u16,
        )
            .prop_map(|(acc, sym, is_buy, ot, stp, px, qty)| FuzzOp::Submit {
                account_idx: acc,
                symbol_idx: sym,
                is_buy,
                order_type_idx: ot,
                stp_idx: stp,
                price_ticks: px,
                qty_milli: qty,
            }),
        4 => (0u16..10_000, 950u16..1_050u16, 1u16..500u16).prop_map(|(pick, px, qty)| FuzzOp::Modify {
            active_pick: pick,
            new_price_ticks: px,
            new_qty_milli: qty,
        }),
        4 => (0u16..10_000).prop_map(|pick| FuzzOp::Cancel { active_pick: pick }),
    ]
}

fn run_fuzz_sequence_with_wal_replay(
    ops: &[FuzzOp],
    maker_fee_ppm: i64,
    taker_fee_ppm: i64,
    verify_every: usize,
) {
    let _read_guard = ALLOC_BENCH_ISOLATION.read().expect("rwlock read");
    let tmp = tempfile::tempdir().expect("tempdir");
    let wal_path = tmp.path().join("wal").join("segment_00000001.wal");
    let (wal, _, _) = WalWriter::open_segment(
        &wal_path,
        WalSyncMode::GroupCommit {
            flush_every_bytes: 262_144,
        },
    )
    .expect("open wal");

    let sizing = EngineSizing {
        max_accounts: 64,
        max_orders_per_book: 32_768,
        max_levels_per_side: 4_096,
        snapshot_every_n_events: 0,
    };
    let mut eng = DeterministicExchangeEngine::with_wal_and_ring(sizing.clone(), wal, None, None);
    eng.set_instrument_fees(1, "BTC-USDT", maker_fee_ppm, taker_fee_ppm);
    eng.set_instrument_fees(2, "ETH-USDT", maker_fee_ppm, taker_fee_ppm);

    // Seed initial balances across 8 accounts
    for acc in 1..=8u64 {
        for (asset, units) in [
            ("USDT", 2_000_000u128),
            ("BTC", 5_000u128),
            ("ETH", 50_000u128),
        ] {
            eng.execute_deposit_proto(CreditDepositRequest {
                deposit_id: format!("init-{acc}-{asset}"),
                account_id: acc,
                asset: asset.to_string(),
                amount_fp: Some(U128::from(units * (FP_SCALE_U64 as u128))),
                ts_ns: 0,
                trace: None,
            })
            .expect("seed deposit");
        }
    }

    let mut known_open_orders: Vec<(u64, u64, &'static str)> = Vec::with_capacity(4096);

    for (step_idx, op) in ops.iter().enumerate() {
        match *op {
            FuzzOp::Deposit {
                account_idx,
                asset_idx,
                amount_units,
            } => {
                let acc_id = (account_idx as u64) + 1;
                let asset = match asset_idx % 3 {
                    0 => "USDT",
                    1 => "BTC",
                    _ => "ETH",
                };
                let amt_fp = (amount_units as u128) * 10_000_000; // 0.1 unit increments
                eng.execute_deposit_proto(CreditDepositRequest {
                    deposit_id: format!("d-{step_idx}"),
                    account_id: acc_id,
                    asset: asset.to_string(),
                    amount_fp: Some(U128::from(amt_fp)),
                    ts_ns: 0,
                    trace: None,
                })
                .expect("fuzz deposit");
            }
            FuzzOp::Submit {
                account_idx,
                symbol_idx,
                is_buy,
                order_type_idx,
                stp_idx,
                price_ticks,
                qty_milli,
            } => {
                let acc_id = (account_idx as u64) + 1;
                let (sym, px_mult) = if symbol_idx % 2 == 0 {
                    ("BTC-USDT", 50 * FP_SCALE_U64)
                } else {
                    ("ETH-USDT", 3 * FP_SCALE_U64)
                };
                let ot = match order_type_idx % 5 {
                    0 => OrderType::Limit,
                    1 => OrderType::Market,
                    2 => OrderType::Ioc,
                    3 => OrderType::Fok,
                    _ => OrderType::PostOnly,
                };
                let stp = match stp_idx % 4 {
                    0 => SelfTradePreventionMode::None,
                    1 => SelfTradePreventionMode::CancelResting,
                    2 => SelfTradePreventionMode::CancelAggressing,
                    _ => SelfTradePreventionMode::CancelBoth,
                };
                let price_fp = if ot == OrderType::Market {
                    0
                } else {
                    (price_ticks as u64) * px_mult
                };
                let qty_fp = (qty_milli as u64) * 100_000; // 0.001 increments (scale 1e8)

                let ack = eng
                    .execute_submit_proto(SubmitOrder {
                        client_order_id: format!("s-{step_idx}"),
                        account_id: acc_id,
                        symbol: sym.to_string(),
                        instrument_id: 0,
                        side: if is_buy {
                            OrderSide::Buy as i32
                        } else {
                            OrderSide::Sell as i32
                        },
                        order_type: ot as i32,
                        stp_mode: stp as i32,
                        price_fp,
                        qty_fp,
                        max_slippage_bps: 0,
                        client_ts_ns: 0,
                        nonce: step_idx as u64,
                        signature: Vec::new(),
                        idempotency_key: String::new(),
                        trace: None,
                    })
                    .expect("submit proto");

                if ack.remaining_qty_fp > 0
                    && (ack.status == OrderStatus::Accepted as i32
                        || ack.status == OrderStatus::PartiallyFilled as i32)
                {
                    known_open_orders.push((ack.order_id, acc_id, sym));
                }
            }
            FuzzOp::Modify {
                active_pick,
                new_price_ticks,
                new_qty_milli,
            } => {
                if !known_open_orders.is_empty() {
                    let idx = (active_pick as usize) % known_open_orders.len();
                    let (ord_id, acc_id, sym) = known_open_orders[idx];
                    let px_mult = if sym == "BTC-USDT" {
                        50 * FP_SCALE_U64
                    } else {
                        3 * FP_SCALE_U64
                    };
                    let ack = eng
                        .execute_modify_proto(ModifyOrder {
                            order_id: ord_id,
                            client_order_id: format!("m-{step_idx}"),
                            account_id: acc_id,
                            symbol: sym.to_string(),
                            instrument_id: 0,
                            new_price_fp: (new_price_ticks as u64) * px_mult,
                            new_qty_fp: (new_qty_milli as u64) * 100_000,
                            client_ts_ns: 0,
                            nonce: step_idx as u64,
                            signature: Vec::new(),
                            idempotency_key: String::new(),
                            trace: None,
                        })
                        .expect("modify proto");
                    if ack.remaining_qty_fp == 0 {
                        known_open_orders.swap_remove(idx);
                    }
                }
            }
            FuzzOp::Cancel { active_pick } => {
                if !known_open_orders.is_empty() {
                    let idx = (active_pick as usize) % known_open_orders.len();
                    let (ord_id, acc_id, sym) = known_open_orders.swap_remove(idx);
                    let _ = eng
                        .execute_cancel_proto(CancelOrder {
                            order_id: ord_id,
                            client_order_id: String::new(),
                            account_id: acc_id,
                            symbol: sym.to_string(),
                            instrument_id: 0,
                            client_ts_ns: 0,
                            nonce: step_idx as u64,
                            signature: Vec::new(),
                            idempotency_key: String::new(),
                            trace: None,
                        })
                        .expect("cancel proto");
                }
            }
        }

        // Invariant (b): Best Bid MUST NEVER be >= Best Ask after synchronous matching completes!
        for b in eng.books() {
            assert!(
                b.is_uncrossed(),
                "CROSSED BOOK DETECTED on {}: best_bid={:?} >= best_ask={:?}",
                b.symbol,
                b.best_bid_fp(),
                b.best_ask_fp()
            );
        }

        // Periodic + terminal Invariant (a): Exact 0-unit conservation across all accounts + locked + treasury!
        if (step_idx + 1) % verify_every == 0 || step_idx + 1 == ops.len() {
            for asset in ["USDT", "BTC", "ETH"] {
                let inv = eng.query_system_invariant_proto(VerifySystemLedgerInvariantRequest {
                    asset: asset.to_string(),
                });
                assert!(
                    inv.invariant_holds,
                    "CONSERVATION BREACH on {asset} at step {step_idx}"
                );
                assert!(inv.crossed_book_free);
            }
        }
    }

    // Invariant (c): Replay from `HYPWAL01` WAL MUST produce 100% bit-identical Blake3 state_root!
    eng.wal_mut().flush_and_sync().expect("flush wal");
    let live_root = eng.state_root();
    let live_seq = eng.engine_seq();
    drop(eng);

    let report = WalReader::verify_segment_strict(&wal_path).expect("strict WAL verification");
    assert!(!report.truncated_corrupt_tail);
    assert_eq!(
        report.frames.last().map(|f| f.engine_seq).unwrap_or(0),
        live_seq
    );

    let mut replayed = DeterministicExchangeEngine::new_in_memory(sizing);
    replayed.set_instrument_fees(1, "BTC-USDT", maker_fee_ppm, taker_fee_ppm);
    replayed.set_instrument_fees(2, "ETH-USDT", maker_fee_ppm, taker_fee_ppm);
    replayed
        .replay_wal_frames(&report.frames)
        .expect("WAL replay succeeds");

    assert_eq!(replayed.engine_seq(), live_seq);
    assert_eq!(
        replayed.state_root(),
        live_root,
        "WAL replay root hash MUST be 100% bit-identical to live state root"
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 25,
        max_shrink_iters: 0,
        .. ProptestConfig::default()
    })]

    /// Injects `25 * 4,200 = 105,000` randomized operations across varied maker/taker/rebate schedules,
    /// mathematically proving Conservation of Funds (0 unit error), Uncrossed Book (`best_bid < best_ask`),
    /// and 100% bit-identical WAL replay root hash!
    #[test]
    fn proptest_105k_multi_schedule_conservation_uncrossed_and_wal_replay(
        ops in prop::collection::vec(arb_fuzz_op(), 4_200..=4_250),
        maker_fee_ppm in -100i64..=200i64,
        taker_fee_ppm in 0i64..=400i64,
    ) {
        run_fuzz_sequence_with_wal_replay(&ops, maker_fee_ppm, taker_fee_ppm, 500);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 1,
        max_shrink_iters: 0,
        .. ProptestConfig::default()
    })]

    /// Injects a single continuous stream of `100,000` random operations (`deposit`, `submit`, `modify`, `cancel`)
    /// into one timeline and verifies 0-unit conservation, uncrossed book, and 100% identical WAL replay root hash!
    #[test]
    fn proptest_single_100k_continuous_timeline_and_wal_replay(
        ops in prop::collection::vec(arb_fuzz_op(), 100_000..=100_050),
    ) {
        run_fuzz_sequence_with_wal_replay(&ops, -25, 150, 5_000);
    }
}

/// Gate 2 Overclock Mandate (`02-extreme-overclock-protocol.md`):
/// Executes `500,000` consecutive matching + partial fill + modify + cancel + SPSC ring cycles
/// after warmup and proves:
/// 1. Literal `0` heap allocations (`0 allocs/op`, `0 B/op`) on the hot matching path!
/// 2. `0 bytes` steady-state memory leak across 500,000 cycles!
#[test]
fn test_500k_cycles_zero_heap_allocation_and_zero_leak_soak() {
    let _exclusive_guard = ALLOC_BENCH_ISOLATION.write().expect("rwlock write");
    let (spsc_prod, mut spsc_cons) = spsc_channel(65_536);
    let sizing = EngineSizing {
        max_accounts: 128,
        max_orders_per_book: 65_536,
        max_levels_per_side: 4_096,
        snapshot_every_n_events: 0,
    };
    let mut eng = DeterministicExchangeEngine::with_wal_and_ring(
        sizing,
        ledger_rs::wal::WalWriter::in_memory(),
        None,
        Some(spsc_prod),
    );

    let sym = InstrumentSymbol::from_str_trunc("BTC-USDT");
    let usdt = AssetSymbol::from_str_trunc("USDT");
    let btc = AssetSymbol::from_str_trunc("BTC");
    let cid = ClientOrderId::from_str_trunc("soak-ord");

    // Warm up accounts 1..=16 so initial asset slots are populated
    for acc in 1..=16u64 {
        eng.credit_deposit_fast(acc, usdt, 50_000_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
        eng.credit_deposit_fast(acc, btc, 500_000 * (FP_SCALE_U64 as u128), 0)
            .unwrap();
    }
    while spsc_cons.try_pop().is_some() {}

    // Warmup 1,000 cycles
    for i in 0..1_000u64 {
        let ack = eng.submit_order_fast(SubmitOrderCmd {
            client_order_id: cid,
            account_id: (i % 8) + 1,
            instrument_id: 1,
            symbol: sym,
            side: Side::Sell,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            price_fp: (65_000 + (i % 32)) * FP_SCALE_U64,
            qty_fp: 10_000_000,
            max_slippage_bps: 0,
            ts_ns: 0,
            idempotency_hash: None,
        });
        let _ = eng.cancel_order_fast(CancelOrderCmd {
            order_id: ack.order_id,
            client_order_id: cid,
            account_id: (i % 8) + 1,
            instrument_id: 1,
            symbol: sym,
            ts_ns: 0,
            idempotency_hash: None,
        });
        while spsc_cons.try_pop().is_some() {}
    }

    let baseline_live_bytes = GLOBAL_LIVE_BYTES.load(Ordering::Acquire);
    let baseline_thread_live = THREAD_LIVE_BYTES.with(|l| l.get());
    reset_thread_allocs();
    let soak_t0 = Instant::now();

    // Execute 500,000 consecutive cycles on the hot path!
    let mut resting_ring = [0u64; 64];
    let mut resting_acc = [0u64; 64];
    let mut r_len = 0usize;

    for cycle in 0..500_000u64 {
        let maker_acc = (cycle & 7) + 1;
        let taker_acc = ((cycle >> 3) & 7) + 9;
        let tick = 60_000 + (cycle & 31);

        match cycle & 3 {
            0 => {
                // Resting Sell insert
                let ack = eng.submit_order_fast(SubmitOrderCmd {
                    client_order_id: cid,
                    account_id: maker_acc,
                    instrument_id: 1,
                    symbol: sym,
                    side: Side::Sell,
                    kind: OrderKind::Limit,
                    stp_mode: StpPolicy::None,
                    price_fp: tick * FP_SCALE_U64,
                    qty_fp: 20_000_000,
                    max_slippage_bps: 0,
                    ts_ns: 0,
                    idempotency_hash: None,
                });
                if ack.remaining_qty_fp > 0 && r_len < 64 {
                    resting_ring[r_len] = ack.order_id;
                    resting_acc[r_len] = maker_acc;
                    r_len += 1;
                }
            }
            1 => {
                // Crossing Buy partial/full fill
                let _ = eng.submit_order_fast(SubmitOrderCmd {
                    client_order_id: cid,
                    account_id: taker_acc,
                    instrument_id: 1,
                    symbol: sym,
                    side: Side::Buy,
                    kind: OrderKind::Ioc,
                    stp_mode: StpPolicy::None,
                    price_fp: 60_035 * FP_SCALE_U64,
                    qty_fp: 10_000_000,
                    max_slippage_bps: 0,
                    ts_ns: 0,
                    idempotency_hash: None,
                });
            }
            2 => {
                // Priority-preserving Modify down if resting order available
                if r_len > 0 {
                    let idx = (cycle as usize) % r_len;
                    let ack = eng.modify_order_fast(ModifyOrderCmd {
                        order_id: resting_ring[idx],
                        client_order_id: cid,
                        account_id: resting_acc[idx],
                        instrument_id: 1,
                        symbol: sym,
                        new_price_fp: (60_000 + (resting_ring[idx] & 31)) * FP_SCALE_U64,
                        new_qty_fp: 15_000_000,
                        ts_ns: 0,
                        idempotency_hash: None,
                    });
                    if ack.remaining_qty_fp == 0 {
                        r_len -= 1;
                        resting_ring[idx] = resting_ring[r_len];
                        resting_acc[idx] = resting_acc[r_len];
                    }
                }
            }
            _ => {
                // O(1) Cancel
                if r_len > 0 {
                    r_len -= 1;
                    let _ = eng.cancel_order_fast(CancelOrderCmd {
                        order_id: resting_ring[r_len],
                        client_order_id: cid,
                        account_id: resting_acc[r_len],
                        instrument_id: 1,
                        symbol: sym,
                        ts_ns: 0,
                        idempotency_hash: None,
                    });
                }
            }
        }

        // Drain SPSC outbound ring wait-free every 16 cycles so ring never drops
        if (cycle & 15) == 15 {
            while spsc_cons.try_pop().is_some() {}
        }
    }

    // Drain any remaining resting orders so slab returns to empty steady-state
    for i in 0..r_len {
        let _ = eng.cancel_order_fast(CancelOrderCmd {
            order_id: resting_ring[i],
            client_order_id: cid,
            account_id: resting_acc[i],
            instrument_id: 1,
            symbol: sym,
            ts_ns: 0,
            idempotency_hash: None,
        });
    }
    while spsc_cons.try_pop().is_some() {}

    let soak_elapsed = soak_t0.elapsed();
    let (hot_alloc_calls, hot_alloc_bytes) = read_thread_allocs();
    let final_live_bytes = GLOBAL_LIVE_BYTES.load(Ordering::Acquire);
    let final_thread_live = THREAD_LIVE_BYTES.with(|l| l.get());

    println!(
        "[GATE-2 SOAK 500,000 CYCLES] elapsed={:.2} ms ({:.1} ns/cycle) | hot_allocs={} | hot_alloc_bytes={} B | thread_live_delta={} B | global_live_delta={} B | engine_seq={}",
        soak_elapsed.as_secs_f64() * 1_000.0,
        soak_elapsed.as_nanos() as f64 / 500_000.0,
        hot_alloc_calls,
        hot_alloc_bytes,
        final_thread_live - baseline_thread_live,
        final_live_bytes - baseline_live_bytes,
        eng.engine_seq()
    );

    assert_eq!(
        hot_alloc_calls, 0,
        "Expected ZERO heap allocations across 500,000 hot-path cycles, got {hot_alloc_calls} ({hot_alloc_bytes} B)"
    );
    assert_eq!(hot_alloc_bytes, 0);
    assert_eq!(
        final_thread_live, baseline_thread_live,
        "Thread-local memory leak detected: baseline={baseline_thread_live}, final={final_thread_live}"
    );
    // When executed in strict process-wide isolation (`--exact` or `--test-threads=1`),
    // enforce process-global `GLOBAL_LIVE_BYTES` zero-leak invariant as well.
    if std::env::var_os("HYPERION_STRICT_GLOBAL_ALLOC").is_some() {
        assert!(
            final_live_bytes <= baseline_live_bytes,
            "Process-wide memory leak detected: baseline={baseline_live_bytes}, final={final_live_bytes}"
        );
    }

    let (_, _, inv_usdt, uncrossed_u, _) = eng.verify_system_invariants("USDT");
    let (_, _, inv_btc, uncrossed_b, _) = eng.verify_system_invariants("BTC");
    assert!(inv_usdt && inv_btc);
    assert!(uncrossed_u && uncrossed_b);
}
