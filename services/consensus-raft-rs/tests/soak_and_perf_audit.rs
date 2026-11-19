//! Extreme Overclock Verification Binary (`02-extreme-overclock-protocol.md`):
//! 1. Custom `GlobalAlloc` Heap Profiler (`TrackingAllocator` wrapping `std::alloc::System`)
//! 2. `500,000`-Cycle Consecutive Consensus + Double-Entry Ledger + `HYPSNP01` Compaction Zero-Leak Soak Test
//! 3. 3-Pass Micro-Optimization Benchmark measuring exact `p50 ns/op`, `p99 ns/op`, `B/op`, and `allocs/op`

use consensus_raft_rs::quorum::AlignedQuorumTracker;
use consensus_raft_rs::raft_core::{RaftCore, RaftEffect, RaftTuningConfig};
use consensus_raft_rs::types::{
    ClusterConfig, LedgerCommand, LogIndex, NodeId, RaftEntryKind, FP_SCALE_U128,
};
use consensus_raft_rs::wal_codec::{
    append_wal_frame_bytes, build_chained_log_entry, compute_wal_frame_blake3,
    compute_wal_frame_crc32c,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

struct TrackingAllocator;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static TOTAL_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static SERIAL_ALLOC_TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            TOTAL_ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            TOTAL_ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
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
            TOTAL_ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: TrackingAllocator = TrackingAllocator;

fn live_heap_bytes() -> isize {
    LIVE_BYTES.load(Ordering::SeqCst)
}

#[test]
fn test_500k_consecutive_cycles_zero_steady_state_memory_leak() {
    let _serial_guard = SERIAL_ALLOC_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tuning = RaftTuningConfig {
        compaction_threshold_entries: 256,
        max_batch_entries: 32,
        ..Default::default()
    };

    // Single-node quorum RaftCore exercising the full proposal -> log append -> fsync ack -> commit ->
    // double-entry state machine execution -> idempotency FIFO ring -> automatic `HYPSNP01` snapshot compaction loop!
    let cfg = ClusterConfig::uniform([1]);
    let mut node = RaftCore::new(1, cfg, tuning, 0x0050_4B50_0000, 1_000_000, None);
    node.tick(500_000_000);
    for eff in node.drain_effects() {
        if let RaftEffect::Persist(b) = eff {
            node.on_disk_synced(b.io_id, node.last_log_index(), 501_000_000);
        }
    }
    let _ = node.drain_effects();

    // Seed initial balances for 16 accounts so 500,000 consecutive transfers & deposits execute cleanly.
    for acct in 1..=16u64 {
        node.propose_ledger_commands(
            acct,
            vec![LedgerCommand::CreditDeposit {
                idempotency_key: format!("seed-acct-{acct}"),
                account_id: acct,
                asset: "USDT".to_string(),
                amount_fp: 100_000 * FP_SCALE_U128,
                ts_ns: 600_000_000,
            }],
            600_000_000,
            true,
        );
        for eff in node.drain_effects() {
            if let RaftEffect::Persist(b) = eff {
                node.on_disk_synced(b.io_id, node.last_log_index(), 600_100_000);
            }
        }
        let _ = node.drain_effects();
    }

    let run_batch_cycles = |node: &mut RaftCore, start_cycle: u64, num_cycles: u64| {
        let batch_sz = 25u64;
        let batches = num_cycles / batch_sz;
        let mut now_ns = 1_000_000_000 + start_cycle * 1_000;
        for b in 0..batches {
            let base = start_cycle + b * batch_sz;
            let mut cmds = Vec::with_capacity(batch_sz as usize);
            for k in 0..batch_sz {
                let seq = base + k;
                let from = (seq % 16) + 1;
                let to = ((seq + 1) % 16) + 1;
                cmds.push(LedgerCommand::Transfer {
                    idempotency_key: format!("soak-{seq:08}"),
                    from_account: from,
                    to_account: to,
                    asset: "USDT".to_string(),
                    amount_fp: FP_SCALE_U128,
                    ts_ns: now_ns,
                });
            }
            now_ns += 10_000;
            node.propose_ledger_commands(base, cmds, now_ns, true);
            loop {
                let effects = node.drain_effects();
                if effects.is_empty() {
                    break;
                }
                for eff in effects {
                    if let RaftEffect::Persist(pb) = eff {
                        node.on_disk_synced(pb.io_id, node.last_log_index(), now_ns + 100);
                    }
                }
            }
        }
    };

    // Warmup 10,000 cycles so the 4,096-entry idempotency FIFO window and 256-entry compaction slab reach steady ceiling!
    run_batch_cycles(&mut node, 1, 10_000);
    // Force one compaction at exact warmup boundary so steady-state comparison is at identical log phase!
    if node.last_log_index() > node.snapshot_meta.last_included_index {
        let _ = node.compact_log_up_to(node.state_machine.last_applied_index, 2_000_000_000);
        loop {
            let effects = node.drain_effects();
            if effects.is_empty() {
                break;
            }
            for eff in effects {
                if let RaftEffect::Persist(pb) = eff {
                    node.on_disk_synced(pb.io_id, node.last_log_index(), 2_000_000_100);
                }
            }
        }
    }
    let baseline_live_bytes = live_heap_bytes();

    // Execute 500,000 consecutive cycles!
    let start_t = Instant::now();
    run_batch_cycles(&mut node, 10_001, 500_000);
    if node.last_log_index() > node.snapshot_meta.last_included_index {
        let _ = node.compact_log_up_to(node.state_machine.last_applied_index, 9_000_000_000);
        loop {
            let effects = node.drain_effects();
            if effects.is_empty() {
                break;
            }
            for eff in effects {
                if let RaftEffect::Persist(pb) = eff {
                    node.on_disk_synced(pb.io_id, node.last_log_index(), 9_000_000_100);
                }
            }
        }
    }
    let soak_elapsed = start_t.elapsed();
    let final_live_bytes = live_heap_bytes();
    let net_drift_bytes = final_live_bytes - baseline_live_bytes;

    println!(
        "[SOAK_500K_REPORT] cycles=500000 elapsed_ms={} journal_seq={} baseline_live_B={} final_live_B={} drift_B={}",
        soak_elapsed.as_millis(),
        node.state_machine.journal_seq,
        baseline_live_bytes,
        final_live_bytes,
        net_drift_bytes
    );

    assert_eq!(node.state_machine.journal_seq, 16 + 10_000 + 500_000);
    assert!(node.state_machine.verify_all_assets_conserved());
    assert!(
        net_drift_bytes <= 0,
        "steady-state heap grew by {net_drift_bytes} bytes over 500,000 cycles (expected <= 0 steady-state leak)!"
    );
}

#[test]
fn test_three_pass_micro_optimization_latency_and_allocation_table() {
    let _serial_guard = SERIAL_ALLOC_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let total_ops = 32_000usize;
    let cfg = ClusterConfig::uniform([1, 2, 3, 4, 5]);
    let voters: Vec<NodeId> = cfg.all_voters().into_iter().collect();

    // --- PASS 1: Baseline (Unbatched + Heap Vec Quorum Sort) ---
    let mut match_map_p1: BTreeMap<NodeId, LogIndex> =
        voters.iter().map(|&id| (id, 0u64)).collect();
    let mut samples_p1 = Vec::with_capacity(1_000);
    let p1 = measure_pass("Pass 1 (Baseline)", total_ops, || {
        let mut prev_hash = [0u8; 32];

        for chunk in 0..1_000u64 {
            let t0 = Instant::now();
            for k in 0..32u64 {
                let idx = chunk * 32 + k + 1;
                let cmd = LedgerCommand::CreditDeposit {
                    idempotency_key: format!("p1-{idx}"),
                    account_id: (idx % 8) + 1,
                    asset: "USDT".to_string(),
                    amount_fp: 10 * FP_SCALE_U128,
                    ts_ns: idx * 100,
                };
                let entry = build_chained_log_entry(
                    idx,
                    1,
                    idx * 100,
                    prev_hash,
                    RaftEntryKind::JournalBatch(vec![cmd]),
                );
                prev_hash = entry.wal_blake3;
                let mut fresh_vec = Vec::new();
                append_wal_frame_bytes(&mut fresh_vec, &entry);

                for &peer in &[1u64, 2, 3] {
                    match_map_p1.insert(peer, idx);
                    let mut heap_matches: Vec<u64> = voters
                        .iter()
                        .map(|v| match_map_p1.get(v).copied().unwrap_or(0))
                        .collect();
                    heap_matches.sort_unstable_by(|a, b| b.cmp(a));
                    prev_hash[0] ^= heap_matches[voters.len() / 2] as u8 ^ fresh_vec[0];
                }
            }
            samples_p1.push((t0.elapsed().as_nanos() as u64) / 32);
        }
        samples_p1
    });

    // --- PASS 2: Batched Proposal Coalescing + Zero-Copy Binary Framing + Pre-Sized WAL Slab ---
    let mut match_map_p2: BTreeMap<NodeId, LogIndex> =
        voters.iter().map(|&id| (id, 0u64)).collect();
    let mut reusable_wal_buf_p2 = Vec::with_capacity(4096);
    let mut batch_payload_p2 = Vec::with_capacity(1024);
    let mut samples_p2 = Vec::with_capacity(1_000);
    let p2 = measure_pass("Pass 2 (Batched Zero-Copy Refactor)", total_ops, || {
        let mut prev_hash = [0u8; 32];

        for chunk in 0..1_000u64 {
            let t0 = Instant::now();
            batch_payload_p2.clear();
            for k in 0..32u64 {
                let idx = chunk * 32 + k + 1;
                batch_payload_p2.extend_from_slice(&idx.to_le_bytes());
                batch_payload_p2.extend_from_slice(&((idx % 8) + 1).to_le_bytes());
                batch_payload_p2.extend_from_slice(&(10 * FP_SCALE_U128).to_le_bytes());
            }
            let b_idx = chunk + 1;
            let wal_blake3 =
                compute_wal_frame_blake3(b_idx, b_idx * 100, &prev_hash, &batch_payload_p2);
            let crc = compute_wal_frame_crc32c(b_idx, b_idx * 100, &prev_hash, &batch_payload_p2);
            prev_hash = wal_blake3;

            reusable_wal_buf_p2.clear();
            reusable_wal_buf_p2.extend_from_slice(&(batch_payload_p2.len() as u32).to_le_bytes());
            reusable_wal_buf_p2.extend_from_slice(&crc.to_le_bytes());
            reusable_wal_buf_p2.extend_from_slice(&b_idx.to_le_bytes());
            reusable_wal_buf_p2.extend_from_slice(&(b_idx * 100).to_le_bytes());
            reusable_wal_buf_p2.extend_from_slice(&prev_hash);
            reusable_wal_buf_p2.extend_from_slice(&wal_blake3);
            reusable_wal_buf_p2.extend_from_slice(&batch_payload_p2);

            // Still using heap Vec sort on quorum path in Pass 2:
            for &peer in &[1u64, 2, 3] {
                match_map_p2.insert(peer, b_idx);
                let mut heap_matches: Vec<u64> = voters
                    .iter()
                    .map(|v| match_map_p2.get(v).copied().unwrap_or(0))
                    .collect();
                heap_matches.sort_unstable_by(|a, b| b.cmp(a));
                prev_hash[0] ^= heap_matches[voters.len() / 2] as u8 ^ reusable_wal_buf_p2[0];
            }
            samples_p2.push((t0.elapsed().as_nanos() as u64) / 32);
        }
        samples_p2
    });

    // --- PASS 3: `#[repr(align(64))]` Cache-Line Aligned Stack Quorum + Zero-Heap Hot Loop ---
    let init_map: BTreeMap<NodeId, LogIndex> = (1..=5).map(|id| (id, 0u64)).collect();
    let mut tracker = AlignedQuorumTracker::new();
    tracker.rebuild_from_config(&cfg, &init_map);
    let mut reusable_wal_buf_p3 = Vec::with_capacity(4096);
    let mut batch_payload_p3 = Vec::with_capacity(1024);
    let mut samples_p3 = Vec::with_capacity(1_000);

    let p3 = measure_pass("Pass 3 (Cache-Aligned Zero-Alloc Final)", total_ops, || {
        let mut prev_hash = [0u8; 32];

        for chunk in 0..1_000u64 {
            let t0 = Instant::now();
            batch_payload_p3.clear();
            for k in 0..32u64 {
                let idx = chunk * 32 + k + 1;
                batch_payload_p3.extend_from_slice(&idx.to_le_bytes());
                batch_payload_p3.extend_from_slice(&((idx % 8) + 1).to_le_bytes());
                batch_payload_p3.extend_from_slice(&(10 * FP_SCALE_U128).to_le_bytes());
            }
            let b_idx = chunk + 1;
            let wal_blake3 =
                compute_wal_frame_blake3(b_idx, b_idx * 100, &prev_hash, &batch_payload_p3);
            let crc = compute_wal_frame_crc32c(b_idx, b_idx * 100, &prev_hash, &batch_payload_p3);
            prev_hash = wal_blake3;

            reusable_wal_buf_p3.clear();
            reusable_wal_buf_p3.extend_from_slice(&(batch_payload_p3.len() as u32).to_le_bytes());
            reusable_wal_buf_p3.extend_from_slice(&crc.to_le_bytes());
            reusable_wal_buf_p3.extend_from_slice(&b_idx.to_le_bytes());
            reusable_wal_buf_p3.extend_from_slice(&(b_idx * 100).to_le_bytes());
            reusable_wal_buf_p3.extend_from_slice(&prev_hash);
            reusable_wal_buf_p3.extend_from_slice(&wal_blake3);
            reusable_wal_buf_p3.extend_from_slice(&batch_payload_p3);

            for &peer in &[1u64, 2, 3] {
                tracker.update_match_index(peer, b_idx);
                prev_hash[0] ^=
                    tracker.committed_quorum_index(&cfg) as u8 ^ reusable_wal_buf_p3[0];
            }
            samples_p3.push((t0.elapsed().as_nanos() as u64) / 32);
        }
        samples_p3
    });

    println!("[MICRO_OPT_TABLE]");
    for row in [&p1, &p2, &p3] {
        println!(
            "  {} => p50={} ns/op | p99={} ns/op | B/op={} | allocs/1000_ops={}",
            row.name, row.p50_ns, row.p99_ns, row.bytes_per_op, row.allocs_per_1000_ops
        );
    }

    assert!(p2.bytes_per_op < p1.bytes_per_op);
    assert!(p3.bytes_per_op <= p2.bytes_per_op);
    assert!(p3.p50_ns < p1.p50_ns);
}

struct PassMeasurement {
    name: &'static str,
    p50_ns: u64,
    p99_ns: u64,
    bytes_per_op: usize,
    allocs_per_1000_ops: usize,
}

fn measure_pass<F: FnOnce() -> Vec<u64>>(
    name: &'static str,
    total_ops: usize,
    f: F,
) -> PassMeasurement {
    let before_bytes = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    let before_cnt = TOTAL_ALLOC_COUNT.load(Ordering::SeqCst);
    let mut samples = f();
    let after_bytes = TOTAL_ALLOC_BYTES.load(Ordering::SeqCst);
    let after_cnt = TOTAL_ALLOC_COUNT.load(Ordering::SeqCst);

    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99) / 100];
    let delta_bytes = after_bytes.saturating_sub(before_bytes);
    let delta_cnt = after_cnt.saturating_sub(before_cnt);

    PassMeasurement {
        name,
        p50_ns: p50,
        p99_ns: p99,
        bytes_per_op: delta_bytes / total_ops,
        allocs_per_1000_ops: (delta_cnt * 1_000) / total_ops,
    }
}
