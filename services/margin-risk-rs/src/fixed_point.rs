//! Hyperion deterministic fixed-point arithmetic (`1e8` atomic ticks, `1e6` ppm, `1e4` bps).
//!
//! Enforces strict integer accounting at all ledger, margin, funding, insurance fund,
//! and ADL settlement boundaries so zero systemic value is lost to IEEE-754 rounding.

use serde::{Deserialize, Serialize};

/// Canonical Hyperion monetary & quantity scale: `1.00000000` = `100_000_000` atomic ticks.
pub const FP_SCALE: i128 = 100_000_000;
pub const FP_SCALE_F64: f64 = 100_000_000.0;
pub const INV_FP_SCALE_F64: f64 = 1.0e-8;

/// Parts-per-million scale (`1.0` = `1_000_000` ppm).
pub const PPM_SCALE: i128 = 1_000_000;
pub const PPM_SCALE_F64: f64 = 1_000_000.0;

/// Basis points scale (`1.0` = `10_000` bps, `1 bps` = `0.01%`).
pub const BPS_SCALE: i128 = 10_000;
pub const BPS_SCALE_F64: f64 = 10_000.0;

/// Two's-complement 128-bit fixed-point split representation matching `hyperion.v1.I128`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireI128 {
    pub hi: i64,
    pub lo: u64,
}

impl WireI128 {
    #[inline]
    pub const fn from_i128(val: i128) -> Self {
        Self {
            hi: (val >> 64) as i64,
            lo: val as u64,
        }
    }

    #[inline]
    pub const fn to_i128(self) -> i128 {
        ((self.hi as i128) << 64) | (self.lo as i128)
    }
}

/// Unsigned 128-bit fixed-point split representation matching `hyperion.v1.U128`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WireU128 {
    pub hi: u64,
    pub lo: u64,
}

impl WireU128 {
    #[inline]
    pub const fn from_u128(val: u128) -> Self {
        Self {
            hi: (val >> 64) as u64,
            lo: val as u64,
        }
    }

    #[inline]
    pub const fn to_u128(self) -> u128 {
        ((self.hi as u128) << 64) | (self.lo as u128)
    }
}

/// Convert `f64` scalar to nearest fixed-point tick (`1e8`).
#[inline]
pub fn f64_to_fp_nearest(x: f64) -> i128 {
    if !x.is_finite() {
        return 0;
    }
    (x * FP_SCALE_F64).round() as i128
}

/// Conservative conversion for margin requirements / obligations: rounds UP (`ceil`) to nearest tick.
#[inline]
pub fn f64_to_fp_ceil(x: f64) -> i128 {
    if !x.is_finite() || x <= 0.0 {
        return 0;
    }
    (x * FP_SCALE_F64).ceil() as i128
}

/// Conservative conversion for mark-to-market collateral value: rounds DOWN (`floor`) to nearest tick.
#[inline]
pub fn f64_to_fp_floor(x: f64) -> i128 {
    if !x.is_finite() {
        return 0;
    }
    (x * FP_SCALE_F64).floor() as i128
}

/// Convert fixed-point tick (`1e8`) to `f64` for vectorized pricing kernels.
#[inline]
pub fn fp_to_f64(ticks: i128) -> f64 {
    (ticks as f64) * INV_FP_SCALE_F64
}

/// Exact fixed-point notional `(price_fp * qty_fp) / 1e8` truncated toward zero.
#[inline]
pub const fn mul_fp_trunc(a_fp: i128, b_fp: i128) -> i128 {
    a_fp.saturating_mul(b_fp) / FP_SCALE
}

/// Exact fixed-point notional `(price_fp * qty_fp)` with floor division (conservative for equity/PnL).
#[inline]
pub const fn mul_fp_floor(a_fp: i128, b_fp: i128) -> i128 {
    let prod = a_fp.saturating_mul(b_fp);
    let q = prod / FP_SCALE;
    let r = prod % FP_SCALE;
    if r < 0 {
        q - 1
    } else {
        q
    }
}

/// Exact fixed-point division `(num_fp * 1e8) / den_fp`.
#[inline]
pub const fn div_fp_trunc(num_fp: i128, den_fp: i128) -> i128 {
    if den_fp == 0 {
        return 0;
    }
    num_fp.saturating_mul(FP_SCALE) / den_fp
}

/// Apply parts-per-million rate `(amount_fp * rate_ppm) / 1_000_000`.
#[inline]
pub const fn apply_ppm_trunc(amount_fp: i128, rate_ppm: i128) -> i128 {
    amount_fp.saturating_mul(rate_ppm) / PPM_SCALE
}

/// Apply basis-points rate `(amount_fp * rate_bps) / 10_000` rounding up (conservative margin).
#[inline]
pub const fn apply_bps_ceil_nonneg(amount_fp: i128, rate_bps: i128) -> i128 {
    if amount_fp <= 0 || rate_bps <= 0 {
        return 0;
    }
    let prod = amount_fp.saturating_mul(rate_bps);
    (prod + BPS_SCALE - 1) / BPS_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wire_i128_roundtrip() {
        for v in [
            0_i128,
            1,
            -1,
            FP_SCALE,
            -FP_SCALE * 250_000,
            i64::MAX as i128 + 999_999,
            -(i64::MAX as i128) - 999_999,
        ] {
            let w = WireI128::from_i128(v);
            assert_eq!(w.to_i128(), v);
        }
    }

    #[test]
    fn test_conservative_rounding() {
        assert_eq!(f64_to_fp_ceil(1.000000001), 100_000_001);
        assert_eq!(f64_to_fp_ceil(0.0), 0);
        assert_eq!(f64_to_fp_ceil(-2.5), 0);
        assert_eq!(f64_to_fp_ceil(f64::NAN), 0);
        assert_eq!(f64_to_fp_ceil(f64::INFINITY), 0);

        assert_eq!(f64_to_fp_floor(1.000000009), 100_000_000);
        assert_eq!(f64_to_fp_floor(-1.000000001), -100_000_001);
        assert_eq!(f64_to_fp_floor(f64::NAN), 0);
        assert_eq!(f64_to_fp_nearest(2.5), 250_000_000);
        assert_eq!(f64_to_fp_nearest(f64::NAN), 0);
        assert!((fp_to_f64(250_000_000) - 2.5).abs() < 1e-15);

        assert_eq!(mul_fp_trunc(3 * FP_SCALE, 2 * FP_SCALE), 6 * FP_SCALE);
        assert_eq!(mul_fp_trunc(-100_000_001, 50_000_000), -50_000_000);
        assert_eq!(mul_fp_floor(-100_000_001, 50_000_000), -50_000_001);
        assert_eq!(mul_fp_floor(100_000_001, 50_000_000), 50_000_000);

        assert_eq!(div_fp_trunc(6 * FP_SCALE, 2 * FP_SCALE), 3 * FP_SCALE);
        assert_eq!(div_fp_trunc(6 * FP_SCALE, 0), 0);

        assert_eq!(apply_ppm_trunc(1_000 * FP_SCALE, 2_500), 250_000_000); // 0.25% of 1000 = 2.5
        assert_eq!(apply_ppm_trunc(1_000 * FP_SCALE, -2_500), -250_000_000);

        // Ceil bps boundary tests (exact multiple vs +1 tick remainder!)
        assert_eq!(apply_bps_ceil_nonneg(10_000, 1), 1);
        assert_eq!(apply_bps_ceil_nonneg(10_001, 1), 2);
        assert_eq!(apply_bps_ceil_nonneg(0, 500), 0);
        assert_eq!(apply_bps_ceil_nonneg(-10_000, 500), 0);
        assert_eq!(apply_bps_ceil_nonneg(10_000, 0), 0);
        assert_eq!(apply_bps_ceil_nonneg(10_000, -5), 0);

        // WireU128 roundtrip
        for u in [0_u128, 1, u64::MAX as u128, (1_u128 << 90) + 77, u128::MAX] {
            assert_eq!(WireU128::from_u128(u).to_u128(), u);
        }
    }
}
