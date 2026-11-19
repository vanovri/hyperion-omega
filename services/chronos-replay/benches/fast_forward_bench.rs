use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use chronos_replay::pb::{OrderType, SelfTradePreventionMode, TimeTravelSeekRequest};
use chronos_replay::{
    reconstruct_pass1_baseline, reconstruct_pass2_zerocopy, reconstruct_pass3_cache_aligned,
    ChronosEngine, ExchangeState, HistoricalTimelineRecorder,
};

struct CountingAlloc;

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static CURRENT_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            CURRENT_LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT_LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn reset_alloc_counters() {
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);
}

fn read_alloc_counters() -> (u64, u64) {
    (
        ALLOC_COUNT.load(Ordering::SeqCst),
        ALLOC_BYTES.load(Ordering::SeqCst),
    )
}

fn percentile_ns(sorted: &[u64], numer: usize, denom: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * numer) / denom;
    sorted[idx]
}

fn main() {
    println!("==================================================================");
    println!("  HYPERION CH08 CHRONOS — 3-PASS MICRO-OPTIMIZATION & SEEK BENCH  ");
    println!("==================================================================");

    // 1. Build 5,000-event deterministic stream for 3-pass comparison (Pass 1 does per-event Merkle so 2,000 events is tractable)
    let mut rec_small = HistoricalTimelineRecorder::new(0, 100, 500);
    let mut ts = 1_795_000_000_000_000_000u64;
    for acc in 1..=16u64 {
        rec_small
            .record_deposit(acc, "USDT", 900_000_000_000_000, ts)
            .expect("deposit usdt");
        rec_small
            .record_deposit(acc, "BTC", 50_000_000_000, ts + 100)
            .expect("deposit btc");
        ts += 1_000;
    }
    let mut oid = 1u64;
    for step in 0..800u64 {
        let acc = (step % 16) + 1;
        let side = if step % 2 == 0 { 1u8 } else { 2u8 };
        let px = if side == 1 {
            6_800_000_000_000 + ((step % 12) * 100_000_000)
        } else {
            6_800_600_000_000 + ((step % 12) * 100_000_000)
        };
        rec_small
            .record_submit_order(
                oid,
                acc,
                "BTC-USDT",
                side,
                OrderType::Limit,
                SelfTradePreventionMode::CancelResting,
                px,
                10_000_000,
                ts,
            )
            .expect("submit");
        oid += 1;
        ts += 1_000;
        if step % 5 == 4 && oid > 10 {
            let _ = rec_small.record_cancel_order(oid - 4, "BTC-USDT", ts);
            ts += 500;
        }
    }
    let (small_store, _) = rec_small.build_store(64).expect("small store");
    let small_events = small_store.events();
    let n_small = small_events.len() as u64;
    println!("3-Pass Equivalence Stream Events: {n_small}");

    let r1 = reconstruct_pass1_baseline(small_events).expect("pass1");
    let r2 = reconstruct_pass2_zerocopy(small_events).expect("pass2");
    let mut prewarmed_state = ExchangeState::with_capacity(4096, 64);
    let _ = reconstruct_pass3_cache_aligned(&mut prewarmed_state, small_events).expect("warmup");
    // Prime dirty account slots once so steady-state Pass 3 performs 0 heap allocations
    let r3 = reconstruct_pass3_cache_aligned(&mut prewarmed_state, small_events).expect("pass3");
    assert_eq!(r1, r2, "Pass 1 vs Pass 2 state_root_blake3 mismatch!");
    assert_eq!(r2, r3, "Pass 2 vs Pass 3 state_root_blake3 mismatch!");
    println!(
        "Bit-Identical Blake3 State Root Verified Across All 3 Passes: {}",
        chronos_replay::fixed::encode_hex_lower(&r3)
    );

    // Measure Pass 1 (Baseline)
    let mut p1_samples_ns = Vec::with_capacity(7);
    reset_alloc_counters();
    for _ in 0..7 {
        let t0 = Instant::now();
        let root = reconstruct_pass1_baseline(small_events).expect("p1");
        std::hint::black_box(root);
        p1_samples_ns.push(t0.elapsed().as_nanos() as u64 / n_small);
    }
    let (p1_allocs, p1_bytes) = read_alloc_counters();
    p1_samples_ns.sort_unstable();

    // Measure Pass 2 (Zero-Copy Deferred Merkle)
    let mut p2_samples_ns = Vec::with_capacity(40);
    reset_alloc_counters();
    for _ in 0..40 {
        let t0 = Instant::now();
        let root = reconstruct_pass2_zerocopy(small_events).expect("p2");
        std::hint::black_box(root);
        p2_samples_ns.push(t0.elapsed().as_nanos() as u64 / n_small);
    }
    let (p2_allocs, p2_bytes) = read_alloc_counters();
    p2_samples_ns.sort_unstable();

    // Measure Pass 3 (Cache-Line Aligned Final Hot/Cold Split)
    // Pre-allocate Reusable StateCommitmentTree inside prewarmed_state (avoiding re-allocating 256KB tree on reset!)
    let mut p3_samples_ns = Vec::with_capacity(80);
    // Warm up once
    let _ = reconstruct_pass3_reuse(&mut prewarmed_state, small_events);
    reset_alloc_counters();
    for _ in 0..80 {
        let t0 = Instant::now();
        let root = reconstruct_pass3_reuse(&mut prewarmed_state, small_events);
        std::hint::black_box(root);
        p3_samples_ns.push(t0.elapsed().as_nanos() as u64 / n_small);
    }
    let (p3_allocs, p3_bytes) = read_alloc_counters();
    p3_samples_ns.sort_unstable();

    println!(
        "PASS 1 (Baseline)            : p50 = {:>7} ns/ev | p99 = {:>7} ns/ev | {:>8} B/ev | {:>6} allocs/run",
        percentile_ns(&p1_samples_ns, 50, 100),
        percentile_ns(&p1_samples_ns, 99, 100),
        p1_bytes / (7 * n_small),
        p1_allocs / 7
    );
    println!(
        "PASS 2 (ZeroCopy Slab)       : p50 = {:>7} ns/ev | p99 = {:>7} ns/ev | {:>8} B/ev | {:>6} allocs/run",
        percentile_ns(&p2_samples_ns, 50, 100),
        percentile_ns(&p2_samples_ns, 99, 100),
        p2_bytes / (40 * n_small),
        p2_allocs / 40
    );
    println!(
        "PASS 3 (64B Cache-Aligned)   : p50 = {:>7} ns/ev | p99 = {:>7} ns/ev | {:>8} B/ev | {:>6} allocs/run",
        percentile_ns(&p3_samples_ns, 50, 100),
        percentile_ns(&p3_samples_ns, 99, 100),
        p3_bytes / (80 * n_small),
        p3_allocs / 80
    );

    // 2. Large-Scale Fast-Forward Throughput Benchmark (100,000+ WAL Events)
    let mut rec_large = HistoricalTimelineRecorder::new(25_000, 100, 500);
    let mut ts2 = 1_795_100_000_000_000_000u64;
    for acc in 1..=32u64 {
        rec_large
            .record_deposit(acc, "USDT", 9_000_000_000_000_000, ts2)
            .expect("dep");
        rec_large
            .record_deposit(acc, "BTC", 500_000_000_000, ts2 + 10)
            .expect("dep");
        ts2 += 100;
    }
    for step in 0..35_000u64 {
        let oid2 = step + 1;
        let acc = (step % 32) + 1;
        let side = if step % 2 == 0 { 1u8 } else { 2u8 };
        let px = if side == 1 {
            6_800_000_000_000 + ((step % 25) * 100_000_000)
        } else {
            6_801_000_000_000 + ((step % 25) * 100_000_000)
        };
        rec_large
            .record_submit_order(
                oid2,
                acc,
                "BTC-USDT",
                side,
                OrderType::Limit,
                SelfTradePreventionMode::CancelResting,
                px,
                5_000_000,
                ts2,
            )
            .expect("ord");
        ts2 += 100;
    }
    let (large_store, _) = rec_large.build_store(64).expect("large store");
    let total_wal_events = large_store.events().len() as u64;
    println!("Large Benchmark Total WAL Events: {total_wal_events}");

    // Full Genesis Linear Fast-Forward (0 -> MaxSeq)
    let mut state_ff = ExchangeState::with_capacity(65_536, 128);
    let _ = reconstruct_pass3_reuse(&mut state_ff, large_store.events());

    let iters = 25u64;
    let t_ff = Instant::now();
    for _ in 0..iters {
        let root = reconstruct_pass3_reuse(&mut state_ff, large_store.events());
        std::hint::black_box(root);
    }
    let elapsed_ff = t_ff.elapsed();
    let total_processed = total_wal_events * iters;
    let ev_per_sec = (total_processed as u128 * 1_000_000_000) / elapsed_ff.as_nanos();
    println!(
        "FULL MEMORY FAST-FORWARD THROUGHPUT: {} events/sec ({} us per {} events)",
        ev_per_sec,
        elapsed_ff.as_micros() as u64 / iters,
        total_wal_events
    );
    assert!(
        ev_per_sec > 500_000,
        "Throughput {} below 500,000 events/sec target!",
        ev_per_sec
    );

    // Snapshot + Sparse Index Seek Latency (p50 / p99 across 200 seeks)
    let engine = ChronosEngine::new(large_store);
    let max_seq = engine.store().max_engine_seq();
    let mut seek_us = Vec::with_capacity(200);
    for k in 1..=200u64 {
        let target_seq = ((k * 7919) % max_seq).max(1);
        let t_s = Instant::now();
        let res = engine
            .seek(&TimeTravelSeekRequest {
                symbol: "BTC-USDT".to_string(),
                target: Some(
                    chronos_replay::pb::time_travel_seek_request::Target::TargetEngineSeq(
                        target_seq,
                    ),
                ),
                l2_depth: 20,
                include_account_balances: false,
                verify_wal_hash_chain: false,
                trace: None,
            })
            .expect("seek");
        std::hint::black_box(res.response.state_root_blake3);
        seek_us.push(t_s.elapsed().as_micros() as u64);
    }
    seek_us.sort_unstable();
    println!(
        "SNAPSHOT + SPARSE INDEX SEEK (max_seq={}): p50 = {} us | p99 = {} us",
        max_seq,
        percentile_ns(&seek_us, 50, 100),
        percentile_ns(&seek_us, 99, 100)
    );

    // 3. 500,000-Cycle Zero-Leak Soak Test (`02-extreme-overclock-protocol.md` Gate 2)
    let soak_events = &small_store.events()[..100.min(small_store.events().len())];
    // Warmup 1,000 cycles
    for _ in 0..1_000 {
        let r = reconstruct_pass3_reuse(&mut prewarmed_state, soak_events);
        std::hint::black_box(r);
    }
    let live_before = CURRENT_LIVE_BYTES.load(Ordering::SeqCst);
    reset_alloc_counters();
    // 5,000 batches * 100 events = 500,000 event-replay & slot-recycle cycles!
    for _ in 0..5_000 {
        let r = reconstruct_pass3_reuse(&mut prewarmed_state, soak_events);
        std::hint::black_box(r);
    }
    let live_after = CURRENT_LIVE_BYTES.load(Ordering::SeqCst);
    let (soak_allocs, soak_bytes) = read_alloc_counters();
    let leaked_bytes = live_after.saturating_sub(live_before);
    println!(
        "500,000-EVENT SOAK TEST      : live_before={} B | live_after={} B | steady_state_leak={} B | allocs={} | transient_bytes={}",
        live_before, live_after, leaked_bytes, soak_allocs, soak_bytes
    );
    assert_eq!(leaked_bytes, 0, "Detected steady-state memory leak!");
}

fn reconstruct_pass3_reuse(
    state: &mut ExchangeState,
    events: &[chronos_replay::IndexedWalEvent],
) -> [u8; 32] {
    chronos_replay::reconstruct_pass3_cache_aligned(state, events)
        .expect("reconstruct_pass3_cache_aligned")
}
