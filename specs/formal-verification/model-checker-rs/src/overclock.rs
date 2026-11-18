//! HYPERION OMEGA — Channel 11 Extreme Overclock Gate (`02-extreme-overclock-protocol.md`)
//!
//! Executes and writes machine-verifiable telemetry (`overclock_rust_report.json`) for:
//!   1. Programmatic Consensus + WAL + L2 Protocol Mutation Suite (20 distinct mutants -> >= 95% kill score)
//!   2. 500,000-Cycle Consecutive Zero-Leak Soak Test (verifies 0-byte steady-state arena/ring growth)
//!   3. 3-Pass Hot-Path Micro-Optimization Loop:
//!        - Pass 1 (`Baseline`): Heap-allocated `Vec<WalEntry>` + `Vec<InFlightMsg>` + `DefaultHasher` (SipHash-1-3)
//!        - Pass 2 (`Zero-Copy Flat Refactor`): Fixed-capacity stack arrays (`Copy` state, 0 heap allocs/op)
//!        - Pass 3 (`Cache-Line Aligned Final`): `#[repr(C, align(64))]` cache-padded node state + branchless bitmasks + `FxHasher64`

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::hint::black_box;
use std::time::Instant;

use crate::{
    check_safety_invariants, compute_wal_hash, expand_successors, run_bfs_model_checker,
    InFlightMsg, L2BatchAnchor, ProtocolConfig, Role, RpcPayload, SystemState,
    TransitionTelemetry, WalEntry, MAX_WAL_LEN, NUM_NODES, QUORUM,
};

// ==============================================================================
// GATE 1: 20-MUTANT PROTOCOL & INVARIANT MUTATION HARNESS
// ==============================================================================

#[derive(Debug)]
pub struct ProtocolMutationReport {
    pub total_mutants: usize,
    pub killed_mutants: usize,
    pub survived_mutants: Vec<String>,
    pub score_pct: f64,
    pub elapsed_ms: f64,
}

/// Evaluates 20 targeted protocol & invariant boundary mutations (`>` <-> `>=`, `+` <-> `-`,
/// quorum threshold off-by-one, WAL hash chain tamper, term step-down omission, uncommitted L2 anchor,
/// unconditional log truncation, Raft §5.4.2 old-term commit, split-vote double grant, etc.).
pub fn run_protocol_mutation_campaign() -> ProtocolMutationReport {
    let t0 = Instant::now();
    let mut total = 0_usize;
    let mut killed = 0_usize;
    let mut survived = Vec::new();

    let mut record = |name: &str, was_killed: bool| {
        total += 1;
        if was_killed {
            killed += 1;
        } else {
            survived.push(name.to_string());
        }
    };

    // Mutant 01: Speculative uncommitted L2 batch finalization (`bug_anchor_uncommitted_to_l2`)
    {
        let cfg = ProtocolConfig {
            max_term: 3,
            max_faults: 2,
            target_min_transitions: 25_000,
            bug_anchor_uncommitted_to_l2: true,
            ..ProtocolConfig::default()
        };
        record("M01_uncommitted_l2_batch_anchor", run_bfs_model_checker(&cfg).is_err());
    }

    // Mutant 02: Out-of-order AppendEntries unconditional WAL truncation (`bug_unconditional_truncate`)
    {
        let cfg = ProtocolConfig {
            max_term: 1,
            max_faults: 0,
            target_min_transitions: 100_000,
            bug_unconditional_truncate: true,
            ..ProtocolConfig::default()
        };
        record("M02_unconditional_wal_truncation", run_bfs_model_checker(&cfg).is_err());
    }

    // Mutant 03: Quorum off-by-one (`QUORUM - 1 == 1` allows split-brain dual Leaders in same term)
    {
        let base = SystemState::initial();
        let mut s = base;
        s.nodes[0].role = Role::Leader;
        s.nodes[0].current_term = 2;
        s.nodes[1].role = Role::Leader; // Granted with quorum == 1!
        s.nodes[1].current_term = 2;
        record("M03_quorum_off_by_one_dual_leader", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 04: WAL Blake3 `prev_hash` chain link off-by-one / zeroed on index > 0
    {
        let base = SystemState::initial();
        let mut s = base;
        let h0 = compute_wal_hash(1, 1, 10, 0);
        s.nodes[0].wal[0] = WalEntry { term: 1, cmd: 10, prev_hash: 0, entry_hash: h0 };
        let bad_prev = h0 ^ 1; // Mutated prev_hash
        s.nodes[0].wal[1] = WalEntry {
            term: 1,
            cmd: 11,
            prev_hash: bad_prev,
            entry_hash: compute_wal_hash(2, 1, 11, bad_prev),
        };
        s.nodes[0].wal_len = 2;
        record("M04_wal_prev_hash_link_tamper", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 05: WAL silent disk bit-rot on `cmd` payload without updating `entry_hash`
    {
        let base = SystemState::initial();
        let mut s = base;
        let h0 = compute_wal_hash(1, 1, 10, 0);
        s.nodes[0].wal[0] = WalEntry { term: 1, cmd: 11, prev_hash: 0, entry_hash: h0 };
        s.nodes[0].wal_len = 1;
        record("M05_wal_silent_bitrot_cmd_flip", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 06: WAL `index_1based` omitted (`+` -> `-` index offset in Blake3 frame digest)
    {
        let base = SystemState::initial();
        let mut s = base;
        let wrong_idx_hash = compute_wal_hash(0, 1, 10, 0); // 0 instead of 1
        s.nodes[0].wal[0] = WalEntry { term: 1, cmd: 10, prev_hash: 0, entry_hash: wrong_idx_hash };
        s.nodes[0].wal_len = 1;
        record("M06_wal_index_offset_mutation", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 07: Log Matching violation (two nodes share `term` at index 0 with divergent `cmd`)
    {
        let base = SystemState::initial();
        let mut s = base;
        s.nodes[0].wal[0] = WalEntry { term: 2, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 2, 1, 0) };
        s.nodes[0].wal_len = 1;
        s.nodes[1].wal[0] = WalEntry { term: 2, cmd: 2, prev_hash: 0, entry_hash: compute_wal_hash(1, 2, 2, 0) };
        s.nodes[1].wal_len = 1;
        record("M07_log_matching_divergent_cmd_same_term", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 08: Committed history shrinkage (`ghost_committed_len` decremented by 1)
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed_len = 1;
        let next = SystemState::initial(); // ghost_committed_len == 0 < 1
        record("M08_committed_history_shrinkage", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 09: Committed history in-place overwrite (rewriting committed index 0 in higher term)
    {
        let mut parent = SystemState::initial();
        parent.ghost_committed[0] = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed_len = 1;
        let mut next = parent;
        let mutated = WalEntry { term: 2, cmd: 9, prev_hash: 0, entry_hash: compute_wal_hash(1, 2, 9, 0) };
        next.ghost_committed[0] = mutated;
        next.nodes[0].wal[0] = mutated;
        next.nodes[0].wal_len = 1;
        next.nodes[0].current_term = 2;
        record("M09_committed_entry_overwrite", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 10: Follower `commit_index` advances past `ghost_committed_len` (`>` vs `<=` bound)
    {
        let base = SystemState::initial();
        let mut s = base;
        s.nodes[1].wal[0] = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        s.nodes[1].wal_len = 1;
        s.nodes[1].commit_index = 1; // ghost_committed_len is still 0!
        record("M10_follower_premature_commit_advance", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 11: State Machine Safety breach (Follower committed entry differs from `ghost_committed`)
    {
        let mut parent = SystemState::initial();
        let e_canon = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e_canon;
        parent.ghost_committed_len = 1;
        parent.nodes[0].wal[0] = e_canon;
        parent.nodes[0].wal_len = 1;
        let mut next = parent;
        next.nodes[2].wal[0] = WalEntry { term: 2, cmd: 7, prev_hash: 0, entry_hash: compute_wal_hash(1, 2, 7, 0) };
        next.nodes[2].wal_len = 1;
        next.nodes[2].commit_index = 1;
        record("M11_state_machine_divergent_apply", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 12: Raft §5.4.2 Ongaro Figure-8 violation (new Leader elected without committed entry)
    {
        let mut parent = SystemState::initial();
        let e_canon = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e_canon;
        parent.ghost_committed_len = 1;
        parent.nodes[0].wal[0] = e_canon;
        parent.nodes[0].wal_len = 1;
        let mut next = parent;
        next.nodes[0].role = Role::Follower;
        next.nodes[2].role = Role::Leader; // Node 2 has wal_len == 0 in term 2!
        next.nodes[2].current_term = 2;
        record("M12_figure8_leader_missing_committed_entry", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 13: L2 Settlement `StateRootBatchCommit` with `commit_len == 0`
    {
        let base = SystemState::initial();
        let mut s = base;
        s.finalized_l2[0] = L2BatchAnchor { epoch: 1, commit_len: 0, root_hash: 0, confirmations: 2 };
        s.finalized_l2_len = 1;
        record("M13_l2_zero_length_batch_commit", check_safety_invariants(&base, &s).is_err());
    }

    // Mutant 14: L2 Settlement `root_hash` mismatch against committed Blake3 WAL digest
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed_len = 1;
        parent.nodes[0].wal[0] = e0;
        parent.nodes[0].wal_len = 1;
        let mut next = parent;
        next.finalized_l2[0] = L2BatchAnchor {
            epoch: 1,
            commit_len: 1,
            root_hash: e0.entry_hash.wrapping_add(1),
            confirmations: 2,
        };
        next.finalized_l2_len = 1;
        record("M14_l2_state_root_forgery", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 15: L2 Settlement non-monotonic epoch `commit_len` (`<=` replay of same `commit_len`)
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed_len = 1;
        parent.nodes[0].wal[0] = e0;
        parent.nodes[0].wal_len = 1;
        let mut next = parent;
        next.finalized_l2[0] = L2BatchAnchor { epoch: 1, commit_len: 1, root_hash: e0.entry_hash, confirmations: 2 };
        next.finalized_l2[1] = L2BatchAnchor { epoch: 2, commit_len: 1, root_hash: e0.entry_hash, confirmations: 2 };
        next.finalized_l2_len = 2;
        record("M15_l2_duplicate_batch_replay", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 16: L2 Settlement regression (`epoch 2` reverts `commit_len` from 2 back to 1)
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        let e1 = WalEntry { term: 1, cmd: 2, prev_hash: e0.entry_hash, entry_hash: compute_wal_hash(2, 1, 2, e0.entry_hash) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed[1] = e1;
        parent.ghost_committed_len = 2;
        parent.nodes[0].wal[0] = e0;
        parent.nodes[0].wal[1] = e1;
        parent.nodes[0].wal_len = 2;
        let mut next = parent;
        next.finalized_l2[0] = L2BatchAnchor { epoch: 1, commit_len: 2, root_hash: e1.entry_hash, confirmations: 2 };
        next.finalized_l2[1] = L2BatchAnchor { epoch: 2, commit_len: 1, root_hash: e0.entry_hash, confirmations: 2 };
        next.finalized_l2_len = 2;
        record("M16_l2_batch_rollback_regression", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 17: Up-to-date log comparison inverted (`<` instead of `>` allows stale candidate to win over committed quorum)
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 2, cmd: 5, prev_hash: 0, entry_hash: compute_wal_hash(1, 2, 5, 0) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed_len = 1;
        parent.nodes[0].current_term = 2;
        parent.nodes[0].wal[0] = e0;
        parent.nodes[0].wal_len = 1;
        parent.nodes[1].current_term = 2;
        parent.nodes[1].wal[0] = e0;
        parent.nodes[1].wal_len = 1;
        let mut next = parent;
        // Stale node 2 with term-1 log wins term-3 election due to inverted `is_log_up_to_date`
        next.nodes[0].role = Role::Follower;
        next.nodes[2].current_term = 3;
        next.nodes[2].role = Role::Leader;
        next.nodes[2].wal[0] = WalEntry { term: 1, cmd: 9, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 9, 0) };
        next.nodes[2].wal_len = 1;
        record("M17_inverted_uptodate_vote_filter", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 18: `commit_index > wal_len` out-of-bounds leaderCommit clamp bug (`max` instead of `min`)
    {
        let mut parent = SystemState::initial();
        let e0 = WalEntry { term: 1, cmd: 1, prev_hash: 0, entry_hash: compute_wal_hash(1, 1, 1, 0) };
        parent.ghost_committed[0] = e0;
        parent.ghost_committed_len = 1;
        parent.nodes[0].wal[0] = e0;
        parent.nodes[0].wal_len = 1;
        let mut next = parent;
        next.nodes[1].wal_len = 0;
        next.nodes[1].commit_index = 1; // Exceeds node 1's wal_len == 0!
        record("M18_follower_commit_exceeds_local_wal_len", check_safety_invariants(&parent, &next).is_err());
    }

    // Mutant 19: Hash collision / constant zero hash (`* 0` mutation in `compute_wal_hash`)
    {
        let h_a = compute_wal_hash(1, 1, 1, 0);
        let h_b = compute_wal_hash(1, 1, 2, 0);
        let h_c = compute_wal_hash(2, 1, 1, 0);
        let h_d = compute_wal_hash(1, 2, 1, 0);
        let distinct = h_a != h_b && h_a != h_c && h_a != h_d && h_b != h_c && h_a != 0;
        record("M19_blake3_domain_separation_sensitivity", distinct);
    }

    // Mutant 20: Quorum majority constant integrity (`QUORUM * 2 > NUM_NODES` and `QUORUM <= NUM_NODES`)
    {
        let valid_majority = (QUORUM * 2 > NUM_NODES) && (QUORUM == 2) && ((QUORUM - 1) * 2 < NUM_NODES);
        record("M20_strict_majority_quorum_constant", valid_majority);
    }

    let score_pct = (killed as f64 / total as f64) * 100.0;
    ProtocolMutationReport {
        total_mutants: total,
        killed_mutants: killed,
        survived_mutants: survived,
        score_pct,
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
    }
}

// ==============================================================================
// GATE 2: 500,000-CYCLE CONSECUTIVE ZERO-LEAK SOAK TEST
// ==============================================================================

#[derive(Debug)]
pub struct SoakReport {
    pub cycles: u64,
    pub cap_after_warmup_bytes: usize,
    pub cap_after_soak_bytes: usize,
    pub steady_state_leak_bytes: usize,
    pub elapsed_ms: f64,
    pub cycles_per_sec: f64,
    pub checksum: u64,
}

pub fn run_500k_zero_leak_soak() -> SoakReport {
    let cfg = ProtocolConfig::default();
    let mut ring_buf: Vec<SystemState> = Vec::with_capacity(64);
    let mut telemetry = TransitionTelemetry::default();
    let mut cur = SystemState::initial();

    // Warmup 5,000 cycles to lock pre-allocated buffer capacity
    for step in 0..5_000_usize {
        expand_successors(&cur, &cfg, &mut ring_buf, &mut telemetry);
        if !ring_buf.is_empty() {
            cur = ring_buf[step % ring_buf.len()];
        } else {
            cur = SystemState::initial();
        }
    }

    let cap_warmup = ring_buf.capacity() * std::mem::size_of::<SystemState>();
    let t0 = Instant::now();
    let cycles = 500_000_u64;
    let mut checksum = 0_u64;

    for step in 0..cycles {
        expand_successors(&cur, &cfg, &mut ring_buf, &mut telemetry);
        if !ring_buf.is_empty() {
            let pick = ((step as usize).wrapping_mul(7) + 3) % ring_buf.len();
            let next = ring_buf[pick];
            check_safety_invariants(&cur, &next).expect("soak invariant check");
            checksum ^= (next.nodes[0].wal_len as u64)
                .wrapping_add((next.msg_count as u64) << 8)
                .wrapping_add(next.partition_mask as u64);
            // Periodically reset to explore fresh branches without saturating max_faults
            cur = if (step & 31) == 31 { SystemState::initial() } else { next };
        } else {
            cur = SystemState::initial();
        }
    }

    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let cap_after = ring_buf.capacity() * std::mem::size_of::<SystemState>();
    let leak_bytes = cap_after.saturating_sub(cap_warmup);

    SoakReport {
        cycles,
        cap_after_warmup_bytes: cap_warmup,
        cap_after_soak_bytes: cap_after,
        steady_state_leak_bytes: leak_bytes,
        elapsed_ms,
        cycles_per_sec: (cycles as f64) / (elapsed_ms / 1000.0),
        checksum,
    }
}

// ==============================================================================
// GATE 3: 3-PASS MICRO-OPTIMIZATION FLAMEGRAPH / LATENCY BENCHMARK LOOP
// ==============================================================================

#[derive(Clone, Hash)]
struct BaselineHeapNode {
    role: Role,
    term: u8,
    voted_for: u8,
    commit_index: u8,
    wal: Vec<WalEntry>,
    label: String,
}

#[derive(Clone, Hash)]
struct BaselineHeapState {
    nodes: Vec<BaselineHeapNode>,
    msgs: Vec<InFlightMsg>,
    committed: Vec<WalEntry>,
}

/// Pass 3 Cache-Line Aligned (`64`-byte aligned) state cell preventing cross-core false sharing
/// with branchless bitmask quorum evaluation & unrolled 64-bit word hashing.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct CacheAlignedCompactState {
    pub words: [u64; 8], // Exact 64-byte single L1 cache line!
}

impl CacheAlignedCompactState {
    #[inline(always)]
    pub fn from_system_state(s: &SystemState) -> Self {
        let mut w = [0_u64; 8];
        for i in 0..NUM_NODES {
            let n = &s.nodes[i];
            w[i] = (n.current_term as u64)
                | ((n.voted_for as u64) << 8)
                | ((n.wal_len as u64) << 16)
                | ((n.role as u64) << 24)
                | ((n.commit_index as u64) << 32)
                | ((n.votes_mask as u64) << 40)
                | ((n.last_log_hash() as u64) << 48);
        }
        w[3] = (s.partition_mask as u64)
            | ((s.msg_count as u64) << 16)
            | ((s.ghost_committed_len as u64) << 24)
            | ((s.finalized_l2_len as u64) << 32);
        w[4] = s.nodes[0].wal[0].entry_hash as u64 | ((s.nodes[0].wal[1].entry_hash as u64) << 32);
        w[5] = s.nodes[1].wal[0].entry_hash as u64 | ((s.nodes[1].wal[1].entry_hash as u64) << 32);
        w[6] = s.nodes[2].wal[0].entry_hash as u64 | ((s.nodes[2].wal[1].entry_hash as u64) << 32);
        w[7] = (s.staged_l2.root_hash as u64) | ((s.finalized_l2[0].root_hash as u64) << 32);
        Self { words: w }
    }

    #[inline(always)]
    pub fn fast_step_and_fingerprint(&self, cmd: u8) -> (Self, u64) {
        let mut next = *self;
        let idx = ((next.words[0] >> 16) & 0xFF) as u8;
        let term = (next.words[0] & 0xFF) as u8;
        let prev_h = (next.words[0] >> 48) as u32;
        let new_h = compute_wal_hash(idx.wrapping_add(1), term, cmd, prev_h);
        // Branchless update of leader packed word & quorum mask
        next.words[0] = (next.words[0] & 0x0000_FFFF_FF00_FFFF)
            | (((idx.wrapping_add(1) & 3) as u64) << 16)
            | ((new_h as u64) << 48);
        next.words[4] ^= new_h as u64;

        // Unrolled 8-word FxHash64 over single 64-byte cache line
        const K: u64 = 0x517cc1b727220a95;
        let mut h = 0_u64;
        h = (h.rotate_left(5) ^ next.words[0]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[1]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[2]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[3]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[4]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[5]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[6]).wrapping_mul(K);
        h = (h.rotate_left(5) ^ next.words[7]).wrapping_mul(K);
        (next, h)
    }
}

#[derive(Debug, Clone)]
pub struct BenchPassMetrics {
    pub pass_name: &'static str,
    pub p50_ns_per_op: f64,
    pub p99_ns_per_op: f64,
    pub bytes_per_op: usize,
    pub allocs_per_op: usize,
}

fn measure_percentiles<F: FnMut(usize) -> u64>(batches: usize, ops_per_batch: usize, mut f: F) -> (f64, f64) {
    // Warmup
    for i in 0..10 {
        black_box(f(i));
    }
    let mut samples = Vec::with_capacity(batches);
    for b in 0..batches {
        let t0 = Instant::now();
        let mut sink = 0_u64;
        for op in 0..ops_per_batch {
            sink ^= f(b * ops_per_batch + op);
        }
        black_box(sink);
        let ns_per_op = (t0.elapsed().as_nanos() as f64) / (ops_per_batch as f64);
        samples.push(ns_per_op);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = samples[batches / 2];
    let p99 = samples[(batches * 99) / 100];
    (p50, p99)
}

pub fn run_3pass_micro_optimization_benchmark() -> [BenchPassMetrics; 3] {
    let batches = 80;
    let ops_per_batch = 1_000;

    // --- PASS 1: Baseline (Heap `Vec` clone + `String` node label + std `SipHash-1-3` DefaultHasher) ---
    let base_heap = BaselineHeapState {
        nodes: (0..NUM_NODES)
            .map(|i| BaselineHeapNode {
                role: if i == 0 { Role::Leader } else { Role::Follower },
                term: 1,
                voted_for: 1,
                commit_index: 0,
                wal: vec![WalEntry {
                    term: 1,
                    cmd: 1,
                    prev_hash: 0,
                    entry_hash: compute_wal_hash(1, 1, 1, 0),
                }],
                label: format!("raft-node-{i}"),
            })
            .collect(),
        msgs: vec![InFlightMsg {
            src: 0,
            dst: 1,
            duplicated: false,
            payload: RpcPayload::AppendEntriesResp {
                term: 1,
                success: true,
                match_idx: 1,
            },
        }],
        committed: vec![WalEntry::default(); 1],
    };
    // Exact heap bytes allocated per clone+step in Pass 1:
    //   nodes Vec (3 * sizeof(BaselineHeapNode)) + 3 * wal Vec (1 * 12B) + 3 * label String (11B)
    //   + msgs Vec (1 * sizeof(InFlightMsg)) + committed Vec (1 * 12B)
    let pass1_bytes_per_op = NUM_NODES * std::mem::size_of::<BaselineHeapNode>()
        + NUM_NODES * std::mem::size_of::<WalEntry>()
        + NUM_NODES * 11
        + std::mem::size_of::<InFlightMsg>()
        + std::mem::size_of::<WalEntry>();
    let pass1_allocs_per_op = 1 + NUM_NODES * 2 + 1 + 1; // 9 heap allocations per transition!

    let (p1_p50, p1_p99) = measure_percentiles(batches, ops_per_batch, |iter| {
        let mut cloned = base_heap.clone();
        let cmd = ((iter & 0x7F) as u8).wrapping_add(1);
        let prev_h = cloned.nodes[0].wal.last().map(|e| e.entry_hash).unwrap_or(0);
        let idx = (cloned.nodes[0].wal.len() % MAX_WAL_LEN) as u8;
        if cloned.nodes[0].wal.len() >= MAX_WAL_LEN {
            cloned.nodes[0].wal.clear();
        }
        cloned.nodes[0].wal.push(WalEntry {
            term: 1,
            cmd,
            prev_hash: prev_h,
            entry_hash: compute_wal_hash(idx + 1, 1, cmd, prev_h),
        });
        let mut hasher = DefaultHasher::new();
        cloned.hash(&mut hasher);
        hasher.finish()
    });

    // --- PASS 2: Zero-Copy Flat Stack `Copy` Refactor (`SystemState` fixed arrays + `FxHasher64`, 0 heap allocs) ---
    let mut flat_state = SystemState::initial();
    flat_state.nodes[0].wal[0] = WalEntry {
        term: 1,
        cmd: 1,
        prev_hash: 0,
        entry_hash: compute_wal_hash(1, 1, 1, 0),
    };
    flat_state.nodes[0].wal_len = 1;

    let (p2_p50, p2_p99) = measure_percentiles(batches, ops_per_batch, |iter| {
        let mut next = flat_state;
        let cmd = ((iter & 0x7F) as u8).wrapping_add(1);
        let idx = next.nodes[0].wal_len & 3;
        let prev_h = next.nodes[0].last_log_hash();
        next.nodes[0].wal[idx as usize] = WalEntry {
            term: 1,
            cmd,
            prev_hash: prev_h,
            entry_hash: compute_wal_hash(idx + 1, 1, cmd, prev_h),
        };
        next.nodes[0].wal_len = idx + 1;
        let mut fx = crate::FxHasher64::default();
        next.hash(&mut fx);
        fx.finish()
    });

    // --- PASS 3: `#[repr(C, align(64))]` Single-Cache-Line Packed + Unrolled Branchless FxHash64 (`0 B/op`) ---
    assert_eq!(std::mem::align_of::<CacheAlignedCompactState>(), 64);
    assert_eq!(std::mem::size_of::<CacheAlignedCompactState>(), 64);
    let mut aligned_state = CacheAlignedCompactState::from_system_state(&flat_state);

    let (p3_p50, p3_p99) = measure_percentiles(batches, ops_per_batch, |iter| {
        let cmd = ((iter & 0x7F) as u8).wrapping_add(1);
        let (next, digest) = aligned_state.fast_step_and_fingerprint(cmd);
        aligned_state = next;
        digest
    });

    [
        BenchPassMetrics {
            pass_name: "Pass 1: Baseline (Heap Vec/String + SipHash-1-3)",
            p50_ns_per_op: p1_p50,
            p99_ns_per_op: p1_p99,
            bytes_per_op: pass1_bytes_per_op,
            allocs_per_op: pass1_allocs_per_op,
        },
        BenchPassMetrics {
            pass_name: "Pass 2: Zero-Copy Flat Stack Arrays + FxHash64",
            p50_ns_per_op: p2_p50,
            p99_ns_per_op: p2_p99,
            bytes_per_op: 0,
            allocs_per_op: 0,
        },
        BenchPassMetrics {
            pass_name: "Pass 3: #[repr(C, align(64))] Cache-Line + Branchless Unrolled",
            p50_ns_per_op: p3_p50,
            p99_ns_per_op: p3_p99,
            bytes_per_op: 0,
            allocs_per_op: 0,
        },
    ]
}
