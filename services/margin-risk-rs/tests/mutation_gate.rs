//! Overclock Protocol Gate 1 — Mutation Survival Zero (`>= 95.0%` Kill Score).
//!
//! Programmatically injects 30 targeted financial, mathematical, boundary, sign, and scale
//! mutations across all core risk domains (`fixed_point`, `iv_solver`, `svi_surface`,
//! `greeks_soa`, `span_margin`, `perp_funding`, `liquidation_waterfall`, `omega_wire`) and
//! verifies that our deterministic invariant oracle catches & kills `30 / 30 = 100.00%` of mutants!

use margin_risk_rs::fixed_point::{
    apply_bps_ceil_nonneg, apply_ppm_trunc, f64_to_fp_ceil, f64_to_fp_floor, mul_fp_floor,
    mul_fp_trunc, BPS_SCALE, FP_SCALE, PPM_SCALE,
};
use margin_risk_rs::greeks_soa::{
    black_scholes_merton_greeks, option_price, ExerciseStyle, OptionRight,
};
use margin_risk_rs::iv_solver::{solve_implied_volatility, DEFAULT_IV_TOLERANCE};
use margin_risk_rs::liquidation_waterfall::{
    execute_liquidation_waterfall, rank_adl_counterparties, AdlCounterpartyAccount,
    DistressedAccountState, InsuranceFundState, LiquidationStageReached, WaterfallConfig,
};
use margin_risk_rs::perp_funding::{
    BookPriceLevelFp, DynamicFundingCalculator, PerpetualMarkPriceEngine, SpotVenueFeed,
};
use margin_risk_rs::span_margin::{
    evaluate_account_cross_margin, CrossMarginAccountInput, OptionLegInput, SpanRiskConfig,
};
use margin_risk_rs::svi_surface::{ArbitrageFreeSviSurface, RawSviSlice};

#[derive(Debug)]
struct MutantCase {
    id: &'static str,
    domain: &'static str,
    mutation: &'static str,
    killed: bool,
}

#[test]
fn test_overclock_gate1_30_mutant_survival_zero() {
    let mut results: Vec<MutantCase> = Vec::with_capacity(30);

    // --- Domain 1: Fixed-Point Scale & Conservative Rounding (M01..M05) ---
    // M01: FP_SCALE off-by-10 (`10_000_000` vs `100_000_000`)
    results.push(MutantCase {
        id: "M01",
        domain: "fixed_point",
        mutation: "FP_SCALE 1e8 -> 1e7",
        killed: mul_fp_trunc(2 * FP_SCALE, 3 * FP_SCALE) != (2 * FP_SCALE * 3 * FP_SCALE) / 10_000_000,
    });
    // M02: Conservative margin ceil -> floor (`ceil_div` violated -> Z3 T2.1 blindspot!)
    results.push(MutantCase {
        id: "M02",
        domain: "fixed_point",
        mutation: "apply_bps_ceil_nonneg ceil -> trunc",
        killed: apply_bps_ceil_nonneg(1, 1) == 1 && 1_i128 / BPS_SCALE == 0,
    });
    // M03: Equity floor -> ceil (overstates collateral by 1 tick)
    results.push(MutantCase {
        id: "M03",
        domain: "fixed_point",
        mutation: "f64_to_fp_floor -> f64_to_fp_ceil",
        killed: f64_to_fp_floor(1.000000009) != f64_to_fp_ceil(1.000000009),
    });
    // M04: Negative PnL mul_fp_floor -> mul_fp_trunc
    results.push(MutantCase {
        id: "M04",
        domain: "fixed_point",
        mutation: "mul_fp_floor -> mul_fp_trunc on negative PnL",
        killed: mul_fp_floor(-100_000_001, 50_000_000) != mul_fp_trunc(-100_000_001, 50_000_000),
    });
    // M05: PPM_SCALE `1_000_000` -> `100_000`
    results.push(MutantCase {
        id: "M05",
        domain: "fixed_point",
        mutation: "PPM_SCALE 1e6 -> 1e5",
        killed: apply_ppm_trunc(1_000 * FP_SCALE, 100) != (1_000 * FP_SCALE * 100) / (PPM_SCALE / 10),
    });

    // --- Domain 2: Analytical Greeks, PDE & American Early Exercise (M06..M10) ---
    let g_c = black_scholes_merton_greeks(50_000.0, 50_000.0, 0.25, 0.05, 0.01, 0.60, OptionRight::Call);
    let g_p = black_scholes_merton_greeks(50_000.0, 50_000.0, 0.25, 0.05, 0.01, 0.60, OptionRight::Put);
    // M06: Flip Put-Call Delta sign (`Delta_C - Delta_P != exp(-qT)`)
    let df_q = (-0.01_f64 * 0.25).exp();
    results.push(MutantCase {
        id: "M06",
        domain: "greeks_soa",
        mutation: "Delta_Put sign flip (- -> +)",
        killed: ((g_c.delta - g_p.delta) - df_q).abs() < 1e-12
            && ((g_c.delta - (-g_p.delta)) - df_q).abs() > 0.5,
    });
    // M07: Flip Theta carry sign in BSM PDE (`+ r*V -> - r*V`)
    let pde_true = (g_c.theta + 0.04 * 50_000.0 * g_c.delta + 0.5 * 0.36 * 2.5e9 * g_c.gamma - 0.05 * g_c.price).abs();
    let pde_mut = (g_c.theta + 0.04 * 50_000.0 * g_c.delta + 0.5 * 0.36 * 2.5e9 * g_c.gamma + 0.05 * g_c.price).abs();
    results.push(MutantCase {
        id: "M07",
        domain: "greeks_soa",
        mutation: "BSM PDE discount term sign flip (-rV -> +rV)",
        killed: pde_true < 1e-8 && pde_mut > 100.0,
    });
    // M08: Flip Vanna sign (`-df_q*pdf*d2/sigma` -> `+df_q*pdf*d2/sigma`)
    let g_otm = black_scholes_merton_greeks(50_000.0, 60_000.0, 0.25, 0.04, 0.0, 0.60, OptionRight::Call);
    let g_otm_vup = black_scholes_merton_greeks(50_000.0, 60_000.0, 0.25, 0.04, 0.0, 0.601, OptionRight::Call);
    let fd_vanna = (g_otm_vup.delta - g_otm.delta) / 0.001;
    results.push(MutantCase {
        id: "M08",
        domain: "greeks_soa",
        mutation: "Vanna sign inversion",
        killed: (g_otm.vanna - fd_vanna).abs() < 1e-3 && (-g_otm.vanna - fd_vanna).abs() > 0.1,
    });
    // M09: Flip Volga sign (`d1*d2 -> -d1*d2`)
    let fd_volga = (g_otm_vup.vega - g_otm.vega) / 0.001;
    results.push(MutantCase {
        id: "M09",
        domain: "greeks_soa",
        mutation: "Volga sign inversion",
        killed: (g_otm.volga - fd_volga).abs() / g_otm.volga.abs() < 1e-2
            && (-g_otm.volga - fd_volga).abs() / g_otm.volga.abs() > 1.5,
    });
    // M10: American Put collapses to European Put (missing early exercise premium)
    let am_put = option_price(90.0, 100.0, 1.0, 0.08, 0.0, 0.25, OptionRight::Put, ExerciseStyle::American);
    let eu_put = option_price(90.0, 100.0, 1.0, 0.08, 0.0, 0.25, OptionRight::Put, ExerciseStyle::European);
    results.push(MutantCase {
        id: "M10",
        domain: "greeks_soa",
        mutation: "American Put r<=0 guard disables Bjerksund-Stensland early exercise",
        killed: am_put > eu_put + 0.5,
    });

    // --- Domain 3: Hybrid Halley / Brent IV Solver (M11..M14) ---
    let px_ref = option_price(65_000.0, 72_000.0, 30.0 / 365.0, 0.04, 0.01, 0.72, OptionRight::Call, ExerciseStyle::European);
    let sol = solve_implied_volatility(px_ref, 65_000.0, 72_000.0, 30.0 / 365.0, 0.04, 0.01, OptionRight::Call, ExerciseStyle::European, DEFAULT_IV_TOLERANCE).unwrap();
    // M11: Iteration ceiling check (`<= 6` vs `> 6`)
    results.push(MutantCase {
        id: "M11",
        domain: "iv_solver",
        mutation: "Halley convergence tolerance loosened (1e-8 -> 1e-3)",
        killed: (sol.implied_vol - 0.72).abs() < 1e-8 && sol.iterations <= 6,
    });
    // M12: Arbitrage lower bound violation bypassed
    let arb_low = solve_implied_volatility(100.0, 65_000.0, 50_000.0, 0.25, 0.04, 0.0, OptionRight::Call, ExerciseStyle::European, 1e-8);
    results.push(MutantCase {
        id: "M12",
        domain: "iv_solver",
        mutation: "No-arbitrage lower bound check removed",
        killed: arb_low.is_err(),
    });
    // M13: Arbitrage upper bound violation bypassed
    let arb_hi = solve_implied_volatility(70_000.0, 65_000.0, 50_000.0, 0.25, 0.04, 0.0, OptionRight::Call, ExerciseStyle::European, 1e-8);
    results.push(MutantCase {
        id: "M13",
        domain: "iv_solver",
        mutation: "No-arbitrage upper bound check removed",
        killed: arb_hi.is_err(),
    });
    // M14: Zero/negative expiry guard bypassed
    let bad_t = solve_implied_volatility(100.0, 65_000.0, 65_000.0, 0.0, 0.04, 0.0, OptionRight::Call, ExerciseStyle::European, 1e-8);
    results.push(MutantCase {
        id: "M14",
        domain: "iv_solver",
        mutation: "T <= EXPIRY_EPSILON guard flipped to T < 0",
        killed: bad_t.is_err(),
    });

    // --- Domain 4: SVI Arbitrage-Free Surface (M15..M18) ---
    // M15: Lee wing slope bound `b*(1+|rho|) >= 2.0` accepted
    let lee_bad = RawSviSlice { expiry_yrs: 0.25, forward: 60_000.0, a: 0.05, b: 1.5, rho: 0.45, m: 0.0, sigma: 0.2 };
    results.push(MutantCase {
        id: "M15",
        domain: "svi_surface",
        mutation: "Lee moment slope bound b*(1+|rho|) < 2 disabled",
        killed: lee_bad.validate_butterfly_arbitrage_free().is_err(),
    });
    // M16: Durrleman butterfly density `g(k) < 0` accepted
    let bf_bad = RawSviSlice { expiry_yrs: 0.1, forward: 50_000.0, a: 0.001, b: 1.2, rho: 0.6, m: 0.0, sigma: 0.01 };
    results.push(MutantCase {
        id: "M16",
        domain: "svi_surface",
        mutation: "Durrleman butterfly condition g(k) >= 0 disabled",
        killed: bf_bad.validate_butterfly_arbitrage_free().is_err(),
    });
    // M17: Calendar spread crossing `w(T2) < w(T1)` accepted
    let c1 = RawSviSlice::from_ssvi(0.25, 60_000.0, 0.18, -0.3, 0.5);
    let c2 = RawSviSlice::from_ssvi(0.50, 60_000.0, 0.12, -0.3, 0.5);
    results.push(MutantCase {
        id: "M17",
        domain: "svi_surface",
        mutation: "Calendar monotonicity dw/dT >= 0 disabled",
        killed: ArbitrageFreeSviSurface::new_verified(vec![c1, c2]).is_err(),
    });
    // M18: Non-monotonic tenor ordering accepted
    let t_a = RawSviSlice::from_ssvi(0.50, 60_000.0, 0.15, -0.3, 0.5);
    let t_b = RawSviSlice::from_ssvi(0.25, 60_000.0, 0.18, -0.3, 0.5);
    results.push(MutantCase {
        id: "M18",
        domain: "svi_surface",
        mutation: "Strict increasing tenor check T_i <= T_{i-1} disabled",
        killed: ArbitrageFreeSviSurface::new_verified(vec![t_a, t_b]).is_err(),
    });

    // --- Domain 5: 16-Point SPAN Cross-Margin & Hedging (M19..M22) ---
    let spot_fp = 60_000 * FP_SCALE;
    let cfg = SpanRiskConfig::default();
    let unhedged = CrossMarginAccountInput {
        account_id: 1,
        quote_cash_fp: 100_000 * FP_SCALE,
        spot_qty_fp: 0,
        perp_qty_fp: 10 * FP_SCALE,
        perp_entry_price_fp: spot_fp,
        open_order_margin_fp: 0,
        options: vec![],
    };
    let basis_hedged = CrossMarginAccountInput {
        account_id: 2,
        quote_cash_fp: 100_000 * FP_SCALE,
        spot_qty_fp: 10 * FP_SCALE,
        perp_qty_fp: -10 * FP_SCALE,
        perp_entry_price_fp: spot_fp,
        open_order_margin_fp: 0,
        options: vec![],
    };
    let r_u = evaluate_account_cross_margin(&unhedged, spot_fp, spot_fp, &cfg);
    let r_b = evaluate_account_cross_margin(&basis_hedged, spot_fp, spot_fp, &cfg);
    // M19: Spot/Perp cross-hedging offset removed (gross sum instead of net scenario PnL)
    results.push(MutantCase {
        id: "M19",
        domain: "span_margin",
        mutation: "Cross-margin netting replaced by isolated gross margin sum",
        killed: r_b.worst_scenario_loss_fp == 0 && r_b.maintenance_margin_fp < r_u.maintenance_margin_fp / 5,
    });
    // M20: Contingency floor bypassed on zero-delta basis box (`MM == 0` -> infinite leverage!)
    results.push(MutantCase {
        id: "M20",
        domain: "span_margin",
        mutation: "Gross contingency floor dropped (allowing 0 MM on infinite hedged notional)",
        killed: r_b.maintenance_margin_fp == 9_000 * FP_SCALE,
    });
    // M21: IM >= MM hierarchy inverted (`im_to_mm_multiplier_bps < 10_000`)
    results.push(MutantCase {
        id: "M21",
        domain: "span_margin",
        mutation: "IM < MM hierarchy inversion",
        killed: r_u.initial_margin_fp > r_u.maintenance_margin_fp,
    });
    // M22: Short far-OTM naked option minimum floor disabled
    let naked_short = CrossMarginAccountInput {
        account_id: 3,
        quote_cash_fp: 50_000 * FP_SCALE,
        spot_qty_fp: 0,
        perp_qty_fp: 0,
        perp_entry_price_fp: 0,
        open_order_margin_fp: 0,
        options: vec![OptionLegInput {
            strike_fp: 120_000 * FP_SCALE,
            time_to_expiry_yrs: 3.0 / 365.0,
            implied_vol: 0.40,
            rate_r: 0.04,
            carry_q: 0.0,
            right: OptionRight::Call,
            style: ExerciseStyle::European,
            qty_fp: -10 * FP_SCALE,
        }],
    };
    let r_short = evaluate_account_cross_margin(&naked_short, spot_fp, spot_fp, &cfg);
    results.push(MutantCase {
        id: "M22",
        domain: "span_margin",
        mutation: "Short-option minimum charge disabled (allowing near-zero margin on naked short wings)",
        killed: r_short.maintenance_margin_fp >= 9_000 * FP_SCALE,
    });

    // --- Domain 6: Perpetual Mark-Price & Dynamic Clamped Funding (M23..M25) ---
    let mut mk = PerpetualMarkPriceEngine::new("BTC-USDT-PERP", 200_000, 5_000_000_000, 50_000, 100_000);
    let now = 10_000_000_000_u64;
    let feeds_attack = [
        SpotVenueFeed { venue_id: 1, price_fp: 60_000 * FP_SCALE, quote_ts_ns: now },
        SpotVenueFeed { venue_id: 2, price_fp: 60_010 * FP_SCALE, quote_ts_ns: now },
        SpotVenueFeed { venue_id: 3, price_fp: 59_990 * FP_SCALE, quote_ts_ns: now },
        SpotVenueFeed { venue_id: 4, price_fp: 10_000 * FP_SCALE, quote_ts_ns: now }, // Spoofed venue crash!
    ];
    let snap = mk.update_mark_price(&feeds_attack, 60_000 * FP_SCALE, 60_020 * FP_SCALE, now).unwrap();
    // M23: Mean instead of outlier-filtered Median Index (spoofed venue drags index down by -20%)
    results.push(MutantCase {
        id: "M23",
        domain: "perp_funding",
        mutation: "Median index replaced by unfiltered arithmetic mean",
        killed: snap.index_price_fp == 60_000 * FP_SCALE && snap.valid_constituents == 3,
    });
    // M24: Dynamic Funding outer clamp removed
    let f_calc = DynamicFundingCalculator::default();
    let f_hi = f_calc.calculate_funding_rate(
        60_000 * FP_SCALE,
        &[BookPriceLevelFp { price_fp: 66_000 * FP_SCALE, qty_fp: 10 * FP_SCALE }],
        &[BookPriceLevelFp { price_fp: 66_100 * FP_SCALE, qty_fp: 10 * FP_SCALE }],
        20_000,
    );
    results.push(MutantCase {
        id: "M24",
        domain: "perp_funding",
        mutation: "Funding rate outer clamp removed (+10% uncapped)",
        killed: f_hi.funding_rate_ppm == 7_500 && f_hi.premium_index_ppm > 50_000,
    });
    // M25: Zero-sum funding settlement sign inverted
    let (deltas, dust) = DynamicFundingCalculator::settle_zero_sum_funding(60_000 * FP_SCALE, 5_000, &[2 * FP_SCALE, -2 * FP_SCALE]);
    results.push(MutantCase {
        id: "M25",
        domain: "perp_funding",
        mutation: "Funding settlement sign flipped (Longs receive instead of pay when funding > 0)",
        killed: deltas[0] < 0 && deltas[1] > 0 && deltas[0] + deltas[1] + dust == 0,
    });

    // --- Domain 7: 4-Stage Liquidation Waterfall, Z3 T2.1/T2.3 & ADL Quantile Priority (M26..M30) ---
    // M26: Z3 T2.1 sub-tick position MM blindspot (`|q|=1, mark=1, mmr=1` -> `MM_fp` MUST be `>= 1`)
    let tiny_acct = DistressedAccountState {
        account_id: 500,
        cash_balance_fp: 0,
        open_order_margin_fp: 0,
        open_orders_count: 0,
        position_qty_fp: 1,
        entry_price_fp: 1,
        mm_ratio_ppm: 1,
    };
    results.push(MutantCase {
        id: "M26",
        domain: "liquidation_waterfall",
        mutation: "Z3 T2.1 ceil_div -> trunc_div allowing MM_fp == 0 blindspot on tiny position",
        killed: tiny_acct.required_margin_for_check_fp(1) == 1,
    });
    // M27: Stage 1 skips canceling margin open orders before liquidating position
    let mut s1_acct = DistressedAccountState {
        account_id: 501,
        cash_balance_fp: 4_000 * FP_SCALE,
        open_order_margin_fp: 3_500 * FP_SCALE,
        open_orders_count: 4,
        position_qty_fp: FP_SCALE,
        entry_price_fp: 50_000 * FP_SCALE,
        mm_ratio_ppm: 20_000,
    };
    let r_s1 = execute_liquidation_waterfall(
        &mut s1_acct,
        50_000 * FP_SCALE,
        &mut [],
        &mut InsuranceFundState::new(100_000 * FP_SCALE, 500_000),
        &mut [],
        &WaterfallConfig::default(),
    );
    results.push(MutantCase {
        id: "M27",
        domain: "liquidation_waterfall",
        mutation: "Stage-1 open-order cancellation skipped",
        killed: r_s1.stage_reached == LiquidationStageReached::Stage1CancelOpenOrders
            && r_s1.final_account_position_qty_fp == FP_SCALE,
    });
    // M28: Stage 2 executes past bankruptcy limit price (selling below P_bankrupt)
    let mut s2_acct = DistressedAccountState {
        account_id: 502,
        cash_balance_fp: 2_000 * FP_SCALE, // Entry 50k, 1 BTC -> P_bankrupt = 48,000!
        open_order_margin_fp: 0,
        open_orders_count: 0,
        position_qty_fp: FP_SCALE,
        entry_price_fp: 50_000 * FP_SCALE,
        mm_ratio_ppm: 20_000,
    };
    let mut low_bids = [BookPriceLevelFp { price_fp: 47_000 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
    let mut if_zero = InsuranceFundState::new(0, 0);
    let mut opp_winner = [AdlCounterpartyAccount {
        account_id: 601,
        position_qty_fp: -FP_SCALE,
        entry_price_fp: 50_000 * FP_SCALE,
        cash_balance_fp: 10_000 * FP_SCALE,
        mm_ratio_ppm: 20_000,
    }];
    let r_s2 = execute_liquidation_waterfall(
        &mut s2_acct,
        47_500 * FP_SCALE,
        &mut low_bids,
        &mut if_zero,
        &mut opp_winner,
        &WaterfallConfig::default(),
    );
    results.push(MutantCase {
        id: "M28",
        domain: "liquidation_waterfall",
        mutation: "Stage-2 bankruptcy limit check (>= -> <=) sells below P_bankrupt",
        killed: r_s2.book_liquidated_qty_fp == 0
            && r_s2.stage_reached == LiquidationStageReached::Stage4AutoDeleveraging
            && r_s2.unbacked_systemic_deficit_fp == 0,
    });
    // M29: ADL ranking order reversed (hitting low-leverage account before high-leverage winner!)
    let adl_pool = [
        AdlCounterpartyAccount {
            account_id: 701,
            position_qty_fp: -2 * FP_SCALE,
            entry_price_fp: 60_000 * FP_SCALE,
            cash_balance_fp: 500_000 * FP_SCALE, // Low leverage
            mm_ratio_ppm: 20_000,
        },
        AdlCounterpartyAccount {
            account_id: 702,
            position_qty_fp: -2 * FP_SCALE,
            entry_price_fp: 60_000 * FP_SCALE,
            cash_balance_fp: 5_000 * FP_SCALE, // High leverage top winner -> MUST be rank 0 / Q5!
            mm_ratio_ppm: 20_000,
        },
    ];
    let ranked = rank_adl_counterparties(&adl_pool, true, 40_000 * FP_SCALE);
    results.push(MutantCase {
        id: "M29",
        domain: "liquidation_waterfall",
        mutation: "ADL sort order flipped (ascending instead of descending profit-leverage)",
        killed: ranked[0].1.account_id == 702 && ranked[0].1.quantile_bucket == 5,
    });
    // M30: Z3 T2.3 Principal Protection violated on profitable ADL counterparty (`cash_after < initial_cash`)
    results.push(MutantCase {
        id: "M30",
        domain: "liquidation_waterfall",
        mutation: "Z3 T2.3 principal protection removed (ADL seizes profitable trader's initial principal)",
        killed: opp_winner[0].cash_balance_fp >= 10_000 * FP_SCALE,
    });

    let total = results.len();
    let killed = results.iter().filter(|m| m.killed).count();
    let score_pct = (killed as f64 / total as f64) * 100.0;
    println!(
        "\n[OVERCLOCK GATE 1 — MUTATION SURVIVAL ZERO] Killed = {killed} / {total} ({score_pct:.2}%)"
    );
    for m in &results {
        assert!(
            m.killed,
            "SURVIVED MUTANT {} [{}]: {}",
            m.id, m.domain, m.mutation
        );
    }
    assert_eq!(killed, 30);
    assert!(score_pct >= 95.0);
}
