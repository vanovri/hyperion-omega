use consensus_raft_rs::opt_rounds::run_three_round_optimization_benchmark;
use consensus_raft_rs::quorum::AlignedQuorumTracker;
use consensus_raft_rs::types::ClusterConfig;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::collections::BTreeMap;

fn bench_raft_quorum_and_wal(c: &mut Criterion) {
    let cfg = ClusterConfig::uniform([1, 2, 3, 4, 5]);
    let init_map: BTreeMap<u64, u64> = (1..=5).map(|id| (id, id * 10)).collect();
    let mut tracker = AlignedQuorumTracker::new();
    tracker.rebuild_from_config(&cfg, &init_map);

    c.bench_function("aligned_quorum_5node_commit_calc", |b| {
        let mut step = 100u64;
        b.iter(|| {
            step = step.wrapping_add(1);
            tracker.update_match_index((step % 5) + 1, step);
            black_box(tracker.committed_quorum_index(black_box(&cfg)))
        });
    });

    c.bench_function("three_round_hot_path_1024_ops", |b| {
        b.iter(|| black_box(run_three_round_optimization_benchmark(1024)));
    });
}

criterion_group!(benches, bench_raft_quorum_and_wal);
criterion_main!(benches);
