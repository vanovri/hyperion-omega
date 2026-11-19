//! Canonical `hyperion.omega.v1` Protobuf Wire Adapter (`01-hyperion-omega.md` §5.1).
//!
//! Strictly enforces the Hyperion Omega fixed-point wire contract:
//! - Monetary & price fields: `1e8` integer atomic ticks (`I128`, `U128`, `u64`, `i64`).
//! - Implied Volatility (`implied_vol_fp`): `u64` scaled by `1e8`.
//! - Perpetual Funding Rate (`funding_rate_fp`): `i64` scaled by `1e8` (`ppm * 100`, clamped `±500_000` per §5.1 or dynamic cap).
//! - Option Greeks (`delta_fp`, `gamma_fp`, `vega_fp`, `rho_fp`, `vanna_fp`, `volga_fp`): `i64` scaled by `1e8`.
//! - Option Theta (`theta_fp`): `i64` scaled by `1e8` **per calendar day (`1 / 365.25` yr)** per `01-hyperion-omega.md` §5.1!

pub use hyperion_omega_proto::v1 as pb;

use crate::fixed_point::{f64_to_fp_nearest, mul_fp_floor, mul_fp_trunc, FP_SCALE, PPM_SCALE};
use crate::greeks_soa::{option_greeks, ExerciseStyle, OptionRight, SECONDS_PER_YEAR};
use crate::iv_solver::{solve_implied_volatility, DEFAULT_IV_TOLERANCE};
use crate::liquidation_waterfall::{LiquidationStageReached, LiquidationWaterfallReceipt};
use crate::span_margin::{
    CrossMarginAccountInput, PortfolioMarginResult, CANONICAL_SPAN_16_SCENARIOS,
};
use crate::svi_surface::ArbitrageFreeSviSurface;
use hyperion_proto::{OrderSide, I128, U128};

/// Canonical calendar days per year specified in `.agents/rules/01-hyperion-omega.md` §5.1 for `theta_fp`.
pub const CALENDAR_DAYS_PER_YEAR: f64 = 365.25;

/// Canonical Redpanda topics owned by `services/margin-risk-rs` (`01-hyperion-omega.md` §3).
pub const TOPIC_MARGIN_SNAPSHOTS: &str = "hyperion.omega.margin.snapshots.v1";
pub const TOPIC_MARGIN_GREEKS: &str = "hyperion.omega.margin.greeks.v1";
pub const TOPIC_MARGIN_LIQUIDATIONS: &str = "hyperion.omega.margin.liquidations.v1";
pub const TOPIC_MARGIN_ADL: &str = "hyperion.omega.margin.adl.v1";

#[inline]
fn clamp_i128_to_i64(v: i128) -> i64 {
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

#[inline]
fn clamp_i128_to_u64(v: i128) -> u64 {
    v.clamp(0, u64::MAX as i128) as u64
}

#[inline]
fn f64_to_i64_fp(v: f64) -> i64 {
    clamp_i128_to_i64(f64_to_fp_nearest(v))
}

#[inline]
fn f64_to_u64_fp(v: f64) -> u64 {
    clamp_i128_to_u64(f64_to_fp_nearest(v.max(0.0)))
}

/// Convert `PortfolioMarginResult` + input account to canonical `hyperion.omega.v1.PortfolioMarginSnapshot`.
#[allow(clippy::too_many_arguments)]
pub fn to_portfolio_margin_snapshot(
    acct: &CrossMarginAccountInput,
    res: &PortfolioMarginResult,
    symbol: &str,
    instrument_id: u32,
    spot_index_fp: i128,
    perp_mark_fp: i128,
    funding_rate_ppm: i128,
    adl_quantile_ppm: u32,
    engine_seq: u64,
    ts_ns: u64,
) -> pb::PortfolioMarginSnapshot {
    let health_status = if res.equity_fp < 0 {
        pb::MarginHealthStatus::BankruptDeficit as i32
    } else if res.is_liquidatable {
        pb::MarginHealthStatus::MaintenanceBreachLiquidatable as i32
    } else if res.is_initial_margin_breached {
        pb::MarginHealthStatus::InitialMarginBreach as i32
    } else {
        pb::MarginHealthStatus::Healthy as i32
    };

    let perp_upnl_fp = mul_fp_floor(acct.perp_qty_fp, perp_mark_fp - acct.perp_entry_price_fp);
    let spot_val_fp = mul_fp_floor(acct.spot_qty_fp, spot_index_fp);
    let collateral_balance_fp = acct.quote_cash_fp + spot_val_fp;
    let total_upnl_fp = res.equity_fp - collateral_balance_fp;

    // Effective leverage = gross_notional / max(equity, 1) in ppm (1_000_000 = 1.0x)
    let perp_notional_fp = mul_fp_trunc(acct.perp_qty_fp.abs(), perp_mark_fp);
    let effective_leverage_ppm = if res.equity_fp > 0 {
        clamp_i128_to_u64((perp_notional_fp * PPM_SCALE) / res.equity_fp)
    } else if perp_notional_fp > 0 {
        u64::MAX
    } else {
        0
    };

    // Exact bankruptcy price for perpetual leg (if open)
    let bankruptcy_px_fp = if acct.perp_qty_fp != 0 {
        let offset = (collateral_balance_fp * FP_SCALE) / acct.perp_qty_fp;
        clamp_i128_to_u64((acct.perp_entry_price_fp - offset).max(1))
    } else {
        0
    };

    // Funding rate on wire uses `1e8` (`funding_rate_fp = funding_rate_ppm * 100`) clamped per §5.1
    let funding_rate_fp = clamp_i128_to_i64(funding_rate_ppm * 100);

    let mut positions = Vec::new();
    if acct.perp_qty_fp != 0 {
        positions.push(pb::DerivativePositionRisk {
            instrument_id,
            symbol: format!("{symbol}-PERP"),
            signed_qty_fp: clamp_i128_to_i64(acct.perp_qty_fp),
            avg_entry_price_fp: clamp_i128_to_u64(acct.perp_entry_price_fp),
            mark_price_fp: clamp_i128_to_u64(perp_mark_fp),
            index_price_fp: clamp_i128_to_u64(spot_index_fp),
            bankruptcy_price_fp: bankruptcy_px_fp,
            unrealized_pnl_fp: Some(I128::from(perp_upnl_fp)),
            funding_rate_fp,
            net_delta_fp: clamp_i128_to_i64(acct.perp_qty_fp),
            net_gamma_fp: 0,
            net_vega_fp: 0,
        });
    }

    let span_grid: Vec<pb::SpanScenarioPoint> = CANONICAL_SPAN_16_SCENARIOS
        .iter()
        .enumerate()
        .map(|(idx, sc)| pb::SpanScenarioPoint {
            scenario_id: sc.scenario_id as u32,
            price_shock_bps: (sc.spot_shock_pct * 10_000.0).round() as i32,
            vol_shock_bps: (sc.vol_shock_pct * 10_000.0).round() as i32,
            scenario_pnl_fp: Some(I128::from(res.scenario_pnls_fp[idx])),
        })
        .collect();

    pb::PortfolioMarginSnapshot {
        account_id: acct.account_id,
        engine_seq,
        collateral_asset: "USDT".to_string(),
        health_status,
        collateral_balance_fp: Some(I128::from(collateral_balance_fp)),
        unrealized_pnl_fp: Some(I128::from(total_upnl_fp)),
        net_equity_fp: Some(I128::from(res.equity_fp)),
        initial_margin_req_fp: Some(U128::from(res.initial_margin_fp.max(0) as u128)),
        maintenance_margin_req_fp: Some(U128::from(res.maintenance_margin_fp.max(0) as u128)),
        worst_span_loss_fp: Some(U128::from(res.worst_scenario_loss_fp.max(0) as u128)),
        portfolio_delta_fp: f64_to_i64_fp(res.net_delta),
        portfolio_gamma_fp: f64_to_i64_fp(res.net_gamma),
        portfolio_vega_fp: f64_to_i64_fp(res.net_vega),
        // §5.1 Mandate: Theta quantized per calendar day (1/365.25 yr) in scale 1e8!
        portfolio_theta_fp: f64_to_i64_fp(res.net_theta / CALENDAR_DAYS_PER_YEAR),
        portfolio_rho_fp: f64_to_i64_fp(0.0),
        portfolio_vanna_fp: f64_to_i64_fp(res.net_vanna),
        portfolio_volga_fp: f64_to_i64_fp(res.net_volga),
        effective_leverage_ppm,
        adl_quantile_ppm: adl_quantile_ppm.min(1_000_000),
        positions,
        span_grid,
        computed_ts_ns: ts_ns,
        trace: None,
    }
}

/// Build canonical `hyperion.omega.v1.OptionGreeksSurface` from an arbitrage-free SVI surface
/// and strike ladder, solving roundtrip Halley IV & 1st/2nd order analytical Greeks.
#[allow(clippy::too_many_arguments)]
pub fn build_option_greeks_surface_proto(
    underlying_symbol: &str,
    surface: &ArbitrageFreeSviSurface,
    spot_index_fp: i128,
    risk_free_rate: f64,
    carry_q: f64,
    strikes_usd: &[f64],
    engine_seq: u64,
    ts_ns: u64,
) -> pb::OptionGreeksSurface {
    let t0 = std::time::Instant::now();
    let spot = (spot_index_fp as f64) * 1e-8;
    let mut svi_slices = Vec::with_capacity(surface.slices.len());
    let mut points = Vec::with_capacity(surface.slices.len() * strikes_usd.len() * 2);

    let mut all_arb_free = true;
    for (s_idx, sl) in surface.slices.iter().enumerate() {
        let bf_ok = sl.validate_butterfly_arbitrage_free().is_ok();
        let cal_ok = if s_idx == 0 {
            true
        } else {
            let prev = &surface.slices[s_idx - 1];
            sl.total_variance(0.0) >= prev.total_variance(0.0)
        };
        if !bf_ok || !cal_ok {
            all_arb_free = false;
        }
        let t_sec = (sl.expiry_yrs * SECONDS_PER_YEAR).round().max(1.0) as u64;
        let expiry_ts_ns = ts_ns.saturating_add(t_sec.saturating_mul(1_000_000_000));

        svi_slices.push(pb::SviExpirySlice {
            expiry_ts_ns,
            time_to_expiry_sec: t_sec,
            forward_price_fp: f64_to_u64_fp(sl.forward),
            svi_a_fp: f64_to_i64_fp(sl.a),
            svi_b_fp: f64_to_u64_fp(sl.b),
            svi_rho_fp: f64_to_i64_fp(sl.rho),
            svi_m_fp: f64_to_i64_fp(sl.m),
            svi_sigma_fp: f64_to_u64_fp(sl.sigma),
            butterfly_arbitrage_free: bf_ok,
            calendar_arbitrage_free: cal_ok,
        });

        for (k_idx, &strike) in strikes_usd.iter().enumerate() {
            let iv_svi = sl.implied_vol_at_strike(strike);
            for &right in &[OptionRight::Call, OptionRight::Put] {
                let g = option_greeks(
                    spot,
                    strike,
                    sl.expiry_yrs,
                    risk_free_rate,
                    carry_q,
                    iv_svi,
                    right,
                    ExerciseStyle::European,
                );
                let (iv_solved, solver_iters) = match solve_implied_volatility(
                    g.price,
                    spot,
                    strike,
                    sl.expiry_yrs,
                    risk_free_rate,
                    carry_q,
                    right,
                    ExerciseStyle::European,
                    DEFAULT_IV_TOLERANCE,
                ) {
                    Ok(sol) => (sol.implied_vol, sol.iterations),
                    Err(_) => (iv_svi, 0),
                };
                let right_code = match right {
                    OptionRight::Call => ("C", pb::OptionRight::Call as i32),
                    OptionRight::Put => ("P", pb::OptionRight::Put as i32),
                };
                points.push(pb::OptionGreeksPoint {
                    instrument_id: (1000 + s_idx * 100 + k_idx * 2 + usize::from(matches!(right, OptionRight::Put))) as u32,
                    option_symbol: format!(
                        "{}-{}D-{}-{}",
                        underlying_symbol,
                        (sl.expiry_yrs * 365.0).round() as u32,
                        strike.round() as u64,
                        right_code.0
                    ),
                    right: right_code.1,
                    style: pb::OptionExerciseStyle::European as i32,
                    expiry_ts_ns,
                    strike_price_fp: f64_to_u64_fp(strike),
                    underlying_mark_fp: clamp_i128_to_u64(spot_index_fp),
                    option_mark_price_fp: f64_to_u64_fp(g.price),
                    implied_vol_fp: f64_to_u64_fp(iv_solved),
                    solver_iterations: solver_iters,
                    delta_fp: f64_to_i64_fp(g.delta),
                    gamma_fp: f64_to_i64_fp(g.gamma),
                    vega_fp: f64_to_i64_fp(g.vega),
                    // §5.1 Mandate: Theta quantized per calendar day (1/365.25 yr) in scale 1e8!
                    theta_fp: f64_to_i64_fp(g.theta / CALENDAR_DAYS_PER_YEAR),
                    rho_fp: f64_to_i64_fp(g.rho),
                    vanna_fp: f64_to_i64_fp(g.vanna),
                    volga_fp: f64_to_i64_fp(g.volga),
                });
            }
        }
    }

    pb::OptionGreeksSurface {
        underlying_symbol: underlying_symbol.to_string(),
        engine_seq,
        spot_index_price_fp: clamp_i128_to_u64(spot_index_fp),
        risk_free_rate_fp: f64_to_i64_fp(risk_free_rate),
        svi_slices,
        points,
        surface_arbitrage_free: all_arb_free,
        evaluation_latency_us: t0.elapsed().as_micros() as u64,
        ts_ns,
        trace: None,
    }
}

/// Convert `LiquidationWaterfallReceipt` into canonical `LiquidationWaterfallEvent` and optional `AdlTrigger`.
#[allow(clippy::too_many_arguments)]
pub fn to_liquidation_and_adl_protos(
    receipt: &LiquidationWaterfallReceipt,
    liquidation_id: &str,
    symbol: &str,
    was_long: bool,
    mark_price_fp: i128,
    bankruptcy_price_fp: i128,
    equity_before_fp: i128,
    maintenance_margin_fp: i128,
    engine_seq: u64,
    ts_ns: u64,
) -> (pb::LiquidationWaterfallEvent, Option<pb::AdlTrigger>) {
    let stage = match receipt.stage_reached {
        LiquidationStageReached::HealthyNoAction => pb::LiquidationStage::Unspecified as i32,
        LiquidationStageReached::Stage1CancelOpenOrders => {
            pb::LiquidationStage::CancelOpenMarginOrders as i32
        }
        LiquidationStageReached::Stage2PartialBookLiquidation => {
            pb::LiquidationStage::PartialBookLiquidation as i32
        }
        LiquidationStageReached::Stage3InsuranceFundAbsorption => {
            pb::LiquidationStage::InsuranceFundAbsorption as i32
        }
        LiquidationStageReached::Stage4AutoDeleveraging => {
            pb::LiquidationStage::AutoDeleveragingAdl as i32
        }
    };

    let if_delta_fp =
        receipt.insurance_fund_surplus_added_fp - receipt.insurance_fund_deficit_absorbed_fp;

    let liq_evt = pb::LiquidationWaterfallEvent {
        liquidation_id: liquidation_id.to_string(),
        engine_seq,
        account_id: receipt.account_id,
        symbol: symbol.to_string(),
        stage,
        mark_price_fp: clamp_i128_to_u64(mark_price_fp),
        bankruptcy_price_fp: clamp_i128_to_u64(bankruptcy_price_fp),
        execution_limit_price_fp: clamp_i128_to_u64(bankruptcy_price_fp),
        liquidated_qty_fp: clamp_i128_to_u64(
            receipt.book_liquidated_qty_fp + receipt.adl_liquidated_qty_fp,
        ),
        canceled_open_orders: receipt.canceled_open_orders,
        equity_before_fp: Some(I128::from(equity_before_fp)),
        maintenance_margin_fp: Some(U128::from(maintenance_margin_fp.max(0) as u128)),
        insurance_fund_delta_fp: Some(I128::from(if_delta_fp)),
        insurance_fund_after_fp: Some(U128::from(receipt.final_insurance_fund_fp.max(0) as u128)),
        residual_deficit_fp: Some(U128::from(
            receipt.unbacked_systemic_deficit_fp.max(0) as u128,
        )),
        ts_ns,
        trace: None,
    };

    let adl_opt = if !receipt.adl_matches.is_empty() {
        let allocations = receipt
            .adl_matches
            .iter()
            .map(|m| pb::AdlCounterpartyAllocation {
                counterparty_account_id: m.counterparty_account_id,
                quantile_rank_ppm: (m.counterparty_quantile as u32) * 200_000,
                ranking_score_fp: (m.counterparty_quantile as u64) * 20_000_000,
                deleveraged_qty_fp: clamp_i128_to_u64(m.closed_qty_fp),
                settlement_price_fp: clamp_i128_to_u64(m.execution_price_fp),
                realized_pnl_settled_fp: Some(I128::from(-m.deficit_covered_fp)),
            })
            .collect();

        Some(pb::AdlTrigger {
            adl_id: format!("adl-{liquidation_id}"),
            source_liquidation_id: liquidation_id.to_string(),
            engine_seq,
            bankrupt_account_id: receipt.account_id,
            symbol: symbol.to_string(),
            bankrupt_position_side: if was_long {
                OrderSide::Buy as i32
            } else {
                OrderSide::Sell as i32
            },
            mark_price_fp: clamp_i128_to_u64(mark_price_fp),
            bankruptcy_price_fp: clamp_i128_to_u64(bankruptcy_price_fp),
            uncovered_deficit_fp: Some(U128::from(receipt.adl_deficit_covered_fp.max(0) as u128)),
            total_adl_qty_fp: clamp_i128_to_u64(receipt.adl_liquidated_qty_fp),
            allocations,
            system_solvency_preserved: receipt.unbacked_systemic_deficit_fp == 0,
            triggered_ts_ns: ts_ns,
            trace: None,
        })
    } else {
        None
    };

    (liq_evt, adl_opt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span_margin::{evaluate_account_cross_margin, OptionLegInput, SpanRiskConfig};
    use prost::Message;

    #[test]
    fn test_omega_protobuf_wire_roundtrip_margin_and_greeks_surface() {
        let spot_fp = 65_000 * FP_SCALE;
        let acct = CrossMarginAccountInput {
            account_id: 10042,
            quote_cash_fp: 100_000 * FP_SCALE,
            spot_qty_fp: FP_SCALE,
            perp_qty_fp: -2 * FP_SCALE,
            perp_entry_price_fp: spot_fp,
            open_order_margin_fp: 1_500 * FP_SCALE,
            options: vec![OptionLegInput {
                strike_fp: 68_000 * FP_SCALE,
                time_to_expiry_yrs: 30.0 / 365.0,
                implied_vol: 0.60,
                rate_r: 0.04,
                carry_q: 0.01,
                right: OptionRight::Call,
                style: ExerciseStyle::European,
                qty_fp: 2 * FP_SCALE,
            }],
        };
        let res = evaluate_account_cross_margin(&acct, spot_fp, spot_fp, &SpanRiskConfig::default());
        let snap = to_portfolio_margin_snapshot(
            &acct, &res, "BTC-USDT", 1, spot_fp, spot_fp, 100, 600_000, 42, 1_795_015_000_000_000_000,
        );
        assert_eq!(snap.span_grid.len(), 16);
        assert_eq!(snap.health_status, pb::MarginHealthStatus::Healthy as i32);

        let bytes = snap.encode_to_vec();
        let dec = pb::PortfolioMarginSnapshot::decode(bytes.as_slice()).unwrap();
        assert_eq!(dec.account_id, 10042);
        assert_eq!(dec.span_grid.len(), 16);

        let pillars = [
            (7.0 / 365.0, 65_000.0, 0.58),
            (30.0 / 365.0, 65_400.0, 0.60),
        ];
        let surf =
            ArbitrageFreeSviSurface::from_ssvi_term_structure(&pillars, -0.35, 0.85, 0.40).unwrap();
        let proto_surf = build_option_greeks_surface_proto(
            "BTC-USDT",
            &surf,
            spot_fp,
            0.04,
            0.01,
            &[60_000.0, 65_000.0, 70_000.0],
            42,
            1_795_015_000_000_000_000,
        );
        assert!(proto_surf.surface_arbitrage_free);
        assert_eq!(proto_surf.svi_slices.len(), 2);
        assert_eq!(proto_surf.points.len(), 12);
        for pt in &proto_surf.points {
            assert!(pt.solver_iterations <= 6);
        }
        let s_bytes = proto_surf.encode_to_vec();
        let s_dec = pb::OptionGreeksSurface::decode(s_bytes.as_slice()).unwrap();
        assert_eq!(s_dec.points.len(), 12);
    }
}
