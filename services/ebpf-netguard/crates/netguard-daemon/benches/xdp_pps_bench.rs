use netguard_common::parser::TCP_ACK;
use netguard_common::{BlockRuleVal, RuleAction, XdpAction};
use netguard_daemon::ebpf_vm::build_canonical_xdp_program;
use netguard_daemon::lpm::{parse_cidr_spec, CidrEntry};
use netguard_daemon::packet_forge::{
    build_hyperion_wire_payload, build_ipv4_tcp_frame, generate_10k_spoofed_syn_storm,
};
use netguard_daemon::pipeline::{BaselineMutexPipeline, NetGuardSharedState, XdpWorkerEngine};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct CountingAlloc;

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL_ALLOC: CountingAlloc = CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }
}

fn snapshot_alloc() -> (usize, usize, usize) {
    (
        ALLOC_COUNT.load(Ordering::SeqCst),
        ALLOC_BYTES.load(Ordering::SeqCst),
        LIVE_BYTES.load(Ordering::SeqCst),
    )
}

fn compute_batch_quantiles(mut samples_ns_per_pkt: Vec<u64>) -> (u64, u64) {
    samples_ns_per_pkt.sort_unstable();
    let n = samples_ns_per_pkt.len();
    let p50 = samples_ns_per_pkt[n / 2];
    let p99 = samples_ns_per_pkt[(n * 99) / 100];
    (p50, p99)
}

fn main() {
    let wire_hb = build_hyperion_wire_payload(0x01, 0, 1, 100, 1_700_000_000_000_000_000, &[0; 16]);
    let valid_pkt = build_ipv4_tcp_frame(
        [172, 16, 20, 9],
        [10, 0, 0, 1],
        50123,
        8080,
        100,
        200,
        TCP_ACK,
        &wire_hb,
    );
    let drop_pkt = build_ipv4_tcp_frame(
        [203, 0, 113, 42],
        [10, 0, 0, 1],
        50123,
        8080,
        100,
        200,
        TCP_ACK,
        &wire_hb,
    );

    let rule = BlockRuleVal {
        action: RuleAction::HardDrop,
        rate_override_rps: 0,
        expires_ts_ns: 0,
        rule_generation: 1,
    };
    let cidrs = [
        ("203.0.113.0/24", rule),
        ("198.51.100.0/24", rule),
        ("192.0.2.0/24", rule),
        ("45.33.0.0/16", rule),
        ("185.220.101.0/24", rule),
        ("91.240.118.0/24", rule),
        ("103.27.124.0/22", rule),
        ("194.26.29.0/24", rule),
    ];

    let base_ns = 1_700_000_000_000_000_000_u64;
    const BATCH: u64 = 10_000;

    // -------------------------------------------------------------------------
    // STAGE 1: Baseline (Mutex + Vec Copy + Linear CIDR Scan)
    // -------------------------------------------------------------------------
    let b1 = BaselineMutexPipeline::new(&cidrs);
    let batches1 = 50_usize; // 500,000 packets
    let mut lat1 = Vec::with_capacity(batches1);
    let (a1_c0, a1_b0, _) = snapshot_alloc();
    let s1 = Instant::now();
    let mut acc1 = 0_u64;
    for b in 0..batches1 {
        let bt = Instant::now();
        for i in 0..BATCH {
            let idx = (b as u64) * BATCH + i;
            let p = if (idx & 3) == 0 { &drop_pkt } else { &valid_pkt };
            acc1 += b1.process_baseline(p, base_ns + idx) as u64;
        }
        lat1.push(bt.elapsed().as_nanos() as u64 / BATCH);
    }
    let d1_ns = s1.elapsed().as_nanos() as u64;
    let (a1_c1, a1_b1, _) = snapshot_alloc();
    let n1 = (batches1 as u64) * BATCH;
    let (p50_1, p99_1) = compute_batch_quantiles(lat1);

    // Setup Shared State for Stages 2, 3, and BPF VM
    let shared = NetGuardSharedState::new("eth0", 1);
    let entries: Vec<CidrEntry> = cidrs
        .iter()
        .map(|(c, r)| {
            let (ip, prefix_len) = parse_cidr_spec(c).unwrap();
            CidrEntry {
                cidr_text: (*c).to_string(),
                ip,
                prefix_len,
                rule: *r,
                rollback_token: "bench".to_string(),
            }
        })
        .collect();
    shared.policy_store.upsert_batch(entries, Vec::new());

    let batches = 500_usize; // 5,000,000 packets
    let n = (batches as u64) * BATCH;
    let mut bv = valid_pkt.clone();
    let mut bd = drop_pkt.clone();

    // -------------------------------------------------------------------------
    // STAGE 2: Zero-Copy Slice Dissector + Bitwise Binary Radix LPM Trie
    // -------------------------------------------------------------------------
    let mut w_radix = XdpWorkerEngine::new(0, shared.clone());
    w_radix.use_radix_for_ipv4 = true;
    // Warmup
    let _ = w_radix.process_ingress_frame(&mut bv, base_ns);
    let mut lat2 = Vec::with_capacity(batches);
    let (a2_c0, a2_b0, _) = snapshot_alloc();
    let s2 = Instant::now();
    let mut acc2 = 0_u64;
    for b in 0..batches {
        let bt = Instant::now();
        for i in 0..BATCH {
            let idx = (b as u64) * BATCH + i;
            let v = if (idx & 3) == 0 {
                w_radix.process_ingress_frame(&mut bd, base_ns + idx)
            } else {
                w_radix.process_ingress_frame(&mut bv, base_ns + idx)
            };
            acc2 += v.action as u64;
        }
        lat2.push(bt.elapsed().as_nanos() as u64 / BATCH);
    }
    let d2_ns = s2.elapsed().as_nanos() as u64;
    let (a2_c1, a2_b1, _) = snapshot_alloc();
    let (p50_2, p99_2) = compute_batch_quantiles(lat2);

    // -------------------------------------------------------------------------
    // STAGE 3: Cache-Line Aligned (`#[repr(align(64))]`) + O(1) DIR-16-8-8 RCU
    // -------------------------------------------------------------------------
    let mut w_dir16 = XdpWorkerEngine::new(0, shared);
    w_dir16.use_radix_for_ipv4 = false;
    let _ = w_dir16.process_ingress_frame(&mut bv, base_ns);
    let mut lat3 = Vec::with_capacity(batches);
    let (a3_c0, a3_b0, _) = snapshot_alloc();
    let s3 = Instant::now();
    let mut acc3 = 0_u64;
    for b in 0..batches {
        let bt = Instant::now();
        for i in 0..BATCH {
            let idx = (b as u64) * BATCH + i;
            let v = if (idx & 3) == 0 {
                w_dir16.process_ingress_frame(&mut bd, base_ns + idx)
            } else {
                w_dir16.process_ingress_frame(&mut bv, base_ns + idx)
            };
            acc3 += v.action as u64;
        }
        lat3.push(bt.elapsed().as_nanos() as u64 / BATCH);
    }
    let d3_ns = s3.elapsed().as_nanos() as u64;
    let (a3_c1, a3_b1, _) = snapshot_alloc();
    let (p50_3, p99_3) = compute_batch_quantiles(lat3);

    // -------------------------------------------------------------------------
    // STAGE 4: Userspace eBPF 64-Bit ISA Bytecode VM (`BpfVm::execute_xdp`)
    // -------------------------------------------------------------------------
    let prog = build_canonical_xdp_program();
    let batches_vm = 250_usize; // 2,500,000 packets
    let n_vm = (batches_vm as u64) * BATCH;
    let mut lat_vm = Vec::with_capacity(batches_vm);
    let (avm_c0, avm_b0, _) = snapshot_alloc();
    let s_vm = Instant::now();
    let mut acc_vm = 0_u64;
    for b in 0..batches_vm {
        let bt = Instant::now();
        for i in 0..BATCH {
            let idx = (b as u64) * BATCH + i;
            let v = if (idx & 3) == 0 {
                w_dir16.process_ingress_via_bpf_vm(&prog, &mut bd, base_ns + idx)
            } else {
                w_dir16.process_ingress_via_bpf_vm(&prog, &mut bv, base_ns + idx)
            };
            acc_vm += v.action as u64;
            debug_assert!(v.action == XdpAction::Pass || v.action == XdpAction::Drop);
        }
        lat_vm.push(bt.elapsed().as_nanos() as u64 / BATCH);
    }
    let d_vm_ns = s_vm.elapsed().as_nanos() as u64;
    let (avm_c1, avm_b1, _) = snapshot_alloc();
    let (p50_vm, p99_vm) = compute_batch_quantiles(lat_vm);

    // -------------------------------------------------------------------------
    // 500,000-CYCLE ZERO-LEAK SOAK TEST (10,000 Spoofed Random IP SYN Storm)
    // -------------------------------------------------------------------------
    let mut storm = generate_10k_spoofed_syn_storm(0xA110_CA7E);
    // Warmup 10k
    for (i, pkt) in storm.iter_mut().enumerate() {
        let _ = w_dir16.process_ingress_frame(pkt, base_ns + i as u64);
    }
    let (soak_c0, soak_b0, live_before) = snapshot_alloc();
    let soak_t0 = Instant::now();
    let mut soak_drops = 0_u64;
    for cycle in 0..500_000_usize {
        let pkt = &mut storm[cycle % 10_000];
        let v = w_dir16.process_ingress_frame(pkt, base_ns + 1_000_000 + (cycle as u64 * 10));
        if v.action == XdpAction::Drop {
            soak_drops += 1;
        }
    }
    let soak_ms = soak_t0.elapsed().as_millis();
    let (soak_c1, soak_b1, live_after) = snapshot_alloc();

    println!("=== OVERCLOCK BENCHMARK & ZERO-LEAK SOAK REPORT ===");
    println!(
        "Stage 1 (Baseline Mutex+Copy+Linear): avg={} ns/pkt | p50={} ns | p99={} ns | {} kPPS/core | allocs/op={} | B/op={} (acc={})",
        d1_ns / n1,
        p50_1,
        p99_1,
        (n1 as u128 * 1_000_000) / (d1_ns.max(1) as u128),
        (a1_c1 - a1_c0) / (n1 as usize),
        (a1_b1 - a1_b0) / (n1 as usize),
        acc1
    );
    println!(
        "Stage 2 (ZeroCopy + Radix LPM)      : avg={} ns/pkt | p50={} ns | p99={} ns | {} kPPS/core | allocs/op={} | B/op={} (acc={})",
        d2_ns / n,
        p50_2,
        p99_2,
        (n as u128 * 1_000_000) / (d2_ns.max(1) as u128),
        (a2_c1 - a2_c0) / (n as usize),
        (a2_b1 - a2_b0) / (n as usize),
        acc2
    );
    println!(
        "Stage 3 (CacheAligned 64B + DIR16)  : avg={} ns/pkt | p50={} ns | p99={} ns | {} kPPS/core | allocs/op={} | B/op={} (acc={})",
        d3_ns / n,
        p50_3,
        p99_3,
        (n as u128 * 1_000_000) / (d3_ns.max(1) as u128),
        (a3_c1 - a3_c0) / (n as usize),
        (a3_b1 - a3_b0) / (n as usize),
        acc3
    );
    println!(
        "Stage 4 (Userspace eBPF Bytecode VM): avg={} ns/pkt | p50={} ns | p99={} ns | {} kPPS/core | allocs/op={} | B/op={} (acc={})",
        d_vm_ns / n_vm,
        p50_vm,
        p99_vm,
        (n_vm as u128 * 1_000_000) / (d_vm_ns.max(1) as u128),
        (avm_c1 - avm_c0) / (n_vm as usize),
        (avm_b1 - avm_b0) / (n_vm as usize),
        acc_vm
    );
    println!(
        "Soak 500,000 cycles (10k Spoofed IP): {} ms | drops={}/500000 | allocs_during_soak={} | bytes_alloc={} | live_heap_delta={} B",
        soak_ms,
        soak_drops,
        soak_c1 - soak_c0,
        soak_b1 - soak_b0,
        live_after as isize - live_before as isize
    );
    assert_eq!(soak_c1 - soak_c0, 0, "hot-path soak MUST allocate 0 times!");
    assert_eq!(live_after, live_before, "steady-state heap leak MUST be 0 bytes!");
}
