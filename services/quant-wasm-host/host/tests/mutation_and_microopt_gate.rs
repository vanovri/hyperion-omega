//! # Boundary-Exact Invariant Suite & 3-Pass Micro-Optimization Gate (`mutation_and_microopt_gate.rs`)
//!
//! Designed specifically to achieve **Mutation Survival Zero (`>= 95%` -> `100%` kill score)**
//! on every boundary condition (`>` vs `>=`, `<` vs `<=`, `+` vs `-`, `*` vs `/`, `%` vs `/`)
//! across `hyperion-quant-sdk`, `PreTradeRiskGate`, and `shm_ring`, plus exact 3-pass
//! `ns/op` + `B/op` + `allocs/op` measurement (`02-extreme-overclock-protocol.md` §1 & §3).

use hyperion_quant_abi::*;
use hyperion_quant_sdk::{fp, RollingWindowFp, StateReader, StateWriter};
use quant_wasm_host::engine::{NullOrderSink, QuantWasmHost, ValidatedSandboxConfig};
use quant_wasm_host::risk_gate::{PreTradeRiskGate, RiskGateOutcome, RiskPolicy};
use quant_wasm_host::shm_ring::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

struct BenchAlloc;
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for BenchAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL_BENCH_ALLOC: BenchAlloc = BenchAlloc;

fn valid_btc_slot(cid: u64, price_fp: u64, qty_fp: u64) -> EgressOrderSlot64 {
    EgressOrderSlot64 {
        client_order_id: cid,
        target_order_id: 0,
        price_fp,
        qty_fp,
        client_ts_ns: 1_000_000,
        strategy_tag: 1,
        instrument_id: 1,
        max_slippage_bps: 10,
        cmd_kind: CMD_KIND_SUBMIT_LIMIT,
        side: SIDE_BUY,
        order_type: ORDER_TYPE_LIMIT,
        stp_mode: STP_CANCEL_BOTH,
        reject_code: RISK_OK,
    }
}

#[test]
fn test_fp_math_exact_boundaries_for_mutation_kill() {
    assert_eq!(fp::isqrt_u128(0), 0);
    assert_eq!(fp::isqrt_u128(1), 1);
    assert_eq!(fp::isqrt_u128(2), 1);
    assert_eq!(fp::isqrt_u128(3), 1);
    assert_eq!(fp::isqrt_u128(4), 2);
    assert_eq!(fp::isqrt_u128(99), 9);
    assert_eq!(fp::isqrt_u128(100), 10);
    assert_eq!(fp::isqrt_u128(10_000_000_000_000_000), 100_000_000);

    // mul_fp exact scale 1e8: 2.5 * -4.0 = -10.0
    assert_eq!(fp::mul_fp(250_000_000, -400_000_000), -1_000_000_000);
    // notional_fp: 68,500.00 * 0.10 = 6,850.00
    assert_eq!(
        fp::notional_fp(6_850_000_000_000, 10_000_000),
        685_000_000_000
    );

    // ratio_ppm:
    assert_eq!(fp::ratio_ppm(100, 0), 0);
    assert_eq!(fp::ratio_ppm(1, 2), 500_000);
    assert_eq!(fp::ratio_ppm(-3, 4), -750_000);

    // tick & lot alignment boundaries:
    assert_eq!(fp::align_bid_tick(10_500_000, 1_000_000), 10_000_000);
    assert_eq!(fp::align_bid_tick(10_000_000, 1_000_000), 10_000_000);
    assert_eq!(fp::align_bid_tick(12_345, 0), 12_345);

    assert_eq!(fp::align_ask_tick(10_500_000, 1_000_000), 11_000_000);
    assert_eq!(fp::align_ask_tick(10_000_000, 1_000_000), 10_000_000);
    assert_eq!(fp::align_ask_tick(12_345, 0), 12_345);

    assert_eq!(fp::align_lot_qty(15_999, 1_000), 15_000);
    assert_eq!(fp::align_lot_qty(15_000, 1_000), 15_000);
    assert_eq!(fp::align_lot_qty(15_999, 0), 15_999);

    // ewma_fp: 0.25 * 200 + 0.75 * 100 = 125
    assert_eq!(fp::ewma_fp(100_000_000, 200_000_000, 250_000), 125_000_000);

    // RollingWindowFp exact mean, std, zscore, and snapshot roundtrip:
    let mut win = RollingWindowFp::<8>::new();
    assert!(win.is_empty());
    assert_eq!(win.mean_and_std_fp(4), (0, 0));
    win.push_with_cap(100_000_000, 4);
    assert_eq!(win.mean_and_std_fp(4), (100_000_000, 0));
    win.push_with_cap(300_000_000, 4);
    // Values [100e6, 300e6] -> mean = 200e6, std = 100e6
    assert_eq!(win.mean_and_std_fp(4), (200_000_000, 100_000_000));
    assert_eq!(win.zscore_ppm(400_000_000, 4), 2_000_000); // +2.0 sigma
    assert_eq!(win.zscore_ppm(0, 4), -2_000_000);          // -2.0 sigma

    // Eviction boundary when exceeding cap=2:
    win.push_with_cap(500_000_000, 2);
    // Window of cap 2 should now hold [300e6, 500e6] -> mean = 400e6, std = 100e6
    assert_eq!(win.mean_and_std_fp(2), (400_000_000, 100_000_000));

    let mut snap = [0u8; 256];
    let mut w = StateWriter::new(&mut snap);
    assert!(win.write_snapshot(&mut w));
    let written = w.written();
    let mut win2 = RollingWindowFp::<8>::new();
    let mut r = StateReader::new(&snap[..written]);
    assert!(win2.read_snapshot(&mut r));
    assert_eq!(win2.mean_and_std_fp(2), (400_000_000, 100_000_000));

    // CRC-32C deterministic check ("123456789" -> canonical Castagnoli 0xE3069283):
    assert_eq!(crc32c_compute(b"123456789"), 0xE306_9283);
    assert_ne!(crc32c_compute(b"123456780"), 0xE306_9283);
}

#[test]
fn test_risk_gate_every_single_boundary_condition() {
    let policy = RiskPolicy {
        max_orders_per_tick: 4,
        max_open_orders: 3,
        max_order_qty_fp: 100_000_000,            // 1.00 BTC
        max_order_notional_fp: 10_000_000_000_000, // 100,000.00 USDT
        max_abs_position_qty_fp: 200_000_000,      // 2.00 BTC
        fat_finger_collar_bps: 500,                // 5.00%
        ..RiskPolicy::default()
    };

    let mut gate = PreTradeRiskGate::new(policy, 1000);
    let mid = 6_000_000_000_000u64; // 60,000.00 USDT
    let bid = mid - 1_000_000;
    let ask = mid + 1_000_000;

    // 1. Account frozen / market halted checks:
    gate.policy.account_active = false;
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(1, bid, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_ACCOUNT_OR_REGIME_HALTED
        }
    );
    gate.policy.account_active = true;
    gate.policy.market_halted = true;
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(1, bid, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_ACCOUNT_OR_REGIME_HALTED
        }
    );
    gate.policy.market_halted = false;

    // 2. Instrument, Side, STP, OrderType checks:
    gate.begin_tick(mid, bid, ask);
    let mut bad_inst = valid_btc_slot(1, bid, 10_000_000);
    bad_inst.instrument_id = 9;
    assert_eq!(
        gate.validate_submit_limit(&bad_inst),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_INSTRUMENT_MISMATCH
        }
    );

    let mut bad_side = valid_btc_slot(2, bid, 10_000_000);
    bad_side.side = 0;
    assert_eq!(
        gate.validate_submit_limit(&bad_side),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_INVALID_SIDE_OR_STP
        }
    );

    let mut bad_stp = valid_btc_slot(3, bid, 10_000_000);
    bad_stp.stp_mode = 5;
    assert_eq!(
        gate.validate_submit_limit(&bad_stp),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_INVALID_SIDE_OR_STP
        }
    );

    let mut mkt_ord = valid_btc_slot(4, bid, 10_000_000);
    mkt_ord.order_type = ORDER_TYPE_MARKET;
    assert_eq!(
        gate.validate_submit_limit(&mkt_ord),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_MARKET_ORDER_FORBIDDEN
        }
    );

    // 5th attempt in same tick exceeds max_orders_per_tick=4 -> HardTrapOrderSpam!
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(5, bid, 10_000_000)),
        RiskGateOutcome::HardTrapOrderSpam {
            attempts: 5,
            max_allowed: 4
        }
    );

    // 3. Tick size, lot size, zero price/qty/cid:
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(0, bid, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_INVALID_PRICE_OR_QTY
        }
    );
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(1, bid + 500_000, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_TICK_SIZE_ALIGNMENT
        }
    );
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(1, bid, 10_000_500)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_LOT_SIZE_ALIGNMENT
        }
    );

    // 4. Exact fat-finger collar boundary (500 bps of 60,000.00 = +/- 3,000.00 USDT):
    gate.begin_tick(mid, bid, ask);
    let exact_collar_px = mid + 300_000_000_000; // +3,000.00 USDT = exact 500 bps -> ACCEPTED!
    let mut ioc_collar = valid_btc_slot(10, exact_collar_px, 10_000_000);
    ioc_collar.order_type = ORDER_TYPE_IOC;
    assert!(matches!(
        gate.validate_submit_limit(&ioc_collar),
        RiskGateOutcome::AcceptedSubmit { .. }
    ));

    let over_collar_px = mid + 300_600_000_000; // 501 bps -> REJECTED!
    let mut ioc_over = valid_btc_slot(11, over_collar_px, 10_000_000);
    ioc_over.order_type = ORDER_TYPE_IOC;
    assert_eq!(
        gate.validate_submit_limit(&ioc_over),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_FAT_FINGER_COLLAR
        }
    );
    gate.rollback_tick();

    // 5. POST_ONLY crossing exact boundary:
    gate.begin_tick(mid, bid, ask);
    let mut po_buy_cross = valid_btc_slot(20, ask, 10_000_000);
    po_buy_cross.order_type = ORDER_TYPE_POST_ONLY;
    assert_eq!(
        gate.validate_submit_limit(&po_buy_cross),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_POST_ONLY_WOULD_CROSS
        }
    );
    let mut po_buy_ok = valid_btc_slot(21, ask - 1_000_000, 10_000_000);
    po_buy_ok.order_type = ORDER_TYPE_POST_ONLY;
    assert!(matches!(
        gate.validate_submit_limit(&po_buy_ok),
        RiskGateOutcome::AcceptedSubmit { .. }
    ));

    let mut po_sell_cross = valid_btc_slot(22, bid, 10_000_000);
    po_sell_cross.side = SIDE_SELL;
    po_sell_cross.order_type = ORDER_TYPE_POST_ONLY;
    assert_eq!(
        gate.validate_submit_limit(&po_sell_cross),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_POST_ONLY_WOULD_CROSS
        }
    );
    gate.rollback_tick();

    // 6. Exact max_order_qty_fp, max_order_notional_fp, duplicate cid, max_open_orders, and position cap:
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(30, bid, 100_001_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_MAX_ORDER_QTY
        }
    );
    // Exact 1.00 BTC @ 60,000 = 60,000 USDT <= 100,000 USDT notional -> ACCEPTED (#1 open):
    let r1 = gate.validate_submit_limit(&valid_btc_slot(31, bid, 100_000_000));
    let RiskGateOutcome::AcceptedSubmit { order_id: oid1 } = r1 else {
        panic!("expected accept");
    };
    // Duplicate client_order_id=31 -> REJECTED:
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(31, bid, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_DUPLICATE_CLIENT_ORDER_ID
        }
    );
    let mut committed = Vec::new();
    gate.commit_tick_with(|a| committed.push(a));
    assert_eq!(committed.len(), 1);
    assert_eq!(gate.open_order_count(), 1);

    // Cross-tick duplicate client_order_id=31 against committed `open_orders` -> REJECTED!
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(31, bid, 10_000_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_DUPLICATE_CLIENT_ORDER_ID
        }
    );

    // Position delta boundary: open BUY 1.00 BTC + new BUY 1.00 BTC = exact 2.00 BTC (== max_abs_position_qty_fp) -> ACCEPTED!
    assert!(matches!(
        gate.validate_submit_limit(&valid_btc_slot(32, bid, 100_000_000)),
        RiskGateOutcome::AcceptedSubmit { .. }
    ));
    // Adding even 1 lot (0.00001 BTC) more exceeds 2.00 BTC -> REJECTED!
    assert_eq!(
        gate.validate_submit_limit(&valid_btc_slot(33, bid, 1_000)),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_MAX_POSITION_DELTA
        }
    );
    gate.commit_tick_with(|_| {});
    assert_eq!(gate.open_order_count(), 2);

    // Cancel unknown vs valid order:
    gate.begin_tick(mid, bid, ask);
    assert_eq!(
        gate.validate_cancel(999_999, 40, 2_000_000),
        RiskGateOutcome::SoftRejected {
            risk_code: RISK_ERR_UNKNOWN_OR_FOREIGN_ORDER
        }
    );
    assert_eq!(
        gate.validate_cancel(oid1, 41, 2_000_000),
        RiskGateOutcome::AcceptedCancel
    );
    gate.commit_tick_with(|_| {});
    assert_eq!(gate.open_order_count(), 1);
}

#[test]
fn test_shm_ring_exact_spatial_bounds() {
    let mem = vec![0u8; 64 + SHM_TOTAL_BYTES];
    // Unaligned pointer:
    assert!(matches!(
        validate_region_bounds(&mem, 32),
        Err(ShmBoundsError::Unaligned { .. })
    ));
    // Exact fit at offset 64:
    assert_eq!(validate_region_bounds(&mem, 64), Ok(64));
    // 1 byte short -> OutOfBounds:
    let short_mem = vec![0u8; 64 + SHM_TOTAL_BYTES - 1];
    assert!(matches!(
        validate_region_bounds(&short_mem, 64),
        Err(ShmBoundsError::OutOfBounds { .. })
    ));
    // In-bounds unaligned egress slot pointer (`12 % 8 != 0`) -> Unaligned:
    assert!(matches!(
        read_egress_order_slot(&mem, 12),
        Err(ShmBoundsError::Unaligned { ptr: 12, align: 8 })
    ));
    // u32::MAX wrap-around overflow attempt:
    assert!(matches!(
        read_egress_order_slot(&mem, u32::MAX - 7),
        Err(ShmBoundsError::Unaligned { .. } | ShmBoundsError::OutOfBounds { .. })
    ));
    assert!(matches!(
        read_egress_order_slot(&mem, 0xFFFF_FFC0),
        Err(ShmBoundsError::OutOfBounds { .. })
    ));
}

#[test]
fn test_three_pass_micro_optimization_latency_and_allocation_table() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../artifacts");
    let obi_wasm = std::fs::read(dir.join("obi_sniper.wasm")).unwrap();

    let mut host = QuantWasmHost::new(Arc::new(NullOrderSink)).unwrap();
    host.deploy_or_hot_swap(
        "opt-obi",
        "OptObi",
        99_001,
        1,
        vec!["BTC-USDT".into()],
        &obi_wasm,
        None,
        None,
        ValidatedSandboxConfig::default(),
        StrategyInitBlock::default_btc(),
        false,
        0,
    )
    .unwrap();

    let mid = 6_850_000_000_000u64;
    let make_tick = |seq: u64| HostBookL2Tick {
        engine_seq: seq,
        ts_ns: seq * 100_000,
        instrument_id: 1,
        bid_levels: 5,
        ask_levels: 5,
        best_bid_fp: mid - 1_000_000,
        best_ask_fp: mid + 1_000_000,
        mid_price_fp: mid,
        spread_fp: 2_000_000,
        ofi_fp: 10_000_000,
        book_imbalance_top5_ppm: 100_000,
        fair_ref_price_fp: mid,
        bids_price_fp: [mid - 1_000_000; 5],
        bids_qty_fp: [50_000_000; 5],
        asks_price_fp: [mid + 1_000_000; 5],
        asks_qty_fp: [50_000_000; 5],
    };

    const N: usize = 20_000;

    // Warmup:
    for i in 1..=2_000u64 {
        let t = make_tick(i);
        host.bench_pass1_baseline_tick("opt-obi", &t).unwrap();
        host.bench_pass2_zerocopy_tick("opt-obi", &t).unwrap();
        host.bench_pass3_cache_aligned_tick("opt-obi", &t).unwrap();
    }

    fn measure_pass<F: FnMut(u64)>(name: &str, mut f: F) -> (u64, u64, usize, usize) {
        let mut samples = vec![0u64; N];
        let b0 = ALLOC_BYTES.load(Ordering::SeqCst);
        let c0 = ALLOC_COUNT.load(Ordering::SeqCst);
        for (i, slot) in samples.iter_mut().enumerate() {
            let t0 = Instant::now();
            f((i + 10_000) as u64);
            *slot = t0.elapsed().as_nanos() as u64;
        }
        let b_per_op = (ALLOC_BYTES.load(Ordering::SeqCst) - b0) / N;
        let c_per_op = (ALLOC_COUNT.load(Ordering::SeqCst) - c0) / N;
        samples.sort_unstable();
        let p50 = samples[N / 2];
        let p99 = samples[(N * 99) / 100];
        println!(
            "[MICRO-OPT] {name:<34} | p50 = {p50:>6} ns | p99 = {p99:>6} ns | {b_per_op:>4} B/op | {c_per_op:>2} allocs/op"
        );
        (p50, p99, b_per_op, c_per_op)
    }

    let (p1_50, p1_99, p1_b, p1_c) = measure_pass("Pass 1: Baseline (Dyn+VecCopy)", |seq| {
        let t = make_tick(seq);
        host.bench_pass1_baseline_tick("opt-obi", &t).unwrap();
    });
    let (p2_50, p2_99, p2_b, p2_c) = measure_pass("Pass 2: Zero-Copy TypedFunc Ring", |seq| {
        let t = make_tick(seq);
        host.bench_pass2_zerocopy_tick("opt-obi", &t).unwrap();
    });
    let (p3_50, p3_99, p3_b, p3_c) = measure_pass("Pass 3: Cache-Aligned Direct Slot", |seq| {
        let t = make_tick(seq);
        host.bench_pass3_cache_aligned_tick("opt-obi", &t).unwrap();
    });

    assert!(p1_b > 0 && p1_c > 0, "Pass 1 baseline should reflect heap Vec allocations");
    assert_eq!(p2_b, 0);
    assert_eq!(p2_c, 0);
    assert_eq!(p3_b, 0);
    assert_eq!(p3_c, 0);
    assert!(p3_99 < 25_000, "Pass 3 p99 ({p3_99} ns) must be well below 25,000 ns (25 µs)");
    let _ = (p1_50, p1_99, p2_50, p2_99, p3_50);
}
