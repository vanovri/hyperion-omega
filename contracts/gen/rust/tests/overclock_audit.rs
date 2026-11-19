use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use hyperion_proto::{
    checked_fee_fp, checked_notional_fp, compute_notional_batch_aligned8,
    compute_notional_batch_baseline, compute_notional_batch_soa_zero_copy, fast_notional_fp,
    AlignedNotionalInput8, AlignedNotionalOutput8, NotionalOrderRow, I128, U128,
};

struct CountingAlloc;

static ALLOC_TEST_LOCK: Mutex<()> = Mutex::new(());
static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static DEALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static TRACKING_THREAD: Cell<bool> = const { Cell::new(false) };
}

fn set_tracking(enabled: bool) {
    TRACKING_THREAD.with(|flag| flag.set(enabled));
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING_THREAD.try_with(|flag| flag.get()).unwrap_or(false) {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACKING_THREAD.try_with(|flag| flag.get()).unwrap_or(false) {
            DEALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn reset_alloc_counters() {
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);
    DEALLOC_BYTES.store(0, Ordering::SeqCst);
}

#[test]
fn overclock_soak_500k_zero_leak_and_zero_alloc() {
    let _guard = ALLOC_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const WARMUP: u64 = 10_000;
    const SOAK_CYCLES: u64 = 500_000;

    let mut sink: u128 = 0;
    let mut aligned_out = AlignedNotionalOutput8::default();
    let aligned_in = AlignedNotionalInput8 {
        prices_fp: [
            6_842_550_000_000,
            350_120_000_000,
            18_450_000_000,
            2_450_000_000,
            6_843_100_000_000,
            350_990_000_000,
            18_500_000_000,
            2_490_000_000,
        ],
        qtys_fp: [
            125_000_000,
            500_000_000,
            1_250_000_000,
            10_000_000_000,
            75_000_000,
            250_000_000,
            3_000_000_000,
            5_000_000_000,
        ],
    };

    for i in 0..WARMUP {
        let n = fast_notional_fp(6_842_550_000_000 + (i & 0xFF), 125_000_000 + (i & 0x7F));
        sink ^= n;
    }

    reset_alloc_counters();
    set_tracking(true);

    for i in 0..SOAK_CYCLES {
        let p = 6_842_550_000_000 + (i & 0xFFFF);
        let q = 125_000_000 + (i & 0x3FFF);
        let n = fast_notional_fp(p, q);
        let fee = checked_fee_fp(n, -20).expect("fee");
        let pb_u = U128::from(n);
        let back_u = u128::from(pb_u);
        let pb_i = I128::from(fee as i128);
        let back_i = i128::from(pb_i);
        compute_notional_batch_aligned8(&aligned_in, &mut aligned_out);
        sink ^= back_u ^ (back_i as u128) ^ aligned_out.notionals_fp[(i & 7) as usize];
    }

    set_tracking(false);
    let allocs = ALLOC_COUNT.load(Ordering::SeqCst);
    let alloc_bytes = ALLOC_BYTES.load(Ordering::SeqCst);
    let dealloc_bytes = DEALLOC_BYTES.load(Ordering::SeqCst);
    let net_leak = alloc_bytes.saturating_sub(dealloc_bytes);

    black_box(sink);
    println!(
        "[RUST 500K SOAK] cycles={SOAK_CYCLES} allocs={allocs} alloc_bytes={alloc_bytes} dealloc_bytes={dealloc_bytes} net_leak_bytes={net_leak}"
    );
    assert_eq!(allocs, 0, "expected 0 heap allocations on hot-path soak");
    assert_eq!(net_leak, 0, "expected 0 bytes memory leak across 500,000 cycles");
}

#[test]
fn overclock_micro_opt_3pass_benchmark_report() {
    let _guard = ALLOC_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const ITERS: usize = 200_000;
    let prices = [
        6_842_550_000_000_u64,
        350_120_000_000,
        18_450_000_000,
        2_450_000_000,
        6_843_100_000_000,
        350_990_000_000,
        18_500_000_000,
        2_490_000_000,
    ];
    let qtys = [
        125_000_000_u64,
        500_000_000,
        1_250_000_000,
        10_000_000_000,
        75_000_000,
        250_000_000,
        3_000_000_000,
        5_000_000_000,
    ];
    let rows: Vec<NotionalOrderRow> = (0..8)
        .map(|i| NotionalOrderRow {
            price_fp: prices[i],
            qty_fp: qtys[i],
        })
        .collect();

    // Verify mathematical equivalence against canonical checked_notional_fp
    for i in 0..8 {
        assert_eq!(
            fast_notional_fp(prices[i], qtys[i]),
            checked_notional_fp(prices[i], qtys[i]).unwrap()
        );
    }

    // Pass 1: Baseline (AoS + Heap Vec + generic u128 division)
    reset_alloc_counters();
    set_tracking(true);
    let t1 = Instant::now();
    let mut sink1: u128 = 0;
    for _ in 0..ITERS {
        let out = compute_notional_batch_baseline(black_box(&rows));
        sink1 ^= out[0] ^ out[7];
    }
    let dt1 = t1.elapsed().as_nanos();
    set_tracking(false);
    let allocs1 = ALLOC_COUNT.load(Ordering::SeqCst);
    let bytes1 = ALLOC_BYTES.load(Ordering::SeqCst);
    black_box(sink1);

    // Pass 2: Vectorized / Zero-Copy SoA Refactor
    let mut out_soa = [0_u128; 8];
    reset_alloc_counters();
    set_tracking(true);
    let t2 = Instant::now();
    let mut sink2: u128 = 0;
    for _ in 0..ITERS {
        let n = compute_notional_batch_soa_zero_copy(
            black_box(&prices),
            black_box(&qtys),
            black_box(&mut out_soa),
        );
        sink2 ^= out_soa[0] ^ out_soa[n - 1];
    }
    let dt2 = t2.elapsed().as_nanos();
    set_tracking(false);
    let allocs2 = ALLOC_COUNT.load(Ordering::SeqCst);
    let bytes2 = ALLOC_BYTES.load(Ordering::SeqCst);
    black_box(sink2);

    // Pass 3: Cache-Line Aligned (align(64)) 8-Lane Unrolled Final
    let aligned_in = AlignedNotionalInput8 {
        prices_fp: prices,
        qtys_fp: qtys,
    };
    let mut aligned_out = AlignedNotionalOutput8::default();
    reset_alloc_counters();
    set_tracking(true);
    let t3 = Instant::now();
    let mut sink3: u128 = 0;
    for _ in 0..ITERS {
        compute_notional_batch_aligned8(black_box(&aligned_in), black_box(&mut aligned_out));
        sink3 ^= aligned_out.notionals_fp[0] ^ aligned_out.notionals_fp[7];
    }
    let dt3 = t3.elapsed().as_nanos();
    set_tracking(false);
    let allocs3 = ALLOC_COUNT.load(Ordering::SeqCst);
    let bytes3 = ALLOC_BYTES.load(Ordering::SeqCst);
    black_box(sink3);

    println!(
        "[RUST 3-PASS BENCH] iters={ITERS} (8 orders/batch):\n  Pass1_Baseline_AoS_Heap:  {:.2} ns/batch ({:.2} ns/order) | allocs/batch={:.2} | B/batch={:.1}\n  Pass2_ZeroCopy_SoA_Fast:  {:.2} ns/batch ({:.2} ns/order) | allocs/batch={:.2} | B/batch={:.1}\n  Pass3_Aligned64_Unrolled: {:.2} ns/batch ({:.2} ns/order) | allocs/batch={:.2} | B/batch={:.1}",
        dt1 as f64 / ITERS as f64,
        dt1 as f64 / (ITERS * 8) as f64,
        allocs1 as f64 / ITERS as f64,
        bytes1 as f64 / ITERS as f64,
        dt2 as f64 / ITERS as f64,
        dt2 as f64 / (ITERS * 8) as f64,
        allocs2 as f64 / ITERS as f64,
        bytes2 as f64 / ITERS as f64,
        dt3 as f64 / ITERS as f64,
        dt3 as f64 / (ITERS * 8) as f64,
        allocs3 as f64 / ITERS as f64,
        bytes3 as f64 / ITERS as f64,
    );
    assert_eq!(allocs2, 0);
    assert_eq!(allocs3, 0);
}
