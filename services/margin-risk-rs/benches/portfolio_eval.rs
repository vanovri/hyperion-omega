use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use margin_risk_rs::fixed_point::FP_SCALE;
use margin_risk_rs::greeks_soa::{ExerciseStyle, OptionPositionsSoA, OptionRight};
use margin_risk_rs::span_margin::{
    CrossMarginAccountInput, OptionLegInput, SoACrossMarginEngineBatch, SpanRiskConfig,
};

fn build_synthetic_accounts(
    num_accounts: usize,
    options_per_account: usize,
) -> Vec<CrossMarginAccountInput> {
    let spot_fp = 65_000 * FP_SCALE;
    let mut accounts = Vec::with_capacity(num_accounts);
    let strikes = [52_000, 58_000, 62_000, 65_000, 68_000, 72_000, 78_000, 85_000];

    for a in 0..num_accounts {
        let mut opts = Vec::with_capacity(options_per_account);
        for j in 0..options_per_account {
            let k_usd = strikes[(a + j) % strikes.len()];
            let right = if (a + j).is_multiple_of(2) {
                OptionRight::Call
            } else {
                OptionRight::Put
            };
            let signed_qty = if j.is_multiple_of(3) { -2 * FP_SCALE } else { 2 * FP_SCALE };
            opts.push(OptionLegInput {
                strike_fp: k_usd * FP_SCALE,
                time_to_expiry_yrs: (7.0 + ((j % 6) as f64) * 14.0) / 365.0,
                implied_vol: 0.52 + ((j % 5) as f64) * 0.04,
                rate_r: 0.04,
                carry_q: 0.01,
                right,
                style: ExerciseStyle::European,
                qty_fp: signed_qty,
            });
        }
        accounts.push(CrossMarginAccountInput {
            account_id: (a + 1) as u64,
            quote_cash_fp: 150_000 * FP_SCALE,
            spot_qty_fp: FP_SCALE,
            perp_qty_fp: -2 * FP_SCALE,
            perp_entry_price_fp: spot_fp,
            open_order_margin_fp: 5_000 * FP_SCALE,
            options: opts,
        });
    }
    accounts
}

fn bench_single_core_portfolio_reevaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_core_soa_portfolio_reeval");
    let spot_fp = 65_000 * FP_SCALE;
    let cfg = SpanRiskConfig::default();

    // 1,000 accounts x (1 spot + 1 perp + 8 options = 10 positions/acct) = 10,000 positions!
    let accounts_10k = build_synthetic_accounts(1_000, 8);
    let mut batch_10k = SoACrossMarginEngineBatch::from_accounts(&accounts_10k, spot_fp);
    let total_pos = batch_10k.total_positions_count() as u64;

    group.throughput(Throughput::Elements(total_pos));

    group.bench_with_input(
        BenchmarkId::new("pass1_baseline_aos", total_pos),
        &total_pos,
        |b, _| {
            let mut tick_offset = 0_i128;
            b.iter(|| {
                tick_offset = (tick_offset + 10 * FP_SCALE) % (500 * FP_SCALE);
                let s = spot_fp + tick_offset;
                let res = SoACrossMarginEngineBatch::reevaluate_all_pass1_baseline_aos(
                    &accounts_10k,
                    black_box(s),
                    black_box(s + 25 * FP_SCALE),
                    &cfg,
                );
                black_box(res[0].maintenance_margin_fp)
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("pass2_vectorized_zerocopy_soa", total_pos),
        &total_pos,
        |b, _| {
            let mut tick_offset = 0_i128;
            b.iter(|| {
                tick_offset = (tick_offset + 10 * FP_SCALE) % (500 * FP_SCALE);
                let s = spot_fp + tick_offset;
                batch_10k.reevaluate_all_single_core(
                    black_box(s),
                    black_box(s + 25 * FP_SCALE),
                    black_box(1.0),
                    &cfg,
                );
                black_box(batch_10k.out_mm_fp[0])
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("pass3_cacheline_aligned_simd", total_pos),
        &total_pos,
        |b, _| {
            let mut tick_offset = 0_i128;
            b.iter(|| {
                tick_offset = (tick_offset + 10 * FP_SCALE) % (500 * FP_SCALE);
                let s = spot_fp + tick_offset;
                batch_10k.reevaluate_all_pass3_aligned_simd(
                    black_box(s),
                    black_box(s + 25 * FP_SCALE),
                    black_box(1.0),
                    &cfg,
                );
                black_box(batch_10k.out_mm_fp[0])
            });
        },
    );

    // Pure vectorized 7-Greeks SoA throughput on 50,000 option positions
    let mut opt_soa = OptionPositionsSoA::with_capacity(50_000);
    for i in 0_usize..50_000 {
        let right = if i.is_multiple_of(2) {
            OptionRight::Call
        } else {
            OptionRight::Put
        };
        opt_soa.push(
            (i as u64) + 1,
            1,
            65_000.0,
            55_000.0 + ((i % 20) as f64) * 1_000.0,
            30.0 / 365.0,
            0.04,
            0.01,
            0.60,
            right,
            ExerciseStyle::European,
            1.5,
        );
    }
    group.throughput(Throughput::Elements(50_000));
    group.bench_function("pure_7greeks_soa_50k_options", |b| {
        b.iter(|| {
            opt_soa.evaluate_all_greeks_soa();
            black_box(opt_soa.out_pos_gammas[0])
        });
    });

    group.finish();
}

criterion_group!(benches, bench_single_core_portfolio_reevaluation);
criterion_main!(benches);
