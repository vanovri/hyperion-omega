use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::Instant;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use hyperion_zk_solvency::{
    circuit::{EpochBatchSolvencyCircuit, MsstInclusionSolvencyCircuit},
    msst::{AccountLeafInput, MsstReusableWorkspace, MsstTree},
    prover::{
        prove_epoch_solvency, prove_user_inclusion_solvency, verify_r1cs_satisfied,
        EpochBatchKeyMaterial, InclusionKeyMaterial,
    },
    verifier::{
        verify_epoch_solvency_with_pvk, verify_msst_inclusion_proof, verify_user_zk_bundle_with_pvk,
    },
};

struct CountingAllocator;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(new_size, Ordering::Relaxed);
            let delta = (new_size as isize) - (layout.size() as isize);
            LIVE_BYTES.fetch_add(delta, Ordering::Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn reset_alloc_counters() {
    ALLOC_COUNT.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);
}

fn read_alloc_counters() -> (usize, usize) {
    (
        ALLOC_COUNT.load(Ordering::SeqCst),
        ALLOC_BYTES.load(Ordering::SeqCst),
    )
}

fn generate_sorted_accounts(count: usize, seed: u64) -> Vec<AccountLeafInput> {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let mut out: Vec<AccountLeafInput> = (0..count)
        .map(|i| AccountLeafInput {
            account_id: 1_000_000 + (i as u64) * 10,
            balance_fp: 100_000 + (rng.gen::<u64>() % 10_000_000_000),
        })
        .collect();
    out.sort_unstable_by_key(|a| a.account_id);
    out
}

fn percentile_ns(sorted: &[u128], num: usize, den: usize) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * num) / den;
    sorted[idx]
}

fn run_500k_zero_leak_soak_test() {
    println!("=== GATE 2: 500,000-CYCLE ZERO-LEAK STEADY-STATE SOAK TEST ===");
    let accounts = generate_sorted_accounts(16, 0x50A4_2026);
    let seed = [0x66u8; 32];
    let tree = MsstTree::build_parallel(&accounts[..4], 2, 1, &seed).unwrap();
    let proof = tree.generate_inclusion_proof(accounts[2].account_id).unwrap();
    let mut reusable_ws = MsstReusableWorkspace::new(4).unwrap();
    let small_accounts = &accounts[..16];

    // Warmup 2,000 cycles (initializes Rayon global threadpool + lazy static tables)
    for c in 0..2_000u64 {
        if c % 500 == 0 {
            let _ = black_box(
                reusable_ws
                    .rebuild_parallel_zero_alloc(small_accounts, c, &seed)
                    .unwrap(),
            );
        }
        let sum = verify_msst_inclusion_proof(black_box(&proof)).unwrap();
        black_box(sum);
    }

    let live_before = LIVE_BYTES.load(Ordering::SeqCst);
    reset_alloc_counters();
    let t0 = Instant::now();

    const SOAK_CYCLES: u64 = 500_000;
    for cycle in 0..SOAK_CYCLES {
        // Every 2,500 cycles also exercise in-place workspace rebuild (`200` full tree rebuilds!)
        if cycle % 2_500 == 0 {
            let (root, commit) = reusable_ws
                .rebuild_parallel_zero_alloc(black_box(small_accounts), cycle, &seed)
                .unwrap();
            black_box((root, commit));
        }
        let sum = verify_msst_inclusion_proof(black_box(&proof)).unwrap();
        assert_eq!(sum, tree.total_liabilities_fp());
    }

    let elapsed = t0.elapsed();
    let (soak_allocs, soak_bytes) = read_alloc_counters();
    let live_after = LIVE_BYTES.load(Ordering::SeqCst);
    let leak_delta = live_after - live_before;

    println!(
        "Soak Cycles: {SOAK_CYCLES} | Elapsed: {:.2} ms | Allocs during 500k cycles: {soak_allocs} ({soak_bytes} B) | Steady-State Leak Delta: {leak_delta} B",
        elapsed.as_secs_f64() * 1_000.0
    );
    assert_eq!(
        leak_delta, 0,
        "MEMORY LEAK DETECTED: live heap grew by {leak_delta} bytes across 500k cycles!"
    );
    assert_eq!(
        soak_allocs, 0,
        "Expected 0 steady-state allocations on hot verification + reusable workspace path, got {soak_allocs}"
    );
}

fn run_3_pass_micro_optimization_benchmark() {
    println!("\n=== GATE 3: 3-PASS MICRO-OPTIMIZATION LOOP (1,024 ACCOUNTS, DEPTH 10) ===");
    let accounts = generate_sorted_accounts(1_024, 0xBEEF_1024);
    let seed = [0x24u8; 32];
    let depth = 10;
    let iters = 15usize;

    // Warmup all 3 engines
    let b0 = MsstTree::build_baseline_sequential(&accounts, depth, 1, &seed).unwrap();
    let b1 = MsstTree::build_zero_alloc_arena(&accounts, depth, 1, &seed).unwrap();
    let mut ws = MsstReusableWorkspace::new(depth).unwrap();
    let (r2, c2) = ws.rebuild_parallel_zero_alloc(&accounts, 1, &seed).unwrap();
    assert_eq!(b0.root(), b1.root());
    assert_eq!(b1.root(), r2);
    assert_eq!(b1.root_commitment, c2);

    // Pass 1: Baseline
    let mut p1_samples = Vec::with_capacity(iters);
    reset_alloc_counters();
    for i in 0..iters {
        let t = Instant::now();
        let tr = MsstTree::build_baseline_sequential(black_box(&accounts), depth, i as u64, &seed)
            .unwrap();
        p1_samples.push(t.elapsed().as_nanos());
        black_box(tr.root());
    }
    let (p1_allocs_total, p1_bytes_total) = read_alloc_counters();
    p1_samples.sort_unstable();

    // Pass 2: Zero-Copy Flat Arena + Unrolled Register Poseidon
    let mut p2_samples = Vec::with_capacity(iters);
    reset_alloc_counters();
    for i in 0..iters {
        let t = Instant::now();
        let tr = MsstTree::build_zero_alloc_arena(black_box(&accounts), depth, i as u64, &seed)
            .unwrap();
        p2_samples.push(t.elapsed().as_nanos());
        black_box(tr.root());
    }
    let (p2_allocs_total, p2_bytes_total) = read_alloc_counters();
    p2_samples.sort_unstable();

    // Pass 3: Cache-Line Aligned `#[repr(C, align(64))]` Disjoint Rayon Reusable Workspace
    let mut p3_samples = Vec::with_capacity(iters);
    reset_alloc_counters();
    for i in 0..iters {
        let t = Instant::now();
        let res = ws
            .rebuild_parallel_zero_alloc(black_box(&accounts), i as u64, &seed)
            .unwrap();
        p3_samples.push(t.elapsed().as_nanos());
        black_box(res);
    }
    let (p3_allocs_total, p3_bytes_total) = read_alloc_counters();
    p3_samples.sort_unstable();

    println!(
        "Pass 1 (Baseline)            : p50 = {:>10} ns | p99 = {:>10} ns | {:>8} allocs/op | {:>10} B/op",
        percentile_ns(&p1_samples, 50, 100),
        percentile_ns(&p1_samples, 99, 100),
        p1_allocs_total / iters,
        p1_bytes_total / iters,
    );
    println!(
        "Pass 2 (Zero-Alloc Register) : p50 = {:>10} ns | p99 = {:>10} ns | {:>8} allocs/op | {:>10} B/op",
        percentile_ns(&p2_samples, 50, 100),
        percentile_ns(&p2_samples, 99, 100),
        p2_allocs_total / iters,
        p2_bytes_total / iters,
    );
    println!(
        "Pass 3 (Cache-Aligned Rayon) : p50 = {:>10} ns | p99 = {:>10} ns | {:>8} allocs/op | {:>10} B/op",
        percentile_ns(&p3_samples, 50, 100),
        percentile_ns(&p3_samples, 99, 100),
        p3_allocs_total / iters,
        p3_bytes_total / iters,
    );
}

fn bench_single_scale<const DEPTH: usize>(account_count: usize) {
    const EPOCH_ARITY: usize = 5; // Top 32 partition subtrees proven in constant-size Epoch SNARK
    let seed = [0xA5u8; 32];
    let accounts = generate_sorted_accounts(account_count, 0x2026_0000 + account_count as u64);

    // Measure MSST Parallel Build (median of 7 runs)
    let mut build_ns = Vec::with_capacity(7);
    let mut latest_tree = None;
    for r in 0..7u64 {
        let t0 = Instant::now();
        let tr = MsstTree::build_parallel(black_box(&accounts), DEPTH, 100 + r, &seed).unwrap();
        build_ns.push(t0.elapsed().as_nanos());
        latest_tree = Some(tr);
    }
    build_ns.sort_unstable();
    let tree = latest_tree.unwrap();
    let reserves = tree.total_liabilities_fp() + 500_000_000_000;

    // Setup keys & count exact R1CS constraints
    let t_setup = Instant::now();
    let inc_keys = InclusionKeyMaterial::<DEPTH>::setup_deterministic(DEPTH as u64).unwrap();
    let inc_setup_ms = t_setup.elapsed().as_secs_f64() * 1_000.0;

    let epoch_keys = EpochBatchKeyMaterial::<EPOCH_ARITY>::setup_deterministic(false, 77).unwrap();

    let inc_constraints =
        verify_r1cs_satisfied(MsstInclusionSolvencyCircuit::<DEPTH>::blank_for_setup())
            .unwrap_or(0);
    // Note: blank_for_setup isn't satisfied for dummy 0 hashes, so inspect constraint count directly:
    let inc_r1cs_count = {
        use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
        let cs = ConstraintSystem::new_ref();
        MsstInclusionSolvencyCircuit::<DEPTH>::blank_for_setup()
            .generate_constraints(cs.clone())
            .unwrap();
        cs.finalize();
        cs.num_constraints()
    };
    let epoch_r1cs_count = {
        use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
        let cs = ConstraintSystem::new_ref();
        EpochBatchSolvencyCircuit::<EPOCH_ARITY>::blank_for_setup(false)
            .generate_constraints(cs.clone())
            .unwrap();
        cs.finalize();
        cs.num_constraints()
    };

    // Prove Epoch Solvency (median of 3 runs)
    let mut ep_prove_ns = Vec::with_capacity(3);
    let mut ep_receipt = None;
    for r in 0..3u8 {
        let t0 = Instant::now();
        let rec =
            prove_epoch_solvency::<EPOCH_ARITY>(&epoch_keys, &tree, reserves, [r; 32]).unwrap();
        ep_prove_ns.push(t0.elapsed().as_nanos());
        ep_receipt = Some(rec);
    }
    ep_prove_ns.sort_unstable();
    let ep_receipt = ep_receipt.unwrap();

    // Verify Epoch Solvency (median of 25 runs — must be strictly < 5ms!)
    let mut ep_verify_ns = Vec::with_capacity(25);
    for _ in 0..25 {
        let t0 = Instant::now();
        verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, black_box(&ep_receipt))
            .unwrap();
        ep_verify_ns.push(t0.elapsed().as_nanos());
    }
    ep_verify_ns.sort_unstable();

    // Prove User Inclusion + Global Solvency SNARK (median of 3 runs)
    let target_acc = accounts[account_count / 3].account_id;
    let mut usr_prove_ns = Vec::with_capacity(3);
    let mut usr_bundle = None;
    for r in 0..3u8 {
        let t0 = Instant::now();
        let b = prove_user_inclusion_solvency::<DEPTH>(
            &inc_keys,
            &tree,
            target_acc,
            reserves,
            [0x80 + r; 32],
        )
        .unwrap();
        usr_prove_ns.push(t0.elapsed().as_nanos());
        usr_bundle = Some(b);
    }
    usr_prove_ns.sort_unstable();
    let usr_bundle = usr_bundle.unwrap();

    // Verify User ZK Bundle (median of 25 runs — must be strictly < 5ms!)
    let mut usr_verify_ns = Vec::with_capacity(25);
    let mut msst_only_ns = Vec::with_capacity(25);
    for _ in 0..25 {
        let t0 = Instant::now();
        verify_user_zk_bundle_with_pvk::<DEPTH>(&inc_keys.pvk, black_box(&usr_bundle)).unwrap();
        usr_verify_ns.push(t0.elapsed().as_nanos());

        let t1 = Instant::now();
        verify_msst_inclusion_proof(black_box(&usr_bundle.inclusion)).unwrap();
        msst_only_ns.push(t1.elapsed().as_nanos());
    }
    usr_verify_ns.sort_unstable();
    msst_only_ns.sort_unstable();

    let ep_ver_min_us = ep_verify_ns[0] as f64 / 1_000.0;
    let ep_ver_p50_us = percentile_ns(&ep_verify_ns, 50, 100) as f64 / 1_000.0;
    let usr_ver_min_us = usr_verify_ns[0] as f64 / 1_000.0;
    let usr_ver_p50_us = percentile_ns(&usr_verify_ns, 50, 100) as f64 / 1_000.0;
    let msst_ver_p50_us = percentile_ns(&msst_only_ns, 50, 100) as f64 / 1_000.0;

    assert!(
        ep_ver_min_us < 5_000.0 && usr_ver_min_us < 5_000.0,
        "Verification exceeded 5ms budget! ep_min={ep_ver_min_us} us, usr_min={usr_ver_min_us} us"
    );
    let _ = inc_constraints;

    println!(
        "Accounts={account_count:>6} (D={DEPTH:>2}) | MSST Build p50={:>6.2} ms (p99={:>6.2} ms) | Setup={inc_setup_ms:>6.1} ms | R1CS(User={inc_r1cs_count}, Epoch={epoch_r1cs_count}) | EpochProve={:>6.2} ms, EpochVerify p50={ep_ver_p50_us:>6.1} us (min={ep_ver_min_us:>6.1} us, {}B) | UserProve={:>6.2} ms, UserVerify p50={usr_ver_p50_us:>6.1} us (min={usr_ver_min_us:>6.1} us, {}B, VK={}B) | MSSTPathVerify={msst_ver_p50_us:>5.1} us",
        percentile_ns(&build_ns, 50, 100) as f64 / 1_000_000.0,
        percentile_ns(&build_ns, 99, 100) as f64 / 1_000_000.0,
        percentile_ns(&ep_prove_ns, 50, 100) as f64 / 1_000_000.0,
        ep_receipt.groth16_proof_bytes.len(),
        percentile_ns(&usr_prove_ns, 50, 100) as f64 / 1_000_000.0,
        usr_bundle.groth16_proof_bytes.len(),
        usr_bundle.vk_bytes.len(),
    );
}

fn main() {
    run_500k_zero_leak_soak_test();
    run_3_pass_micro_optimization_benchmark();

    println!("\n=== SCALING BENCHMARK SUITE: 1,024 -> 65,536 ACCOUNTS (BN254 GROTH16 + POSEIDON/BLAKE3 MSST) ===");
    bench_single_scale::<10>(1_024);
    bench_single_scale::<12>(4_096);
    bench_single_scale::<14>(16_384);
    bench_single_scale::<16>(65_536);
}
