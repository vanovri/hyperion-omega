//! # End-to-End Quantitative Strategy Integration, Live Hot-Swap, & gRPC Telemetry Tests

use ed25519_dalek::{Signer, SigningKey};
use hyperion_quant_abi::*;
use quant_wasm_host::engine::{
    QuantWasmHost, RecordingOrderSink, ValidatedSandboxConfig,
};
use quant_wasm_host::grpc_service::WasmQuantGrpcService;
use quant_wasm_host::proto::omega::wasm_quant_host_service_client::WasmQuantHostServiceClient;
use quant_wasm_host::proto::omega::wasm_quant_host_service_server::WasmQuantHostServiceServer;
use quant_wasm_host::proto::omega::{
    wasm_telemetry_envelope, DeployStrategyModule, PauseOrTerminateStrategyRequest,
    StrategyLifecycleState, StrategySandboxLimits, StreamWasmTelemetryRequest,
};
use quant_wasm_host::risk_gate::ValidatedOrderAction;
use quant_wasm_host::shm_ring::{HostBookL2Tick, HostOrderFill, HostTradeTick};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::StreamExt;
use tonic::transport::Server;

fn load_wasm_artifact(filename: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../artifacts")
        .join(filename);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed reading artifact {}: {e}", path.display()))
}

#[allow(clippy::too_many_arguments)]
fn make_l2(
    seq: u64,
    ts_ns: u64,
    mid_fp: u64,
    spread_fp: u64,
    fair_ref_fp: u64,
    ofi_fp: i64,
    bids_qty: [u64; 5],
    asks_qty: [u64; 5],
) -> HostBookL2Tick {
    let half = spread_fp / 2;
    let bid = mid_fp - half;
    let ask = mid_fp + half;
    HostBookL2Tick {
        engine_seq: seq,
        ts_ns,
        instrument_id: 1,
        bid_levels: 5,
        ask_levels: 5,
        best_bid_fp: bid,
        best_ask_fp: ask,
        mid_price_fp: mid_fp,
        spread_fp,
        ofi_fp,
        book_imbalance_top5_ppm: 0,
        fair_ref_price_fp: fair_ref_fp,
        bids_price_fp: [
            bid,
            bid - 1_000_000,
            bid - 2_000_000,
            bid - 3_000_000,
            bid - 4_000_000,
        ],
        bids_qty_fp: bids_qty,
        asks_price_fp: [
            ask,
            ask + 1_000_000,
            ask + 2_000_000,
            ask + 3_000_000,
            ask + 4_000_000,
        ],
        asks_qty_fp: asks_qty,
    }
}

#[test]
fn test_stat_arb_zscore_mean_reversion_and_unwind() {
    let sink = Arc::new(RecordingOrderSink::new());
    let mut host = QuantWasmHost::new(sink.clone()).unwrap();
    let wasm = load_wasm_artifact("stat_arb_zscore.wasm");

    let mut init = StrategyInitBlock::default_btc();
    init.zscore_window = 16;
    init.zscore_entry_ppm = 1_500_000; // 1.5 sigma
    init.zscore_exit_ppm = 400_000;    // 0.4 sigma
    init.cooldown_ns = 1_000;

    let (_, handed_off) = host
        .deploy_or_hot_swap(
            "stat-arb-1",
            "StatArbZScore",
            10_001,
            1,
            vec!["BTC-USDT".into()],
            &wasm,
            None,
            None,
            ValidatedSandboxConfig::default(),
            init,
            false,
            0,
        )
        .unwrap();
    assert_eq!(handed_off, 0);
    // Verify exact 1 MiB (16 pages) initial linear memory (`01-hyperion-omega.md` §4.2):
    assert_eq!(host.strategy_memory_bytes("stat-arb-1"), Some(1_048_576));

    let fair_fp = 6_850_000_000_000u64; // 68,500.00 USDT
    let balanced_q = [50_000_000u64; 5];

    // Warm up rolling window with mild +/- 2.00 USDT oscillations around fair_fp:
    for i in 1..=16u64 {
        let osc = if i % 2 == 0 { 200_000_000 } else { 0 };
        let mid = fair_fp - 100_000_000 + osc;
        let res = host
            .on_book_l2_tick(
                "stat-arb-1",
                &make_l2(i, i * 10_000, mid, 2_000_000, fair_fp, 0, balanced_q, balanced_q),
            )
            .unwrap();
        assert!(res.fuel_consumed > 0);
    }
    sink.drain();

    // Dislocate downward sharply (-25.00 USDT below fair value -> Z < -3.0 sigma) with positive OFI:
    let dislocated_mid = fair_fp - 2_500_000_000;
    let res = host
        .on_book_l2_tick(
            "stat-arb-1",
            &make_l2(
                17,
                200_000,
                dislocated_mid,
                2_000_000,
                fair_fp,
                80_000_000,
                balanced_q,
                balanced_q,
            ),
        )
        .unwrap();
    assert_eq!(res.orders_emitted, 1);

    let emitted = sink.drain();
    assert_eq!(emitted.len(), 1);
    let entry_oid = match emitted[0].1 {
        ValidatedOrderAction::SubmitLimit {
            order_id,
            side,
            order_type,
            qty_fp,
            ..
        } => {
            assert_eq!(side, SIDE_BUY);
            assert_eq!(order_type, ORDER_TYPE_POST_ONLY);
            assert_eq!(qty_fp, init.order_qty_fp);
            order_id
        }
        other => panic!("expected SubmitLimit BUY, got {other:?}"),
    };

    // Simulate full fill of the entry BUY order (+0.10 BTC inventory):
    host.on_order_fill(
        "stat-arb-1",
        &HostOrderFill {
            engine_seq: 18,
            ts_ns: 210_000,
            order_id: entry_oid,
            client_order_id: 1,
            instrument_id: 1,
            side: SIDE_BUY,
            order_status: ORDER_STATUS_FILLED,
            fill_price_fp: dislocated_mid - 1_000_000,
            fill_qty_fp: init.order_qty_fp,
            remaining_qty_fp: 0,
            net_inventory_fp: init.order_qty_fp as i64,
            external_delta_fp: 0,
        },
    )
    .unwrap();

    // Feed equilibrium reversion back to fair value -> bot must emit SELL unwind order!
    let mut unwind_seen = false;
    for k in 19..=24u64 {
        host.on_book_l2_tick(
            "stat-arb-1",
            &make_l2(k, k * 20_000, fair_fp, 2_000_000, fair_fp, 0, balanced_q, balanced_q),
        )
        .unwrap();
        for (_, act) in sink.drain() {
            if let ValidatedOrderAction::SubmitLimit { side, qty_fp, .. } = act {
                if side == SIDE_SELL && qty_fp == init.order_qty_fp {
                    unwind_seen = true;
                }
            }
        }
    }
    assert!(unwind_seen, "stat_arb_zscore.wasm must unwind long inventory upon mean reversion");
}

#[test]
fn test_obi_sniper_5level_slope_and_cooldown() {
    let sink = Arc::new(RecordingOrderSink::new());
    let mut host = QuantWasmHost::new(sink.clone()).unwrap();
    let wasm = load_wasm_artifact("obi_sniper.wasm");

    let mut init = StrategyInitBlock::default_btc();
    init.obi_threshold_ppm = 350_000;
    init.cooldown_ns = 5_000_000; // 5 ms cooldown

    host.deploy_or_hot_swap(
        "obi-1",
        "ObiSniper",
        10_002,
        1,
        vec!["BTC-USDT".into()],
        &wasm,
        None,
        None,
        ValidatedSandboxConfig::default(),
        init,
        false,
        0,
    )
    .unwrap();

    let mid = 6_850_000_000_000u64;
    let balanced = [50_000_000u64; 5];

    // 1. Balanced book -> 0 snipes
    let r0 = host
        .on_book_l2_tick(
            "obi-1",
            &make_l2(1, 1_000_000, mid, 2_000_000, 0, 0, balanced, balanced),
        )
        .unwrap();
    assert_eq!(r0.orders_emitted, 0);

    // 2. Bullish trade flow + massive 5-level bid wall vs thin asks -> IOC BUY snipe at best_ask!
    host.on_trade_tick(
        "obi-1",
        &HostTradeTick {
            engine_seq: 2,
            ts_ns: 2_000_000,
            fill_id: 501,
            instrument_id: 1,
            aggressor_side: SIDE_BUY,
            price_fp: mid + 1_000_000,
            qty_fp: 25_000_000,
            vwap_fp: mid,
            ofi_delta_fp: 25_000_000,
        },
    )
    .unwrap();

    let heavy_bids = [500_000_000, 400_000_000, 300_000_000, 250_000_000, 200_000_000];
    let thin_asks = [15_000_000, 15_000_000, 20_000_000, 20_000_000, 25_000_000];
    let r1 = host
        .on_book_l2_tick(
            "obi-1",
            &make_l2(3, 10_000_000, mid, 2_000_000, 0, 100_000_000, heavy_bids, thin_asks),
        )
        .unwrap();
    assert_eq!(r1.orders_emitted, 1);
    let actions = sink.drain();
    match actions[0].1 {
        ValidatedOrderAction::SubmitLimit {
            side,
            order_type,
            price_fp,
            ..
        } => {
            assert_eq!(side, SIDE_BUY);
            assert_eq!(order_type, ORDER_TYPE_IOC);
            assert_eq!(price_fp, mid + 1_000_000);
        }
        other => panic!("expected IOC BUY snipe, got {other:?}"),
    }

    // 3. Immediate tick 1 ms later (< 5 ms cooldown) -> suppressed (0 orders)!
    let r_cool = host
        .on_book_l2_tick(
            "obi-1",
            &make_l2(4, 11_000_000, mid, 2_000_000, 0, 100_000_000, heavy_bids, thin_asks),
        )
        .unwrap();
    assert_eq!(r_cool.orders_emitted, 0);

    // 4. Bearish ask wall after cooldown -> IOC SELL snipe at best_bid!
    host.on_trade_tick(
        "obi-1",
        &HostTradeTick {
            engine_seq: 5,
            ts_ns: 18_000_000,
            fill_id: 502,
            instrument_id: 1,
            aggressor_side: SIDE_SELL,
            price_fp: mid - 1_000_000,
            qty_fp: 30_000_000,
            vwap_fp: mid,
            ofi_delta_fp: -30_000_000,
        },
    )
    .unwrap();
    let r_sell = host
        .on_book_l2_tick(
            "obi-1",
            &make_l2(6, 20_000_000, mid, 2_000_000, 0, -100_000_000, thin_asks, heavy_bids),
        )
        .unwrap();
    assert_eq!(r_sell.orders_emitted, 1);
    let actions_sell = sink.drain();
    match actions_sell[0].1 {
        ValidatedOrderAction::SubmitLimit {
            side,
            order_type,
            price_fp,
            ..
        } => {
            assert_eq!(side, SIDE_SELL);
            assert_eq!(order_type, ORDER_TYPE_IOC);
            assert_eq!(price_fp, mid - 1_000_000);
        }
        other => panic!("expected IOC SELL snipe, got {other:?}"),
    }
}

#[test]
fn test_delta_hedger_deadband_passive_and_urgent_regimes() {
    let sink = Arc::new(RecordingOrderSink::new());
    let mut host = QuantWasmHost::new(sink.clone()).unwrap();
    let wasm = load_wasm_artifact("delta_hedger.wasm");

    let mut init = StrategyInitBlock::default_btc();
    init.hedge_band_fp = 5_000_000; // 0.05 BTC deadband
    init.cooldown_ns = 1_000;

    host.deploy_or_hot_swap(
        "hedger-1",
        "DeltaHedger",
        10_003,
        1,
        vec!["BTC-USDT".into()],
        &wasm,
        None,
        None,
        ValidatedSandboxConfig::default(),
        init,
        false,
        0,
    )
    .unwrap();

    let mid = 6_850_000_000_000u64;
    let q = [50_000_000u64; 5];

    // Cache top-of-book:
    host.on_book_l2_tick("hedger-1", &make_l2(1, 10_000, mid, 2_000_000, 0, 0, q, q))
        .unwrap();
    assert!(sink.is_empty());

    // 1. Sub-deadband exposure (+0.03 BTC <= 0.05 BTC) -> 0 hedge orders:
    host.on_order_fill(
        "hedger-1",
        &HostOrderFill {
            engine_seq: 2,
            ts_ns: 20_000,
            order_id: 999,
            client_order_id: 999,
            instrument_id: 1,
            side: SIDE_BUY,
            order_status: ORDER_STATUS_FILLED,
            fill_price_fp: mid,
            fill_qty_fp: 3_000_000,
            remaining_qty_fp: 0,
            net_inventory_fp: 3_000_000,
            external_delta_fp: 0,
        },
    )
    .unwrap();
    assert!(sink.is_empty());

    // 2. Moderate exposure (+0.12 BTC in (0.05, 0.15]) -> passive POST_ONLY SELL at best_ask:
    host.on_order_fill(
        "hedger-1",
        &HostOrderFill {
            engine_seq: 3,
            ts_ns: 30_000,
            order_id: 1000,
            client_order_id: 1000,
            instrument_id: 1,
            side: SIDE_BUY,
            order_status: ORDER_STATUS_FILLED,
            fill_price_fp: mid,
            fill_qty_fp: 9_000_000,
            remaining_qty_fp: 0,
            net_inventory_fp: 12_000_000,
            external_delta_fp: 0,
        },
    )
    .unwrap();
    let acts = sink.drain();
    assert_eq!(acts.len(), 1);
    match acts[0].1 {
        ValidatedOrderAction::SubmitLimit {
            side,
            order_type,
            qty_fp,
            ..
        } => {
            assert_eq!(side, SIDE_SELL);
            assert_eq!(order_type, ORDER_TYPE_POST_ONLY);
            assert_eq!(qty_fp, 12_000_000);
        }
        other => panic!("expected passive POST_ONLY SELL hedge, got {other:?}"),
    }
    assert_eq!(host.strategy_open_orders("hedger-1").unwrap().len(), 1);

    // 3. Urgent shock (-0.30 BTC external options delta -> net -0.18 BTC, |excess| > 3 * 0.05 BTC)
    //    -> must cancel resting passive SELL order AND emit urgent IOC BUY order!
    host.on_order_fill(
        "hedger-1",
        &HostOrderFill {
            engine_seq: 4,
            ts_ns: 50_000,
            order_id: 1001,
            client_order_id: 1001,
            instrument_id: 1,
            side: SIDE_SELL,
            order_status: ORDER_STATUS_PARTIALLY_FILLED,
            fill_price_fp: mid,
            fill_qty_fp: 0,
            remaining_qty_fp: 12_000_000,
            net_inventory_fp: 12_000_000,
            external_delta_fp: -30_000_000,
        },
    )
    .unwrap();
    let urgent_acts = sink.drain();
    assert_eq!(urgent_acts.len(), 2, "must cancel stale passive quote and submit IOC hedge");
    assert!(matches!(urgent_acts[0].1, ValidatedOrderAction::Cancel { .. }));
    match urgent_acts[1].1 {
        ValidatedOrderAction::SubmitLimit {
            side,
            order_type,
            qty_fp,
            ..
        } => {
            assert_eq!(side, SIDE_BUY);
            assert_eq!(order_type, ORDER_TYPE_IOC);
            assert_eq!(qty_fp, 18_000_000);
        }
        other => panic!("expected urgent IOC BUY hedge, got {other:?}"),
    }
}

#[test]
fn test_live_atomic_hot_swap_preserves_open_orders_and_rollback_on_failure() {
    let sink = Arc::new(RecordingOrderSink::new());
    let mut host = QuantWasmHost::new(sink.clone()).unwrap();
    let wasm_hedger = load_wasm_artifact("delta_hedger.wasm");
    let wasm_obi = load_wasm_artifact("obi_sniper.wasm");

    let signing_key = SigningKey::from_bytes(&[7u8; 32]);
    host.set_trusted_author_key(signing_key.verifying_key());

    let sign_deploy = |sid: &str, ver: u32, code: &[u8]| -> ([u8; 32], Vec<u8>) {
        let h = *blake3::hash(code).as_bytes();
        let mut msg = Vec::new();
        msg.extend_from_slice(sid.as_bytes());
        msg.extend_from_slice(&ver.to_be_bytes());
        msg.extend_from_slice(&h);
        let sig = signing_key.sign(&msg).to_bytes().to_vec();
        (h, sig)
    };

    let mut init = StrategyInitBlock::default_btc();
    init.hedge_band_fp = 5_000_000;
    init.cooldown_ns = 1_000;

    let (b3_v1, sig_v1) = sign_deploy("live-bot", 1, &wasm_hedger);
    host.deploy_or_hot_swap(
        "live-bot",
        "DeltaHedger",
        20_001,
        1,
        vec!["BTC-USDT".into()],
        &wasm_hedger,
        Some(&b3_v1),
        Some(&sig_v1),
        ValidatedSandboxConfig::default(),
        init,
        false,
        0,
    )
    .unwrap();

    let mid = 6_850_000_000_000u64;
    let q = [50_000_000u64; 5];
    host.on_book_l2_tick("live-bot", &make_l2(1, 10_000, mid, 2_000_000, 0, 0, q, q))
        .unwrap();

    // Create a resting passive order (+0.10 BTC -> passive SELL order):
    host.on_order_fill(
        "live-bot",
        &HostOrderFill {
            engine_seq: 2,
            ts_ns: 20_000,
            order_id: 555,
            client_order_id: 555,
            instrument_id: 1,
            side: SIDE_BUY,
            order_status: ORDER_STATUS_FILLED,
            fill_price_fp: mid,
            fill_qty_fp: 10_000_000,
            remaining_qty_fp: 0,
            net_inventory_fp: 10_000_000,
            external_delta_fp: 0,
        },
    )
    .unwrap();
    sink.drain();

    let open_before = host.strategy_open_orders("live-bot").unwrap();
    assert_eq!(open_before.len(), 1);
    let resting_oid = open_before[0].order_id;

    // 1. Live Atomic Hot-Swap v1 -> v2 (updated parameters, preserving open order & state!):
    init.hedge_band_fp = 4_000_000;
    let (b3_v2, sig_v2) = sign_deploy("live-bot", 2, &wasm_hedger);
    let (_, handed_off) = host
        .deploy_or_hot_swap(
            "live-bot",
            "DeltaHedgerV2",
            20_001,
            2,
            vec!["BTC-USDT".into()],
            &wasm_hedger,
            Some(&b3_v2),
            Some(&sig_v2),
            ValidatedSandboxConfig::default(),
            init,
            true,
            2,
        )
        .unwrap();
    assert_eq!(handed_off, 1, "must hand off exact open order count across v1->v2 swap");
    assert_eq!(host.strategy_version("live-bot"), Some(2));
    assert_eq!(host.strategy_open_orders("live-bot").unwrap()[0].order_id, resting_oid);
    assert_eq!(host.strategy_summary("live-bot").unwrap().hot_swap_count, 1);

    // 2. Attempt incompatible hot-swap v2 -> v3 with wrong strategy kind (`obi_sniper.wasm`) -> must fail & preserve v2!
    let (b3_v3, sig_v3) = sign_deploy("live-bot", 3, &wasm_obi);
    let bad_swap = host.deploy_or_hot_swap(
        "live-bot",
        "WrongKindSwap",
        20_001,
        3,
        vec!["BTC-USDT".into()],
        &wasm_obi,
        Some(&b3_v3),
        Some(&sig_v3),
        ValidatedSandboxConfig::default(),
        init,
        true,
        3,
    );
    assert!(bad_swap.is_err(), "incompatible strategy_kind snapshot import must fail");
    assert_eq!(host.strategy_version("live-bot"), Some(2), "v2 must stay active on rollback");
    assert_eq!(host.strategy_open_orders("live-bot").unwrap()[0].order_id, resting_oid);

    // 3. Verify v2 still manages and cancels `resting_oid` seamlessly when inventory returns to 0!
    host.on_order_fill(
        "live-bot",
        &HostOrderFill {
            engine_seq: 4,
            ts_ns: 60_000,
            order_id: 9999,
            client_order_id: 9999,
            instrument_id: 1,
            side: SIDE_SELL,
            order_status: ORDER_STATUS_FILLED,
            fill_price_fp: mid,
            fill_qty_fp: 10_000_000,
            remaining_qty_fp: 0,
            net_inventory_fp: 0,
            external_delta_fp: 0,
        },
    )
    .unwrap();

    let post_swap_acts = sink.drain();
    assert_eq!(post_swap_acts.len(), 1);
    match post_swap_acts[0].1 {
        ValidatedOrderAction::Cancel { order_id, .. } => {
            assert_eq!(order_id, resting_oid, "v2 canceled the exact resting order opened by v1!");
        }
        other => panic!("expected Cancel of inherited order_id, got {other:?}"),
    }
}

#[test]
fn test_nan_canonicalization_and_cross_host_determinism() {
    let sink_a = Arc::new(RecordingOrderSink::new());
    let sink_b = Arc::new(RecordingOrderSink::new());
    let mut host_a = QuantWasmHost::new(sink_a.clone()).unwrap();
    let mut host_b = QuantWasmHost::new(sink_b.clone()).unwrap();

    // Verify Cranelift NaN canonicalization directly via Engine:
    let nan_mod = wasmtime::Module::new(
        host_a.engine(),
        r#"(module
            (func (export "canon_nan64") (result i64)
                f64.const 0.0
                f64.const 0.0
                f64.div
                i64.reinterpret_f64)
            (func (export "canon_payload_nan") (result i64)
                i64.const 0x7ff8123456789abc
                f64.reinterpret_i64
                f64.const 1.0
                f64.mul
                i64.reinterpret_f64)
        )"#,
    )
    .unwrap();
    let mut st = wasmtime::Store::new(host_a.engine(), ());
    st.set_fuel(10_000).unwrap();
    st.set_epoch_deadline(10);
    let inst = wasmtime::Instance::new(&mut st, &nan_mod, &[]).unwrap();
    let f1 = inst.get_typed_func::<(), i64>(&mut st, "canon_nan64").unwrap();
    let f2 = inst.get_typed_func::<(), i64>(&mut st, "canon_payload_nan").unwrap();
    assert_eq!(f1.call(&mut st, ()).unwrap() as u64, 0x7FF8_0000_0000_0000);
    assert_eq!(f2.call(&mut st, ()).unwrap() as u64, 0x7FF8_0000_0000_0000);

    // Cross-host bit-exact determinism over 500 market ticks:
    let wasm = load_wasm_artifact("stat_arb_zscore.wasm");
    let init = StrategyInitBlock::default_btc();
    host_a
        .deploy_or_hot_swap(
            "det",
            "DetA",
            30_001,
            1,
            vec!["BTC-USDT".into()],
            &wasm,
            None,
            None,
            ValidatedSandboxConfig::default(),
            init,
            false,
            0,
        )
        .unwrap();
    host_b
        .deploy_or_hot_swap(
            "det",
            "DetB",
            30_001,
            1,
            vec!["BTC-USDT".into()],
            &wasm,
            None,
            None,
            ValidatedSandboxConfig::default(),
            init,
            false,
            0,
        )
        .unwrap();

    let q = [50_000_000u64; 5];
    for seq in 1..=500u64 {
        let wave = ((seq * 73) % 41) as i64 - 20;
        let mid = (6_850_000_000_000i64 + wave * 150_000_000) as u64;
        let tick = make_l2(
            seq,
            seq * 2_000_000,
            mid,
            2_000_000,
            6_850_000_000_000,
            wave * 5_000_000,
            q,
            q,
        );
        let ra = host_a.on_book_l2_tick("det", &tick).unwrap();
        let rb = host_b.on_book_l2_tick("det", &tick).unwrap();
        assert_eq!(ra.fuel_consumed, rb.fuel_consumed, "deterministic fuel mismatch at seq {seq}");
        assert_eq!(ra.orders_emitted, rb.orders_emitted);
    }
    assert_eq!(sink_a.drain(), sink_b.drain(), "order stream must be bit-identical across hosts");
}

#[tokio::test]
async fn test_grpc_wasm_quant_service_deploy_telemetry_and_terminate() {
    let sink = Arc::new(RecordingOrderSink::new());
    let host = Arc::new(Mutex::new(QuantWasmHost::new(sink).unwrap()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let svc = WasmQuantGrpcService::new(host.clone());

    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(WasmQuantHostServiceServer::new(svc))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let mut client = WasmQuantHostServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap();

    let wasm = load_wasm_artifact("obi_sniper.wasm");
    let b3 = blake3::hash(&wasm).as_bytes().to_vec();

    let deploy_resp = client
        .deploy_strategy(DeployStrategyModule {
            strategy_id: "grpc-obi-1".into(),
            account_id: 77_001,
            version: 1,
            strategy_name: "GrpcObiSniper".into(),
            subscribed_symbols: vec!["BTC-USDT".into()],
            wasm_bytecode: wasm,
            bytecode_blake3: b3,
            author_ed25519_signature: Vec::new(),
            limits: Some(StrategySandboxLimits {
                max_linear_memory_bytes: 16 * 1024 * 1024,
                max_stack_bytes: 512 * 1024,
                max_fuel_per_tick: 50_000,
                max_epoch_walltime_ns: 80_000,
                max_orders_per_tick: 16,
                max_order_notional_fp: None,
                max_abs_position_qty_fp: 500_000_000,
            }),
            atomic_hot_swap: false,
            initial_config_payload: Vec::new(),
            submitted_ts_ns: 1_000,
            trace: None,
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(deploy_resp.state, StrategyLifecycleState::Active as i32);
    assert!(deploy_resp.jit_compile_latency_us > 0);

    // Subscribe to live telemetry stream BEFORE firing ticks:
    let mut stream = client
        .stream_wasm_telemetry(StreamWasmTelemetryRequest {
            strategy_id: "grpc-obi-1".into(),
            include_fuel_reports: true,
            include_traps: true,
        })
        .await
        .unwrap()
        .into_inner();

    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    {
        let mut g = host.lock().unwrap();
        let q = [50_000_000u64; 5];
        g.on_book_l2_tick(
            "grpc-obi-1",
            &make_l2(1, 50_000, 6_850_000_000_000, 2_000_000, 0, 0, q, q),
        )
        .unwrap();
    }

    let env = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match env.payload.unwrap() {
        wasm_telemetry_envelope::Payload::FuelReport(rep) => {
            assert_eq!(rep.strategy_id, "grpc-obi-1");
            assert!(rep.fuel_consumed > 0);
            assert!(rep.execution_latency_ns < 5_000_000);
        }
        other => panic!("expected FuelReport, got {other:?}"),
    }

    let term_resp = client
        .pause_or_terminate_strategy(PauseOrTerminateStrategyRequest {
            strategy_id: "grpc-obi-1".into(),
            account_id: 77_001,
            terminate_permanently: true,
            cancel_open_orders: true,
            trace: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(term_resp.state, StrategyLifecycleState::Terminated as i32);

    server_task.abort();
}
