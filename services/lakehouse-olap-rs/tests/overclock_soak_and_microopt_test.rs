use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::Instant;

use arrow::array::{Array, Int64Array};
use datafusion::common::ScalarValue;

use lakehouse_olap_rs::config::FP_SCALE;
use lakehouse_olap_rs::engine::CachePaddedAtomicU64;
use lakehouse_olap_rs::math_fp::{
    compute_fill_markout_pnl_i128, evaluate_parkinson_from_q64_sum, evaluate_twap_segments,
    log2_ratio_q32, KylesLambdaState, TwapSegment,
};
use lakehouse_olap_rs::udfs::asof_join::{asof_locate_binary, asof_locate_galloping_cursor};
use lakehouse_olap_rs::udfs::markout::SortedMidSeries;
use lakehouse_olap_rs::wal_ingest::{encode_wal_segment, parse_cold_wal_segment, wire};

struct CountingAllocator;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static TOTAL_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
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
            let diff = (new_size as isize) - (layout.size() as isize);
            LIVE_BYTES.fetch_add(diff, Ordering::Relaxed);
            if new_size > layout.size() {
                TOTAL_ALLOC_BYTES.fetch_add(new_size - layout.size(), Ordering::Relaxed);
            }
            TOTAL_ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[test]
fn test_500k_consecutive_soak_zero_memory_leak_and_three_pass_micro_optimization() {
    // Verify 64-byte hardware cache-line alignment on EngineTelemetry atomic counters
    assert_eq!(std::mem::align_of::<CachePaddedAtomicU64>(), 64);
    assert_eq!(std::mem::size_of::<CachePaddedAtomicU64>(), 64);

    // =========================================================================
    // GATE 2: 500,000-CYCLE CONSECUTIVE STEADY-STATE ZERO-LEAK SOAK TEST
    // =========================================================================
    let sample_wal = encode_wal_segment(
        &[wire::MarketEventEnvelopeWire {
            engine_seq: 1,
            symbol: "BTC-USDT".into(),
            ts_ns: 1_795_000_000_000_000_000,
            payload: Some(wire::EnvelopePayloadWire::Fill(wire::ExecutionFillWire {
                fill_id: 1,
                engine_seq: 1,
                symbol: "BTC-USDT".into(),
                instrument_id: 1,
                maker_order_id: 10,
                taker_order_id: 11,
                maker_account_id: 1001,
                taker_account_id: 2002,
                aggressor_side: 1,
                price_fp: 6_850_000_000_000,
                qty_fp: 100_000_000,
                notional_fp: None,
                maker_fee_fp: -13_700,
                taker_fee_fp: 68_500,
                exec_ts_ns: 1_795_000_000_000_000_000,
                wal_blake3_hash: vec![0xAA; 32],
            })),
        }],
        [0_u8; 32],
    );

    let mid_series = SortedMidSeries {
        timestamps_ns: (0..1024).map(|i| i * 1_000_000).collect(),
        mid_prices_fp: (0..1024).map(|i| (68_500 + i) * FP_SCALE).collect(),
    };

    let mut checksum_guard: i128 = 0;
    let mut live_after_warmup: isize = 0;

    // 50,000 warmup cycles + 450,000 steady-state measurement cycles = 500,000 total cycles!
    for cycle in 0..500_000_usize {
        if cycle == 50_000 {
            live_after_warmup = LIVE_BYTES.load(Ordering::SeqCst);
        }

        // Hot-path cycle: WAL CRC32C+Blake3 parse (every 64th cycle) + TWAP + Parkinson + Kyle + Markout + Cursor ASOF
        if (cycle & 63) == 0 {
            let decoded = parse_cold_wal_segment(&sample_wal, Some([0_u8; 32])).expect("wal ok");
            checksum_guard ^= decoded.trades[0].price_fp as i128;
        }

        let p0 = (68_500 + (cycle & 15) as i64) * FP_SCALE;
        let p1 = p0 + 2 * FP_SCALE;
        let seg = TwapSegment::try_from_monotonic_slices(&[p0, p1], &[1_000, 3_000]).unwrap();
        let twap = evaluate_twap_segments(&mut [seg]).unwrap();

        let q32 = log2_ratio_q32(p1, p0).unwrap();
        let park = evaluate_parkinson_from_q64_sum((q32 as u128) * (q32 as u128), 1).unwrap();

        let mut kyle = KylesLambdaState::default();
        kyle.update_pair(FP_SCALE, FP_SCALE);
        kyle.update_pair(2 * FP_SCALE, 2 * FP_SCALE);
        let lam = kyle.evaluate_fp().unwrap();

        let mut cur = 0_usize;
        let fut_mid = mid_series
            .mid_asof_with_cursor(((cycle & 1023) as i64) * 1_000_000 + 500, &mut cur)
            .unwrap();
        let mpnl = compute_fill_markout_pnl_i128(1, p0, fut_mid, FP_SCALE);

        checksum_guard = checksum_guard
            .wrapping_add(twap as i128)
            .wrapping_add(park as i128)
            .wrapping_add(lam as i128)
            .wrapping_add(mpnl);
    }

    let live_after_500k = LIVE_BYTES.load(Ordering::SeqCst);
    let steady_state_delta = live_after_500k - live_after_warmup;
    println!(
        "[CH17 500K SOAK] live_after_warmup={live_after_warmup} B | live_after_500k={live_after_500k} B | steady_state_delta={steady_state_delta} B | guard={checksum_guard}"
    );
    assert_eq!(
        steady_state_delta, 0,
        "500,000-cycle consecutive soak MUST exhibit 0 bytes steady-state memory leak!"
    );

    // =========================================================================
    // GATE 3: 3-PASS MICRO-OPTIMIZATION BENCHMARK (65,536-ROW BATCH ALIGNMENT + TWAP)
    // =========================================================================
    let n_trades = 65_536_usize;
    let n_quotes = 16_384_usize;

    let quote_ts: Vec<i64> = (0..n_quotes).map(|q| (q as i64) * 4_000).collect();
    let quote_mid: Vec<i64> = (0..n_quotes)
        .map(|q| 6_850_000_000_000 + (q as i64) * 1_000_000)
        .collect();

    let trade_ts_arr = Int64Array::from(
        (0..n_trades)
            .map(|t| (t as i64) * 1_000 + 250)
            .collect::<Vec<i64>>(),
    );
    let trade_price_arr = Int64Array::from(
        (0..n_trades)
            .map(|t| 6_850_000_000_000 + ((t % 97) as i64) * 500_000)
            .collect::<Vec<i64>>(),
    );

    let iters = 40_usize;

    // --- PASS 1: Baseline (Row-by-row ScalarValue extraction + uncached linear/boxed scan) ---
    let mut pass1_ns = Vec::with_capacity(iters);
    let b_before_1 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    let a_before_1 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let mut ref_sum_1: i128 = 0;

    for _ in 0..iters {
        let t_start = Instant::now();
        let mut boxed_rows: Vec<(ScalarValue, ScalarValue)> = Vec::with_capacity(n_trades);
        for i in 0..n_trades {
            let sv_ts = ScalarValue::try_from_array(&trade_ts_arr, i).unwrap();
            let sv_p = ScalarValue::try_from_array(&trade_price_arr, i).unwrap();
            boxed_rows.push((sv_ts, sv_p));
        }
        let mut acc: i128 = 0;
        for (sv_ts, sv_p) in &boxed_rows {
            if let (ScalarValue::Int64(Some(ts)), ScalarValue::Int64(Some(p))) = (sv_ts, sv_p) {
                let pos = quote_ts.partition_point(|&q| q <= *ts);
                let mid = if pos > 0 { quote_mid[pos - 1] } else { quote_mid[0] };
                acc += i128::from(mid - *p);
            }
        }
        ref_sum_1 = acc;
        pass1_ns.push(t_start.elapsed().as_nanos() as u64);
    }
    let pass1_bytes_per_op = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - b_before_1) / iters;
    let pass1_allocs_per_op = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_before_1) / iters;

    // --- PASS 2: Vectorized Zero-Copy Arrow Slice + Binary Search (`asof_locate_binary`) ---
    let mut pass2_ns = Vec::with_capacity(iters);
    let b_before_2 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    let a_before_2 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let mut ref_sum_2: i128 = 0;

    for _ in 0..iters {
        let t_start = Instant::now();
        let ts_s = trade_ts_arr.values();
        let p_s = trade_price_arr.values();
        let mut acc: i128 = 0;
        if trade_ts_arr.null_count() == 0 && trade_price_arr.null_count() == 0 {
            for i in 0..n_trades {
                let mid = match asof_locate_binary(&quote_ts, ts_s[i], false) {
                    Some(idx) => quote_mid[idx],
                    None => quote_mid[0],
                };
                acc += i128::from(mid - p_s[i]);
            }
        }
        ref_sum_2 = acc;
        pass2_ns.push(t_start.elapsed().as_nanos() as u64);
    }
    let pass2_bytes_per_op = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - b_before_2) / iters;
    let pass2_allocs_per_op = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_before_2) / iters;

    // --- PASS 3: Cache-Line Aligned + L1 Monotonic Galloping Cursor (`asof_locate_galloping_cursor`) ---
    let mut pass3_ns = Vec::with_capacity(iters);
    let b_before_3 = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    let a_before_3 = TOTAL_ALLOC_CALLS.load(Ordering::SeqCst);
    let mut ref_sum_3: i128 = 0;

    for _ in 0..iters {
        let t_start = Instant::now();
        let ts_s = trade_ts_arr.values();
        let p_s = trade_price_arr.values();
        let mut cursor = 0_usize;
        let mut acc: i128 = 0;
        if trade_ts_arr.null_count() == 0 && trade_price_arr.null_count() == 0 {
            for i in 0..n_trades {
                let mid = match asof_locate_galloping_cursor(&quote_ts, ts_s[i], false, &mut cursor)
                {
                    Some(idx) => quote_mid[idx],
                    None => quote_mid[0],
                };
                acc += i128::from(mid - p_s[i]);
            }
        }
        ref_sum_3 = acc;
        pass3_ns.push(t_start.elapsed().as_nanos() as u64);
    }
    let pass3_bytes_per_op = (TOTAL_ALLOC_BYTES.load(Ordering::SeqCst) - b_before_3) / iters;
    let pass3_allocs_per_op = (TOTAL_ALLOC_CALLS.load(Ordering::SeqCst) - a_before_3) / iters;

    assert_eq!(ref_sum_1, ref_sum_2);
    assert_eq!(ref_sum_2, ref_sum_3);

    pass1_ns.sort_unstable();
    pass2_ns.sort_unstable();
    pass3_ns.sort_unstable();

    let p50_idx = iters / 2;
    let p99_idx = (iters * 99) / 100;

    println!(
        "[CH17 3-PASS MICRO-OPT (65,536 rows/batch)]\n\
         Pass 1 (Baseline ScalarValue Row-Wise) : p50 = {:>9} ns ({:>4} ns/row) | p99 = {:>9} ns | {} B/op | {} allocs/op\n\
         Pass 2 (Vectorized Slice BinarySearch) : p50 = {:>9} ns ({:>4} ns/row) | p99 = {:>9} ns | {} B/op | {} allocs/op\n\
         Pass 3 (Cache-Aligned GallopingCursor) : p50 = {:>9} ns ({:>4} ns/row) | p99 = {:>9} ns | {} B/op | {} allocs/op",
        pass1_ns[p50_idx],
        pass1_ns[p50_idx] / (n_trades as u64),
        pass1_ns[p99_idx],
        pass1_bytes_per_op,
        pass1_allocs_per_op,
        pass2_ns[p50_idx],
        pass2_ns[p50_idx] / (n_trades as u64),
        pass2_ns[p99_idx],
        pass2_bytes_per_op,
        pass2_allocs_per_op,
        pass3_ns[p50_idx],
        pass3_ns[p50_idx] / (n_trades as u64),
        pass3_ns[p99_idx],
        pass3_bytes_per_op,
        pass3_allocs_per_op,
    );

    assert_eq!(pass2_bytes_per_op, 0);
    assert_eq!(pass3_bytes_per_op, 0);
    assert!(pass2_ns[p50_idx] < pass1_ns[p50_idx]);
    assert!(pass3_ns[p50_idx] <= pass2_ns[p50_idx]);
}
