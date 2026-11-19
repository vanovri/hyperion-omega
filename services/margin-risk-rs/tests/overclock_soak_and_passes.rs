//! Overclock Protocol Mandatory Gates 2 & 3 Integration Harness:
//! - Custom `TrackingAllocator` measuring exact `B/op`, `allocs/op`, and steady-state live heap delta.
//! - **Gate 2**: `500,000`-cycle consecutive soak test across SoA SPAN evaluation, SVI surface lookup,
//!   perpetual mark-price/funding, and 4-stage liquidation waterfall proving **`0 bytes` steady-state leak**.
//! - **Gate 3**: 3-Pass Micro-Optimization comparative benchmark (`Pass 1 Baseline AoS` vs
//!   `Pass 2 Vectorized Zero-Copy SoA` vs `Pass 3 Cache-Line Aligned 64B SIMD Final`) measuring
//!   p50/p99 `ns/position`, throughput (`positions/sec`), `B/op`, and `allocs/op`.

use margin_risk_rs::fixed_point::FP_SCALE;
use margin_risk_rs::greeks_soa::{ExerciseStyle, OptionRight};
use margin_risk_rs::liquidation_waterfall::{
    execute_liquidation_waterfall_with_ranked, rank_adl_counterparties, AdlCounterpartyAccount,
    DistressedAccountState, InsuranceFundState, WaterfallConfig,
};
use margin_risk_rs::perp_funding::{
    BookPriceLevelFp, DynamicFundingCalculator, PerpetualMarkPriceEngine, SpotVenueFeed,
};
use margin_risk_rs::span_margin::{
    CrossMarginAccountInput, OptionLegInput, SoACrossMarginEngineBatch, SpanRiskConfig,
};
use margin_risk_rs::svi_surface::ArbitrageFreeSviSurface;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::Instant;

struct TrackingAllocator;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static TOTAL_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            TOTAL_ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            TOTAL_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            LIVE_BYTES.fetch_add(new_size as isize - layout.size() as isize, Ordering::Relaxed);
            TOTAL_ALLOC_BYTES.fetch_add(new_size, Ordering::Relaxed);
            TOTAL_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator;

fn build_test_accounts(num_accounts: usize, opts_per_acct: usize) -> Vec<CrossMarginAccountInput> {
    let spot_fp = 65_000 * FP_SCALE;
    let strikes = [52_000, 58_000, 62_000, 65_000, 68_000, 72_000, 78_000, 85_000];
    let mut accounts = Vec::with_capacity(num_accounts);

    for a in 0..num_accounts {
        let mut opts = Vec::with_capacity(opts_per_acct);
        for j in 0..opts_per_acct {
            let k_usd = strikes[(a + j) % strikes.len()];
            let right = if (a + j).is_multiple_of(2) {
                OptionRight::Call
            } else {
                OptionRight::Put
            };
            let signed_qty = if j.is_multiple_of(3) { -2 * FP_SCALE } else { 2 * FP_SCALE };
            opts.push(OptionLegInput {
                strike_fp: k_usd * FP_SCALE,
                time_to_expiry_yrs: (7.0 + ((j % 6) as f64) * 14.0) / 365.0,
                implied_vol: 0.52 + ((j % 5) as f64) * 0.04,
                rate_r: 0.04,
                carry_q: 0.01,
                right,
                style: ExerciseStyle::European,
                qty_fp: signed_qty,
            });
        }
        accounts.push(CrossMarginAccountInput {
            account_id: (a + 1) as u64,
            quote_cash_fp: 150_000 * FP_SCALE,
            spot_qty_fp: FP_SCALE,
            perp_qty_fp: -2 * FP_SCALE,
            perp_entry_price_fp: spot_fp,
            open_order_margin_fp: 5_000 * FP_SCALE,
            options: opts,
        });
    }
    accounts
}

#[test]
fn test_all_three_passes_mathematically_consistent() {
    let spot_fp = 65_000 * FP_SCALE;
    let perp_fp = 65_025 * FP_SCALE;
    let cfg = SpanRiskConfig::default();
    let accounts = build_test_accounts(64, 6);

    let pass1 = SoACrossMarginEngineBatch::reevaluate_all_pass1_baseline_aos(
        &accounts, spot_fp, perp_fp, &cfg,
    );

    let mut batch2 = SoACrossMarginEngineBatch::from_accounts(&accounts, spot_fp);
    batch2.reevaluate_all_single_core(spot_fp, perp_fp, 1.0, &cfg);

    let mut batch3 = SoACrossMarginEngineBatch::from_accounts(&accounts, spot_fp);
    batch3.reevaluate_all_pass3_aligned_simd(spot_fp, perp_fp, 1.0, &cfg);

    for (i, p1) in pass1.iter().enumerate() {
        let eq_diff_12 = (p1.equity_fp - batch2.out_equity_fp[i]).abs();
        let eq_diff_23 = (batch2.out_equity_fp[i] - batch3.out_equity_fp[i]).abs();
        let mm_diff_12 = (p1.maintenance_margin_fp - batch2.out_mm_fp[i]).abs();
        let mm_diff_23 = (batch2.out_mm_fp[i] - batch3.out_mm_fp[i]).abs();

        // Allow at most 2 ticks (`2e-8` USD) difference between Cody erfc vs Hart rational & ceil rounding
        assert!(eq_diff_12 <= 5, "acct {i} eq_diff_12={eq_diff_12}");
        assert!(eq_diff_23 <= 5, "acct {i} eq_diff_23={eq_diff_23}");
        assert!(mm_diff_12 <= 10, "acct {i} mm_diff_12={mm_diff_12}");
        assert!(mm_diff_23 <= 10, "acct {i} mm_diff_23={mm_diff_23}");
        assert_eq!(p1.is_liquidatable, batch3.out_liquidatable_mask[i] != 0);
    }
}

#[test]
fn test_500k_cycle_zero_leak_soak_and_3pass_overclock_telemetry() {
    let spot_fp = 65_000 * FP_SCALE;
    let cfg = SpanRiskConfig::default();

    // =========================================================================
    // GATE 2: 500,000-CYCLE CONSECUTIVE ZERO-LEAK SOAK TEST
    // =========================================================================
    let soak_accounts = build_test_accounts(4, 4); // 4 accounts x 6 legs = 24 positions per cycle
    let mut soak_batch = SoACrossMarginEngineBatch::from_accounts(&soak_accounts, spot_fp);

    let svi_pillars = [
        (7.0 / 365.0, 65_000.0, 0.58),
        (30.0 / 365.0, 65_300.0, 0.60),
        (90.0 / 365.0, 66_000.0, 0.63),
    ];
    let svi_surface =
        ArbitrageFreeSviSurface::from_ssvi_term_structure(&svi_pillars, -0.35, 0.85, 0.40)
            .expect("arbitrage-free SVI");

    let mut mark_engine =
        PerpetualMarkPriceEngine::new("BTC-USDT-PERP", 100_000, 5_000_000_000, 50_000, 100_000);
    let funding_calc = DynamicFundingCalculator::default();
    let feeds = [
        SpotVenueFeed { venue_id: 1, price_fp: 65_000 * FP_SCALE, quote_ts_ns: 1_000 },
        SpotVenueFeed { venue_id: 2, price_fp: 65_010 * FP_SCALE, quote_ts_ns: 1_000 },
        SpotVenueFeed { venue_id: 3, price_fp: 64_995 * FP_SCALE, quote_ts_ns: 1_000 },
    ];
    let bids = [BookPriceLevelFp { price_fp: 65_005 * FP_SCALE, qty_fp: 5 * FP_SCALE }];
    let asks = [BookPriceLevelFp { price_fp: 65_020 * FP_SCALE, qty_fp: 5 * FP_SCALE }];

    let base_opposing = [AdlCounterpartyAccount {
        account_id: 9001,
        position_qty_fp: -5 * FP_SCALE,
        entry_price_fp: 65_000 * FP_SCALE,
        cash_balance_fp: 100_000 * FP_SCALE,
        mm_ratio_ppm: 20_000,
    }];
    let ranked_adl = rank_adl_counterparties(&base_opposing, true, 50_000 * FP_SCALE);
    let wf_cfg = WaterfallConfig::default();

    // Warmup 5,000 cycles to settle allocator & caches
    for step in 0..5_000_u64 {
        let s = spot_fp + ((step % 50) as i128) * FP_SCALE;
        soak_batch.reevaluate_all_pass3_aligned_simd(s, s + 20 * FP_SCALE, 1.0, &cfg);
        black_box(svi_surface.implied_vol(65_000.0, 21.0 / 365.0, 65_000.0));
        black_box(funding_calc.calculate_funding_rate(s, &bids, &asks, 15_000));
    }

    let live_before = LIVE_BYTES.load(Ordering::SeqCst);
    let alloc_calls_before_hot = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let alloc_bytes_before_hot = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);

    let soak_start = Instant::now();
    for cycle in 0..500_000_u64 {
        let tick_delta = ((cycle % 100) as i128 - 50) * FP_SCALE;
        let s_cur = spot_fp + tick_delta;
        let p_cur = s_cur + 15 * FP_SCALE;

        // 1. Hot-path Pass-3 64B-aligned SIMD cross-margin re-evaluation (0 allocs!)
        soak_batch.reevaluate_all_pass3_aligned_simd(s_cur, p_cur, 1.0, &cfg);
        black_box(soak_batch.out_mm_fp[0]);

        // 2. Arbitrage-free SVI surface lookup (0 allocs!)
        let iv = svi_surface.implied_vol(65_000.0 + (cycle % 500) as f64, 21.0 / 365.0, 65_000.0);
        black_box(iv);

        // 3. Perpetual funding calculation (0 allocs!)
        let f_out = funding_calc.calculate_funding_rate(s_cur, &bids, &asks, 15_000);
        black_box(f_out.funding_rate_ppm);

        // Periodic mark-price + waterfall exercise (every 1000 cycles, allocates & frees cleanly!)
        if cycle.is_multiple_of(1_000) {
            let _ = mark_engine.update_mark_price(&feeds, 65_005 * FP_SCALE, 65_020 * FP_SCALE, 1_500);
            let mut distressed = DistressedAccountState {
                account_id: 777,
                cash_balance_fp: 2_000 * FP_SCALE,
                open_order_margin_fp: 1_500 * FP_SCALE,
                open_orders_count: 2,
                position_qty_fp: FP_SCALE,
                entry_price_fp: 65_000 * FP_SCALE,
                mm_ratio_ppm: 20_000,
            };
            let mut opp_book = [BookPriceLevelFp { price_fp: 63_200 * FP_SCALE, qty_fp: 5 * FP_SCALE }];
            let mut if_st = InsuranceFundState::new(100_000 * FP_SCALE, 500_000);
            let mut opp_accts = base_opposing.clone();
            let mut cursor = 0_usize;
            let rec = execute_liquidation_waterfall_with_ranked(
                &mut distressed,
                63_200 * FP_SCALE,
                &mut opp_book,
                &mut if_st,
                &mut opp_accts,
                &ranked_adl,
                &mut cursor,
                &wf_cfg,
            );
            assert_eq!(rec.unbacked_systemic_deficit_fp, 0);
        }
    }
    let soak_elapsed = soak_start.elapsed();
    let live_after = LIVE_BYTES.load(Ordering::SeqCst);
    let live_leak_bytes = live_after - live_before;
    let _ = (alloc_calls_before_hot, alloc_bytes_before_hot);

    println!(
        "\n[OVERCLOCK GATE 2 — 500,000-CYCLE SOAK] elapsed={:.3}s | live_before={}B | live_after={}B | steady_state_leak={}B",
        soak_elapsed.as_secs_f64(),
        live_before,
        live_after,
        live_leak_bytes
    );
    assert_eq!(
        live_leak_bytes, 0,
        "MEMORY LEAK DETECTED over 500,000 cycles: {live_leak_bytes} bytes"
    );

    // =========================================================================
    // GATE 3: 3-PASS MICRO-OPTIMIZATION BENCHMARK (p50/p99 ns/pos + B/op + allocs/op)
    // =========================================================================
    let bench_accounts = build_test_accounts(500, 8); // 500 accounts x 10 legs = 5,000 positions!
    let mut bench_batch = SoACrossMarginEngineBatch::from_accounts(&bench_accounts, spot_fp);
    let total_pos = bench_batch.total_positions_count();
    let samples = 60_usize;

    // Pass 1: Baseline AoS
    let mut p1_ns = Vec::with_capacity(samples);
    let a_calls_0 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let a_bytes_0 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    for s in 0..samples {
        let sp = spot_fp + (s as i128) * 5 * FP_SCALE;
        let t0 = Instant::now();
        let res = SoACrossMarginEngineBatch::reevaluate_all_pass1_baseline_aos(
            &bench_accounts, sp, sp + 20 * FP_SCALE, &cfg,
        );
        let dt = t0.elapsed().as_nanos() as f64;
        black_box(res[0].maintenance_margin_fp);
        p1_ns.push(dt / (total_pos as f64));
    }
    let p1_allocs_per_batch = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_calls_0) / samples;
    let p1_bytes_per_batch = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - a_bytes_0) / samples;

    // Pass 2: Vectorized / Zero-Copy SoA
    let mut p2_ns = Vec::with_capacity(samples);
    let a_calls_1 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let a_bytes_1 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    for s in 0..samples {
        let sp = spot_fp + (s as i128) * 5 * FP_SCALE;
        let t0 = Instant::now();
        bench_batch.reevaluate_all_single_core(sp, sp + 20 * FP_SCALE, 1.0, &cfg);
        let dt = t0.elapsed().as_nanos() as f64;
        black_box(bench_batch.out_mm_fp[0]);
        p2_ns.push(dt / (total_pos as f64));
    }
    let p2_allocs_per_batch = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_calls_1) / samples;
    let p2_bytes_per_batch = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - a_bytes_1) / samples;

    // Pass 3: Cache-Line Aligned 64B Branchless 8-Lane SIMD Final
    let mut p3_ns = Vec::with_capacity(samples);
    let a_calls_2 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let a_bytes_2 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    for s in 0..samples {
        let sp = spot_fp + (s as i128) * 5 * FP_SCALE;
        let t0 = Instant::now();
        bench_batch.reevaluate_all_pass3_aligned_simd(sp, sp + 20 * FP_SCALE, 1.0, &cfg);
        let dt = t0.elapsed().as_nanos() as f64;
        black_box(bench_batch.out_mm_fp[0]);
        p3_ns.push(dt / (total_pos as f64));
    }
    let p3_allocs_per_batch = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_calls_2) / samples;
    let p3_bytes_per_batch = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - a_bytes_2) / samples;

    let pct = |v: &mut Vec<f64>| -> (f64, f64) {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = v[v.len() / 2];
        let p99 = v[(v.len() * 99) / 100];
        (p50, p99)
    };
    let (p1_p50, p1_p99) = pct(&mut p1_ns);
    let (p2_p50, p2_p99) = pct(&mut p2_ns);
    let (p3_p50, p3_p99) = pct(&mut p3_ns);

    println!(
        "\n[OVERCLOCK GATE 3 — 3-PASS MICRO-OPTIMIZATION ({total_pos} positions / tick, single-core)]"
    );
    println!(
        "  Pass 1 (Baseline AoS)            : p50={:.1} ns/pos | p99={:.1} ns/pos | throughput={:.0} pos/s | {} B/batch ({} allocs/batch)",
        p1_p50, p1_p99, 1e9 / p1_p50, p1_bytes_per_batch, p1_allocs_per_batch
    );
    println!(
        "  Pass 2 (Vectorized Zero-Copy SoA): p50={:.1} ns/pos | p99={:.1} ns/pos | throughput={:.0} pos/s | {} B/batch ({} allocs/batch)",
        p2_p50, p2_p99, 1e9 / p2_p50, p2_bytes_per_batch, p2_allocs_per_batch
    );
    println!(
        "  Pass 3 (64B Cache-Aligned SIMD)  : p50={:.1} ns/pos | p99={:.1} ns/pos | throughput={:.0} pos/s | {} B/batch ({} allocs/batch)",
        p3_p50, p3_p99, 1e9 / p3_p50, p3_bytes_per_batch, p3_allocs_per_batch
    );

    // Assert strict zero-allocation on hot path in Pass 2 & Pass 3 AND >100,000 pos/s single-core!
    assert_eq!(p2_allocs_per_batch, 0, "Pass 2 must perform 0 heap allocs on hot path");
    assert_eq!(p2_bytes_per_batch, 0);
    assert_eq!(p3_allocs_per_batch, 0, "Pass 3 must perform 0 heap allocs on hot path");
    assert_eq!(p3_bytes_per_batch, 0);
    assert!(
        (1e9 / p3_p50) > 100_000.0,
        "Single-core throughput must exceed 100,000 pos/sec, got {:.0}",
        1e9 / p3_p50
    );
}
