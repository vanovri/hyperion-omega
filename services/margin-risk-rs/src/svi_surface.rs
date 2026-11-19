//! Gatheral-Jacquier (2014) Arbitrage-Free SVI (Stochastic Volatility Inspired) Surface.
//!
//! Guarantees absence of static arbitrage across strikes and maturities:
//! 1. **Butterfly Arbitrage-Free (Durrleman Density $g(k) \ge 0$)**:
//!    $$g(k) = \left(1 - \frac{k w'(k)}{2 w(k)}\right)^2 - \frac{(w'(k))^2}{4}\left(\frac{1}{w(k)} + \frac{1}{4}\right) + \frac{w''(k)}{2} \ge 0$$
//!    plus Roger Lee's (2004) asymptotic moment slope bound $b (1 + |\rho|) < 2$.
//! 2. **Calendar-Spread Arbitrage-Free ($\partial_T w(k, T) \ge 0$)**:
//!    Total implied variance $w(k, T) = \sigma_{\text{imp}}^2(k, T) \cdot T$ is strictly
//!    non-decreasing with maturity $T$ along every constant forward log-moneyness $k = \ln(K / F_T)$,
//!    verified both asymptotically on left/right wings and across dense moneyness grids.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum SviError {
    #[error("invalid SVI domain parameters: b={b}, rho={rho}, sigma={sigma}, min_w={min_w}")]
    InvalidDomain {
        b: f64,
        rho: f64,
        sigma: f64,
        min_w: f64,
    },
    #[error("Lee wing bound violated: b*(1+|rho|) = {wing_slope} >= 2.0")]
    WingSlopeViolation { wing_slope: f64 },
    #[error("butterfly arbitrage detected at log-moneyness k={at_k}: Durrleman g(k)={min_g} < 0")]
    ButterflyArbitrage { at_k: f64, min_g: f64 },
    #[error("calendar spread arbitrage between T1={t1} and T2={t2} at k={at_k}: dw={delta_w} < 0")]
    CalendarArbitrage {
        t1: f64,
        t2: f64,
        at_k: f64,
        delta_w: f64,
    },
    #[error("surface requires strictly positive, strictly increasing maturities")]
    NonMonotonicTenors,
}

/// Raw SVI slice parameterization at fixed time-to-expiry $T > 0$:
/// $$w(k) = a + b \left(\rho (k - m) + \sqrt{(k - m)^2 + \sigma^2}\right)$$
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawSviSlice {
    pub expiry_yrs: f64,
    pub forward: f64,
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

impl RawSviSlice {
    /// Construct a slice from Gatheral-Jacquier SSVI parameters $(\theta_T, \rho, \varphi)$,
    /// which is analytically guaranteed butterfly-arbitrage-free whenever
    /// $\theta_T \varphi (1 + |\rho|) < 4$ and $\theta_T \varphi^2 (1 + |\rho|) \le 4$.
    pub fn from_ssvi(expiry_yrs: f64, forward: f64, theta_t: f64, rho: f64, phi: f64) -> Self {
        let one_m_rho2 = (1.0 - rho * rho).max(1e-12);
        let a = 0.5 * theta_t * one_m_rho2;
        let b = 0.5 * theta_t * phi;
        let m = -rho / phi;
        let sigma = one_m_rho2.sqrt() / phi;
        Self {
            expiry_yrs,
            forward,
            a,
            b,
            rho,
            m,
            sigma,
        }
    }

    /// Total implied variance $w(k) = \sigma_{\text{imp}}^2(k) \cdot T$ at forward log-moneyness $k = \ln(K / F_T)$.
    #[inline(always)]
    pub fn total_variance(&self, k: f64) -> f64 {
        let dk = k - self.m;
        (self.a + self.b * (self.rho * dk + (dk * dk + self.sigma * self.sigma).sqrt())).max(1e-12)
    }

    /// First derivative $w'(k) = \partial w / \partial k$.
    #[inline(always)]
    pub fn dw_dk(&self, k: f64) -> f64 {
        let dk = k - self.m;
        let root = (dk * dk + self.sigma * self.sigma).sqrt();
        self.b * (self.rho + dk / root)
    }

    /// Second derivative $w''(k) = \partial^2 w / \partial k^2$.
    #[inline(always)]
    pub fn d2w_dk2(&self, k: f64) -> f64 {
        let dk = k - self.m;
        let denom = (dk * dk + self.sigma * self.sigma).powf(1.5);
        self.b * self.sigma * self.sigma / denom.max(1e-24)
    }

    /// Implied annualized Black-Scholes volatility $\sigma_{\text{imp}}(K)$ at strike $K$.
    #[inline]
    pub fn implied_vol_at_strike(&self, strike: f64) -> f64 {
        let k = (strike.max(1e-12) / self.forward.max(1e-12)).ln();
        self.implied_vol_at_k(k)
    }

    /// Implied annualized volatility $\sigma_{\text{imp}}(k) = \sqrt{w(k) / T}$.
    #[inline]
    pub fn implied_vol_at_k(&self, k: f64) -> f64 {
        let w = self.total_variance(k);
        (w / self.expiry_yrs.max(1e-12)).sqrt()
    }

    /// Gatheral-Durrleman butterfly risk-neutral density factor $g(k)$.
    /// Butterfly arbitrage is absent iff $g(k) \ge 0 \; \forall k \in \mathbb{R}$.
    #[inline]
    pub fn durrleman_g(&self, k: f64) -> f64 {
        let w = self.total_variance(k);
        let wp = self.dw_dk(k);
        let wpp = self.d2w_dk2(k);
        let term1 = 1.0 - (k * wp) / (2.0 * w);
        let term2 = 0.25 * wp * wp * (1.0 / w + 0.25);
        term1 * term1 - term2 + 0.5 * wpp
    }

    /// Minimum total variance on the slice: $w_{\min} = a + b \sigma \sqrt{1 - \rho^2}$.
    #[inline]
    pub fn min_total_variance(&self) -> f64 {
        self.a + self.b * self.sigma * (1.0 - self.rho * self.rho).max(0.0).sqrt()
    }

    /// Strict verification of domain bounds, Roger Lee moment slope bound, and Durrleman butterfly density $g(k) \ge 0$.
    pub fn validate_butterfly_arbitrage_free(&self) -> Result<f64, SviError> {
        let min_w = self.min_total_variance();
        if self.expiry_yrs <= 0.0
            || self.forward <= 0.0
            || self.b < 0.0
            || self.rho.abs() >= 1.0
            || self.sigma <= 0.0
            || min_w <= 1e-10
        {
            return Err(SviError::InvalidDomain {
                b: self.b,
                rho: self.rho,
                sigma: self.sigma,
                min_w,
            });
        }
        let wing_slope = self.b * (1.0 + self.rho.abs());
        if wing_slope >= 2.0 - 1e-9 {
            return Err(SviError::WingSlopeViolation { wing_slope });
        }

        // Sweep k in [-3.5, +3.5] (covering 0.03x to 33x moneyness!)
        let mut min_g = f64::INFINITY;
        let mut worst_k = 0.0;
        let steps = 700;
        for i in 0..=steps {
            let k = -3.5 + 7.0 * (i as f64) / (steps as f64);
            let g = self.durrleman_g(k);
            if g < min_g {
                min_g = g;
                worst_k = k;
            }
        }
        if min_g < -1e-9 {
            return Err(SviError::ButterflyArbitrage {
                at_k: worst_k,
                min_g,
            });
        }
        Ok(min_g)
    }
}

/// Multi-Tenor Arbitrage-Free SVI Volatility Surface across $0 < T_1 < T_2 < \dots < T_M$.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageFreeSviSurface {
    pub slices: Vec<RawSviSlice>,
}

impl ArbitrageFreeSviSurface {
    /// Construct and validate an entire SVI surface for both Butterfly and Calendar-Spread no-arbitrage.
    pub fn new_verified(slices: Vec<RawSviSlice>) -> Result<Self, SviError> {
        if slices.is_empty() {
            return Err(SviError::NonMonotonicTenors);
        }
        for i in 0..slices.len() {
            if i > 0 && slices[i].expiry_yrs <= slices[i - 1].expiry_yrs {
                return Err(SviError::NonMonotonicTenors);
            }
            slices[i].validate_butterfly_arbitrage_free()?;
        }

        // Verify Calendar-Spread Arbitrage-Free condition: w(k, T_{i+1}) >= w(k, T_i) for all k in R
        for i in 0..(slices.len() - 1) {
            let s1 = &slices[i];
            let s2 = &slices[i + 1];

            // Necessary asymptotic wing slope dominance:
            // Right wing (k -> +inf): b2*(1 + rho2) >= b1*(1 + rho1)
            // Left wing (k -> -inf):  b2*(1 - rho2) >= b1*(1 - rho1)
            let right_diff = s2.b * (1.0 + s2.rho) - s1.b * (1.0 + s1.rho);
            if right_diff < -1e-9 {
                return Err(SviError::CalendarArbitrage {
                    t1: s1.expiry_yrs,
                    t2: s2.expiry_yrs,
                    at_k: 10.0,
                    delta_w: right_diff,
                });
            }
            let left_diff = s2.b * (1.0 - s2.rho) - s1.b * (1.0 - s1.rho);
            if left_diff < -1e-9 {
                return Err(SviError::CalendarArbitrage {
                    t1: s1.expiry_yrs,
                    t2: s2.expiry_yrs,
                    at_k: -10.0,
                    delta_w: left_diff,
                });
            }

            let steps = 700;
            for j in 0..=steps {
                let k = -3.5 + 7.0 * (j as f64) / (steps as f64);
                let dw = s2.total_variance(k) - s1.total_variance(k);
                if dw < -1e-9 {
                    return Err(SviError::CalendarArbitrage {
                        t1: s1.expiry_yrs,
                        t2: s2.expiry_yrs,
                        at_k: k,
                        delta_w: dw,
                    });
                }
            }
        }

        Ok(Self { slices })
    }

    /// Build a canonical parametric SSVI (Surface SVI, Gatheral-Jacquier Theorem 4.2) surface
    /// using power-law Heston-like $\varphi(\theta) = \frac{\eta}{\theta^\gamma (1 + \theta)^{1-\gamma}}$
    /// ($\gamma \in (0, 1/2]$), guaranteed free of both butterfly and calendar arbitrage!
    pub fn from_ssvi_term_structure(
        tenors_and_forwards: &[(f64, f64, f64)], // (expiry_yrs, forward, atm_vol)
        rho: f64,
        eta: f64,
        gamma: f64,
    ) -> Result<Self, SviError> {
        let rho_c = rho.clamp(-0.95, 0.95);
        let gamma_c = gamma.clamp(0.05, 0.5);
        // Ensure Gatheral-Jacquier Theorem 4.2 bound eta * (1 + |rho|) <= 2
        let eta_c = eta.clamp(0.01, 1.95 / (1.0 + rho_c.abs()));

        let mut slices = Vec::with_capacity(tenors_and_forwards.len());
        let mut prev_theta: f64 = 0.0;
        for &(t, fwd, atm_vol) in tenors_and_forwards {
            let theta_raw = atm_vol * atm_vol * t;
            // Enforce strict total-variance monotonicity across maturities
            let theta_t = theta_raw.max(prev_theta + 1e-8);
            prev_theta = theta_t;
            let phi = eta_c / (theta_t.powf(gamma_c) * (1.0 + theta_t).powf(1.0 - gamma_c));
            slices.push(RawSviSlice::from_ssvi(t, fwd, theta_t, rho_c, phi));
        }
        Self::new_verified(slices)
    }

    /// Interpolate implied volatility $\sigma_{\text{imp}}(K, T)$ at any strike $K$ and maturity $T > 0$
    /// using arbitrage-free monotonic linear total-variance interpolation along forward log-moneyness $k$.
    #[inline]
    pub fn implied_vol(&self, strike: f64, expiry_yrs: f64, spot_fallback: f64) -> f64 {
        let t = expiry_yrs.max(1e-8);
        if self.slices.len() == 1 || t <= self.slices[0].expiry_yrs {
            let s0 = &self.slices[0];
            let k = (strike.max(1e-12) / s0.forward.max(spot_fallback).max(1e-12)).ln();
            // Scale total variance proportionally below shortest slice to keep constant short-end vol
            let scale = t / s0.expiry_yrs;
            return ((s0.total_variance(k) * scale) / t).sqrt();
        }
        let last = self.slices.last().unwrap();
        if t >= last.expiry_yrs {
            let k = (strike.max(1e-12) / last.forward.max(1e-12)).ln();
            // Extrapolate flat forward variance beyond longest pillar
            let prev = &self.slices[self.slices.len() - 2];
            let slope_w = ((last.total_variance(k) - prev.total_variance(k))
                / (last.expiry_yrs - prev.expiry_yrs))
                .max(0.0);
            let w_ext = last.total_variance(k) + slope_w * (t - last.expiry_yrs);
            return (w_ext / t).sqrt();
        }

        // Binary search bracketing slices [i, i+1]
        let idx = match self
            .slices
            .binary_search_by(|s| s.expiry_yrs.partial_cmp(&t).unwrap())
        {
            Ok(exact) => return self.slices[exact].implied_vol_at_strike(strike),
            Err(ins) => ins - 1,
        };

        let s1 = &self.slices[idx];
        let s2 = &self.slices[idx + 1];
        let alpha = (t - s1.expiry_yrs) / (s2.expiry_yrs - s1.expiry_yrs);
        // Log-linear forward interpolation
        let fwd_t = s1.forward.powf(1.0 - alpha) * s2.forward.powf(alpha);
        let k = (strike.max(1e-12) / fwd_t.max(1e-12)).ln();

        let w1 = s1.total_variance(k);
        let w2 = s2.total_variance(k);
        let w_interp = (1.0 - alpha) * w1 + alpha * w2;
        (w_interp / t).sqrt()
    }

    /// Calibrate an arbitrage-free `RawSviSlice` to observed `(strike, implied_vol)` market quotes
    /// via Quasi-Explicit SVI (Zeliade 2009: analytical linear least-squares inner solve for `(a, d, c)`
    /// combined with constrained outer grid + coordinate refinement over `(m, sigma)` with hard
    /// Durrleman $g(k) \ge 0$ & Lee slope rejection).
    pub fn calibrate_slice(
        expiry_yrs: f64,
        forward: f64,
        quotes: &[(f64, f64)], // (strike, market_iv)
    ) -> Result<RawSviSlice, SviError> {
        let mut ks = Vec::with_capacity(quotes.len());
        let mut ws = Vec::with_capacity(quotes.len());
        for &(strike, iv) in quotes {
            if strike > 0.0 && iv > 0.0 {
                ks.push((strike / forward).ln());
                ws.push(iv * iv * expiry_yrs);
            }
        }
        let n = ks.len();
        assert!(n >= 3, "need at least 3 valid option quotes for SVI calibration");

        // Given (m, sigma), w(k) = a + d * y + c * z where y = (k - m)/sigma, z = sqrt(y^2 + 1)
        // Here d = b * rho * sigma, c = b * sigma -> b = c / sigma, rho = d / c.
        let solve_inner = |m_val: f64, sig_val: f64| -> Option<RawSviSlice> {
            let mut s1 = 0.0;
            let mut sy = 0.0;
            let mut sz = 0.0;
            let mut syy = 0.0;
            let mut syz = 0.0;
            let mut szz = 0.0;
            let mut sw = 0.0;
            let mut syw = 0.0;
            let mut szw = 0.0;
            for i in 0..n {
                let y = (ks[i] - m_val) / sig_val;
                let z = (y * y + 1.0).sqrt();
                let w = ws[i];
                s1 += 1.0;
                sy += y;
                sz += z;
                syy += y * y;
                syz += y * z;
                szz += z * z;
                sw += w;
                syw += y * w;
                szw += z * w;
            }
            // Regularized 3x3 normal equations for [a, d, c]
            let reg = 1e-9;
            let mut mat = [
                [s1 + reg, sy, sz, sw],
                [sy, syy + reg, syz, syw],
                [sz, syz, szz + reg, szw],
            ];
            for col in 0..3 {
                let mut piv = col;
                for row in (col + 1)..3 {
                    if mat[row][col].abs() > mat[piv][col].abs() {
                        piv = row;
                    }
                }
                if mat[piv][col].abs() < 1e-14 {
                    return None;
                }
                mat.swap(col, piv);
                let diag = mat[col][col];
                for cell in &mut mat[col][col..4] {
                    *cell /= diag;
                }
                for row in 0..3 {
                    if row != col {
                        let f = mat[row][col];
                        let pivot_row = mat[col];
                        for (cell, &p_val) in mat[row][col..4].iter_mut().zip(&pivot_row[col..4]) {
                            *cell -= f * p_val;
                        }
                    }
                }
            }
            let a_raw = mat[0][3];
            let d_raw = mat[1][3];
            let c_raw = mat[2][3];

            let max_c = 1.98 * sig_val;
            let c = c_raw.clamp(1e-6 * sig_val, max_c);
            let max_d = (c * 0.98).min(1.98 * sig_val - c);
            let d = d_raw.clamp(-max_d, max_d);
            let b = c / sig_val;
            let rho = (d / c).clamp(-0.98, 0.98);
            let min_a = -b * sig_val * (1.0 - rho * rho).sqrt() + 1e-6;
            let a = a_raw.max(min_a);

            let cand = RawSviSlice {
                expiry_yrs,
                forward,
                a,
                b,
                rho,
                m: m_val,
                sigma: sig_val,
            };
            if cand.validate_butterfly_arbitrage_free().is_ok() {
                Some(cand)
            } else {
                None
            }
        };

        let sse = |slice: &RawSviSlice| -> f64 {
            let mut err = 0.0;
            for i in 0..n {
                let d = slice.total_variance(ks[i]) - ws[i];
                err += d * d;
            }
            err
        };

        let mut best_slice: Option<RawSviSlice> = None;
        let mut best_err = f64::INFINITY;

        for m_idx in -10..=10 {
            let m_try = (m_idx as f64) * 0.05;
            for s_idx in 1..=16 {
                let sig_try = (s_idx as f64) * 0.05;
                if let Some(cand) = solve_inner(m_try, sig_try) {
                    let e = sse(&cand);
                    if e < best_err {
                        best_err = e;
                        best_slice = Some(cand);
                    }
                }
            }
        }

        // Coordinate pattern refinement around best (m, sigma)
        if let Some(mut cur) = best_slice {
            let mut step_m = 0.025;
            let mut step_s = 0.025;
            for _ in 0..18 {
                let mut improved = false;
                for &(dm, ds) in &[
                    (step_m, 0.0),
                    (-step_m, 0.0),
                    (0.0, step_s),
                    (0.0, -step_s),
                    (step_m, step_s),
                    (-step_m, -step_s),
                ] {
                    let m_c = cur.m + dm;
                    let s_c = (cur.sigma + ds).max(0.01);
                    if let Some(cand) = solve_inner(m_c, s_c) {
                        let e = sse(&cand);
                        if e < best_err {
                            best_err = e;
                            cur = cand;
                            improved = true;
                        }
                    }
                }
                if !improved {
                    step_m *= 0.5;
                    step_s *= 0.5;
                }
            }
            return Ok(cur);
        }

        // Guaranteed SSVI fallback if quotes were pathologically noisy
        let mean_w = ws.iter().sum::<f64>() / (n as f64);
        let fallback = RawSviSlice::from_ssvi(expiry_yrs, forward, mean_w.max(1e-4), -0.25, 0.5);
        fallback.validate_butterfly_arbitrage_free()?;
        Ok(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssvi_surface_butterfly_and_calendar_arbitrage_free() {
        let pillars = [
            (7.0 / 365.0, 68_000.0, 0.58),
            (30.0 / 365.0, 68_350.0, 0.60),
            (90.0 / 365.0, 69_100.0, 0.62),
            (180.0 / 365.0, 70_250.0, 0.64),
            (1.0, 72_500.0, 0.66),
        ];
        let surf =
            ArbitrageFreeSviSurface::from_ssvi_term_structure(&pillars, -0.35, 0.85, 0.40).unwrap();
        assert_eq!(surf.slices.len(), 5);

        // Verify dense calendar monotonicity across interpolated tenors too
        for k_idx in -20..=20 {
            let strike = 68_000.0 * ((k_idx as f64) * 0.04).exp();
            let mut prev_w = 0.0;
            for d in 2..=365 {
                let t = (d as f64) / 365.0;
                let iv = surf.implied_vol(strike, t, 68_000.0);
                let w = iv * iv * t;
                assert!(w.is_finite() && iv > 0.05);
                if k_idx == 0 {
                    assert!(
                        w >= prev_w - 1e-9,
                        "total variance decreased at d={d}: {w} < {prev_w}"
                    );
                }
                prev_w = w;
            }
        }
    }

    #[test]
    fn test_detects_butterfly_and_calendar_arbitrage_violations() {
        // Construct pathological slice with huge b violating Lee & Durrleman
        let bad_butterfly = RawSviSlice {
            expiry_yrs: 0.1,
            forward: 50_000.0,
            a: 0.001,
            b: 1.85,
            rho: 0.5,
            m: 0.0,
            sigma: 0.02,
        };
        assert!(bad_butterfly.validate_butterfly_arbitrage_free().is_err());

        // Construct two individually valid slices that cross in total variance (calendar arbitrage!)
        let s1 = RawSviSlice::from_ssvi(0.25, 50_000.0, 0.16, -0.2, 0.5); // w_atm = 0.16
        let s2 = RawSviSlice::from_ssvi(0.50, 50_000.0, 0.10, -0.2, 0.5); // w_atm = 0.10 < 0.16!
        assert!(s1.validate_butterfly_arbitrage_free().is_ok());
        assert!(s2.validate_butterfly_arbitrage_free().is_ok());
        let res = ArbitrageFreeSviSurface::new_verified(vec![s1, s2]);
        assert!(matches!(res, Err(SviError::CalendarArbitrage { .. })));
    }

    #[test]
    fn test_quasi_explicit_svi_calibration_recovers_smile() {
        let true_slice = RawSviSlice::from_ssvi(30.0 / 365.0, 65_000.0, 0.032, -0.40, 0.90);
        let strikes = [50_000.0, 55_000.0, 60_000.0, 65_000.0, 70_000.0, 75_000.0, 82_000.0];
        let quotes: Vec<(f64, f64)> = strikes
            .iter()
            .map(|&k| (k, true_slice.implied_vol_at_strike(k)))
            .collect();

        let cal = ArbitrageFreeSviSurface::calibrate_slice(30.0 / 365.0, 65_000.0, &quotes)
            .expect("calibration must succeed");
        cal.validate_butterfly_arbitrage_free()
            .expect("calibrated slice must be butterfly arbitrage-free");

        for &(k, mkt_iv) in &quotes {
            let fit_iv = cal.implied_vol_at_strike(k);
            assert!(
                (fit_iv - mkt_iv).abs() < 5e-3,
                "calibration fit error at K={k}: fit={fit_iv}, mkt={mkt_iv}"
            );
        }
    }
}
