use std::net::SocketAddr;
use std::sync::Arc;

use chronos_replay::merkle::{verify_merkle_proof, ProofStep};
use chronos_replay::pb::{
    chronos_replay_service_client::ChronosReplayServiceClient,
    market_event_envelope::Payload as EnvelopePayload, time_travel_seek_request::Target,
    AccountPolicyOverride, AccountStatus, ControlReplaySessionRequest, L3BookDelta, L3DeltaOp,
    LedgerEntryLine, LedgerJournalEntry, MarketEventEnvelope, OrderSide, OrderType,
    ReplayControlAction, SelfTradePreventionMode, SimulationPolicyOverride,
    StreamReplayFramesRequest, TimeTravelSeekRequest, TimelineForkRequest,
};
use chronos_replay::{
    build_default_demo_scenario, build_http_router, encode_snapshot_bytes,
    parse_and_verify_snapshot, ChronosEngine, ChronosError, ChronosGrpcService,
    ChronosServiceState, ChronosWalStore, ExchangeState, HistoricalTimelineRecorder,
    WalSegmentWriter,
};
use proptest::prelude::*;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server as TonicServer;

#[derive(Debug, Clone)]
enum OpSpec {
    LimitOrder {
        acc_idx: usize,
        is_buy: bool,
        price_offset_ticks: i64,
        qty_lots: u64,
    },
    IocOrder {
        acc_idx: usize,
        is_buy: bool,
        price_offset_ticks: i64,
        qty_lots: u64,
    },
    CancelRecent {
        back_offset: usize,
    },
}

fn arb_op_spec() -> impl Strategy<Value = OpSpec> {
    prop_oneof![
        4 => (0..6usize, any::<bool>(), -6i64..=6i64, 1u64..=8u64).prop_map(
            |(acc_idx, is_buy, price_offset_ticks, qty_lots)| OpSpec::LimitOrder {
                acc_idx,
                is_buy,
                price_offset_ticks,
                qty_lots,
            }
        ),
        2 => (0..6usize, any::<bool>(), -6i64..=6i64, 1u64..=6u64).prop_map(
            |(acc_idx, is_buy, price_offset_ticks, qty_lots)| OpSpec::IocOrder {
                acc_idx,
                is_buy,
                price_offset_ticks,
                qty_lots,
            }
        ),
        1 => (1..8usize).prop_map(|back_offset| OpSpec::CancelRecent { back_offset }),
    ]
}

fn build_randomized_store(ops: &[OpSpec], snapshot_interval: u64) -> ChronosWalStore {
    let mut rec = HistoricalTimelineRecorder::new(snapshot_interval, 100, 400);
    rec.set_segment_max_frames(60); // Force multi-segment WAL boundaries!
    let accounts = [101u64, 102, 103, 104, 105, 106];
    let mut ts = 1_795_000_000_000_000_000u64;

    for &acc in &accounts {
        rec.record_deposit(acc, "USDT", 900_000_000_000_000, ts)
            .expect("deposit usdt");
        ts += 1_000;
        rec.record_deposit(acc, "BTC", 80_000_000_000, ts)
            .expect("deposit btc");
        ts += 1_000;
    }

    let base_px: i64 = 6_850_000_000_000;
    let tick: i64 = 500_000_000;
    let mut next_oid = 1u64;

    for op in ops {
        match *op {
            OpSpec::LimitOrder {
                acc_idx,
                is_buy,
                price_offset_ticks,
                qty_lots,
            } => {
                let px = (base_px + price_offset_ticks * tick).max(100_000_000) as u64;
                let qty = qty_lots * 5_000_000;
                rec.record_submit_order(
                    next_oid,
                    accounts[acc_idx],
                    "BTC-USDT",
                    if is_buy { 1 } else { 2 },
                    OrderType::Limit,
                    SelfTradePreventionMode::CancelResting,
                    px,
                    qty,
                    ts,
                )
                .expect("limit submit");
                next_oid += 1;
                ts += 10_000;
            }
            OpSpec::IocOrder {
                acc_idx,
                is_buy,
                price_offset_ticks,
                qty_lots,
            } => {
                let px = (base_px + price_offset_ticks * tick).max(100_000_000) as u64;
                let qty = qty_lots * 5_000_000;
                rec.record_submit_order(
                    next_oid,
                    accounts[acc_idx],
                    "BTC-USDT",
                    if is_buy { 1 } else { 2 },
                    OrderType::Ioc,
                    SelfTradePreventionMode::CancelResting,
                    px,
                    qty,
                    ts,
                )
                .expect("ioc submit");
                next_oid += 1;
                ts += 10_000;
            }
            OpSpec::CancelRecent { back_offset } => {
                if next_oid > back_offset as u64 {
                    let target = next_oid - (back_offset as u64);
                    rec.record_cancel_order(target, "BTC-USDT", ts)
                        .expect("cancel");
                    ts += 5_000;
                }
            }
        }
    }

    let (store, _) = rec.build_store(16).expect("build store");
    store
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Requirement 4(a): Random back-and-forth jumping across hundreds of sequence IDs & timestamps
    /// ALWAYS yields the exact 32-byte Blake3 `state_root_blake3` identical to linear replay from genesis (0).
    #[test]
    fn prop_random_seek_back_and_forth_matches_linear_genesis(
        ops in proptest::collection::vec(arb_op_spec(), 45..95),
        jump_seeds in proptest::collection::vec(0u64..10_000u64, 120..220),
    ) {
        let store = build_randomized_store(&ops, 25);
        let max_seq = store.max_engine_seq();
        prop_assume!(max_seq >= 12);

        // Compute ground-truth linear genesis reference state hashes at EVERY sequence `1..=max_seq`
        let mut linear_state = ExchangeState::new();
        let mut expected_roots: Vec<[u8; 32]> = vec![[0u8; 32]; (max_seq + 1) as usize];
        expected_roots[0] = linear_state.compute_state_root();

        for ev in store.events() {
            linear_state.apply_wal_envelope(&ev.envelope).expect("linear apply");
            expected_roots[ev.engine_seq as usize] = linear_state.compute_state_root();
        }

        let engine = ChronosEngine::new(store);

        // Jump erratically forward & backward across 120..220 random sequence points!
        for (idx, &seed) in jump_seeds.iter().enumerate() {
            let target_seq = seed % (max_seq + 1);
            let out = engine.seek(&TimeTravelSeekRequest {
                symbol: "BTC-USDT".to_string(),
                target: Some(Target::TargetEngineSeq(target_seq)),
                l2_depth: 50,
                include_account_balances: idx % 15 == 0,
                verify_wal_hash_chain: idx == 0,
                trace: None,
            }).expect("seek by seq");

            prop_assert_eq!(
                out.response.state_root_blake3.as_slice(),
                expected_roots[target_seq as usize].as_slice(),
                "State hash mismatch when seeking to seq {}",
                target_seq
            );

            // Also verify Merkle inclusion proofs whenever requested
            for proof in &out.response.account_states {
                prop_assert!(proof.double_entry_balanced);
                let mut leaf = [0u8; 32];
                leaf.copy_from_slice(&proof.leaf_blake3_hash);
                let mut root = [0u8; 32];
                root.copy_from_slice(&proof.state_root_blake3);
                let steps: Vec<ProofStep> = proof.proof_path.iter().map(|n| {
                    let mut sib = [0u8; 32];
                    sib.copy_from_slice(&n.sibling_hash);
                    ProofStep {
                        sibling_hash: sib,
                        is_left_sibling: n.is_left_sibling,
                        depth: n.depth,
                    }
                }).collect();
                prop_assert!(verify_merkle_proof(&leaf, &steps, &root));
            }
        }
    }

    /// Requirement 4(b): Counterfactual fork with empty / noop `SimulationPolicyOverride`
    /// across arbitrary `[T1, T2]` windows produces an ABSOLUTE ZERO DIFF against baseline.
    #[test]
    fn prop_noop_counterfactual_fork_yields_absolute_zero_diff(
        ops in proptest::collection::vec(arb_op_spec(), 35..80),
        frac_a in 0u64..100u64,
        frac_b in 0u64..100u64,
    ) {
        let store = build_randomized_store(&ops, 20);
        let max_seq = store.max_engine_seq();
        prop_assume!(max_seq >= 15);

        // Pick clean command boundary for start_seq so we don't slice mid-batch
        let events = store.events();
        let idx_a = ((events.len() - 1) * (frac_a as usize)) / 100;
        let idx_b = ((events.len() - 1) * (frac_b as usize)) / 100;
        let lo_idx = idx_a.min(idx_b);
        let hi_idx = idx_a.max(idx_b);

        // Snap start_seq to a boundary right before a tagged command (or 0)
        let start_seq = find_clean_batch_boundary_before(events, lo_idx);
        let end_seq = find_clean_batch_end_after(events, hi_idx).max(start_seq);

        let engine = ChronosEngine::new(store);
        let diff = engine.fork_timeline(&TimelineForkRequest {
            fork_name: "proptest-noop".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: start_seq,
            end_engine_seq: end_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride::default()),
            trace: None,
        }).expect("noop fork");

        prop_assert!(diff.state_identical, "Noop fork was not state_identical! diff={:?}", diff);
        prop_assert_eq!(&diff.baseline_state_root_blake3, &diff.forked_state_root_blake3);
        prop_assert_eq!(diff.clearing_price_delta_fp, 0);
        prop_assert_eq!(diff.baseline_volatility_bps, diff.forked_volatility_bps);
        prop_assert_eq!(diff.max_slippage_delta_bps, 0);
        prop_assert_eq!(chronos_replay::fixed::pb_to_u128(diff.prevented_wash_volume_fp), 0);
        prop_assert_eq!(diff.prevented_fills_count, 0);
        prop_assert_eq!(diff.rejected_orders_count, 0);
        prop_assert_eq!(chronos_replay::fixed::pb_to_i128(diff.treasury_fee_delta_fp), 0);
        prop_assert!(diff.account_diffs.is_empty());
        prop_assert!(diff.l2_level_diffs.is_empty());
    }
}

fn find_clean_batch_boundary_before(events: &[chronos_replay::IndexedWalEvent], idx: usize) -> u64 {
    if idx == 0 {
        return 0;
    }
    let mut i = idx;
    while i > 0 {
        if has_cmd_tag(&events[i].envelope)
            || matches!(
                &events[i].envelope.payload,
                Some(EnvelopePayload::JournalEntry(j)) if j.reference_type == "DEPOSIT"
            )
        {
            return events[i - 1].engine_seq;
        }
        i -= 1;
    }
    0
}

fn find_clean_batch_end_after(events: &[chronos_replay::IndexedWalEvent], idx: usize) -> u64 {
    if events.is_empty() {
        return 0;
    }
    let mut i = idx + 1;
    while i < events.len() {
        if has_cmd_tag(&events[i].envelope)
            || matches!(
                &events[i].envelope.payload,
                Some(EnvelopePayload::JournalEntry(j)) if j.reference_type == "DEPOSIT"
            )
        {
            return events[i - 1].engine_seq;
        }
        i += 1;
    }
    events.last().map(|e| e.engine_seq).unwrap_or(0)
}

fn has_cmd_tag(env: &MarketEventEnvelope) -> bool {
    let trace = match &env.payload {
        Some(EnvelopePayload::L3Delta(d)) => d.trace.as_ref(),
        Some(EnvelopePayload::Fill(f)) => f.trace.as_ref(),
        Some(EnvelopePayload::OrderAck(a)) => a.trace.as_ref(),
        _ => None,
    };
    trace
        .map(|t| t.source_service.starts_with("chronos-cmd:"))
        .unwrap_or(false)
}

#[test]
fn test_wal_and_snapshot_cryptographic_tamper_detection() {
    let mut writer = WalSegmentWriter::new();
    for seq in 1..=5u64 {
        writer
            .append_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: "BTC-USDT".to_string(),
                ts_ns: 1_795_000_000_000_000_000 + seq * 1_000,
                payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: seq,
                    prev_engine_seq: seq - 1,
                    symbol: "BTC-USDT".to_string(),
                    instrument_id: 1,
                    op: L3DeltaOp::Add as i32,
                    side: OrderSide::Buy as i32,
                    order_id: seq,
                    account_id: 10,
                    price_fp: 6_800_000_000_000,
                    remaining_qty_fp: 10_000_000,
                    delta_qty_fp: 10_000_000,
                    event_ts_ns: 1_795_000_000_000_000_000 + seq * 1_000,
                    state_root_blake3: Vec::new(),
                    trace: None,
                })),
            })
            .expect("append");
    }
    let valid_bytes = writer.finish_bytes();

    // 1. Corrupt magic header
    let mut bad_magic = valid_bytes.to_vec();
    bad_magic[0] ^= 0xFF;
    let err = ChronosWalStore::from_backings_and_snapshots(
        vec![(
            None,
            chronos_replay::wal::SegmentBacking::Memory(bad_magic.into()),
        )],
        vec![],
        4,
    );
    assert!(matches!(err, Err(ChronosError::InvalidWalMagic(_))));

    // 2. Corrupt payload byte (caught by CRC32C)
    let mut bad_crc = valid_bytes.to_vec();
    let last_idx = bad_crc.len() - 1;
    bad_crc[last_idx] ^= 0x01;
    let err_crc = ChronosWalStore::from_backings_and_snapshots(
        vec![(
            None,
            chronos_replay::wal::SegmentBacking::Memory(bad_crc.into()),
        )],
        vec![],
        4,
    );
    assert!(matches!(err_crc, Err(ChronosError::WalCrcMismatch { .. })));

    // 3. Corrupt frame_blake3 (CRC32C still matches because CRC covers seq||ts||prev||payload, caught by Blake3!)
    let mut bad_blake3 = valid_bytes.to_vec();
    bad_blake3[16 + 56] ^= 0x80; // First byte of frame_blake3 in frame #1
    let err_b3 = ChronosWalStore::from_backings_and_snapshots(
        vec![(
            None,
            chronos_replay::wal::SegmentBacking::Memory(bad_blake3.into()),
        )],
        vec![],
        4,
    );
    assert!(matches!(
        err_b3,
        Err(ChronosError::WalBlake3Mismatch { .. })
    ));

    // 4. Corrupt HYPSNP01 snapshot CRC & state root
    let mut st = ExchangeState::new();
    st.credit_deposit(42, "USDT", 500_000_000_000).expect("dep");
    let root = st.compute_state_root();
    let payload = st.to_snapshot_payload();
    let snap_bytes = encode_snapshot_bytes(10, 12345, root, &payload).expect("encode snap");

    let mut bad_snap = snap_bytes.clone();
    let s_last = bad_snap.len() - 2;
    bad_snap[s_last] ^= 0x04;
    assert!(matches!(
        parse_and_verify_snapshot(&bad_snap, None),
        Err(ChronosError::SnapshotCrcMismatch { .. })
    ));

    // Valid CRC but wrong state_root_blake3 -> rejected on ExchangeState::from_snapshot!
    let mut wrong_root = root;
    wrong_root[0] ^= 0xFF;
    let forged_snap_bytes =
        encode_snapshot_bytes(10, 12345, wrong_root, &payload).expect("forged snap");
    let parsed_forged = parse_and_verify_snapshot(&forged_snap_bytes, None).expect("crc ok");
    assert!(matches!(
        ExchangeState::from_snapshot(&parsed_forged),
        Err(ChronosError::SnapshotStateRootMismatch { .. })
    ));
}

#[test]
fn test_counterfactual_wash_ring_fee_override_and_size_cap_scenarios() {
    let store = build_default_demo_scenario().expect("demo scenario");
    let max_seq = store.max_engine_seq();
    let max_ts = store.max_ts_ns();
    let engine = ChronosEngine::new(store);

    // Baseline check before & after forks to prove baseline immutability
    let baseline_before = engine
        .seek(&TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetEngineSeq(max_seq)),
            l2_depth: 50,
            include_account_balances: true,
            verify_wal_hash_chain: true,
            trace: None,
        })
        .expect("baseline before");

    // Scenario 1: "What if Aegis froze syndicate account 666 30 seconds earlier?"
    let freeze_ts = max_ts.saturating_sub(30_000_000_000);
    let diff_wash = engine
        .fork_timeline(&TimelineForkRequest {
            fork_name: "aegis-freeze-syndicate-666-early".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: 14, // right after initial deposits
            end_engine_seq: max_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                account_overrides: vec![AccountPolicyOverride {
                    account_id: 666,
                    status_override: AccountStatus::Frozen as i32,
                    effective_from_seq: 14,
                    effective_from_ts_ns: freeze_ts,
                    rate_limit_rps: 0,
                    cancel_resting_orders_on_activation: true,
                }],
                ..Default::default()
            }),
            trace: None,
        })
        .expect("wash fork");

    assert!(!diff_wash.state_identical);
    assert!(
        chronos_replay::fixed::pb_to_u128(diff_wash.prevented_wash_volume_fp) > 0,
        "Expected prevented wash volume > 0"
    );
    assert!(diff_wash.prevented_fills_count > 0);
    assert!(diff_wash.rejected_orders_count > 0);

    // Scenario 2: "What if max order size was cut by 50% (blocking the 6.0 BTC flash-crash dump)?"
    let diff_cap = engine
        .fork_timeline(&TimelineForkRequest {
            fork_name: "cut-max-order-size-50pct".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: 14,
            end_engine_seq: max_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                max_order_qty_fp_override: Some(300_000_000), // 3.0 BTC cap blocks 6.0 BTC dump!
                ..Default::default()
            }),
            trace: None,
        })
        .expect("size cap fork");

    assert!(!diff_cap.state_identical);
    assert!(
        diff_cap.forked_clearing_price_fp > diff_cap.baseline_clearing_price_fp,
        "Blocking flash-crash dump must preserve higher clearing price!"
    );
    assert!(
        diff_cap.max_slippage_delta_bps < 0,
        "Blocking flash-crash dump must reduce max slippage!"
    );

    // Scenario 3: "What if Maker had -50 ppm rebate and Taker paid +800 ppm?"
    let diff_fee = engine
        .fork_timeline(&TimelineForkRequest {
            fork_name: "maker-rebate-fee-restructure".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: 14,
            end_engine_seq: max_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                maker_fee_ppm_override: Some(-50),
                taker_fee_ppm_override: Some(800),
                ..Default::default()
            }),
            trace: None,
        })
        .expect("fee fork");

    assert!(!diff_fee.state_identical);
    // Net fee rate rose from 100 + 500 = 600 ppm to -50 + 800 = 750 ppm (+150 ppm), so treasury delta MUST be strictly positive!
    let treas_delta = chronos_replay::fixed::pb_to_i128(diff_fee.treasury_fee_delta_fp);
    assert!(
        treas_delta > 0,
        "Expected strictly positive treasury fee delta when net fee rises from 600 to 750 ppm, got {treas_delta}"
    );
    assert!(!diff_fee.account_diffs.is_empty());
    // Double-entry conservation must hold in forked sandbox too!
    let fork_art = engine.get_fork(&diff_fee.fork_id).expect("stored fork");
    fork_art
        .final_state_at_end
        .verify_double_entry_conservation()
        .expect("forked sandbox double-entry conservation");

    // Prove baseline production state is 100% untouched!
    let baseline_after = engine
        .seek(&TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetEngineSeq(max_seq)),
            l2_depth: 50,
            include_account_balances: true,
            verify_wal_hash_chain: false,
            trace: None,
        })
        .expect("baseline after");
    assert_eq!(
        baseline_before.response.state_root_blake3,
        baseline_after.response.state_root_blake3
    );
}

#[test]
fn test_disk_mmap_open_data_dir_and_untagged_external_ledger_wal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wal_dir = tmp.path().join("wal");
    std::fs::create_dir_all(&wal_dir).expect("mkdir wal");

    // Simulate external `ledger-rs` writing untagged MarketEventEnvelope frames directly to disk
    let mut writer = WalSegmentWriter::new();
    let ts0 = 1_795_500_000_000_000_000u64;
    // seq 1 & 2: deposits for acc 1 & acc 2
    writer
        .append_envelope(MarketEventEnvelope {
            engine_seq: 1,
            symbol: "BTC-USDT".to_string(),
            ts_ns: ts0,
            payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                journal_seq: 1,
                engine_seq: 1,
                reference_type: "DEPOSIT".to_string(),
                reference_id: 1,
                lines: vec![LedgerEntryLine {
                    account_id: 1,
                    asset: "BTC".to_string(),
                    sub_account: "AVAILABLE".to_string(),
                    debit_fp: Some(chronos_replay::fixed::i128_to_pb(0)),
                    credit_fp: Some(chronos_replay::fixed::i128_to_pb(500_000_000)),
                    balance_after_fp: Some(chronos_replay::fixed::i128_to_pb(500_000_000)),
                }],
                ts_ns: ts0,
                prev_journal_blake3: vec![],
                journal_blake3: vec![],
            })),
        })
        .expect("w1");
    writer
        .append_envelope(MarketEventEnvelope {
            engine_seq: 2,
            symbol: "BTC-USDT".to_string(),
            ts_ns: ts0 + 100,
            payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                journal_seq: 2,
                engine_seq: 2,
                reference_type: "DEPOSIT".to_string(),
                reference_id: 2,
                lines: vec![LedgerEntryLine {
                    account_id: 2,
                    asset: "USDT".to_string(),
                    sub_account: "AVAILABLE".to_string(),
                    debit_fp: Some(chronos_replay::fixed::i128_to_pb(0)),
                    credit_fp: Some(chronos_replay::fixed::i128_to_pb(90_000_000_000_000)),
                    balance_after_fp: Some(chronos_replay::fixed::i128_to_pb(90_000_000_000_000)),
                }],
                ts_ns: ts0 + 100,
                prev_journal_blake3: vec![],
                journal_blake3: vec![],
            })),
        })
        .expect("w2");
    // seq 3: untagged L3 ADD from acc 1 selling 1 BTC @ 68,000 USDT
    writer
        .append_envelope(MarketEventEnvelope {
            engine_seq: 3,
            symbol: "BTC-USDT".to_string(),
            ts_ns: ts0 + 200,
            payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                engine_seq: 3,
                prev_engine_seq: 0,
                symbol: "BTC-USDT".to_string(),
                instrument_id: 1,
                op: L3DeltaOp::Add as i32,
                side: OrderSide::Sell as i32,
                order_id: 501,
                account_id: 1,
                price_fp: 6_800_000_000_000,
                remaining_qty_fp: 100_000_000,
                delta_qty_fp: 100_000_000,
                event_ts_ns: ts0 + 200,
                state_root_blake3: vec![],
                trace: None,
            })),
        })
        .expect("w3");

    writer
        .write_to_path(&wal_dir.join("segment_00000001.wal"))
        .expect("write segment");

    let disk_store = ChronosWalStore::open_data_dir(tmp.path(), 2).expect("open mmap store");
    assert_eq!(disk_store.max_engine_seq(), 3);
    assert!(disk_store.is_chain_verified());

    let engine = ChronosEngine::new(disk_store);
    let seek_res = engine
        .seek(&TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetTsNs(ts0 + 250)),
            l2_depth: 10,
            include_account_balances: true,
            verify_wal_hash_chain: true,
            trace: None,
        })
        .expect("seek disk");
    assert_eq!(seek_res.response.resolved_engine_seq, 3);
    assert_eq!(
        seek_res.response.l2_snapshot.unwrap().best_ask_fp,
        6_800_000_000_000
    );
}

#[tokio::test]
async fn test_grpc_and_http_vcr_streaming_controls_end_to_end() {
    let store = build_default_demo_scenario().expect("demo");
    let engine = Arc::new(ChronosEngine::new(store));
    let svc_state = ChronosServiceState::new(engine);

    // 1. Bind ephemeral gRPC server
    let grpc_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind grpc");
    let grpc_addr: SocketAddr = grpc_listener.local_addr().expect("grpc local_addr");
    let grpc_svc = ChronosGrpcService::new(Arc::clone(&svc_state)).into_server();
    tokio::spawn(async move {
        let _ = TonicServer::builder()
            .add_service(grpc_svc)
            .serve_with_incoming(TcpListenerStream::new(grpc_listener))
            .await;
    });

    // 2. Bind ephemeral HTTP/SSE server
    let http_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind http");
    let http_addr: SocketAddr = http_listener.local_addr().expect("http local_addr");
    let app = build_http_router(Arc::clone(&svc_state));
    tokio::spawn(async move {
        let _ = axum::serve(http_listener, app).await;
    });

    // Connect gRPC client
    let mut client = ChronosReplayServiceClient::connect(format!("http://{grpc_addr}"))
        .await
        .expect("connect grpc");

    // Test gRPC Seek
    let seek_resp = client
        .time_travel_seek(TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetEngineSeq(40)),
            l2_depth: 10,
            include_account_balances: true,
            verify_wal_hash_chain: true,
            trace: None,
        })
        .await
        .expect("grpc seek")
        .into_inner();
    assert_eq!(seek_resp.resolved_engine_seq, 40);
    assert!(seek_resp.wal_chain_verified);

    // Test gRPC VCR Stream + Pause + StepForward + StepBackward + Rewind + Speed 0.1x..100x
    let mut stream = client
        .stream_replay_frames(StreamReplayFramesRequest {
            session_id: "test-vcr-01".to_string(),
            timeline_id: "baseline".to_string(),
            symbol: "BTC-USDT".to_string(),
            from_engine_seq: 15,
            to_engine_seq: 60,
            initial_speed_milli: 100, // 0.1x slow-motion!
            l2_depth: 10,
        })
        .await
        .expect("start stream")
        .into_inner();

    let first_frame = stream.message().await.expect("msg").expect("frame 1");
    assert_eq!(first_frame.session_id, "test-vcr-01");
    assert_eq!(first_frame.speed_milli, 100);

    // Pause session
    let pause_resp = client
        .control_replay_session(ControlReplaySessionRequest {
            session_id: "test-vcr-01".to_string(),
            action: ReplayControlAction::Pause as i32,
            speed_milli: 0,
            step_count: 0,
        })
        .await
        .expect("pause")
        .into_inner();
    assert!(pause_resp.is_paused);
    let paused_seq = pause_resp.current_engine_seq;

    // Single-event StepForward
    let step_fwd = client
        .control_replay_session(ControlReplaySessionRequest {
            session_id: "test-vcr-01".to_string(),
            action: ReplayControlAction::StepForward as i32,
            speed_milli: 0,
            step_count: 1,
        })
        .await
        .expect("step forward")
        .into_inner();
    assert_eq!(step_fwd.current_engine_seq, paused_seq + 1);

    // StepBackward
    let step_bwd = client
        .control_replay_session(ControlReplaySessionRequest {
            session_id: "test-vcr-01".to_string(),
            action: ReplayControlAction::StepBackward as i32,
            speed_milli: 0,
            step_count: 1,
        })
        .await
        .expect("step backward")
        .into_inner();
    assert_eq!(step_bwd.current_engine_seq, paused_seq);

    // Rewind to anchor
    let rew = client
        .control_replay_session(ControlReplaySessionRequest {
            session_id: "test-vcr-01".to_string(),
            action: ReplayControlAction::Rewind as i32,
            speed_milli: 0,
            step_count: 0,
        })
        .await
        .expect("rewind")
        .into_inner();
    assert!(rew.is_paused);

    // SetSpeed 100x (100_000 milli) & Play!
    let speed_resp = client
        .control_replay_session(ControlReplaySessionRequest {
            session_id: "test-vcr-01".to_string(),
            action: ReplayControlAction::Play as i32,
            speed_milli: 100_000,
            step_count: 0,
        })
        .await
        .expect("play 100x")
        .into_inner();
    assert!(!speed_resp.is_paused);
    assert_eq!(speed_resp.effective_speed_milli, 100_000);

    // Verify HTTP /healthz via raw TCP HTTP/1.1 request (zero extra HTTP client crate needed!)
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(http_addr)
        .await
        .expect("connect http");
    tcp.write_all(
        b"GET /api/v1/chronos/seek?symbol=BTC-USDT&target_engine_seq=40 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    )
    .await
    .expect("write http");
    let mut buf = String::new();
    tcp.read_to_string(&mut buf).await.expect("read http");
    assert!(buf.starts_with("HTTP/1.1 200 OK"), "HTTP response: {buf}");
    assert!(buf.contains("\"symbol\":\"BTC-USDT\""));
    assert!(buf.contains("\"state_root_blake3\""));
}

#[test]
fn test_ch02_ledger_snapshot_v1_and_tail_wal_bit_identical_state_root() {
    use chronos_replay::wal::{
        compute_snapshot_crc32c, LedgerSnapshotAccountV1, LedgerSnapshotAssetBalanceV1,
        LedgerSnapshotAssetTotalV1, LedgerSnapshotInstrumentV1, LedgerSnapshotOrderV1,
        LedgerSnapshotPayloadV1, SNAPSHOT_MAGIC, SNAPSHOT_VERSION,
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let wal_dir = tmp.path().join("wal");
    let snap_dir = tmp.path().join("snapshots");
    std::fs::create_dir_all(&wal_dir).expect("mkdir wal");
    std::fs::create_dir_all(&snap_dir).expect("mkdir snapshots");

    // Ground-truth ExchangeState at engine_seq = 10 matching CH02 ledger-rs state
    let mut gt = ExchangeState::new();
    gt.engine_seq = 10;
    gt.ts_ns = 1_796_000_000_000_000_000;
    gt.maker_fee_ppm = 100;
    gt.taker_fee_ppm = 250;
    gt.credit_deposit(1, "BTC", 500_000_000).expect("dep btc");
    gt.credit_deposit(2, "USDT", 50_000_000_000_000)
        .expect("dep usdt");
    // Lock 1 BTC for account 1's resting ask order #101 @ 68,000 USDT
    let btc_sym = chronos_replay::fixed::AssetSymbol::from_str_trunc("BTC");
    assert!(gt.lock_funds(1, btc_sym, 100_000_000).expect("lock btc"));
    gt.insert_resting_order_raw(
        101,
        "ch02-ask-101",
        1,
        "BTC-USDT",
        1,
        2,
        6_800_000_000_000,
        100_000_000,
        100_000_000,
        8,
        gt.ts_ns,
    );
    let snap_root = gt.compute_state_root();
    let anchor_wal_hash = [0xABu8; 32];

    // Encode exact CH02 `SnapshotPayloadV1` JSON inside `HYPSNP01`
    let ch02_snap = LedgerSnapshotPayloadV1 {
        engine_seq: 10,
        ts_ns: gt.ts_ns,
        journal_seq: 3,
        last_wal_blake3: anchor_wal_hash,
        last_journal_blake3: [0x11u8; 32],
        next_order_id: 102,
        next_fill_id: 1,
        state_root_blake3: snap_root,
        accounts: vec![
            LedgerSnapshotAccountV1 {
                account_id: 0,
                status: 1,
                balances: vec![],
            },
            LedgerSnapshotAccountV1 {
                account_id: 1,
                status: 1,
                balances: vec![LedgerSnapshotAssetBalanceV1 {
                    asset: "BTC".to_string(),
                    available_fp: "400000000".to_string(),
                    locked_fp: "100000000".to_string(),
                }],
            },
            LedgerSnapshotAccountV1 {
                account_id: 2,
                status: 1,
                balances: vec![LedgerSnapshotAssetBalanceV1 {
                    asset: "USDT".to_string(),
                    available_fp: "50000000000000".to_string(),
                    locked_fp: "0".to_string(),
                }],
            },
        ],
        asset_totals: vec![
            LedgerSnapshotAssetTotalV1 {
                asset: "BTC".to_string(),
                total_deposits_fp: "500000000".to_string(),
                total_available_fp: "400000000".to_string(),
                total_locked_fp: "100000000".to_string(),
                treasury_fee_pool_fp: "0".to_string(),
            },
            LedgerSnapshotAssetTotalV1 {
                asset: "USDT".to_string(),
                total_deposits_fp: "50000000000000".to_string(),
                total_available_fp: "50000000000000".to_string(),
                total_locked_fp: "0".to_string(),
                treasury_fee_pool_fp: "0".to_string(),
            },
        ],
        instruments: vec![LedgerSnapshotInstrumentV1 {
            instrument_id: 1,
            symbol: "BTC-USDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            regime: 1,
            maker_fee_ppm: 100,
            taker_fee_ppm: 250,
            prev_l3_seq: 8,
            resting_orders: vec![LedgerSnapshotOrderV1 {
                order_id: 101,
                client_order_id: "ch02-ask-101".to_string(),
                account_id: 1,
                side: 2,
                kind: 1,
                stp_mode: 2,
                price_fp: 6_800_000_000_000,
                initial_qty_fp: 100_000_000,
                remaining_qty_fp: 100_000_000,
                locked_quote_fp: "0".to_string(),
                created_ts_ns: gt.ts_ns,
                priority_seq: 8,
            }],
        }],
        exec_leaf_hashes: vec![],
    };

    let payload_bytes = serde_json::to_vec(&ch02_snap).expect("json");
    let crc = compute_snapshot_crc32c(10, gt.ts_ns, &snap_root, &payload_bytes);
    let mut snap_file_bytes = Vec::new();
    snap_file_bytes.extend_from_slice(SNAPSHOT_MAGIC);
    snap_file_bytes.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    snap_file_bytes.extend_from_slice(&crc.to_le_bytes());
    snap_file_bytes.extend_from_slice(&10u64.to_le_bytes());
    snap_file_bytes.extend_from_slice(&gt.ts_ns.to_le_bytes());
    snap_file_bytes.extend_from_slice(&snap_root);
    snap_file_bytes.extend_from_slice(&(payload_bytes.len() as u64).to_le_bytes());
    snap_file_bytes.extend_from_slice(&payload_bytes);
    std::fs::write(
        snap_dir.join("snap_0000000000000010.snap"),
        &snap_file_bytes,
    )
    .expect("write ch02 snap");

    // Append compacted tail WAL starting at seq 11 anchored to `anchor_wal_hash`
    let mut tail_writer = WalSegmentWriter::with_chain_anchor(11, anchor_wal_hash);
    //CH02 2-leg EXCHANGE_VAULT + AVAILABLE deposit for account 3
    tail_writer
        .append_envelope(MarketEventEnvelope {
            engine_seq: 11,
            symbol: "USDT".to_string(),
            ts_ns: gt.ts_ns + 500,
            payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                journal_seq: 4,
                engine_seq: 11,
                reference_type: "DEPOSIT".to_string(),
                reference_id: 11,
                lines: vec![
                    LedgerEntryLine {
                        account_id: 0,
                        asset: "USDT".to_string(),
                        sub_account: "EXCHANGE_VAULT".to_string(),
                        debit_fp: Some(chronos_replay::fixed::i128_to_pb(10_000_000_000_000)),
                        credit_fp: Some(chronos_replay::fixed::i128_to_pb(0)),
                        balance_after_fp: Some(chronos_replay::fixed::i128_to_pb(
                            60_000_000_000_000,
                        )),
                    },
                    LedgerEntryLine {
                        account_id: 3,
                        asset: "USDT".to_string(),
                        sub_account: "AVAILABLE".to_string(),
                        debit_fp: Some(chronos_replay::fixed::i128_to_pb(0)),
                        credit_fp: Some(chronos_replay::fixed::i128_to_pb(10_000_000_000_000)),
                        balance_after_fp: Some(chronos_replay::fixed::i128_to_pb(
                            10_000_000_000_000,
                        )),
                    },
                ],
                ts_ns: gt.ts_ns + 500,
                prev_journal_blake3: vec![],
                journal_blake3: vec![],
            })),
        })
        .expect("append seq 11");
    tail_writer
        .write_to_path(&wal_dir.join("segment_00000001.wal"))
        .expect("write tail wal");

    // Advance ground-truth state identically
    gt.credit_deposit(3, "USDT", 10_000_000_000_000)
        .expect("gt dep 3");
    let expected_root_11 = gt.compute_state_root();

    let store = ChronosWalStore::open_data_dir(tmp.path(), 4).expect("open ch02 data dir");
    let engine = ChronosEngine::new(store);
    let seek_10 = engine
        .seek(&TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetEngineSeq(10)),
            l2_depth: 10,
            include_account_balances: true,
            verify_wal_hash_chain: true,
            trace: None,
        })
        .expect("seek 10");
    assert_eq!(seek_10.response.state_root_blake3, snap_root.to_vec());

    let seek_11 = engine
        .seek(&TimeTravelSeekRequest {
            symbol: "BTC-USDT".to_string(),
            target: Some(Target::TargetEngineSeq(11)),
            l2_depth: 10,
            include_account_balances: true,
            verify_wal_hash_chain: true,
            trace: None,
        })
        .expect("seek 11");
    assert_eq!(
        seek_11.response.state_root_blake3,
        expected_root_11.to_vec()
    );
    // Double-entry conservation must be balanced (no double-counting from EXCHANGE_VAULT + AVAILABLE!)
    assert!(seek_11
        .response
        .account_states
        .iter()
        .all(|p| p.double_entry_balanced));
}

#[test]
fn test_exact_boundary_invariants_for_math_merkle_wal_and_sandbox() {
    use chronos_replay::fixed::{
        checked_buyer_reserve_fp, checked_fee_fp, checked_notional_fp, checked_slippage_limit_fp,
        compute_realized_volatility_bps, compute_slippage_bps, isqrt_u128, AssetSymbol,
    };
    use chronos_replay::merkle::{
        hash_account_leaf, hash_merkle_branch, verify_merkle_proof, AssetBalanceLeaf,
        LobStateAccumulator, ProofStep, StateCommitmentTree,
    };
    use chronos_replay::wal::{parse_wal_segment_header, SparseIndexEntry, SparseWalIndex};

    // 1. Fixed-point math & exact boundaries
    assert_eq!(
        checked_notional_fp(6_800_000_000_000, 100_000_000).unwrap(),
        6_800_000_000_000
    );
    assert_eq!(checked_fee_fp(1_000_000_000_000, 500).unwrap(), 500_000_000);
    assert_eq!(checked_fee_fp(1_000_000_000_000, -50).unwrap(), -50_000_000);
    assert_eq!(
        checked_buyer_reserve_fp(100_000_000, 100_000_000, 100, 500).unwrap(),
        100_050_000
    );
    assert_eq!(
        checked_slippage_limit_fp(10_000_000_000, 100, true).unwrap(),
        10_100_000_000
    );
    assert_eq!(
        checked_slippage_limit_fp(10_000_000_000, 100, false).unwrap(),
        9_900_000_000
    );
    assert_eq!(isqrt_u128(10_000_000_000_000_000), 100_000_000);
    // Volatility for len == 2 must be computed (> 0 when returns differ across 3 or == 0 for 1 return, wait: for 2 prices [100, 110] variance of 1 sample is 0, wait: what distinguishes len < 2 vs len <= 2? Wait: for 2 prices there is 1 return so variance around its own mean is 0! Let's check!)
    assert_eq!(compute_realized_volatility_bps(&[10_000_000_000]), 0);
    assert!(compute_realized_volatility_bps(&[10_000_000_000, 10_200_000_000, 9_800_000_000]) > 0);
    assert_eq!(
        compute_slippage_bps(10_000_000_000, 10_050_000_000, true),
        50
    );
    assert_eq!(compute_slippage_bps(10_000_000_000, 9_950_000_000, true), 0);
    assert_eq!(
        compute_slippage_bps(10_000_000_000, 9_950_000_000, false),
        50
    );

    // 2. Merkle proof left/right verification, explicit RFC/domain-separation byte 0x01, & LtHash cancel symmetry
    let left = [0x11u8; 32];
    let right = [0x22u8; 32];
    let parent = hash_merkle_branch(&left, &right);
    let mut expected_internal_preimage = [0u8; 65];
    expected_internal_preimage[0] = 0x01;
    expected_internal_preimage[1..33].copy_from_slice(&left);
    expected_internal_preimage[33..65].copy_from_slice(&right);
    assert_eq!(
        parent,
        *blake3::hash(&expected_internal_preimage).as_bytes(),
        "Internal Merkle branch must use domain separator byte 0x01"
    );
    expected_internal_preimage[0] = 0x00;
    assert_ne!(
        parent,
        *blake3::hash(&expected_internal_preimage).as_bytes(),
        "Internal Merkle branch must never collide with leaf domain separator 0x00"
    );
    assert!(verify_merkle_proof(
        &right,
        &[ProofStep {
            sibling_hash: left,
            is_left_sibling: true,
            depth: 0
        }],
        &parent
    ));
    assert!(!verify_merkle_proof(
        &right,
        &[ProofStep {
            sibling_hash: left,
            is_left_sibling: false,
            depth: 0
        }],
        &parent
    ));

    let mut lob = LobStateAccumulator::new();
    let empty_lob_hash = lob.commitment_hash();
    let lanes = LobStateAccumulator::order_state_lanes(1, 10, 1, 1, 100, 50, 1);
    lob.add_lanes(lanes);
    assert_ne!(lob.commitment_hash(), empty_lob_hash);
    lob.sub_lanes(lanes);
    assert_eq!(lob.commitment_hash(), empty_lob_hash);

    let mut tree = StateCommitmentTree::new();
    let base_root = tree.commit_state_root(&lob);
    let leaf = hash_account_leaf(
        7,
        &[AssetBalanceLeaf {
            asset: AssetSymbol::from_str_trunc("USDT"),
            available_fp: 100,
            locked_fp: 0,
        }],
    );
    tree.upsert_account_leaf(7, leaf);
    let updated_root = tree.commit_state_root(&lob);
    assert_ne!(base_root, updated_root);

    // 3. SparseWalIndex exact equality boundaries (`<=` vs `<`) & WAL version check
    let mut idx = SparseWalIndex::new(4);
    idx.push(SparseIndexEntry {
        engine_seq: 10,
        ts_ns: 1000,
        segment_idx: 0,
        event_idx: 0,
        byte_offset: 16,
        prev_blake3: [0; 32],
        frame_blake3: [1; 32],
    });
    idx.push(SparseIndexEntry {
        engine_seq: 20,
        ts_ns: 2000,
        segment_idx: 0,
        event_idx: 4,
        byte_offset: 400,
        prev_blake3: [1; 32],
        frame_blake3: [2; 32],
    });
    assert_eq!(idx.anchor_for_seq(20).unwrap().engine_seq, 20);
    assert_eq!(idx.anchor_for_ts_ns(2000).unwrap().ts_ns, 2000);

    let mut bad_ver_hdr = Vec::new();
    bad_ver_hdr.extend_from_slice(b"HYPWAL01");
    bad_ver_hdr.extend_from_slice(&99u32.to_le_bytes());
    bad_ver_hdr.extend_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        parse_wal_segment_header(&bad_ver_hdr),
        Err(ChronosError::UnsupportedWalVersion(99))
    ));

    // 4. State exact-balance lock (`available_fp == amount_fp`), spread_fp, & crossed-book (`bid == ask`)
    let mut st = ExchangeState::new();
    st.credit_deposit(1, "USDT", 500_000_000).expect("dep");
    let usdt = AssetSymbol::from_str_trunc("USDT");
    assert!(st.lock_funds(1, usdt, 500_000_000).unwrap());
    st.verify_double_entry_conservation().expect("conserved");

    st.insert_resting_order_raw(1, "b1", 1, "BTC-USDT", 1, 1, 100_000_000, 10_000, 0, 1, 100);
    st.insert_resting_order_raw(2, "a1", 1, "BTC-USDT", 2, 2, 105_000_000, 10_000, 0, 2, 101);
    assert!(st.is_book_uncrossed("BTC-USDT"));
    assert_eq!(
        st.compute_liquidity_metrics("BTC-USDT").spread_fp,
        5_000_000
    );
    // Insert ask at exact same price 100_000_000 -> book is now crossed (`bid == ask`)!
    st.insert_resting_order_raw(3, "a2", 1, "BTC-USDT", 2, 2, 100_000_000, 10_000, 0, 3, 102);
    assert!(!st.is_book_uncrossed("BTC-USDT"));

    // 5. Engine exact boundaries: `end_seq == start_seq`, `qty_fp == max_qty`, `cur_seq == effective_from_seq`, `best_ask == limit_px`
    let mut rec = HistoricalTimelineRecorder::new(0, 100, 500);
    rec.record_deposit(1, "BTC", 100_000_000, 1000).unwrap();
    rec.record_deposit(2, "USDT", 10_000_000_000_000, 1001)
        .unwrap();
    let ask_seq = rec
        .record_submit_order(
            1,
            1,
            "BTC-USDT",
            2,
            OrderType::Limit,
            SelfTradePreventionMode::CancelResting,
            6_800_000_000_000,
            100_000_000,
            1002,
        )
        .unwrap();
    // Exact price match `limit_px == best_ask` (`6_800_000_000_000`) and exact `qty == 100_000_000`
    let buy_seq = rec
        .record_submit_order(
            2,
            2,
            "BTC-USDT",
            1,
            OrderType::Limit,
            SelfTradePreventionMode::CancelResting,
            6_800_000_000_000,
            100_000_000,
            1003,
        )
        .unwrap();
    let (b_store, _) = rec.build_store(4).unwrap();
    let b_eng = ChronosEngine::new(b_store);

    // `end_seq == start_seq` must succeed with zero replayed events
    let empty_fork = b_eng
        .fork_timeline(&TimelineForkRequest {
            fork_name: "zero-len".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: ask_seq,
            end_engine_seq: ask_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: None,
            trace: None,
        })
        .expect("end == start valid");
    assert!(empty_fork.state_identical);

    // `qty_fp == max_qty` (`100_000_000`) must NOT be rejected!
    let exact_cap_fork = b_eng
        .fork_timeline(&TimelineForkRequest {
            fork_name: "exact-cap".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: ask_seq,
            end_engine_seq: buy_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                max_order_qty_fp_override: Some(100_000_000),
                ..Default::default()
            }),
            trace: None,
        })
        .expect("exact cap fork");
    assert_eq!(exact_cap_fork.rejected_orders_count, 0);
    assert!(exact_cap_fork.state_identical);

    // `effective_from_seq` equal to exact command start seq (`ask_seq + 1`) must activate & freeze account 2!
    let exact_freeze_fork = b_eng
        .fork_timeline(&TimelineForkRequest {
            fork_name: "exact-seq-freeze".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: ask_seq,
            end_engine_seq: buy_seq,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                account_overrides: vec![AccountPolicyOverride {
                    account_id: 2,
                    status_override: AccountStatus::Frozen as i32,
                    effective_from_seq: ask_seq + 1,
                    effective_from_ts_ns: 0,
                    rate_limit_rps: 0,
                    cancel_resting_orders_on_activation: false,
                }],
                ..Default::default()
            }),
            trace: None,
        })
        .expect("exact freeze fork");
    assert_eq!(exact_freeze_fork.rejected_orders_count, 1);
}
