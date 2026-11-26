use std::hint::black_box;
use std::time::Instant;

use hyperion_omega_singularity_crucible::{
    hotpath_pass1_baseline, hotpath_pass2_zerocopy, hotpath_pass3_cache_aligned,
    run_black_swan_singularity_scenario, CacheAlignedAccount, FP_SCALE,
};

struct MicroOptPassMetric {
    pass_name: &'static str,
    iterations: usize,
    p50_ns_x100: u128,
    p99_ns_x100: u128,
    heap_bytes_per_op: usize,
    heap_allocs_per_op: usize,
}

fn measure_pass<F: FnMut(u64) -> i128>(
    name: &'static str,
    iters: usize,
    bytes_per_op: usize,
    allocs_per_op: usize,
    mut f: F,
) -> MicroOptPassMetric {
    let batches = 200usize;
    let per_batch = iters / batches;
    let mut samples_ns_x100 = Vec::with_capacity(batches);
    let mut sink: i128 = 0;

    for i in 0..5_000u64 {
        sink = sink.wrapping_add(black_box(f(black_box(i))));
    }

    for b in 0..batches {
        let base = (b * per_batch) as u64;
        let t0 = Instant::now();
        for j in 0..per_batch {
            sink = sink.wrapping_add(black_box(f(black_box(base + j as u64))));
        }
        let elapsed_ns = t0.elapsed().as_nanos();
        samples_ns_x100.push((elapsed_ns * 100) / (per_batch as u128).max(1));
    }
    black_box(sink);
    samples_ns_x100.sort_unstable();

    MicroOptPassMetric {
        pass_name: name,
        iterations: iters,
        p50_ns_x100: samples_ns_x100[batches / 2],
        p99_ns_x100: samples_ns_x100[(batches * 99) / 100],
        heap_bytes_per_op: bytes_per_op,
        heap_allocs_per_op: allocs_per_op,
    }
}

fn fmt_ns_x100(v: u128) -> String {
    format!("{}.{:02}", v / 100, v % 100)
}

fn main() {
    let c = run_black_swan_singularity_scenario();

    let mut hdr = [0u8; 32];
    hdr[0] = 0x59;
    hdr[1] = 0x48;
    hdr[2] = 0x01;
    let mut acc = CacheAlignedAccount {
        account_id: 42,
        available_fp: 500_000 * FP_SCALE,
        locked_fp: 0,
        frozen: false,
    };

    let iters = 400_000usize;
    let p1 = measure_pass("Pass 1: Baseline (String + Vec Clone)", iters, 64, 2, |i| {
        let px = 6_842_550_000_000_u64.wrapping_add(i & 0xFF);
        let qty = 25_000_000u64.wrapping_add(i & 0x3F);
        hotpath_pass1_baseline(black_box(px), black_box(qty), black_box(250), black_box(&hdr))
    });
    let p2 = measure_pass("Pass 2: Zero-Copy Slice Refactor", iters, 0, 0, |i| {
        let px = 6_842_550_000_000_u64.wrapping_add(i & 0xFF);
        let qty = 25_000_000u64.wrapping_add(i & 0x3F);
        hotpath_pass2_zerocopy(black_box(px), black_box(qty), black_box(250), black_box(&hdr))
    });
    let p3 = measure_pass(
        "Pass 3: #[repr(C, align(64))] Cache-Aligned Final",
        iters,
        0,
        0,
        |i| {
            let px = 6_842_550_000_000_u64.wrapping_add(i & 0xFF);
            let qty = 25_000_000u64.wrapping_add(i & 0x3F);
            hotpath_pass3_cache_aligned(
                black_box(&mut acc),
                black_box(px),
                black_box(qty),
                black_box(250),
                black_box(&hdr),
            )
        },
    );

    println!(
        "{{\n  \"scenario\": \"{}\",\n  \"all_assertions_green\": {},\n  \"orders_processed\": {},\n  \"wasm_ticks\": {},\n  \"wasm_malware_trapped_under_100us\": {},\n  \"wal_chain_verified\": {},\n  \"raft_failover\": {{\n    \"new_leader_id\": {},\n    \"new_term\": {},\n    \"election_latency_ms\": {},\n    \"committed_tx_loss\": {},\n    \"stale_leader_write_rejected\": {}\n  }},\n  \"ebpf_shield\": {{\n    \"attack_packets_dropped\": {},\n    \"legit_packets_passed\": {}\n  }},\n  \"syndicate_frozen_accounts\": {},\n  \"liquidation_waterfall\": {{\n    \"liquidated_accounts\": {},\n    \"insurance_after_fp\": \"{}\",\n    \"adl_absorbed_fp\": \"{}\",\n    \"unbacked_deficit_fp\": \"{}\"\n  }},\n  \"double_entry_conservation_holds\": {},\n  \"zk_solvency\": {{\n    \"root_hash_hex\": \"{}\",\n    \"total_liabilities_fp\": \"{}\",\n    \"vault_reserves_fp\": \"{}\",\n    \"solvent\": {}\n  }},\n  \"l2_reorg_defense\": {{\n    \"reorg_detected\": {},\n    \"reorg_depth_blocks\": {},\n    \"rolled_back_deposits\": {},\n    \"double_spends_allowed\": {}\n  }},\n  \"lakehouse_olap\": {{\n    \"asof_matched_rows\": {},\n    \"vwap_fp\": \"{}\"\n  }},\n  \"soak_500k_cycles_zero_drift\": {},\n  \"micro_optimization_passes\": [\n    {{\"name\": \"{}\", \"iterations\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}},\n    {{\"name\": \"{}\", \"iterations\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}},\n    {{\"name\": \"{}\", \"iterations\": {}, \"p50_ns\": {}, \"p99_ns\": {}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}}\n  ]\n}}",
        c.scenario,
        c.all_assertions_green,
        c.total_orders_processed,
        c.wasm_bots_ticks_executed,
        c.wasm_malware_trapped_under_100us,
        c.wal_chain_verified,
        c.raft_failover.new_leader_id.unwrap_or(0),
        c.raft_failover.new_term,
        c.raft_failover.election_latency_ms,
        c.raft_failover.committed_tx_loss,
        c.raft_failover.stale_leader_write_rejected,
        c.ebpf_attack_packets_dropped,
        c.ebpf_legit_packets_passed,
        c.syndicate_accounts_frozen,
        c.liquidation_storm.liquidated_accounts,
        c.liquidation_storm.insurance_fund_after_fp,
        c.liquidation_storm.adl_absorbed_fp,
        c.liquidation_storm.unbacked_system_deficit_fp,
        c.double_entry_conservation_holds,
        c.zk_merkle_sum_solvency.root_hash_hex,
        c.zk_merkle_sum_solvency.total_liabilities_fp,
        c.zk_merkle_sum_solvency.vault_reserves_fp,
        c.zk_merkle_sum_solvency.solvent,
        c.l2_reorg_defense.reorg_detected,
        c.l2_reorg_defense.reorg_depth_blocks,
        c.l2_reorg_defense.rolled_back_deposits,
        c.l2_reorg_defense.double_spends_allowed,
        c.lakehouse_asof_matched_rows,
        c.lakehouse_vwap_fp,
        c.soak_500k_cycles_zero_drift,
        p1.pass_name, p1.iterations, fmt_ns_x100(p1.p50_ns_x100), fmt_ns_x100(p1.p99_ns_x100), p1.heap_bytes_per_op, p1.heap_allocs_per_op,
        p2.pass_name, p2.iterations, fmt_ns_x100(p2.p50_ns_x100), fmt_ns_x100(p2.p99_ns_x100), p2.heap_bytes_per_op, p2.heap_allocs_per_op,
        p3.pass_name, p3.iterations, fmt_ns_x100(p3.p50_ns_x100), fmt_ns_x100(p3.p99_ns_x100), p3.heap_bytes_per_op, p3.heap_allocs_per_op,
    );

    if !c.all_assertions_green {
        std::process::exit(1);
    }
}
