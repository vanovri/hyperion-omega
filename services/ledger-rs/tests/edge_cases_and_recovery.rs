use hyperion_proto::orderbook_service_client::OrderbookServiceClient;
use hyperion_proto::{
    ApplyAccountRiskActionRequest, CancelOrder, CancelOrderRequest, CreditDepositRequest,
    GetL2BookSnapshotRequest, GetMerkleLedgerProofRequest, MarketRegime, ModifyOrder,
    ModifyOrderRequest, OrderSide, OrderStatus, OrderType, RejectReason, SelfTradePreventionMode,
    StreamMarketEventsRequest, SubmitOrder, SubmitOrderRequest, TraceMetadata, U128,
    VerifySystemLedgerInvariantRequest,
};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use ledger_rs::engine::{DeterministicExchangeEngine, EngineSizing};
use ledger_rs::fixed::FP_SCALE_U64;
use ledger_rs::grpc::{
    spawn_health_and_metrics_http_server, HyperionOrderbookGrpcService, LmaxDisruptorHandle,
};
use ledger_rs::merkle::{verify_merkle_proof, ProofStep};
use ledger_rs::publisher::{
    deterministic_partition, encode_kafka_record_batch_v2, kafka_murmur2,
    spawn_async_redpanda_publisher, CapturedRedpandaSink, PublisherMetrics, TOPIC_EXECUTIONS,
    TOPIC_JOURNALS, TOPIC_L3_DELTAS,
};
use ledger_rs::ring::spsc_channel;
use ledger_rs::wal::{WalError, WalReader, WalSyncMode, WalWriter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;

fn test_sizing() -> EngineSizing {
    EngineSizing {
        max_accounts: 512,
        max_orders_per_book: 8_192,
        max_levels_per_side: 2_048,
        snapshot_every_n_events: 0,
    }
}

fn fund_account(eng: &mut DeterministicExchangeEngine, account_id: u64, asset: &str, units: u64) {
    let amt = (units as u128) * (FP_SCALE_U64 as u128);
    eng.execute_deposit_proto(CreditDepositRequest {
        deposit_id: format!("dep-{account_id}-{asset}"),
        account_id,
        asset: asset.to_string(),
        amount_fp: Some(U128::from(amt)),
        ts_ns: 0,
        trace: None,
    })
    .expect("deposit succeeds");
}

#[allow(clippy::too_many_arguments)]
fn make_order(
    account_id: u64,
    symbol: &str,
    side: OrderSide,
    order_type: OrderType,
    stp: SelfTradePreventionMode,
    price_fp: u64,
    qty_fp: u64,
    idem: &str,
) -> SubmitOrder {
    SubmitOrder {
        client_order_id: format!("cid-{account_id}-{price_fp}-{qty_fp}"),
        account_id,
        symbol: symbol.to_string(),
        instrument_id: 0,
        side: side as i32,
        order_type: order_type as i32,
        stp_mode: stp as i32,
        price_fp,
        qty_fp,
        max_slippage_bps: 0,
        client_ts_ns: 0,
        nonce: 1,
        signature: Vec::new(),
        idempotency_key: idem.to_string(),
        trace: None,
    }
}

#[test]
fn test_partial_fills_fifo_priority_maker_rebate_and_price_improvement() {
    let mut eng = DeterministicExchangeEngine::new_in_memory(test_sizing());
    // Configure negative maker rebate (-50 ppm = -0.5 bps) and taker fee (+200 ppm = +2.0 bps)
    eng.set_instrument_fees(1, "BTC-USDT", -50, 200);

    fund_account(&mut eng, 101, "BTC", 10);
    fund_account(&mut eng, 102, "BTC", 10);
    fund_account(&mut eng, 201, "USDT", 500_000);

    // Maker 101 places 1.0 BTC @ 50,000 USDT first; Maker 102 places 2.0 BTC @ 50,000 USDT second; Maker 101 places 1.0 BTC @ 50,100 USDT
    let ack_m1 = eng
        .execute_submit_proto(make_order(
            101,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            50_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "m1",
        ))
        .unwrap();
    let ack_m2 = eng
        .execute_submit_proto(make_order(
            102,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            50_000 * FP_SCALE_U64,
            2 * FP_SCALE_U64,
            "m2",
        ))
        .unwrap();
    let _ack_m3 = eng
        .execute_submit_proto(make_order(
            101,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            50_100 * FP_SCALE_U64,
            FP_SCALE_U64,
            "m3",
        ))
        .unwrap();

    // Taker 201 submits Limit Buy for 2.5 BTC @ 51,000 USDT (price improvement of 1,000 USDT/BTC!)
    let taker_ack = eng
        .execute_submit_proto(make_order(
            201,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            51_000 * FP_SCALE_U64,
            250_000_000, // 2.5 BTC
            "t1",
        ))
        .unwrap();

    assert_eq!(taker_ack.status, OrderStatus::Filled as i32);
    assert_eq!(taker_ack.filled_qty_fp, 250_000_000);
    assert_eq!(taker_ack.remaining_qty_fp, 0);
    assert_eq!(taker_ack.avg_fill_price_fp, 50_000 * FP_SCALE_U64);
    assert_eq!(taker_ack.fills.len(), 2);
    // Strict FIFO time priority: m1 filled completely (1.0 BTC) before m2 partially filled (1.5 BTC)!
    assert_eq!(taker_ack.fills[0].maker_order_id, ack_m1.order_id);
    assert_eq!(taker_ack.fills[0].qty_fp, FP_SCALE_U64);
    assert_eq!(taker_ack.fills[1].maker_order_id, ack_m2.order_id);
    assert_eq!(taker_ack.fills[1].qty_fp, 150_000_000);

    // Taker 201 must have 0 locked USDT left after price-improvement release!
    let (_, _, _, _, _, _, balanced) = eng.generate_account_merkle_proof(201, "USDT");
    assert!(balanced);
    let inv_usdt = eng.query_system_invariant_proto(VerifySystemLedgerInvariantRequest {
        asset: "USDT".to_string(),
    });
    assert!(inv_usdt.invariant_holds);
    assert!(inv_usdt.crossed_book_free);
    assert_eq!(inv_usdt.total_locked_fp.unwrap().lo, 0); // No BUY orders remain open!

    let inv_btc = eng.query_system_invariant_proto(VerifySystemLedgerInvariantRequest {
        asset: "BTC".to_string(),
    });
    assert!(inv_btc.invariant_holds);
    // Remaining resting BTC: 0.5 BTC on m2 + 1.0 BTC on m3 = 1.5 BTC locked
    assert_eq!(inv_btc.total_locked_fp.unwrap().lo, 150_000_000);
}

#[test]
fn test_fok_ioc_post_only_market_and_all_stp_modes() {
    let mut eng = DeterministicExchangeEngine::new_in_memory(test_sizing());
    fund_account(&mut eng, 10, "BTC", 20);
    fund_account(&mut eng, 10, "USDT", 1_000_000);
    fund_account(&mut eng, 20, "BTC", 20);
    fund_account(&mut eng, 20, "USDT", 1_000_000);

    // Account 10 rests Sell 1.0 BTC @ 60,000
    let m10 = eng
        .execute_submit_proto(make_order(
            10,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            60_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(m10.status, OrderStatus::Accepted as i32);

    // 1. Post-Only Buy @ 60,000 MUST reject without crossing!
    let po_ack = eng
        .execute_submit_proto(make_order(
            20,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::PostOnly,
            SelfTradePreventionMode::None,
            60_000 * FP_SCALE_U64,
            50_000_000,
            "",
        ))
        .unwrap();
    assert_eq!(po_ack.status, OrderStatus::Rejected as i32);
    assert_eq!(
        po_ack.reject_reason,
        RejectReason::PostOnlyWouldCross as i32
    );

    // 2. FOK Buy for 2.0 BTC @ 60,000 MUST reject cleanly with 0 fills (only 1.0 BTC available)!
    let state_before_fok = eng.state_root();
    let fok_ack = eng
        .execute_submit_proto(make_order(
            20,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Fok,
            SelfTradePreventionMode::None,
            60_000 * FP_SCALE_U64,
            2 * FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(fok_ack.status, OrderStatus::Rejected as i32);
    assert_eq!(
        fok_ack.reject_reason,
        RejectReason::FokInsufficientLiquidity as i32
    );
    assert_eq!(fok_ack.filled_qty_fp, 0);
    assert_eq!(eng.state_root(), state_before_fok);

    // 3. FOK Buy from SAME account (10) for 1.0 BTC with STP CancelAggressing MUST reject (self-liquidity cannot fill FOK!)
    let fok_stp = eng
        .execute_submit_proto(make_order(
            10,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Fok,
            SelfTradePreventionMode::CancelAggressing,
            60_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(fok_stp.status, OrderStatus::Rejected as i32);
    assert_eq!(
        fok_stp.reject_reason,
        RejectReason::FokInsufficientLiquidity as i32
    );

    // 4. STP CancelResting: Account 10 sends Limit Buy 1.0 BTC @ 60,000 -> cancels m10 and rests Buy!
    let stp_cr = eng
        .execute_submit_proto(make_order(
            10,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Limit,
            SelfTradePreventionMode::CancelResting,
            60_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(stp_cr.status, OrderStatus::Accepted as i32);
    assert_eq!(stp_cr.filled_qty_fp, 0);
    assert_eq!(stp_cr.remaining_qty_fp, FP_SCALE_U64);

    // 5. STP CancelBoth: Account 10 sends Limit Sell 1.0 BTC @ 60,000 -> cancels BOTH resting Buy and aggressing Sell!
    let stp_cb = eng
        .execute_submit_proto(make_order(
            10,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::CancelBoth,
            60_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(stp_cb.status, OrderStatus::Rejected as i32);
    assert_eq!(
        stp_cb.reject_reason,
        RejectReason::SelfTradePrevented as i32
    );
    let snap = eng
        .query_l2_snapshot_proto(GetL2BookSnapshotRequest {
            symbol: "BTC-USDT".to_string(),
            depth: 10,
        })
        .unwrap()
        .snapshot
        .unwrap();
    assert!(snap.bids.is_empty() && snap.asks.is_empty());

    // 6. IOC partial fill + expire remainder: Account 10 rests Sell 1.0 BTC @ 60,500; Account 20 sends IOC Buy 1.5 BTC @ 60,500
    eng.execute_submit_proto(make_order(
        10,
        "BTC-USDT",
        OrderSide::Sell,
        OrderType::Limit,
        SelfTradePreventionMode::None,
        60_500 * FP_SCALE_U64,
        FP_SCALE_U64,
        "",
    ))
    .unwrap();
    let ioc_ack = eng
        .execute_submit_proto(make_order(
            20,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Ioc,
            SelfTradePreventionMode::None,
            60_500 * FP_SCALE_U64,
            150_000_000,
            "",
        ))
        .unwrap();
    assert_eq!(ioc_ack.status, OrderStatus::Canceled as i32);
    assert_eq!(ioc_ack.filled_qty_fp, FP_SCALE_U64);
    assert_eq!(ioc_ack.remaining_qty_fp, 0);

    // Verify 100% conservation across BTC and USDT
    for asset in ["USDT", "BTC"] {
        let inv = eng.query_system_invariant_proto(VerifySystemLedgerInvariantRequest {
            asset: asset.to_string(),
        });
        assert!(inv.invariant_holds, "invariant failed for {asset}");
        assert!(inv.crossed_book_free);
    }
}

#[test]
fn test_modify_order_priority_and_merkle_tamper_detection() {
    let mut eng = DeterministicExchangeEngine::new_in_memory(test_sizing());
    fund_account(&mut eng, 1, "BTC", 10);
    fund_account(&mut eng, 2, "BTC", 10);
    fund_account(&mut eng, 3, "USDT", 500_000);

    // Acc 1 places 2.0 BTC @ 40,000; Acc 2 places 1.0 BTC @ 40,000
    let o1 = eng
        .execute_submit_proto(make_order(
            1,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            40_000 * FP_SCALE_U64,
            2 * FP_SCALE_U64,
            "idem-o1",
        ))
        .unwrap();
    // Duplicate idempotency key rejected!
    let dup = eng
        .execute_submit_proto(make_order(
            1,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            40_000 * FP_SCALE_U64,
            2 * FP_SCALE_U64,
            "idem-o1",
        ))
        .unwrap();
    assert_eq!(
        dup.reject_reason,
        RejectReason::DuplicateIdempotencyKey as i32
    );

    let o2 = eng
        .execute_submit_proto(make_order(
            2,
            "BTC-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            40_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();

    // Acc 1 reduces o1 size from 2.0 -> 1.0 BTC at same price -> MUST preserve FIFO priority ahead of o2!
    let mod_down = eng
        .execute_modify_proto(ModifyOrder {
            order_id: o1.order_id,
            client_order_id: "o1-mod".to_string(),
            account_id: 1,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            new_price_fp: 40_000 * FP_SCALE_U64,
            new_qty_fp: FP_SCALE_U64,
            client_ts_ns: 0,
            nonce: 2,
            signature: Vec::new(),
            idempotency_key: String::new(),
            trace: None,
        })
        .unwrap();
    assert_eq!(mod_down.status, OrderStatus::Modified as i32);

    // Now Acc 1 increases o1 size from 1.0 -> 1.5 BTC -> MUST lose priority and move BEHIND o2!
    let mod_up = eng
        .execute_modify_proto(ModifyOrder {
            order_id: o1.order_id,
            client_order_id: "o1-mod2".to_string(),
            account_id: 1,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            new_price_fp: 40_000 * FP_SCALE_U64,
            new_qty_fp: 150_000_000,
            client_ts_ns: 0,
            nonce: 3,
            signature: Vec::new(),
            idempotency_key: String::new(),
            trace: None,
        })
        .unwrap();
    assert_eq!(mod_up.status, OrderStatus::Modified as i32);

    // Taker 3 buys 1.0 BTC @ 40,000 -> MUST fill o2 (not o1)!
    let buy = eng
        .execute_submit_proto(make_order(
            3,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            40_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(buy.fills.len(), 1);
    assert_eq!(buy.fills[0].maker_order_id, o2.order_id);

    // Verify both Account Merkle Proof AND Execution Fill Merkle Proof against state_root_blake3!
    let acc_proof = eng
        .query_merkle_ledger_proof_for_test(3)
        .proof
        .expect("account proof");
    let mut leaf = [0u8; 32];
    leaf.copy_from_slice(&acc_proof.leaf_blake3_hash);
    let mut root = [0u8; 32];
    root.copy_from_slice(&acc_proof.state_root_blake3);
    let steps: Vec<ProofStep> = acc_proof
        .proof_path
        .iter()
        .map(|n| {
            let mut s = [0u8; 32];
            s.copy_from_slice(&n.sibling_hash);
            ProofStep {
                sibling_hash: s,
                is_left_sibling: n.is_left_sibling,
                depth: n.depth,
            }
        })
        .collect();
    assert!(verify_merkle_proof(&leaf, &steps, &root));

    // Tamper 1 bit in leaf -> proof MUST fail!
    let mut tampered_leaf = leaf;
    tampered_leaf[0] ^= 0x01;
    assert!(!verify_merkle_proof(&tampered_leaf, &steps, &root));

    // Execution history Merkle proof for fill_id = 1
    let exec_proof = eng.generate_execution_merkle_proof(1).expect("fill 1 proof");
    assert_eq!(exec_proof.state_root_blake3, root);
    assert!(verify_merkle_proof(
        &exec_proof.leaf_blake3_hash,
        &exec_proof.proof_path,
        &exec_proof.state_root_blake3
    ));
}

trait ProofTestExt {
    fn query_merkle_ledger_proof_for_test(
        &mut self,
        account_id: u64,
    ) -> hyperion_proto::GetMerkleLedgerProofResponse;
}

impl ProofTestExt for DeterministicExchangeEngine {
    fn query_merkle_ledger_proof_for_test(
        &mut self,
        account_id: u64,
    ) -> hyperion_proto::GetMerkleLedgerProofResponse {
        self.query_merkle_proof_proto(GetMerkleLedgerProofRequest {
            account_id,
            at_engine_seq: 0,
        })
    }
}

#[test]
fn test_wal_tail_corruption_healing_mid_chain_tamper_and_snapshot_crash_recovery() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("ledger");
    let wal_path = data_dir.join("wal").join("segment_00000001.wal");
    let snap_dir = data_dir.join("snapshots");

    let expected_root_at_crash: [u8; 32];
    let expected_seq_at_crash: u64;
    let resting_buy_id: u64;

    {
        let (wal, _, _) = WalWriter::open_segment(
            &wal_path,
            WalSyncMode::GroupCommit {
                flush_every_bytes: 4096,
            },
        )
        .unwrap();
        let mut eng = DeterministicExchangeEngine::with_wal_and_ring(
            test_sizing(),
            wal,
            Some(snap_dir.clone()),
            None,
        );

        for acc in 1..=6u64 {
            fund_account(&mut eng, acc, "USDT", 500_000);
            fund_account(&mut eng, acc, "BTC", 20);
            fund_account(&mut eng, acc, "ETH", 100);
        }

        // Phase 1: pre-snapshot orders & trades across BTC-USDT and ETH-USDT
        for i in 0..40u64 {
            let maker_acc = (i % 3) + 1;
            let taker_acc = (i % 3) + 4;
            eng.execute_submit_proto(make_order(
                maker_acc,
                "BTC-USDT",
                OrderSide::Sell,
                OrderType::Limit,
                SelfTradePreventionMode::None,
                (65_000 + (i % 10) * 10) * FP_SCALE_U64,
                50_000_000,
                "",
            ))
            .unwrap();
            eng.execute_submit_proto(make_order(
                taker_acc,
                "BTC-USDT",
                OrderSide::Buy,
                OrderType::Limit,
                SelfTradePreventionMode::None,
                (64_950 + (i % 15) * 10) * FP_SCALE_U64,
                40_000_000,
                "",
            ))
            .unwrap();
        }

        // Write periodic `HYPSNP01` snapshot!
        let snap_file = eng.write_snapshot_to_dir(&snap_dir).expect("write snapshot");
        assert!(snap_file.exists());

        // Phase 2: post-snapshot tail WAL events (including a resting Buy order and RiskAction!)
        let b_ack = eng
            .execute_submit_proto(make_order(
                4,
                "ETH-USDT",
                OrderSide::Buy,
                OrderType::Limit,
                SelfTradePreventionMode::None,
                3_200 * FP_SCALE_U64,
                2 * FP_SCALE_U64,
                "",
            ))
            .unwrap();
        resting_buy_id = b_ack.order_id;

        eng.execute_submit_proto(make_order(
            1,
            "ETH-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            3_200 * FP_SCALE_U64,
            50_000_000, // Partial fill 0.5 ETH against resting_buy_id!
            "",
        ))
        .unwrap();

        eng.execute_risk_action_proto(ApplyAccountRiskActionRequest {
            account_id: 6,
            new_status: hyperion_proto::AccountStatus::Frozen as i32,
            symbol_regime_override: MarketRegime::Normal as i32,
            target_symbol: "BTC-USDT".to_string(),
            cancel_all_open_orders: true,
            command_id: "risk-freeze-6".to_string(),
            reason: "wash ring detected".to_string(),
            trace: None,
        })
        .unwrap();

        eng.wal_mut().flush_and_sync().unwrap();
        expected_root_at_crash = eng.state_root();
        expected_seq_at_crash = eng.engine_seq();
    }

    // Inject torn/corrupted bytes at the tail of `segment_00000001.wal` to simulate sudden power loss mid-frame!
    {
        let mut f = OpenOptions::new().append(true).open(&wal_path).unwrap();
        // Write a fake partial WAL frame header + corrupted payload at EOF
        f.write_all(&[0x50, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x99, 0x88, 0x77])
            .unwrap();
        f.sync_all().unwrap();
    }

    // Strict verifier rejects corrupted tail
    assert!(matches!(
        WalReader::verify_segment_strict(&wal_path),
        Err(WalError::CrcMismatch { .. })
    ));

    // Crash recovery heals torn tail + loads Snapshot + replays Tail WAL -> 100% BIT-IDENTICAL STATE ROOT!
    let (mut recovered, replayed_tail_frames, healed_tail) =
        DeterministicExchangeEngine::recover_from_data_dir(
            test_sizing(),
            &data_dir,
            WalSyncMode::SyncEveryFrame,
            None,
        )
        .expect("crash recovery succeeds");

    assert!(healed_tail, "must detect and heal corrupted tail bytes");
    assert!(replayed_tail_frames > 0, "must replay post-snapshot tail frames");
    assert_eq!(recovered.engine_seq(), expected_seq_at_crash);
    assert_eq!(recovered.state_root(), expected_root_at_crash);

    // Account 6 must still be FROZEN after recovery!
    let frozen_try = recovered
        .execute_submit_proto(make_order(
            6,
            "BTC-USDT",
            OrderSide::Buy,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            60_000 * FP_SCALE_U64,
            FP_SCALE_U64,
            "",
        ))
        .unwrap();
    assert_eq!(frozen_try.status, OrderStatus::Rejected as i32);
    assert_eq!(frozen_try.reject_reason, RejectReason::AccountFrozen as i32);

    // Post-recovery live order matching against partially-filled `resting_buy_id` (remaining 1.5 ETH) must keep 0 drift!
    let fill_rem = recovered
        .execute_submit_proto(make_order(
            2,
            "ETH-USDT",
            OrderSide::Sell,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            3_200 * FP_SCALE_U64,
            150_000_000,
            "",
        ))
        .unwrap();
    assert_eq!(fill_rem.status, OrderStatus::Filled as i32);
    assert_eq!(fill_rem.fills[0].maker_order_id, resting_buy_id);

    for asset in ["USDT", "BTC", "ETH"] {
        let inv = recovered.query_system_invariant_proto(VerifySystemLedgerInvariantRequest {
            asset: asset.to_string(),
        });
        assert!(inv.invariant_holds, "post-recovery invariant failed on {asset}");
        assert!(inv.crossed_book_free);
    }

    // Also verify mid-chain byte tampering (in the first frame) is strictly caught as CrcMismatch!
    drop(recovered);
    {
        let mut f = OpenOptions::new().read(true).write(true).open(&wal_path).unwrap();
        f.seek(SeekFrom::Start(24)).unwrap(); // Inside first frame's engine_seq!
        f.write_all(&[0xFF, 0xFF]).unwrap();
        f.sync_all().unwrap();
    }
    assert!(WalReader::recover_segment(&wal_path).is_err());
}

#[test]
fn test_kafka_murmur2_and_record_batch_v2_wire_encoding() {
    // Deterministic partition consistency check
    let p1 = deterministic_partition(b"BTC-USDT", 8);
    let p2 = deterministic_partition(b"BTC-USDT", 8);
    assert_eq!(p1, p2);
    assert!((0..8).contains(&p1));
    assert_ne!(kafka_murmur2(b"BTC-USDT"), kafka_murmur2(b"ETH-USDT"));

    let rec = ledger_rs::publisher::PublishedKafkaRecord {
        topic: TOPIC_EXECUTIONS,
        partition: p1,
        key: b"BTC-USDT".to_vec(),
        payload: vec![1, 2, 3, 4, 5],
        engine_seq: 42,
        ts_ms: 1_700_000_000_000,
        schema_type: "hyperion.v1.ExecutionFill",
        traceparent: "00-0000000000000000000000000000002a-000000000000002a-01".to_string(),
    };
    let batch = encode_kafka_record_batch_v2(&rec);
    assert_eq!(batch[16], 2u8, "Kafka RecordBatch magic byte must be 2");
    let stored_crc = u32::from_be_bytes([batch[17], batch[18], batch[19], batch[20]]);
    let calc_crc = crc32c::crc32c(&batch[21..]);
    assert_eq!(stored_crc, calc_crc, "RecordBatch v2 CRC32C must verify");
}

#[tokio::test]
async fn test_grpc_server_otel_spans_streaming_and_mock_redpanda_broker_e2e() {
    // 1. Spin up mock Redpanda Kafka TCP broker speaking ProduceResponse v3
    let mock_broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let broker_addr = mock_broker.local_addr().unwrap().to_string();
    let broker_received = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let br_clone = Arc::clone(&broker_received);

    tokio::spawn(async move {
        if let Ok((mut sock, _)) = mock_broker.accept().await {
            let mut len_buf = [0u8; 4];
            while sock.read_exact(&mut len_buf).await.is_ok() {
                let req_len = i32::from_be_bytes(len_buf) as usize;
                let mut body = vec![0u8; req_len];
                if sock.read_exact(&mut body).await.is_err() {
                    break;
                }
                let corr_id = i32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                br_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Minimal 8-byte ack frame (length=4, correlation_id)
                let mut resp = [0u8; 8];
                resp[0..4].copy_from_slice(&4i32.to_be_bytes());
                resp[4..8].copy_from_slice(&corr_id.to_be_bytes());
                let _ = sock.write_all(&resp).await;
            }
        }
    });

    // 2. Spin up LMAX single-writer + SPSC async publisher + Tonic gRPC server + HTTP health/metrics
    let (spsc_prod, spsc_cons) = spsc_channel(4096);
    let eng = DeterministicExchangeEngine::with_wal_and_ring(
        test_sizing(),
        WalWriter::in_memory(),
        None,
        Some(spsc_prod),
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let pub_metrics = Arc::new(PublisherMetrics::default());
    let captured = CapturedRedpandaSink::new();
    let (stream_tx, _) = broadcast::channel(1024);

    let _pub_handle = spawn_async_redpanda_publisher(
        spsc_cons,
        Some(broker_addr),
        Some(captured.clone()),
        stream_tx.clone(),
        Arc::clone(&pub_metrics),
        Arc::clone(&shutdown),
    );

    let handle = LmaxDisruptorHandle::spawn(
        eng,
        4096,
        Arc::clone(&pub_metrics),
        stream_tx,
        Arc::clone(&shutdown),
    );

    let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap().to_string();
    drop(http_listener);
    let _http_task = spawn_health_and_metrics_http_server(
        http_addr.clone(),
        handle.clone(),
        Arc::clone(&shutdown),
    );

    let grpc_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_addr = grpc_listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
    let svc = HyperionOrderbookGrpcService::new(handle.clone()).into_server();
    let sd_grpc = Arc::clone(&shutdown);
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(svc)
            .serve_with_incoming_shutdown(incoming, async move {
                while !sd_grpc.load( std::sync::atomic::Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await;
    });

    tokio::time::sleep(Duration::from_millis(40)).await;

    // 3. Connect real Tonic gRPC client
    let mut client = OrderbookServiceClient::connect(format!("http://{grpc_addr}"))
        .await
        .expect("connect gRPC client");

    // Subscribe to StreamMarketEvents
    let mut ev_stream = client
        .stream_market_events(StreamMarketEventsRequest {
            symbols: vec!["BTC-USDT".to_string()],
            from_engine_seq: 1,
            include_l3_deltas: true,
            include_fills: true,
        })
        .await
        .unwrap()
        .into_inner();

    // Credit deposits
    for (acc, asset, amt) in [
        (1001u64, "BTC", 5 * FP_SCALE_U64 as u128),
        (1002u64, "USDT", 500_000 * FP_SCALE_U64 as u128),
    ] {
        client
            .credit_deposit(CreditDepositRequest {
                deposit_id: format!("d-{acc}"),
                account_id: acc,
                asset: asset.to_string(),
                amount_fp: Some(U128::from(amt)),
                ts_ns: 0,
                trace: Some(TraceMetadata {
                    trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_string(),
                    span_id: "00f067aa0ba902b7".to_string(),
                    correlation_id: "e2e-corr-1".to_string(),
                    source_service: "gateway-go".to_string(),
                    ingest_ts_ns: 1_700_000_000_000_000_000,
                }),
            })
            .await
            .unwrap();
    }

    // Place resting Sell + crossing Buy + Cancel
    let maker_resp = client
        .submit_order(SubmitOrderRequest {
            order: Some(make_order(
                1001,
                "BTC-USDT",
                OrderSide::Sell,
                OrderType::Limit,
                SelfTradePreventionMode::None,
                68_000 * FP_SCALE_U64,
                2 * FP_SCALE_U64,
                "g-m1",
            )),
        })
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert_eq!(maker_resp.status, OrderStatus::Accepted as i32);

    let taker_resp = client
        .submit_order(SubmitOrderRequest {
            order: Some(make_order(
                1002,
                "BTC-USDT",
                OrderSide::Buy,
                OrderType::Limit,
                SelfTradePreventionMode::None,
                68_000 * FP_SCALE_U64,
                FP_SCALE_U64,
                "g-t1",
            )),
        })
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert_eq!(taker_resp.status, OrderStatus::Filled as i32);
    assert_eq!(taker_resp.fills.len(), 1);

    // Modify remaining 1.0 BTC -> 0.5 BTC on maker_resp over gRPC, then cancel
    let mod_resp = client
        .modify_order(ModifyOrderRequest {
            modify: Some(ModifyOrder {
                order_id: maker_resp.order_id,
                client_order_id: "g-m1-mod".to_string(),
                account_id: 1001,
                symbol: "BTC-USDT".to_string(),
                instrument_id: 1,
                new_price_fp: 68_000 * FP_SCALE_U64,
                new_qty_fp: 150_000_000, // 1.0 already filled + 0.5 remaining
                client_ts_ns: 0,
                nonce: 2,
                signature: Vec::new(),
                idempotency_key: "g-mod1".to_string(),
                trace: None,
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert_eq!(mod_resp.status, OrderStatus::Modified as i32);
    assert_eq!(mod_resp.remaining_qty_fp, 50_000_000);

    // Cancel remaining 0.5 BTC on maker_resp
    let cancel_resp = client
        .cancel_order(CancelOrderRequest {
            cancel: Some(CancelOrder {
                order_id: maker_resp.order_id,
                client_order_id: String::new(),
                account_id: 1001,
                symbol: "BTC-USDT".to_string(),
                instrument_id: 1,
                client_ts_ns: 0,
                nonce: 3,
                signature: Vec::new(),
                idempotency_key: "g-c1".to_string(),
                trace: None,
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert_eq!(cancel_resp.status, OrderStatus::Canceled as i32);

    // Receive streamed events from StreamMarketEvents
    let streamed_first = tokio::time::timeout(Duration::from_secs(2), ev_stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(streamed_first.symbol, "BTC-USDT");

    // Verify Merkle proof & System Invariant over gRPC
    let merkle_resp = client
        .get_merkle_ledger_proof(GetMerkleLedgerProofRequest {
            account_id: 1002,
            at_engine_seq: 0,
        })
        .await
        .unwrap()
        .into_inner()
        .proof
        .unwrap();
    assert!(merkle_resp.double_entry_balanced);

    let inv_resp = client
        .verify_system_ledger_invariant(VerifySystemLedgerInvariantRequest {
            asset: "USDT".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(inv_resp.invariant_holds);
    assert!(inv_resp.crossed_book_free);

    // Wait briefly for async Redpanda SPSC drain
    tokio::time::sleep(Duration::from_millis(50)).await;
    let recs = captured.snapshot();
    assert!(recs.iter().any(|r| r.topic == TOPIC_L3_DELTAS));
    assert!(recs.iter().any(|r| r.topic == TOPIC_EXECUTIONS));
    assert!(recs.iter().any(|r| r.topic == TOPIC_JOURNALS));
    assert!(
        broker_received.load(std::sync::atomic::Ordering::Relaxed) >= recs.len(),
        "mock Redpanda TCP broker must receive Kafka Produce v3 wire frames"
    );

    // Probe HTTP /healthz and /metrics
    for path in ["/healthz", "/metrics"] {
        let mut s = tokio::net::TcpStream::connect(&http_addr).await.unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).await.unwrap();
        assert!(resp.contains("200 OK"), "{path} must return 200 OK");
    }

    handle.shutdown().await;
}
