//! Perpetual Futures Fair Mark-Price (Constituent Median Index + EMA Basis) &
//! Dynamic Clamped Funding Rate Engine.
//!
//! 100% compliant with Hyperion's integer fixed-point invariant (`FP_SCALE = 1e8` for prices
//! and notionals, `PPM_SCALE = 1e6` for funding rates and EMA smoothing weights).

use crate::fixed_point::{apply_ppm_trunc, mul_fp_trunc, FP_SCALE, PPM_SCALE};
use serde::{Deserialize, Serialize};

/// Single spot constituent feed quote for Index Price computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotVenueFeed {
    pub venue_id: u32,
    pub price_fp: i128,
    pub quote_ts_ns: u64,
}

/// L2 Order Book level `(price_fp, qty_fp)` in `1e8` fixed-point atomic ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookPriceLevelFp {
    pub price_fp: i128,
    pub qty_fp: i128,
}

/// Deterministic Perpetual Mark-Price Calculator:
/// - **Index Price ($I_t$)**: Exact median of non-stale constituent spot feeds (outlier-filtered
///   within configurable deviation collar around raw median).
/// - **EMA Basis ($\text{EMA}_t$)**: Exponentially smoothed basis $B_t = P_{\text{perp\_mid}, t} - I_t$
///   updated strictly in integer fixed-point (`alpha_ppm` $\in [1, 1\_000\_000]$).
/// - **Fair Mark Price ($P_{\text{mark}, t}$)**:
///   $$P_{\text{mark}, t} = \text{clamp}\Big(\text{median}\big(P_{\text{bid}}, P_{\text{ask}}, I_t + \text{EMA}_t\big), \; I_t(1 - c_{\text{band}}), \; I_t(1 + c_{\text{band}})\Big)$$
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerpetualMarkPriceEngine {
    pub symbol: String,
    /// EMA smoothing factor in parts-per-million (`1_000_000` = `1.0`, e.g., `50_000` = `0.05`).
    pub ema_alpha_ppm: i128,
    /// Max allowed stale feed age in nanoseconds (e.g., `5_000_000_000` = `5s`).
    pub max_feed_staleness_ns: u64,
    /// Outlier filter collar around raw median in ppm (e.g., `50_000` = `5%`).
    pub outlier_collar_ppm: i128,
    /// Hard mark-price band around index price in ppm (e.g., `100_000` = `10%`).
    pub mark_band_ppm: i128,
    /// Current state: smoothed basis in `1e8` ticks.
    pub ema_basis_fp: i128,
    pub last_index_price_fp: i128,
    pub last_mark_price_fp: i128,
    pub initialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkPriceSnapshot {
    pub index_price_fp: i128,
    pub instantaneous_basis_fp: i128,
    pub ema_basis_fp: i128,
    pub mark_price_fp: i128,
    pub valid_constituents: usize,
}

impl PerpetualMarkPriceEngine {
    pub fn new(
        symbol: impl Into<String>,
        ema_alpha_ppm: i128,
        max_feed_staleness_ns: u64,
        outlier_collar_ppm: i128,
        mark_band_ppm: i128,
    ) -> Self {
        Self {
            symbol: symbol.into(),
            ema_alpha_ppm: ema_alpha_ppm.clamp(1, PPM_SCALE),
            max_feed_staleness_ns,
            outlier_collar_ppm: outlier_collar_ppm.max(1_000),
            mark_band_ppm: mark_band_ppm.max(5_000),
            ema_basis_fp: 0,
            last_index_price_fp: 0,
            last_mark_price_fp: 0,
            initialized: false,
        }
    }

    /// Exact integer median of a non-empty sorted slice.
    #[inline]
    fn exact_median_sorted(sorted: &[i128]) -> i128 {
        let n = sorted.len();
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2
        }
    }

    /// Compute robust constituent Median Index Price in `1e8` fixed-point ticks.
    pub fn compute_index_median_fp(&self, feeds: &[SpotVenueFeed], now_ts_ns: u64) -> Option<(i128, usize)> {
        let mut valid: Vec<i128> = feeds
            .iter()
            .filter(|f| {
                f.price_fp > 0
                    && now_ts_ns.saturating_sub(f.quote_ts_ns) <= self.max_feed_staleness_ns
            })
            .map(|f| f.price_fp)
            .collect();
        if valid.is_empty() {
            return None;
        }
        valid.sort_unstable();
        let raw_med = Self::exact_median_sorted(&valid);
        let max_dev = apply_ppm_trunc(raw_med, self.outlier_collar_ppm);

        let filtered: Vec<i128> = valid
            .into_iter()
            .filter(|&p| (p - raw_med).abs() <= max_dev)
            .collect();

        if filtered.is_empty() {
            Some((raw_med, 1))
        } else {
            let count = filtered.len();
            Some((Self::exact_median_sorted(&filtered), count))
        }
    }

    /// Update EMA Basis and return deterministic `MarkPriceSnapshot`.
    pub fn update_mark_price(
        &mut self,
        feeds: &[SpotVenueFeed],
        best_bid_fp: i128,
        best_ask_fp: i128,
        now_ts_ns: u64,
    ) -> Option<MarkPriceSnapshot> {
        let (index_fp, valid_cnt) = self.compute_index_median_fp(feeds, now_ts_ns)?;

        let book_valid = best_bid_fp > 0 && best_ask_fp >= best_bid_fp;
        let perp_mid_fp = if book_valid {
            (best_bid_fp + best_ask_fp) / 2
        } else {
            index_fp + self.ema_basis_fp
        };

        let band_fp = apply_ppm_trunc(index_fp, self.mark_band_ppm);
        let raw_basis_fp = (perp_mid_fp - index_fp).clamp(-band_fp, band_fp);

        if !self.initialized {
            self.ema_basis_fp = raw_basis_fp;
            self.initialized = true;
        } else {
            let delta = raw_basis_fp - self.ema_basis_fp;
            self.ema_basis_fp += apply_ppm_trunc(delta, self.ema_alpha_ppm);
        }

        let fair_ema_price_fp = index_fp + self.ema_basis_fp;
        let mark_unclamped = if book_valid {
            let mut trio = [best_bid_fp, best_ask_fp, fair_ema_price_fp];
            trio.sort_unstable();
            trio[1]
        } else {
            fair_ema_price_fp
        };

        let mark_fp = mark_unclamped.clamp((index_fp - band_fp).max(1), index_fp + band_fp);
        self.last_index_price_fp = index_fp;
        self.last_mark_price_fp = mark_fp;

        Some(MarkPriceSnapshot {
            index_price_fp: index_fp,
            instantaneous_basis_fp: raw_basis_fp,
            ema_basis_fp: self.ema_basis_fp,
            mark_price_fp: mark_fp,
            valid_constituents: valid_cnt,
        })
    }
}

/// Dynamic Clamped Funding Rate Engine (`PPM_SCALE = 1e6`, e.g., `100 ppm` = `0.01%` = `1 bps`).
///
/// Implements institutional impact-notional L2 book walk + interest-rate differential inner
/// damper + dynamic Maintenance-Margin-bounded outer clamp:
/// $$F = \text{clamp}\Big(P_{\text{prem}} + \text{clamp}(I_{\text{rate}} - P_{\text{prem}}, -d_{\text{in}}, +d_{\text{in}}), \; -F_{\max}, \; +F_{\max}\Big)$$
/// where $F_{\max} = \min\big(F_{\text{hard\_cap}}, \; \eta \cdot \text{MM\_ratio}\big)$.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DynamicFundingCalculator {
    /// Impact Margin Notional walked through L2 bids/asks in `1e8` quote ticks (e.g., `25_000 * 1e8`).
    pub impact_notional_fp: i128,
    /// Base interest rate per funding interval in ppm (e.g., `100` = `0.01%` per 8h).
    pub base_interest_rate_ppm: i128,
    /// Inner neutral zone damper in ppm (e.g., `500` = `0.05%`).
    pub inner_damper_ppm: i128,
    /// Hard outer funding rate cap in ppm (e.g., `7_500` = `0.75%` per interval).
    pub hard_cap_ppm: i128,
    /// Dynamic cap multiplier of Maintenance Margin ratio in ppm (e.g., `750_000` = `75%` of MM ratio).
    pub mm_cap_fraction_ppm: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingRateOutput {
    pub index_price_fp: i128,
    pub impact_bid_fp: i128,
    pub impact_ask_fp: i128,
    pub premium_index_ppm: i128,
    pub effective_cap_ppm: i128,
    pub funding_rate_ppm: i128,
}

impl Default for DynamicFundingCalculator {
    fn default() -> Self {
        Self {
            impact_notional_fp: 25_000 * FP_SCALE,
            base_interest_rate_ppm: 100, // +0.01% per 8h
            inner_damper_ppm: 500,       // +/- 0.05% damper
            hard_cap_ppm: 7_500,         // +/- 0.75% cap
            mm_cap_fraction_ppm: 750_000, // 75% * MM_ratio
        }
    }
}

impl DynamicFundingCalculator {
    /// Walk sorted L2 book side (`bids` descending or `asks` ascending) to compute VWAP
    /// execution price for `impact_notional_fp` quote value.
    pub fn compute_impact_price_fp(&self, levels: &[BookPriceLevelFp], fallback_fp: i128) -> i128 {
        if levels.is_empty() || self.impact_notional_fp <= 0 {
            return fallback_fp;
        }
        let target_notional = self.impact_notional_fp;
        let mut rem_notional = target_notional;
        let mut accum_base_fp: i128 = 0;

        for lvl in levels {
            if lvl.price_fp <= 0 || lvl.qty_fp <= 0 {
                continue;
            }
            let lvl_notional = mul_fp_trunc(lvl.price_fp, lvl.qty_fp);
            if lvl_notional >= rem_notional {
                // Fractional base quantity needed at this marginal tick: (rem_notional * 1e8) / price_fp
                let needed_base_fp = (rem_notional * FP_SCALE) / lvl.price_fp;
                accum_base_fp += needed_base_fp;
                rem_notional = 0;
                break;
            } else {
                accum_base_fp += lvl.qty_fp;
                rem_notional -= lvl_notional;
            }
        }

        // If book depth is thinner than impact_notional_fp, fill remainder at worst resting tick
        if rem_notional > 0 {
            let last_px = levels.last().map(|l| l.price_fp).unwrap_or(fallback_fp).max(1);
            accum_base_fp += (rem_notional * FP_SCALE) / last_px;
        }

        if accum_base_fp <= 0 {
            fallback_fp
        } else {
            (target_notional * FP_SCALE) / accum_base_fp
        }
    }

    /// Calculate dynamically clamped funding rate in parts-per-million (`ppm`, `1e6`).
    /// `maintenance_margin_ratio_ppm`: e.g. `10_000` (`1.0%`) -> dynamic cap = `75% * 1.0% = 7_500 ppm`.
    pub fn calculate_funding_rate(
        &self,
        index_price_fp: i128,
        bids: &[BookPriceLevelFp],
        asks: &[BookPriceLevelFp],
        maintenance_margin_ratio_ppm: i128,
    ) -> FundingRateOutput {
        let idx = index_price_fp.max(1);
        let impact_bid_fp = self.compute_impact_price_fp(bids, idx);
        let impact_ask_fp = self.compute_impact_price_fp(asks, idx);

        let bid_excess_fp = (impact_bid_fp - idx).max(0);
        let ask_discount_fp = (idx - impact_ask_fp).max(0);
        let premium_index_ppm = ((bid_excess_fp - ask_discount_fp) * PPM_SCALE) / idx;

        let ir_diff = (self.base_interest_rate_ppm - premium_index_ppm)
            .clamp(-self.inner_damper_ppm, self.inner_damper_ppm);
        let unclamped_ppm = premium_index_ppm + ir_diff;

        let dyn_mm_cap = if maintenance_margin_ratio_ppm > 0 {
            apply_ppm_trunc(maintenance_margin_ratio_ppm, self.mm_cap_fraction_ppm).max(100)
        } else {
            self.hard_cap_ppm
        };
        let effective_cap_ppm = self.hard_cap_ppm.min(dyn_mm_cap);
        let funding_rate_ppm = unclamped_ppm.clamp(-effective_cap_ppm, effective_cap_ppm);

        FundingRateOutput {
            index_price_fp: idx,
            impact_bid_fp,
            impact_ask_fp,
            premium_index_ppm,
            effective_cap_ppm,
            funding_rate_ppm,
        }
    }

    /// Execute exact zero-sum integer fixed-point (`1e8`) funding settlement across accounts!
    /// Positive `funding_rate_ppm` -> Longs pay Shorts; Negative -> Shorts pay Longs.
    /// Any sub-tick rounding remainder (< N ticks) is absorbed deterministically into `treasury_dust_fp`
    /// so `Sum(cash_deltas) + treasury_dust_fp == 0` holds bit-exactly!
    pub fn settle_zero_sum_funding(
        mark_price_fp: i128,
        funding_rate_ppm: i128,
        signed_perp_qtys_fp: &[i128],
    ) -> (Vec<i128>, i128) {
        let mut cash_deltas = Vec::with_capacity(signed_perp_qtys_fp.len());
        let mut sum_deltas: i128 = 0;

        for &qty_fp in signed_perp_qtys_fp {
            // Position signed notional = (qty_fp * mark_price_fp) / 1e8
            let signed_notional_fp = mul_fp_trunc(qty_fp, mark_price_fp);
            // Cash delta = - round_toward_zero(signed_notional_fp * funding_rate_ppm / 1e6)
            let payment_fp = -apply_ppm_trunc(signed_notional_fp, funding_rate_ppm);
            cash_deltas.push(payment_fp);
            sum_deltas += payment_fp;
        }
        // Treasury/Insurance rounding dust offset ensures exact global conservation Sum == 0!
        let treasury_dust_fp = -sum_deltas;
        (cash_deltas, treasury_dust_fp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_median_index_outlier_rejection_and_ema_basis() {
        let mut engine = PerpetualMarkPriceEngine::new("BTC-USDT-PERP", 200_000, 5_000_000_000, 50_000, 100_000);
        let now = 10_000_000_000_u64;
        let feeds = [
            SpotVenueFeed { venue_id: 1, price_fp: 70_000 * FP_SCALE, quote_ts_ns: now - 100 },
            SpotVenueFeed { venue_id: 2, price_fp: 70_020 * FP_SCALE, quote_ts_ns: now - 200 },
            SpotVenueFeed { venue_id: 3, price_fp: 69_990 * FP_SCALE, quote_ts_ns: now - 150 },
            // Flash crash / corrupt single exchange outlier (-30%) -> rejected by 5% outlier collar!
            SpotVenueFeed { venue_id: 4, price_fp: 49_000 * FP_SCALE, quote_ts_ns: now - 50 },
            // Stale feed (10s old > 5s limit) -> rejected!
            SpotVenueFeed { venue_id: 5, price_fp: 80_000 * FP_SCALE, quote_ts_ns: now - 9_000_000_000 },
        ];

        let snap = engine
            .update_mark_price(&feeds, 70_050 * FP_SCALE, 70_070 * FP_SCALE, now)
            .unwrap();
        assert_eq!(snap.valid_constituents, 3);
        assert_eq!(snap.index_price_fp, 70_000 * FP_SCALE);
        // Perp mid = 70,060 -> raw basis = +60 -> initial EMA = +60 -> fair_ema = 70,060 inside [70,050, 70,070]
        assert_eq!(snap.mark_price_fp, 70_060 * FP_SCALE);

        // Second tick tests EMA smoothing transition (alpha = 20% = 200_000 ppm):
        // New perp mid = 70,160 -> raw basis = +160 -> delta = +100 -> EMA = 60 + 20 = +80!
        // Book best_bid=70,150, best_ask=70,170 -> median(70150, 70170, 70080) = 70,150!
        let snap2 = engine
            .update_mark_price(&feeds, 70_150 * FP_SCALE, 70_170 * FP_SCALE, now + 100)
            .unwrap();
        assert_eq!(snap2.ema_basis_fp, 80 * FP_SCALE);
        assert_eq!(snap2.mark_price_fp, 70_150 * FP_SCALE);

        // Even number of valid feeds tests exact even median averaging: (69,990 + 70,010)/2 = 70,000
        let even_feeds = [
            SpotVenueFeed { venue_id: 1, price_fp: 69_990 * FP_SCALE, quote_ts_ns: now },
            SpotVenueFeed { venue_id: 2, price_fp: 70_010 * FP_SCALE, quote_ts_ns: now },
        ];
        let (med_even, cnt) = engine.compute_index_median_fp(&even_feeds, now).unwrap();
        assert_eq!(cnt, 2);
        assert_eq!(med_even, 70_000 * FP_SCALE);

        // Empty or all-stale feeds return None
        assert!(engine.compute_index_median_fp(&[], now).is_none());
    }

    #[test]
    fn test_dynamic_clamped_funding_and_zero_sum_settlement() {
        let calc = DynamicFundingCalculator::default();
        let idx = 50_000 * FP_SCALE;
        // Multi-level L2 book walk test (impact notional = $25,000 -> 0.2 BTC @ 50,000 = $10k + 0.3 BTC @ 50,500 = $15,150)
        let multi_asks = [
            BookPriceLevelFp { price_fp: 50_000 * FP_SCALE, qty_fp: 20_000_000 }, // 0.2 BTC = $10k
            BookPriceLevelFp { price_fp: 50_500 * FP_SCALE, qty_fp: 50_000_000 }, // 0.5 BTC
        ];
        let impact_ask = calc.compute_impact_price_fp(&multi_asks, idx);
        // 0.2 BTC + (15_000 / 50_500 = 0.29702970 BTC) = 0.49702970 BTC -> VWAP ≈ 50,298.80
        assert!(impact_ask > 50_290 * FP_SCALE && impact_ask < 50_310 * FP_SCALE);

        // Negative discount funding test (asks way below index -> clamped at -7,500 ppm!)
        let crash_bids = [BookPriceLevelFp { price_fp: 48_500 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
        let crash_asks = [BookPriceLevelFp { price_fp: 48_600 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
        let out_neg = calc.calculate_funding_rate(idx, &crash_bids, &crash_asks, 20_000);
        assert_eq!(out_neg.funding_rate_ppm, -7_500);

        // Neutral zone damper test: premium = 0 -> funding_rate == base_interest_rate_ppm (+100 ppm)!
        let flat_bids = [BookPriceLevelFp { price_fp: 50_000 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
        let flat_asks = [BookPriceLevelFp { price_fp: 50_000 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
        let out_flat = calc.calculate_funding_rate(idx, &flat_bids, &flat_asks, 20_000);
        assert_eq!(out_flat.premium_index_ppm, 0);
        assert_eq!(out_flat.funding_rate_ppm, 100);

        // Extreme perp premium (+2% above index) -> must be clamped at +7,500 ppm (+0.75%)!
        let bids = [BookPriceLevelFp { price_fp: 51_000 * FP_SCALE, qty_fp: 10 * FP_SCALE }];
        let asks = [BookPriceLevelFp { price_fp: 51_020 * FP_SCALE, qty_fp: 10 * FP_SCALE }];

        let out = calc.calculate_funding_rate(idx, &bids, &asks, 20_000);
        assert_eq!(out.funding_rate_ppm, 7_500);

        // Tighter MM ratio (0.60% = 6,000 ppm) -> dynamic cap = 75% * 6,000 = 4,500 ppm!
        let out_tight = calc.calculate_funding_rate(idx, &bids, &asks, 6_000);
        assert_eq!(out_tight.effective_cap_ppm, 4_500);
        assert_eq!(out_tight.funding_rate_ppm, 4_500);

        // Zero-sum settlement across matched book (+3 BTC, +2 BTC, -5 BTC)
        let qtys = [3 * FP_SCALE, 2 * FP_SCALE, -5 * FP_SCALE];
        let (deltas, dust) = DynamicFundingCalculator::settle_zero_sum_funding(idx, 4_500, &qtys);
        assert!(deltas[0] < 0 && deltas[1] < 0 && deltas[2] > 0); // Longs pay Shorts!
        let total: i128 = deltas.iter().sum::<i128>() + dust;
        assert_eq!(total, 0);
        assert_eq!(dust, 0);
    }
}
