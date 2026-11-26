use crate::arena_lob::{InstrumentOrderBook, OrderKind, OrderRecord, Side, StpPolicy};
use crate::engine::{
    AckStatus, CancelOrderCmd, DeterministicExchangeEngine, EngineSizing, ModifyOrderCmd,
    RejectCode, SubmitOrderCmd,
};
use crate::fixed::{
    checked_buyer_reserve_fp, checked_fee_fp, checked_notional_fp, checked_slippage_limit_fp,
    AssetSymbol, ClientOrderId, InstrumentSymbol, MathError, FP_SCALE_U64, MAX_ABS_FEE_PPM,
};
use crate::ledger::{DoubleEntryLedger, LedgerError};
use crate::merkle::{
    hash_account_leaf, hash_merkle_branch, verify_merkle_proof, AssetBalanceLeaf,
    IncrementalMerkleState, LobStateAccumulator,
};
use crate::ring::{spsc_channel, MpscDisruptorRing};
use crate::wal::{
    compute_wal_frame_blake3, compute_wal_frame_crc32c, SnapshotStore, WalError, WalReader,
    WalSyncMode, WalWriter,
};

fn small_engine() -> DeterministicExchangeEngine {
    DeterministicExchangeEngine::new_in_memory(EngineSizing {
        max_accounts: 64,
        max_orders_per_book: 1024,
        max_levels_per_side: 256,
        snapshot_every_n_events: 0,
    })
}

#[test]
fn unit_fixed_point_exact_boundaries_and_signs() {
    // Exact 1e8 scale boundary
    assert_eq!(
        checked_notional_fp(250_000_000, 400_000_000).unwrap(),
        1_000_000_000
    ); // 2.5 * 4.0 = 10.0
    assert_eq!(checked_notional_fp(1, 99_999_999).unwrap(), 0); // sub-tick floor
    assert_eq!(checked_notional_fp(1, 100_000_000).unwrap(), 1); // exact 1 atomic unit

    // Fee rate exact cap boundaries: -MAX_ABS_FEE_PPM and +MAX_ABS_FEE_PPM must succeed; +-1 beyond must fail!
    assert_eq!(checked_fee_fp(1_000_000, MAX_ABS_FEE_PPM).unwrap(), 500_000);
    assert_eq!(
        checked_fee_fp(1_000_000, -MAX_ABS_FEE_PPM).unwrap(),
        -500_000
    );
    assert_eq!(
        checked_fee_fp(1_000_000, MAX_ABS_FEE_PPM + 1),
        Err(MathError::InvalidFeeRate(MAX_ABS_FEE_PPM + 1))
    );
    assert_eq!(
        checked_fee_fp(1_000_000, -MAX_ABS_FEE_PPM - 1),
        Err(MathError::InvalidFeeRate(-MAX_ABS_FEE_PPM - 1))
    );
    assert_eq!(checked_fee_fp(1_000_000, 0).unwrap(), 0);

    // Buyer reserve across all maker/taker combinations
    // 1) taker > maker > 0
    assert_eq!(
        checked_buyer_reserve_fp(100_000_000, 100_000_000, 100, 250).unwrap(),
        100_025_000
    );
    // 2) maker > taker > 0
    assert_eq!(
        checked_buyer_reserve_fp(100_000_000, 100_000_000, 300, 100).unwrap(),
        100_030_000
    );
    // 3) maker == taker > 0
    assert_eq!(
        checked_buyer_reserve_fp(100_000_000, 100_000_000, 200, 200).unwrap(),
        100_020_000
    );
    // 4) negative maker rebate & zero taker -> reserve equals exact notional (no negative subtraction!)
    assert_eq!(
        checked_buyer_reserve_fp(100_000_000, 100_000_000, -150, 0).unwrap(),
        100_000_000
    );

    // Slippage buy (+bps) vs sell (-bps)
    assert_eq!(
        checked_slippage_limit_fp(10_000_000_000, 50, true).unwrap(),
        10_050_000_000
    );
    assert_eq!(
        checked_slippage_limit_fp(10_000_000_000, 50, false).unwrap(),
        9_950_000_000
    );
}

#[test]
fn unit_lob_uncrossed_boundary_and_lthash_reversibility() {
    let mut book = InstrumentOrderBook::new(1, "BTC-USDT", "BTC", "USDT", 64, 32);
    let mut acc = LobStateAccumulator::new();
    let empty_commit = acc.commitment_hash();
    let cid = ClientOrderId::from_str_trunc("u1");

    let s1 = book
        .insert_resting(
            OrderRecord {
                order_id: 1,
                account_id: 10,
                instrument_id: 1,
                side: Side::Buy,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 100_000_000,
                initial_qty_fp: 50_000_000,
                remaining_qty_fp: 50_000_000,
                locked_quote_fp: 50_000_000,
                created_ts_ns: 1,
                priority_seq: 1,
                client_order_id: cid,
            },
            &mut acc,
        )
        .unwrap();
    assert_ne!(acc.commitment_hash(), empty_commit);

    // Insert a strictly lower second bid (99.00) -> highest bid (100.00) MUST remain best_bid (kills M11!)
    let s1_low = book
        .insert_resting(
            OrderRecord {
                order_id: 10,
                account_id: 11,
                instrument_id: 1,
                side: Side::Buy,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 99_000_000,
                initial_qty_fp: 30_000_000,
                remaining_qty_fp: 30_000_000,
                locked_quote_fp: 30_000_000,
                created_ts_ns: 1,
                priority_seq: 1,
                client_order_id: cid,
            },
            &mut acc,
        )
        .unwrap();
    assert_eq!(book.best_bid_fp(), Some(100_000_000));
    let (bids_l2, _) = book.snapshot_l2(2);
    assert_eq!(bids_l2.len(), 2);
    assert_eq!(bids_l2[0].price_fp, 100_000_000);
    assert_eq!(bids_l2[1].price_fp, 99_000_000);

    // Ask strictly greater than Bid -> uncrossed == true
    let s2 = book
        .insert_resting(
            OrderRecord {
                order_id: 2,
                account_id: 20,
                instrument_id: 1,
                side: Side::Sell,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 100_000_001,
                initial_qty_fp: 50_000_000,
                remaining_qty_fp: 50_000_000,
                locked_quote_fp: 0,
                created_ts_ns: 2,
                priority_seq: 2,
                client_order_id: cid,
            },
            &mut acc,
        )
        .unwrap();
    assert!(book.is_uncrossed());

    // Ask EQUAL to Bid -> crossed (`is_uncrossed` MUST be false!)
    let s3 = book
        .insert_resting(
            OrderRecord {
                order_id: 3,
                account_id: 20,
                instrument_id: 1,
                side: Side::Sell,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 100_000_000,
                initial_qty_fp: 10_000_000,
                remaining_qty_fp: 10_000_000,
                locked_quote_fp: 0,
                created_ts_ns: 3,
                priority_seq: 3,
                client_order_id: cid,
            },
            &mut acc,
        )
        .unwrap();
    assert!(!book.is_uncrossed());

    // Reduce qty in-place changes commitment AND updates price-level total_qty_fp (kills M12!),
    // removing all restores exact `empty_commit`!
    let c_before_red = acc.commitment_hash();
    book.reduce_resting_qty_in_place(s1, 25_000_000, 25_000_000, &mut acc);
    assert_ne!(acc.commitment_hash(), c_before_red);
    let (bids_after_red, _) = book.snapshot_l2(2);
    assert_eq!(bids_after_red[0].total_qty_fp, 25_000_000);

    book.remove_resting_by_slot(s3, &mut acc);
    book.remove_resting_by_slot(s2, &mut acc);
    book.remove_resting_by_slot(s1, &mut acc);
    book.remove_resting_by_slot(s1_low, &mut acc);
    assert_eq!(acc.commitment_hash(), empty_commit);
}

#[test]
fn unit_ledger_exact_balance_boundaries_and_net_negative_rebate() {
    let mut ledger = DoubleEntryLedger::with_capacity(16);
    let mut merkle = IncrementalMerkleState::new();
    let usdt = AssetSymbol::from_str_trunc("USDT");
    let btc = AssetSymbol::from_str_trunc("BTC");

    assert_eq!(
        ledger.credit_deposit(&mut merkle, 1, 1, 0, usdt, 100, 1),
        Err(LedgerError::InvalidAmount)
    );
    assert_eq!(
        ledger.credit_deposit(&mut merkle, 1, 1, 1, usdt, 0, 1),
        Err(LedgerError::InvalidAmount)
    );

    ledger
        .credit_deposit(&mut merkle, 1, 1, 1, usdt, 1_000_000, 10)
        .unwrap();
    ledger
        .credit_deposit(&mut merkle, 2, 2, 2, btc, 500_000, 11)
        .unwrap();

    // Locking 1_000_001 fails; locking exact 1_000_000 succeeds leaving 0 available!
    assert_eq!(
        ledger.lock_funds(&mut merkle, 3, 100, 1, usdt, 1_000_001, 12),
        Err(LedgerError::InsufficientBalance)
    );
    ledger
        .lock_funds(&mut merkle, 3, 100, 1, usdt, 1_000_000, 12)
        .unwrap();
    assert_eq!(ledger.available_balance(1, usdt), 0);
    assert_eq!(ledger.locked_balance(1, usdt), 1_000_000);

    // Unlocking 1_000_001 fails; unlocking 200_000 leaves 800_000 locked
    assert_eq!(
        ledger.unlock_funds(&mut merkle, 4, 100, 1, usdt, 1_000_001, "REL", 13),
        Err(LedgerError::InsufficientLockedBalance)
    );
    ledger
        .unlock_funds(&mut merkle, 4, 100, 1, usdt, 200_000, "REL", 13)
        .unwrap();
    assert_eq!(ledger.available_balance(1, usdt), 200_000);
    assert_eq!(ledger.locked_balance(1, usdt), 800_000);

    // Lock exact 500_000 BTC on Seller (acc 2)
    ledger
        .lock_funds(&mut merkle, 5, 101, 2, btc, 500_000, 14)
        .unwrap();

    // Settle trade where Maker Rebate (-5_000) exceeds Taker Fee (+2_000) -> net_treasury_fee = -3_000!
    let (spent, j) = ledger
        .settle_fill(
            &mut merkle,
            6,
            1,
            1,
            2,
            btc,
            usdt,
            500_000,
            700_000,
            2_000,  // buyer taker fee
            -5_000, // seller maker rebate
            15,
        )
        .unwrap();
    assert_eq!(spent, 702_000);
    assert_eq!(ledger.locked_balance(1, usdt), 98_000);
    assert_eq!(ledger.available_balance(2, usdt), 705_000);
    assert_eq!(ledger.asset_totals(usdt).treasury_fee_pool_fp, -3_000);
    // Double-entry balance must still hold to 0 atomic units!
    let sum_d: i128 = j.legs_slice().iter().map(|l| l.debit_fp).sum();
    let sum_c: i128 = j.legs_slice().iter().map(|l| l.credit_fp).sum();
    assert_eq!(sum_d, sum_c);
    assert!(ledger.verify_asset_conservation(usdt).1);
    assert!(ledger.verify_asset_conservation(btc).1);
}

#[test]
fn unit_engine_crossing_fok_postonly_and_modify_exact_boundaries() {
    let mut eng = small_engine();
    let sym = InstrumentSymbol::from_str_trunc("BTC-USDT");
    let usdt = AssetSymbol::from_str_trunc("USDT");
    let btc = AssetSymbol::from_str_trunc("BTC");
    let cid = ClientOrderId::from_str_trunc("c");

    eng.credit_deposit_fast(1, btc, 100_000_000, 1).unwrap(); // exact 1.0 BTC
    eng.credit_deposit_fast(2, usdt, 20_000_000_000, 2).unwrap(); // 200 USDT

    // Seller 1 rests exact 1.0 BTC @ 100.00 USDT
    let sell = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 1,
        instrument_id: 1,
        symbol: sym,
        side: Side::Sell,
        kind: OrderKind::Limit,
        stp_mode: StpPolicy::None,
        price_fp: 100 * FP_SCALE_U64,
        qty_fp: FP_SCALE_U64,
        max_slippage_bps: 0,
        ts_ns: 3,
        idempotency_hash: None,
    });
    assert_eq!(sell.status, AckStatus::Accepted);

    // PostOnly Buy @ 99.99999999 USDT (1 tick below ask) MUST succeed; @ 100.00 USDT MUST reject!
    let po_ok = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::PostOnly,
        stp_mode: StpPolicy::None,
        price_fp: 100 * FP_SCALE_U64 - 1,
        qty_fp: 10_000_000,
        max_slippage_bps: 0,
        ts_ns: 4,
        idempotency_hash: None,
    });
    assert_eq!(po_ok.status, AckStatus::Accepted);
    let _ = eng.cancel_order_fast(CancelOrderCmd {
        order_id: po_ok.order_id,
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        ts_ns: 5,
        idempotency_hash: None,
    });

    let po_rej = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::PostOnly,
        stp_mode: StpPolicy::None,
        price_fp: 100 * FP_SCALE_U64,
        qty_fp: 10_000_000,
        max_slippage_bps: 0,
        ts_ns: 6,
        idempotency_hash: None,
    });
    assert_eq!(po_rej.reject_reason, RejectCode::PostOnlyWouldCross);

    // FOK Buy for 1.00000001 BTC MUST reject; FOK Buy for exact 1.00000000 BTC @ exact 100.00 USDT MUST fill!
    let fok_rej = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::Fok,
        stp_mode: StpPolicy::None,
        price_fp: 100 * FP_SCALE_U64,
        qty_fp: FP_SCALE_U64 + 1,
        max_slippage_bps: 0,
        ts_ns: 7,
        idempotency_hash: None,
    });
    assert_eq!(fok_rej.reject_reason, RejectCode::FokInsufficientLiquidity);

    let fok_ok = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::Fok,
        stp_mode: StpPolicy::None,
        price_fp: 100 * FP_SCALE_U64,
        qty_fp: FP_SCALE_U64,
        max_slippage_bps: 0,
        ts_ns: 8,
        idempotency_hash: None,
    });
    assert_eq!(fok_ok.status, AckStatus::Filled);
    assert_eq!(fok_ok.filled_qty_fp, FP_SCALE_U64);

    // Modify boundary: failed balance increase leaves existing order alive!
    let b_rest = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::Limit,
        stp_mode: StpPolicy::None,
        price_fp: 50 * FP_SCALE_U64,
        qty_fp: FP_SCALE_U64,
        max_slippage_bps: 0,
        ts_ns: 9,
        idempotency_hash: None,
    });
    assert_eq!(b_rest.status, AckStatus::Accepted);
    let mod_fail = eng.modify_order_fast(ModifyOrderCmd {
        order_id: b_rest.order_id,
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        new_price_fp: 500 * FP_SCALE_U64, // Needs 500 USDT > remaining ~100 USDT
        new_qty_fp: FP_SCALE_U64,
        ts_ns: 10,
        idempotency_hash: None,
    });
    assert_eq!(mod_fail.reject_reason, RejectCode::InsufficientBalance);
    // Original resting order MUST still cancel cleanly!
    let can_ok = eng.cancel_order_fast(CancelOrderCmd {
        order_id: b_rest.order_id,
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        ts_ns: 11,
        idempotency_hash: None,
    });
    assert_eq!(can_ok.status, AckStatus::Canceled);

    // Sell-side PostOnly & exact-price Sell crossing boundary checks (kills M26 & M29!)
    let rest_bid = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 2,
        instrument_id: 1,
        symbol: sym,
        side: Side::Buy,
        kind: OrderKind::Limit,
        stp_mode: StpPolicy::None,
        price_fp: 80 * FP_SCALE_U64,
        qty_fp: 50_000_000,
        max_slippage_bps: 0,
        ts_ns: 12,
        idempotency_hash: None,
    });
    assert_eq!(rest_bid.status, AckStatus::Accepted);

    // Top-up exact 0.5 BTC to account 1 (account 1 sold its initial 1.0 BTC in `fok_ok` above).
    eng.credit_deposit_fast(1, btc, 50_000_000, 12).unwrap();

    let po_sell_rej = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 1,
        instrument_id: 1,
        symbol: sym,
        side: Side::Sell,
        kind: OrderKind::PostOnly,
        stp_mode: StpPolicy::None,
        price_fp: 80 * FP_SCALE_U64, // Exact equal to best_bid -> MUST reject!
        qty_fp: 10_000_000,
        max_slippage_bps: 0,
        ts_ns: 13,
        idempotency_hash: None,
    });
    assert_eq!(po_sell_rej.reject_reason, RejectCode::PostOnlyWouldCross);

    let sell_cross_eq = eng.submit_order_fast(SubmitOrderCmd {
        client_order_id: cid,
        account_id: 1,
        instrument_id: 1,
        symbol: sym,
        side: Side::Sell,
        kind: OrderKind::Limit,
        stp_mode: StpPolicy::None,
        price_fp: 80 * FP_SCALE_U64, // Exact equal to best_bid -> MUST cross & fill!
        qty_fp: 50_000_000,
        max_slippage_bps: 0,
        ts_ns: 14,
        idempotency_hash: None,
    });
    assert_eq!(sell_cross_eq.status, AckStatus::Filled);
}

#[test]
fn unit_merkle_wal_and_ring_primitives() {
    // Merkle left vs right order sensitivity
    let a = hash_account_leaf(
        1,
        &[AssetBalanceLeaf {
            asset: AssetSymbol::from_str_trunc("USDT"),
            available_fp: 100,
            locked_fp: 25,
        }],
    );
    let b = hash_account_leaf(2, &[]);
    assert_ne!(a, b);
    assert_ne!(hash_merkle_branch(&a, &b), hash_merkle_branch(&b, &a));

    let mut ms = IncrementalMerkleState::new();
    let lob = LobStateAccumulator::new();
    ms.upsert_account_leaf(1, a);
    let (leaf_h, proof, root) = ms.generate_account_proof(1, &lob);
    assert_eq!(leaf_h, a);
    assert!(verify_merkle_proof(&leaf_h, &proof, &root));
    let mut flipped = proof.clone();
    flipped[0].is_left_sibling = !flipped[0].is_left_sibling;
    assert!(!verify_merkle_proof(&leaf_h, &flipped, &root));

    // Exhaustive O(1)-amortized Left-to-Right Bagged MMR proof verification for every N in 1..=33!
    for n in 1..=33u64 {
        let leaf = *blake3::hash(&n.to_le_bytes()).as_bytes();
        ms.append_execution_leaf(leaf);
        let cur_root = ms.commit_state_root(&lob);
        for fill_id in 1..=n {
            let p = ms
                .generate_execution_proof(fill_id, n, &lob)
                .expect("execution proof exists");
            assert_eq!(p.state_root_blake3, cur_root);
            assert!(
                verify_merkle_proof(&p.leaf_blake3_hash, &p.proof_path, &cur_root),
                "MMR proof failed for fill_id={fill_id} at total N={n}"
            );
        }
    }

    // WAL CRC32C & Blake3 chain checks
    let tmp = tempfile::tempdir().unwrap();
    let seg = tmp.path().join("seg.wal");
    let (mut w, _, _) = WalWriter::open_segment(&seg, WalSyncMode::SyncEveryFrame).unwrap();
    let h1 = w.append_frame(1, 100, b"frame-1").unwrap();
    assert_eq!(h1, compute_wal_frame_blake3(1, 100, &[0u8; 32], b"frame-1"));
    assert_ne!(
        compute_wal_frame_crc32c(1, 100, &[0u8; 32], b"frame-1"),
        compute_wal_frame_crc32c(1, 101, &[0u8; 32], b"frame-1")
    );
    assert!(matches!(
        w.append_frame(3, 102, b"gap"),
        Err(WalError::SequenceGap {
            expected: 2,
            actual: 3
        })
    ));
    drop(w);
    let rep = WalReader::verify_segment_strict(&seg).unwrap();
    assert_eq!(rep.frames.len(), 1);

    // Snapshot CRC tamper check
    let mut eng = small_engine();
    let snap_path = eng.write_snapshot_to_dir(tmp.path()).unwrap();
    assert!(SnapshotStore::read_snapshot(&snap_path).is_ok());

    // SPSC & MPSC ring full/empty boundaries
    let (mut p, mut c) = spsc_channel::<u64>(4);
    for v in 1..=4 {
        assert!(p.try_push(v).is_ok());
    }
    assert_eq!(p.try_push(5), Err(5));
    assert_eq!(p.dropped_count(), 1);
    for v in 1..=4 {
        assert_eq!(c.try_pop(), Some(v));
    }
    assert_eq!(c.try_pop(), None);

    let mpsc = MpscDisruptorRing::<u64>::new(4);
    for v in 10..14 {
        assert!(mpsc.try_send(v).is_ok());
    }
    assert_eq!(mpsc.try_send(99), Err(99));
    for v in 10..14 {
        assert_eq!(mpsc.try_recv_single_consumer(), Some(v));
    }
    assert_eq!(mpsc.try_recv_single_consumer(), None);
}
