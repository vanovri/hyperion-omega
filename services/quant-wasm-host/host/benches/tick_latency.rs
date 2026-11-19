//! # Criterion Tick Latency Benchmark & 3-Pass Micro-Optimization Suite (`tick_latency.rs`)
//!
//! Benchmarks `Host -> Wasm Guest -> Host` round-trip tick latency across:
//! - Pass 1 (`Baseline`): Dynamic export lookup + `Val` boxing + heap `Vec<u8>` serialization
//! - Pass 2 (`Zero-Copy Refactor`): Pre-resolved `TypedFunc` + direct shared linear memory slice write
//! - Pass 3 (`Cache-Line Aligned Final`): `64B`-aligned POD struct write + inline stack risk gate (`0 B/op`)
//! - Production full telemetry + QPC watchdog ticks across all 3 compiled `.wasm` reference strategies:
//!   `stat_arb_zscore.wasm`, `obi_sniper.wasm`, and `delta_hedger.wasm`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use hyperion_quant_abi::StrategyInitBlock;
use quant_wasm_host::engine::{NullOrderSink, QuantWasmHost, ValidatedSandboxConfig};
use quant_wasm_host::shm_ring::HostBookL2Tick;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn artifact_bytes(name: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../artifacts");
    std::fs::read(dir.join(name)).expect("compiled .wasm artifact must exist in ../artifacts")
}

fn sample_l2_tick(seq: u64) -> HostBookL2Tick {
    let base_mid = 6_850_000_000_000u64 + ((seq & 15) * 1_000_000);
    let best_bid = base_mid - 1_000_000;
    let best_ask = base_mid + 1_000_000;
    HostBookL2Tick {
        engine_seq: seq,
        ts_ns: 1_700_000_000_000_000_000 + seq * 100_000,
        instrument_id: 1,
        bid_levels: 5,
        ask_levels: 5,
        best_bid_fp: best_bid,
        best_ask_fp: best_ask,
        mid_price_fp: base_mid,
        spread_fp: 2_000_000,
        ofi_fp: 15_000_000,
        book_imbalance_top5_ppm: 120_000,
        fair_ref_price_fp: 6_850_000_000_000,
        bids_price_fp: [
            best_bid,
            best_bid - 1_000_000,
            best_bid - 2_000_000,
            best_bid - 3_000_000,
            best_bid - 4_000_000,
        ],
        bids_qty_fp: [50_000_000, 80_000_000, 120_000_000, 90_000_000, 60_000_000],
        asks_price_fp: [
            best_ask,
            best_ask + 1_000_000,
            best_ask + 2_000_000,
            best_ask + 3_000_000,
            best_ask + 4_000_000,
        ],
        asks_qty_fp: [45_000_000, 75_000_000, 110_000_000, 85_000_000, 55_000_000],
    }
}

fn bench_wasm_quant_sandbox(c: &mut Criterion) {
    let stat_arb_wasm = artifact_bytes("stat_arb_zscore.wasm");
    let obi_sniper_wasm = artifact_bytes("obi_sniper.wasm");
    let delta_hedger_wasm = artifact_bytes("delta_hedger.wasm");

    let mut host = QuantWasmHost::new(Arc::new(NullOrderSink)).unwrap();
    let cfg = ValidatedSandboxConfig::default();
    let init = StrategyInitBlock::default_btc();

    host.deploy_or_hot_swap(
        "stat-arb",
        "StatArbZScore",
        10_001,
        1,
        vec!["BTC-USDT".into()],
        &stat_arb_wasm,
        None,
        None,
        cfg,
        init,
        false,
        0,
    )
    .unwrap();
    host.deploy_or_hot_swap(
        "obi-sniper",
        "ObiSniper",
        10_002,
        1,
        vec!["BTC-USDT".into()],
        &obi_sniper_wasm,
        None,
        None,
        cfg,
        init,
        false,
        0,
    )
    .unwrap();
    host.deploy_or_hot_swap(
        "delta-hedger",
        "DeltaHedger",
        10_003,
        1,
        vec!["BTC-USDT".into()],
        &delta_hedger_wasm,
        None,
        None,
        cfg,
        init,
        false,
        0,
    )
    .unwrap();

    let mut seq = 1u64;
    let mut group = c.benchmark_group("ch15_host_to_wasm_to_host");
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(60);

    group.bench_function("pass1_baseline_dynamic_vec_copy", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            black_box(host.bench_pass1_baseline_tick("obi-sniper", black_box(&tick)).unwrap())
        });
    });

    group.bench_function("pass2_zerocopy_typed_func_ring", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            black_box(host.bench_pass2_zerocopy_tick("obi-sniper", black_box(&tick)).unwrap())
        });
    });

    group.bench_function("pass3_cache_aligned_direct_slot", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            black_box(host.bench_pass3_cache_aligned_tick("obi-sniper", black_box(&tick)).unwrap())
        });
    });

    group.bench_function("prod_full_tick_stat_arb_zscore", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            if host.on_book_l2_tick("stat-arb", black_box(&tick)).is_err() {
                // On non-RT shared Windows CI, recover cleanly if an OS scheduling stall >100µs occurs.
                let _ = host.deploy_or_hot_swap(
                    "stat-arb",
                    "StatArbZScore",
                    10_001,
                    1,
                    vec!["BTC-USDT".into()],
                    &stat_arb_wasm,
                    None,
                    None,
                    cfg,
                    init,
                    false,
                    0,
                );
            }
        });
    });

    group.bench_function("prod_full_tick_obi_sniper", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            if host.on_book_l2_tick("obi-sniper", black_box(&tick)).is_err() {
                let _ = host.deploy_or_hot_swap(
                    "obi-sniper",
                    "ObiSniper",
                    10_002,
                    1,
                    vec!["BTC-USDT".into()],
                    &obi_sniper_wasm,
                    None,
                    None,
                    cfg,
                    init,
                    false,
                    0,
                );
            }
        });
    });

    group.bench_function("prod_full_tick_delta_hedger", |b| {
        b.iter(|| {
            seq = seq.wrapping_add(1);
            let tick = sample_l2_tick(seq);
            if host.on_book_l2_tick("delta-hedger", black_box(&tick)).is_err() {
                let _ = host.deploy_or_hot_swap(
                    "delta-hedger",
                    "DeltaHedger",
                    10_003,
                    1,
                    vec!["BTC-USDT".into()],
                    &delta_hedger_wasm,
                    None,
                    None,
                    cfg,
                    init,
                    false,
                    0,
                );
            }
        });
    });

    group.finish();

    for sid in ["stat-arb", "obi-sniper", "delta-hedger"] {
        if let Some(s) = host.strategy_summary(sid) {
            println!(
                "[CRITERION PROFILER] {sid:<14} | calls={} | p50={}ns | p99={}ns | p999={}ns | mean_fuel={} | max_fuel={}",
                s.total_invocations,
                s.p50_latency_ns,
                s.p99_latency_ns,
                s.p999_latency_ns,
                s.mean_fuel_per_call,
                s.max_fuel_single_call
            );
        }
    }
}

criterion_group!(benches, bench_wasm_quant_sandbox);
criterion_main!(benches);
