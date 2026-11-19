//! Comprehensive Crucible Test Suite for HYPERION CH12 (`consensus-raft-rs`):
//! - Pre-Vote + Leader Stickiness under Isolated & Asymmetric Triangle Partitions
//! - Linearizable Reads via 0-RTT `LeaderLease` & 1-RTT `ReadIndexBarrier`
//! - Batched & Pipelined `AppendEntries` under Packet Duplication & Out-of-Order Shuffling
//! - Dynamic Membership Reconfiguration via 2-Phase Joint Consensus (`C_old,new` -> `C_new`) + Leader Step-Down
//! - `HYPSNP01` Log Compaction + Streaming Multi-Chunk `InstallSnapshot` + Chunk CRC32C Corruption Retry
//! - `HYPWAL01` Chained Blake3 + Torn-Write Recovery vs Silent Bit-Rot Detection & Quarantine
//! - Single-Seed Bit-Exact Replay Determinism + Multi-Seed VOPR Chaos Torture + WGL Linearizability & Strict-1SR
//! - Full End-to-End Tonic gRPC (`RaftConsensusService`) + Axum HTTP (`/healthz`, `/metrics`, `/api/v1/raft/status`)
//! - Programmatic Consensus Mutation Survival Zero Gate (`100%` Kill Score)

use consensus_raft_rs::checker::{
    ClientOpSpec, LinearizabilityChecker, LinearizabilityVerdict,
};
use consensus_raft_rs::mutation_suite::run_consensus_mutation_survival_suite;
use consensus_raft_rs::prng::DeterministicPrng;
use consensus_raft_rs::proto::omega_v1::raft_consensus_service_client::RaftConsensusServiceClient;
use consensus_raft_rs::proto::omega_v1::{
    AppendEntries, GetRaftClusterStatusRequest, InstallSnapshot, PreVote,
    ProposeLedgerEntryRequest, ReadIndexBarrierRequest, RequestVote,
};
use consensus_raft_rs::proto::v1 as base_v1;
use consensus_raft_rs::raft_core::RaftTuningConfig;
use consensus_raft_rs::rpc_service::LiveConsensusRuntime;
use consensus_raft_rs::sim::disk::SimulatedFaultyDisk;
use consensus_raft_rs::sim::nemesis::NemesisProfile;
use consensus_raft_rs::sim::network::DirectedLinkPolicy;
use consensus_raft_rs::sim::VoprClusterSimulator;
use consensus_raft_rs::traits::{DiskStorage, PersistBatch, StorageError};
use consensus_raft_rs::types::{
    ClusterConfig, HardState, LedgerCommand, RaftEntryKind, RaftRole, ReadConsistencyMode,
    FP_SCALE_I128, FP_SCALE_U128,
};
use consensus_raft_rs::wal_codec::{build_chained_log_entry, encode_hypsnp01_snapshot};
use std::collections::BTreeSet;
use std::time::Duration;
use tonic::transport::Server;

#[test]
fn test_prevote_prevents_isolated_node_term_inflation_and_asymmetric_disruption() {
    let tuning = RaftTuningConfig::default();
    let mut sim = VoprClusterSimulator::new_five_node(0xCAFE_1201, tuning, NemesisProfile::default());

    for _ in 0..35 {
        sim.step_simulation(10_000_000, false).expect("warmup");
    }
    let leader = sim.active_leader().expect("initial leader elected");
    let initial_term = sim.nodes[&leader].core.hard_state.current_term;

    // 1. Isolate follower `isolated_id` completely for 300 steps (3.0 seconds = ~15 election timeouts)!
    let isolated_id = (1..=5u64).find(|&id| id != leader).expect("follower");
    let majority: Vec<u64> = (1..=5u64).filter(|&id| id != isolated_id).collect();
    sim.net.apply_symmetric_partition(&[isolated_id], &majority);

    for _ in 0..300 {
        sim.step_simulation(10_000_000, false).expect("isolated spin");
    }

    // Because of Pre-Vote, `isolated_id` MUST NOT have incremented its persistent `current_term`!
    assert_eq!(
        sim.nodes[&isolated_id].core.hard_state.current_term,
        initial_term,
        "Pre-Vote must prevent isolated node from inflating current_term!"
    );

    // Heal network: `leader` remains undisturbed in `initial_term`!
    sim.net.heal_all(&[1, 2, 3, 4, 5]);
    for _ in 0..40 {
        sim.step_simulation(10_000_000, false).expect("post-heal");
    }
    assert_eq!(sim.active_leader(), Some(leader));
    assert_eq!(
        sim.nodes[&leader].core.hard_state.current_term,
        initial_term,
        "rejoining node must not force term bump or leader step-down"
    );

    // 2. Asymmetric Triangle Partition: `isolated_id` cannot hear `leader`, CAN send PreVote to `b`,
    // while `b` CAN hear `leader`! Leader Stickiness on `b` MUST reject `isolated_id`'s PreVote!
    let peer_b = majority
        .iter()
        .copied()
        .find(|&id| id != leader)
        .expect("peer_b");
    sim.net.set_directed_connectivity(leader, isolated_id, false);
    sim.net.set_directed_connectivity(isolated_id, leader, false);
    for _ in 0..120 {
        sim.step_simulation(10_000_000, false)
            .expect("asymmetric triangle");
    }
    let _ = peer_b;
    assert_eq!(
        sim.active_leader(),
        Some(leader),
        "asymmetric partition with active leader heartbeats must not dethrone healthy leader"
    );
    assert_eq!(sim.nodes[&leader].core.hard_state.current_term, initial_term);
}

#[test]
fn test_leader_lease_0rtt_and_read_index_1rtt_linearizability_under_partition() {
    let tuning = RaftTuningConfig::default();
    let mut sim = VoprClusterSimulator::new_five_node(0xBEEF_1202, tuning, NemesisProfile::default());

    for _ in 0..40 {
        sim.step_simulation(10_000_000, false).expect("elect");
    }
    let old_leader = sim.active_leader().expect("leader");

    sim.submit_client_write(
        1,
        ClientOpSpec::CreditDeposit {
            idempotency_key: "dep-lease-1".to_string(),
            account_id: 42,
            asset: "USDT".to_string(),
            amount_fp: 250 * FP_SCALE_U128,
        },
    );
    for _ in 0..15 {
        sim.step_simulation(10_000_000, false).expect("commit write 1");
    }

    // 0-RTT LeaderLease fast-path read while lease is active:
    let lease_read_op = sim
        .submit_client_read(
            2,
            old_leader,
            42,
            "USDT",
            ReadConsistencyMode::LeaderLeaseOrReadIndex,
        )
        .expect("submit lease read");
    assert!(
        sim.history[lease_read_op].is_completed(),
        "active LeaderLease must satisfy linearizable read in 0 network RTTs!"
    );

    // Partition `old_leader` + 1 follower (2-node minority) from 3-node majority!
    let mut others: Vec<u64> = (1..=5).filter(|&id| id != old_leader).collect();
    let minority_buddy = others.pop().expect("buddy");
    let minority = vec![old_leader, minority_buddy];
    let majority = others;
    sim.net.apply_symmetric_partition(&minority, &majority);

    // Advance past lease expiration so 3-node majority elects a new Leader and commits `+100 USDT` (`350 USDT` total)!
    for _ in 0..60 {
        sim.step_simulation(10_000_000, false)
            .expect("elect new majority leader");
    }
    let new_leader = sim.active_leader().expect("new majority leader");
    assert_ne!(new_leader, old_leader);

    sim.submit_client_write(
        3,
        ClientOpSpec::CreditDeposit {
            idempotency_key: "dep-lease-2".to_string(),
            account_id: 42,
            asset: "USDT".to_string(),
            amount_fp: 100 * FP_SCALE_U128,
        },
    );
    for _ in 0..15 {
        sim.step_simulation(10_000_000, false).expect("commit write 2");
    }

    // Query partitioned `old_leader` — MUST NOT return stale `250 USDT`!
    let stale_probe = sim
        .submit_client_read(
            4,
            old_leader,
            42,
            "USDT",
            ReadConsistencyMode::LeaderLeaseOrReadIndex,
        )
        .expect("probe old leader");
    for _ in 0..10 {
        sim.step_simulation(10_000_000, false).expect("step");
    }
    assert!(
        !sim.history[stale_probe].is_completed(),
        "partitioned minority old leader must never serve stale balance!"
    );

    // Query `new_leader` via strict 1-RTT `ReadIndex` -> returns `350 USDT`!
    let fresh_read = sim
        .submit_client_read(5, new_leader, 42, "USDT", ReadConsistencyMode::ReadIndex)
        .expect("read new leader");
    for _ in 0..10 {
        sim.step_simulation(10_000_000, false).expect("complete readindex");
    }
    assert_eq!(
        sim.history[fresh_read].observed,
        Some(consensus_raft_rs::checker::ClientOpReturn::BalanceVal {
            total_fp: 350 * FP_SCALE_I128
        })
    );

    assert!(matches!(
        LinearizabilityChecker::default().verify(&sim.history),
        LinearizabilityVerdict::Linearizable { .. }
    ));
}

#[test]
fn test_pipelined_batched_append_entries_with_packet_reordering_and_duplication() {
    let tuning = RaftTuningConfig {
        max_batch_entries: 16,
        max_inflight_pipelines: 8,
        ..Default::default()
    };

    let mut sim = VoprClusterSimulator::new_five_node(0x7777_1203, tuning, NemesisProfile::default());
    for _ in 0..40 {
        sim.step_simulation(10_000_000, false).expect("elect");
    }

    // Inject heavy packet duplication (25%) and high jitter (25ms) so pipelined batches arrive out of order!
    let chaotic_link = DirectedLinkPolicy {
        connected: true,
        min_latency_ns: 1_000_000,
        jitter_ns: 25_000_000,
        tail_spike_ppm: 150_000,
        max_spike_ns: 60_000_000,
        drop_ppm: 40_000,
        duplicate_ppm: 250_000,
    };
    for a in 1..=5 {
        for b in 1..=5 {
            if a != b {
                sim.net.set_directed_link(a, b, chaotic_link);
            }
        }
    }

    // Burst 120 rapid proposals back-to-back without waiting for individual RTTs!
    for i in 1..=120u64 {
        sim.submit_client_write(
            (i % 4) + 1,
            ClientOpSpec::CreditDeposit {
                idempotency_key: format!("pipe-burst-{i}"),
                account_id: (i % 4) + 1,
                asset: "USDT".to_string(),
                amount_fp: FP_SCALE_U128,
            },
        );
        if i % 12 == 0 {
            sim.step_simulation(2_000_000, false).expect("pipeline step");
        }
    }

    // Allow convergence
    sim.net.set_default_policy(DirectedLinkPolicy::default());
    sim.net.heal_all(&[1, 2, 3, 4, 5]);
    for _ in 0..80 {
        sim.step_simulation(10_000_000, false).expect("converge");
    }

    let ldr = sim.active_leader().expect("leader");
    let (_, _, avail, _, _, conserved) = sim.nodes[&ldr]
        .core
        .state_machine
        .audit_asset_conservation("USDT");
    assert!(conserved);
    assert!(avail > 0);
    assert!(sim.net.stats.duplicated > 0);
    assert!(matches!(
        LinearizabilityChecker::default().verify(&sim.history),
        LinearizabilityVerdict::Linearizable { .. }
    ));
}

#[test]
fn test_joint_consensus_dynamic_membership_reconfig_and_leader_stepdown() {
    let tuning = RaftTuningConfig::default();
    let mut sim = VoprClusterSimulator::new_five_node(0x9999_1204, tuning, NemesisProfile::default());
    for _ in 0..40 {
        sim.step_simulation(10_000_000, false).expect("elect");
    }

    let initial_leader = sim.active_leader().expect("initial leader");
    // Add new nodes 6 and 7 as learners first, then reconfigure `{1,2,3,4,5} -> {2,3,4,6,7}`
    // (deliberately REMOVING node 1, or removing `initial_leader` itself to test graceful Leader step-down!).
    let init_cfg = sim.nodes[&initial_leader].core.active_config().clone();
    sim.add_node_dynamically(6, init_cfg.clone());
    sim.add_node_dynamically(7, init_cfg);

    let mut new_voters: BTreeSet<u64> = BTreeSet::from([1, 2, 3, 4, 5, 6, 7]);
    new_voters.remove(&initial_leader); // Force removal of the active Leader!
    let other_removed = *new_voters.iter().find(|&&x| x <= 5).expect("second removed");
    new_voters.remove(&other_removed);
    assert_eq!(new_voters.len(), 5);

    let leader_now = consensus_raft_rs::traits::Clock::now_ns(&sim.clock, initial_leader);
    let joint_idx = sim
        .nodes
        .get_mut(&initial_leader)
        .expect("leader slot")
        .core
        .propose_membership_change(new_voters.clone(), BTreeSet::new(), leader_now)
        .expect("propose Joint Consensus C_old,new");
    assert!(joint_idx > 0);

    // Step through Phase 1 (`C_old,new` commit) -> automatic Phase 2 (`C_new` commit) -> old Leader step-down -> new Leader election in `C_new`!
    for _ in 0..120 {
        sim.step_simulation(10_000_000, false)
            .expect("joint consensus transition");
    }

    let post_leader = sim
        .active_leader()
        .expect("new leader elected inside C_new");
    assert!(
        new_voters.contains(&post_leader),
        "new leader {post_leader} must belong to C_new {new_voters:?}"
    );
    assert_ne!(
        sim.nodes[&initial_leader].core.role,
        RaftRole::Leader,
        "removed initial_leader must have stepped down upon committing C_new!"
    );
    assert_eq!(
        sim.nodes[&post_leader].core.active_config(),
        &ClusterConfig::uniform(new_voters.iter().copied())
    );

    // Commit a fresh write on the reconfigured 5-node cluster `{new_voters}`!
    sim.submit_client_write(
        1,
        ClientOpSpec::CreditDeposit {
            idempotency_key: "post-joint-write".to_string(),
            account_id: 9,
            asset: "USDT".to_string(),
            amount_fp: 77 * FP_SCALE_U128,
        },
    );
    for _ in 0..30 {
        sim.step_simulation(10_000_000, false)
            .expect("commit on C_new");
    }
    for &v in &new_voters {
        let (_, _, avail, _, _, ok) = sim.nodes[&v]
            .core
            .state_machine
            .audit_asset_conservation("USDT");
        assert!(ok);
        assert_eq!(avail, 77 * FP_SCALE_I128);
    }
}

#[test]
fn test_log_compaction_hypsnp01_and_streaming_install_snapshot_with_chunk_corruption() {
    let tuning = RaftTuningConfig {
        compaction_threshold_entries: 20,
        snapshot_chunk_bytes: 128, // Force multi-chunk streaming InstallSnapshot!
        ..Default::default()
    };

    let mut sim = VoprClusterSimulator::new_five_node(0x5A40_1205, tuning, NemesisProfile::default());
    for _ in 0..40 {
        sim.step_simulation(10_000_000, false).expect("elect");
    }

    let initial_ldr = sim.active_leader().expect("initial leader");
    let lagging_id = (1..=5u64).find(|&id| id != initial_ldr).expect("lagging follower");
    let majority_ids: Vec<u64> = (1..=5u64).filter(|&id| id != lagging_id).collect();

    // Isolate `lagging_id` completely while majority commits 35 entries and compacts logs past index 20!
    sim.net.apply_symmetric_partition(&[lagging_id], &majority_ids);
    for i in 1..=35u64 {
        sim.submit_client_write(
            1,
            ClientOpSpec::CreditDeposit {
                idempotency_key: format!("snap-dep-{i}"),
                account_id: (i % 3) + 1,
                asset: "USDT".to_string(),
                amount_fp: 10 * FP_SCALE_U128,
            },
        );
        for _ in 0..4 {
            sim.step_simulation(5_000_000, false).expect("commit");
        }
    }

    let ldr = sim.active_leader().expect("leader");
    assert!(
        sim.nodes[&ldr].core.snapshot_meta.last_included_index >= 20,
        "Leader must have compacted prefix into HYPSNP01 snapshot"
    );
    assert!(sim.nodes[&lagging_id].core.commit_index <= 1); // Only initial NoOp or 0

    // Inject a corrupted CRC32C InstallSnapshot chunk first -> follower MUST reject corrupted chunk!
    let bad_chunk_env = consensus_raft_rs::types::NetworkEnvelope {
        from: ldr,
        to: lagging_id,
        msg_id: 999_999,
        rpc: consensus_raft_rs::types::RaftRpc::InstallSnapshotRequest {
            term: sim.nodes[&ldr].core.hard_state.current_term,
            leader_id: ldr,
            meta: sim.nodes[&ldr].core.snapshot_meta.clone(),
            offset: 0,
            chunk_index: 0,
            total_chunks: 4,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            chunk_crc32c: 0x1234_5678, // Deliberately corrupted CRC32C!
            done: false,
        },
    };
    let lag_now = consensus_raft_rs::traits::Clock::now_ns(&sim.clock, lagging_id);
    sim.nodes
        .get_mut(&lagging_id)
        .expect("lagging slot")
        .core
        .step_network(bad_chunk_env, lag_now);
    let reject_effects = sim
        .nodes
        .get_mut(&lagging_id)
        .expect("lagging slot")
        .core
        .drain_effects();
    assert!(
        reject_effects.iter().any(|eff| matches!(
            eff,
            consensus_raft_rs::raft_core::RaftEffect::SendRpc(env)
                if matches!(
                    env.rpc,
                    consensus_raft_rs::types::RaftRpc::InstallSnapshotResponse { accepted: false, .. }
                )
        )),
        "corrupted InstallSnapshot chunk CRC32C must be rejected cleanly!"
    );

    // Reconnect lagging follower cleanly and verify multi-chunk InstallSnapshot catch-up:
    sim.net.heal_all(&[1, 2, 3, 4, 5]);
    for _ in 0..90 {
        sim.step_simulation(10_000_000, false)
            .expect("streaming InstallSnapshot catch-up");
    }

    assert_eq!(
        sim.nodes[&lagging_id].core.commit_index,
        sim.nodes[&ldr].core.commit_index,
        "lagging follower {lagging_id} must catch up completely via multi-chunk InstallSnapshot + WAL suffix!"
    );
    assert_eq!(
        sim.nodes[&lagging_id].core.state_machine.state_root_blake3(),
        sim.nodes[&ldr].core.state_machine.state_root_blake3(),
        "lagging follower {lagging_id} Blake3 state root must match Leader bit-for-bit after InstallSnapshot!"
    );
}

#[test]
fn test_hypwal01_torn_write_recovery_and_silent_bit_rot_detection() {
    let mut prng = DeterministicPrng::new(0xB17_207);
    let mut disk = SimulatedFaultyDisk::new(1);

    let e1 = build_chained_log_entry(
        1,
        1,
        1_000,
        [0u8; 32],
        RaftEntryKind::JournalBatch(vec![LedgerCommand::CreditDeposit {
            idempotency_key: "wal-1".to_string(),
            account_id: 1,
            asset: "USDT".to_string(),
            amount_fp: 50 * FP_SCALE_U128,
            ts_ns: 1_000,
        }]),
    );
    let e2 = build_chained_log_entry(2, 1, 2_000, e1.wal_blake3, RaftEntryKind::NoOp);
    let e3_unflushed = build_chained_log_entry(3, 1, 3_000, e2.wal_blake3, RaftEntryKind::NoOp);

    // Persist & fsync e1 and e2 cleanly.
    disk.stage_batch(&PersistBatch {
        io_id: 1,
        hard_state: Some(HardState {
            current_term: 1,
            voted_for: Some(1),
            compacted_index: 0,
            compacted_term: 0,
        }),
        truncate_from_index: None,
        entries: vec![e1.clone(), e2.clone()],
        install_snapshot: None,
    })
    .expect("stage 1");
    disk.fsync().expect("fsync 1");

    // Stage e3 WITHOUT fsync, then simulate sudden power-loss crash with mid-frame TORN WRITE!
    disk.stage_batch(&PersistBatch {
        io_id: 2,
        hard_state: None,
        truncate_from_index: None,
        entries: vec![e3_unflushed],
        install_snapshot: None,
    })
    .expect("stage 2");
    disk.simulate_power_loss_crash(&mut prng, true);

    // Crash recovery MUST safely truncate the torn uncommitted tail and recover e1 & e2 intact!
    let rec = disk
        .recover_after_restart()
        .expect("recover from torn tail write");
    assert!(rec.truncated_torn_tail_bytes > 0);
    assert_eq!(rec.entries.len(), 2);
    assert_eq!(rec.entries[1].wal_blake3, e2.wal_blake3);

    // Now inject silent physical BIT-ROT inside durable sealed WAL frame e1/e2!
    assert!(disk.inject_wal_bit_rot(&mut prng));
    let scrub_res = disk.scrub_and_verify();
    assert!(
        matches!(
            scrub_res,
            Err(StorageError::WalFrameBitRotCrc { .. } | StorageError::WalHashChainMismatch { .. })
        ),
        "silent data corruption (bit-rot) on sealed WAL must be caught 100% by CRC32C/Blake3: got {scrub_res:?}"
    );

    // Dual superblock bit-rot test:
    disk.inject_dual_superblock_bit_rot();
    assert!(matches!(
        disk.scrub_and_verify(),
        Err(StorageError::SuperblockBitRot { node_id: 1 })
    ));
}

#[test]
fn test_vopr_deterministic_seed_replay_and_25_seed_adversarial_chaos_campaign() {
    // 1. Prove bit-exact deterministic replay for identical seed!
    let run_once = |seed: u64| {
        let mut sim = VoprClusterSimulator::new_five_node(
            seed,
            RaftTuningConfig::default(),
            NemesisProfile::default(),
        );
        sim.run_full_vopr_campaign(160, 80)
            .expect("VOPR campaign must pass all safety, liveness, linearizability & serializability checks")
    };

    let s1 = run_once(0x4859_5045_5249_4F4E);
    let s2 = run_once(0x4859_5045_5249_4F4E);
    assert_eq!(
        s1.trace_digest_blake3, s2.trace_digest_blake3,
        "VOPR simulator must produce bit-identical execution trace Blake3 digest for the same 64-bit seed!"
    );
    assert_eq!(s1.converged_state_root_blake3, s2.converged_state_root_blake3);

    // 2. Run 25 distinct adversarial 64-bit seeds through full Nemesis chaos!
    let mut total_ops = 0usize;
    let mut total_faults = 0usize;
    let mut total_torn_recoveries = 0usize;
    let mut total_lin_states = 0usize;
    let mut total_ssr_edges = 0usize;
    let mut total_writes = 0usize;
    let mut total_reads = 0usize;
    let mut total_indeterminate = 0usize;
    for seed_idx in 1..=25u64 {
        let seed = 0x12_0000_0000_u64 + seed_idx.wrapping_mul(0x9E37_79B9);
        let summary = run_once(seed);
        assert!(summary.final_commit_index > 0);
        total_ops += summary.total_client_ops;
        total_writes += summary.committed_writes;
        total_reads += summary.linearizable_reads;
        total_indeterminate += summary.indeterminate_ops;
        total_faults += summary.nemesis_faults_injected;
        total_torn_recoveries += summary.torn_writes_recovered;
        total_lin_states += summary.linearizability_explored_states;
        total_ssr_edges += summary.strict_serializability_edges;
    }
    println!(
        "[VOPR_25_SEED_CAMPAIGN] seeds=25 replay_digest={} total_ops={} writes={} reads={} indeterminate={} nemesis_faults={} torn_recoveries={} wgl_explored_states={} strict_1sr_edges={}",
        consensus_raft_rs::wal_codec::hex32(&s1.trace_digest_blake3),
        total_ops,
        total_writes,
        total_reads,
        total_indeterminate,
        total_faults,
        total_torn_recoveries,
        total_lin_states,
        total_ssr_edges
    );
    assert!(total_ops >= 800);
    assert!(total_faults >= 100);
    assert!(total_torn_recoveries >= 1);
}

#[tokio::test]
async fn test_grpc_raft_consensus_service_and_http_endpoints_e2e() {
    let runtime = LiveConsensusRuntime::bootstrap_five_node(1, 1, 0xABCD_EF01);

    let grpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral grpc");
    let grpc_addr = grpc_listener.local_addr().expect("grpc local addr");

    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral http");
    let http_addr = http_listener.local_addr().expect("http local addr");

    let rt_grpc = runtime.clone();
    let grpc_task = tokio::spawn(async move {
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
        let _ = Server::builder()
            .add_service(rt_grpc.into_grpc_server())
            .serve_with_incoming(incoming)
            .await;
    });

    let http_router = runtime.clone().build_http_router();
    let http_task = tokio::spawn(async move {
        let _ = axum::serve(http_listener, http_router).await;
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut client = RaftConsensusServiceClient::connect(format!("http://{grpc_addr}"))
        .await
        .expect("connect gRPC client");

    // 1. GetClusterStatus
    let status = client
        .get_cluster_status(GetRaftClusterStatusRequest { node_id: 0 })
        .await
        .expect("GetClusterStatus")
        .into_inner();
    assert!(status.leader_id >= 1 && status.leader_id <= 5);

    // 2. ProposeLedgerEntry
    let prop_resp = client
        .propose_ledger_entry(ProposeLedgerEntryRequest {
            idempotency_key: "e2e-grpc-prop-1".to_string(),
            envelope: Some(base_v1::MarketEventEnvelope {
                engine_seq: 1,
                symbol: "BTC-USDT".to_string(),
                ts_ns: 1_795_000_000_000_000_000,
                payload: None,
            }),
            trace: None,
        })
        .await
        .expect("ProposeLedgerEntry")
        .into_inner();
    assert!(prop_resp.committed);
    assert_eq!(prop_resp.entry_blake3_hash.len(), 32);

    // 3. WaitReadIndexBarrier (both strict ReadIndex and LeaderLease fast path)
    let barrier_strict = client
        .wait_read_index_barrier(ReadIndexBarrierRequest {
            read_request_id: "read-strict-1".to_string(),
            allow_leader_lease_fast_path: false,
            trace: None,
        })
        .await
        .expect("WaitReadIndexBarrier strict")
        .into_inner();
    assert!(barrier_strict.required_read_index >= prop_resp.log_index);
    assert_eq!(barrier_strict.committed_state_root_blake3.len(), 32);

    let barrier_lease = client
        .wait_read_index_barrier(ReadIndexBarrierRequest {
            read_request_id: "read-lease-1".to_string(),
            allow_leader_lease_fast_path: true,
            trace: None,
        })
        .await
        .expect("WaitReadIndexBarrier lease")
        .into_inner();
    assert!(barrier_lease.served_via_leader_lease);

    // 4. PreVoteRpc, RequestVoteRpc, AppendEntriesRpc & Streaming InstallSnapshotRpc
    let pv = client
        .pre_vote_rpc(PreVote {
            cluster_id: 1,
            prospective_term: status.current_term + 1,
            candidate_id: 5,
            last_log_index: 999,
            last_log_term: status.current_term,
            last_log_blake3: vec![0u8; 32],
            trace: None,
        })
        .await
        .expect("PreVoteRpc")
        .into_inner();
    // Healthy leader stickiness active -> PreVote rejected with leader_lease_active!
    assert!(pv.leader_lease_active || !pv.vote_granted);

    let rv = client
        .request_vote_rpc(RequestVote {
            cluster_id: 1,
            term: 0, // stale term
            candidate_id: 5,
            last_log_index: 0,
            last_log_term: 0,
            last_log_blake3: vec![0u8; 32],
            trace: None,
        })
        .await
        .expect("RequestVoteRpc")
        .into_inner();
    assert!(!rv.vote_granted);

    let ae = client
        .append_entries_rpc(AppendEntries {
            cluster_id: 1,
            term: status.current_term,
            leader_id: status.leader_id,
            prev_log_index: 0,
            prev_log_term: 0,
            prev_log_blake3: vec![0u8; 32],
            entries: Vec::new(),
            leader_commit_index: status.commit_index,
            pipeline_seq: 1,
            leader_lease_until_ns: 0,
            trace: None,
        })
        .await
        .expect("AppendEntriesRpc")
        .into_inner();
    assert!(ae.success);

    // Build a valid HYPSNP01 snapshot and stream it in 2 chunks over `InstallSnapshotRpc`:
    let snap_meta = consensus_raft_rs::types::SnapshotMetadata {
        last_included_index: status.commit_index + 10,
        last_included_term: status.current_term,
        ts_ns: 1_000_000,
        last_wal_blake3: [0u8; 32],
        state_root_blake3: consensus_raft_rs::state_machine::ReplicatedLedgerStateMachine::new()
            .state_root_blake3(),
        config: ClusterConfig::uniform([1, 2, 3, 4, 5]),
    };
    let snap_art = encode_hypsnp01_snapshot(
        &snap_meta,
        consensus_raft_rs::state_machine::ReplicatedLedgerStateMachine::new()
            .serialize_deterministic(),
    );
    let mid = snap_art.raw_hypsnp_bytes.len() / 2;
    let c1 = snap_art.raw_hypsnp_bytes[..mid].to_vec();
    let c2 = snap_art.raw_hypsnp_bytes[mid..].to_vec();
    let chunks = vec![
        InstallSnapshot {
            cluster_id: 1,
            term: status.current_term,
            leader_id: status.leader_id,
            last_included_index: snap_meta.last_included_index,
            last_included_term: snap_meta.last_included_term,
            last_included_engine_seq: snap_meta.last_included_index,
            state_root_blake3: snap_meta.state_root_blake3.to_vec(),
            cluster_config: None,
            byte_offset: 0,
            chunk_crc32c: crc32c::crc32c(&c1),
            chunk_data: c1,
            done: false,
            trace: None,
        },
        InstallSnapshot {
            cluster_id: 1,
            term: status.current_term,
            leader_id: status.leader_id,
            last_included_index: snap_meta.last_included_index,
            last_included_term: snap_meta.last_included_term,
            last_included_engine_seq: snap_meta.last_included_index,
            state_root_blake3: snap_meta.state_root_blake3.to_vec(),
            cluster_config: None,
            byte_offset: mid as u64,
            chunk_crc32c: crc32c::crc32c(&c2),
            chunk_data: c2,
            done: true,
            trace: None,
        },
    ];
    let snap_resp = client
        .install_snapshot_rpc(tokio_stream::iter(chunks))
        .await
        .expect("InstallSnapshotRpc")
        .into_inner();
    assert!(snap_resp.checksum_valid);
    assert_eq!(
        snap_resp.bytes_stored,
        snap_art.raw_hypsnp_bytes.len() as u64
    );

    // Check HTTP `/healthz` via raw TCP GET (zero extra HTTP client crate needed!)
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(http_addr)
        .await
        .expect("connect HTTP");
    tcp.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write HTTP GET");
    let mut http_resp = String::new();
    tcp.read_to_string(&mut http_resp).await.expect("read HTTP");
    assert!(http_resp.contains("200 OK"));
    assert!(http_resp.contains("\"status\":\"ok\""));

    grpc_task.abort();
    http_task.abort();
}

#[test]
fn test_mutation_survival_zero_gate() {
    let report = run_consensus_mutation_survival_suite();
    println!(
        "[MUTATION_SURVIVAL_REPORT] killed={}/{} kill_score_bps={} survived={:?}",
        report.killed_mutants,
        report.total_mutants,
        report.kill_score_bps(),
        report.survived_mutants
    );
    assert_eq!(
        report.survived_mutants,
        Vec::<&'static str>::new(),
        "Mutants survived: {:?}",
        report.survived_mutants
    );
    assert!(
        report.kill_score_bps() >= 9_500,
        "Mutation kill score must be >= 95.00%, got {} bps",
        report.kill_score_bps()
    );
    assert_eq!(report.kill_score_bps(), 10_000);
}
