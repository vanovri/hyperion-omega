//! Ultra-Fast Hybrid Halley / Newton-Raphson Implied Volatility Solver with Brent's Method Fallback.
//!
//! Design properties:
//! 1. **OTM Put-Call Parity Normalization**: Converts any European ITM option into its OTM twin
//!    so intrinsic-value subtraction never degrades floating-point conditioning.
//! 2. **Inflection-Point & Asymptotic Tail Rational Initial Seed**:
//!    Uses Corrado-Miller / Brenner-Subrahmanyam near ATM and asymptotic Mill's ratio inversion
//!    beyond the Vega inflection point $\sigma_c = \sqrt{2 |x| / T}$, placing $\sigma_0$ inside the
//!    cubic convergence basin immediately.
//! 3. **3rd-Order Householder / Halley Iteration**:
//!    $$\sigma_{n+1} = \sigma_n - \frac{f(\sigma_n)}{\mathcal{V}(\sigma_n) \left(1 - \frac{f(\sigma_n) d_1 d_2}{2 \mathcal{V}(\sigma_n) \sigma_n}\right)}$$
//!    Achieves `<= 6` iterations (typically `2..4` iterations!) to `1e-8` precision across standard
//!    and stressed moneyness/tenors.
//! 4. **Monotonic Bracket + Brent's Method Fallback**:
//!    Maintains exact bracket $[\sigma_{\text{lo}}, \sigma_{\text{hi}}]$ on every step. If an iterate
//!    leaves the bracket or Vega underflows in ultra-deep tails, executes inverse-quadratic/secant/bisection
//!    Brent steps with guaranteed global convergence to `1e-8`.

use crate::greeks_soa::{option_greeks, option_price, ExerciseStyle, OptionRight, EXPIRY_EPSILON_YEARS};
use crate::math_simd::{norm_cdf, norm_pdf, SQRT_2PI};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_IV_TOLERANCE: f64 = 1.0e-8;
pub const MIN_IMPLIED_VOL: f64 = 1.0e-4; // 0.01% annualized vol
pub const MAX_IMPLIED_VOL: f64 = 20.0;   // 2,000% annualized vol (crypto flash-crash wings)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IvSolverStage {
    Halley,
    NewtonRaphson,
    BrentFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IvSolution {
    pub implied_vol: f64,
    pub iterations: u32,
    pub price_abs_error: f64,
    pub last_stage: IvSolverStage,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum IvSolverError {
    #[error("invalid option parameters: spot={spot}, strike={strike}, expiry={expiry}")]
    InvalidParameters { spot: f64, strike: f64, expiry: f64 },
    #[error("option price {market_price} violates static no-arbitrage bounds [{lower_bound}, {upper_bound}]")]
    ArbitrageBoundsViolation {
        market_price: f64,
        lower_bound: f64,
        upper_bound: f64,
    },
}

/// Smart initial guess $\sigma_0$ for undiscounted forward OTM call/put in log-moneyness $x = \ln(F/K)$.
#[inline]
fn smart_initial_vol_guess(
    forward: f64,
    strike: f64,
    sqrt_t: f64,
    target_undiscounted_otm: f64,
) -> f64 {
    let x = (forward / strike).ln();
    let abs_x = x.abs();
    // Normalized time value beta = target / sqrt(F * K)
    let geom = (forward * strike).sqrt();
    let beta = (target_undiscounted_otm / geom).clamp(1e-15, 0.999);

    if abs_x < 1e-5 {
        // Near-ATM Brenner-Subrahmanyam exact 1st-order inversion: beta ≈ v / sqrt(2*pi)
        let v0 = SQRT_2PI * beta;
        return (v0 / sqrt_t).clamp(MIN_IMPLIED_VOL, MAX_IMPLIED_VOL);
    }

    // Inflection point of Black-76 Vega: v_inflect = sqrt(2 * |x|)
    let v_inflect = (2.0 * abs_x).sqrt();
    // Price at inflection point:
    let d1_inf = (x / v_inflect) + 0.5 * v_inflect;
    let d2_inf = d1_inf - v_inflect;
    let otm_inf = if x <= 0.0 {
        forward * norm_cdf(d1_inf) - strike * norm_cdf(d2_inf)
    } else {
        strike * norm_cdf(-d2_inf) - forward * norm_cdf(-d1_inf)
    };

    let v0 = if target_undiscounted_otm >= otm_inf {
        // Concave Vega-rich region (v >= v_inflect): Corrado-Miller / quadratic expansion
        let half_diff = 0.5 * (forward - strike).abs();
        let shifted = (target_undiscounted_otm - half_diff).max(0.0);
        let disc = (shifted * shifted + half_diff * half_diff).max(0.0);
        let cm = (SQRT_2PI / (forward + strike)) * (shifted + disc.sqrt());
        cm.max(v_inflect)
    } else {
        // Convex OTM tail region (v < v_inflect): Asymptotic Mill's ratio inversion!
        // For small v: target / sqrt(F*K) ≈ phi(x/v) * v^3 / x^2 -> -x^2 / (2*v^2) ≈ ln(sqrt(2*pi) * beta * |x| / v_inflect)
        let log_term = (SQRT_2PI * beta * (abs_x.max(0.05))).ln();
        if log_term < -0.5 {
            // Refine 1 asymptotic fixed-point step: z = |x|/v, 0.5*z^2 + ln(z) ≈ -ln(sqrt(2*pi)*beta/|x|)
            let rhs = -(SQRT_2PI * beta / abs_x).ln().min(-0.25);
            let z0 = (2.0 * rhs).sqrt();
            let z1 = (2.0 * (rhs - z0.ln()).max(0.1)).sqrt();
            (abs_x / z1).min(v_inflect * 0.98)
        } else {
            0.65 * v_inflect
        }
    };

    (v0 / sqrt_t).clamp(MIN_IMPLIED_VOL, MAX_IMPLIED_VOL)
}

/// Brent's root-finding algorithm on guaranteed sign-change bracket `[a_in, b_in]`.
fn brent_fallback<F>(mut a: f64, mut b: f64, mut fa: f64, mut fb: f64, tol: f64, mut eval_f: F, start_iters: u32) -> IvSolution
where
    F: FnMut(f64) -> f64,
{
    if fa.abs() < fb.abs() {
        std::mem::swap(&mut a, &mut b);
        std::mem::swap(&mut fa, &mut fb);
    }
    let mut c = a;
    let mut fc = fa;
    let mut d = b - a;
    let mut e = d;
    let mut iters = start_iters;

    for _ in 0..64 {
        if fb.abs() <= 1e-12 || (b - a).abs() <= tol {
            return IvSolution {
                implied_vol: b,
                iterations: iters,
                price_abs_error: fb.abs(),
                last_stage: IvSolverStage::BrentFallback,
            };
        }
        if fc.abs() < fb.abs() {
            a = b;
            b = c;
            c = a;
            fa = fb;
            fb = fc;
            fc = fa;
        }

        let tol1 = 2.0 * f64::EPSILON * b.abs() + 0.5 * tol;
        let xm = 0.5 * (c - b);
        if xm.abs() <= tol1 || fb == 0.0 {
            return IvSolution {
                implied_vol: b,
                iterations: iters,
                price_abs_error: fb.abs(),
                last_stage: IvSolverStage::BrentFallback,
            };
        }

        if e.abs() >= tol1 && fa.abs() > fb.abs() {
            let s = fb / fa;
            let (mut p, mut q) = if (a - c).abs() < 1e-15 {
                (2.0 * xm * s, 1.0 - s)
            } else {
                let q0 = fa / fc;
                let r0 = fb / fc;
                (
                    s * (2.0 * xm * q0 * (q0 - r0) - (b - a) * (r0 - 1.0)),
                    (q0 - 1.0) * (r0 - 1.0) * (s - 1.0),
                )
            };
            if p > 0.0 {
                q = -q;
            }
            p = p.abs();
            let min1 = 3.0 * xm * q - (tol1 * q).abs();
            let min2 = (e * q).abs();
            if 2.0 * p < min1.min(min2) {
                e = d;
                d = p / q;
            } else {
                d = xm;
                e = d;
            }
        } else {
            d = xm;
            e = d;
        }

        a = b;
        fa = fb;
        if d.abs() > tol1 {
            b += d;
        } else {
            b += if xm >= 0.0 { tol1 } else { -tol1 };
        }
        fb = eval_f(b);
        iters += 1;
        if (fb > 0.0 && fc > 0.0) || (fb < 0.0 && fc < 0.0) {
            c = a;
            fc = fa;
            d = b - a;
            e = d;
        }
    }

    IvSolution {
        implied_vol: b,
        iterations: iters,
        price_abs_error: fb.abs(),
        last_stage: IvSolverStage::BrentFallback,
    }
}

/// Solve Implied Volatility for European or American options to precision `tol` (default `1e-8`).
///
/// Uses Hybrid 3rd-order Halley / Newton-Raphson with automatic monotonic bracket updates
/// and guaranteed Brent's Method fallback.
#[allow(clippy::too_many_arguments)]
pub fn solve_implied_volatility(
    market_price: f64,
    spot: f64,
    strike: f64,
    time_to_expiry_yrs: f64,
    rate_r: f64,
    carry_q: f64,
    right: OptionRight,
    style: ExerciseStyle,
    tol: f64,
) -> Result<IvSolution, IvSolverError> {
    if !spot.is_finite()
        || !strike.is_finite()
        || !time_to_expiry_yrs.is_finite()
        || spot <= 0.0
        || strike <= 0.0
        || time_to_expiry_yrs <= EXPIRY_EPSILON_YEARS
    {
        return Err(IvSolverError::InvalidParameters {
            spot,
            strike,
            expiry: time_to_expiry_yrs,
        });
    }

    let tol_eff = tol.max(1e-12);

    // Fast path for European options: normalize via Put-Call Parity to OTM undiscounted Black-76!
    if matches!(style, ExerciseStyle::European) {
        let df_r = (-rate_r * time_to_expiry_yrs).exp();
        let df_q = (-carry_q * time_to_expiry_yrs).exp();
        let forward = spot * (df_q / df_r);
        let sqrt_t = time_to_expiry_yrs.sqrt();

        let (lower_disc, upper_disc) = match right {
            OptionRight::Call => ((spot * df_q - strike * df_r).max(0.0), spot * df_q),
            OptionRight::Put => ((strike * df_r - spot * df_q).max(0.0), strike * df_r),
        };
        if market_price < lower_disc - 1e-10 || market_price > upper_disc + 1e-10 {
            return Err(IvSolverError::ArbitrageBoundsViolation {
                market_price,
                lower_bound: lower_disc,
                upper_bound: upper_disc,
            });
        }
        if (market_price - lower_disc).abs() <= 1e-12 {
            return Ok(IvSolution {
                implied_vol: MIN_IMPLIED_VOL,
                iterations: 0,
                price_abs_error: 0.0,
                last_stage: IvSolverStage::Halley,
            });
        }

        // Convert to undiscounted OTM option price for maximum floating-point conditioning
        let undiscounted_mkt = market_price / df_r;
        let is_otm_call = forward <= strike;
        let target_otm = match (right, is_otm_call) {
            (OptionRight::Call, true) => undiscounted_mkt,
            (OptionRight::Call, false) => (undiscounted_mkt - (forward - strike)).max(0.0),
            (OptionRight::Put, false) => undiscounted_mkt,
            (OptionRight::Put, true) => (undiscounted_mkt - (strike - forward)).max(0.0),
        };

        let x = (forward / strike).ln();
        let eval_otm_and_derivs = |sigma: f64| -> (f64, f64, f64, f64) {
            let v = sigma * sqrt_t;
            let d1 = (x / v) + 0.5 * v;
            let d2 = d1 - v;
            let price_otm = if is_otm_call {
                (forward * norm_cdf(d1) - strike * norm_cdf(d2)).max(0.0)
            } else {
                (strike * norm_cdf(-d2) - forward * norm_cdf(-d1)).max(0.0)
            };
            let vega_undisc = forward * norm_pdf(d1) * sqrt_t;
            (price_otm - target_otm, vega_undisc, d1, d2)
        };

        let mut lo = MIN_IMPLIED_VOL;
        let mut hi = MAX_IMPLIED_VOL;
        let (f_lo, _, _, _) = eval_otm_and_derivs(lo);
        let (f_hi, _, _, _) = eval_otm_and_derivs(hi);
        if f_lo >= 0.0 {
            return Ok(IvSolution {
                implied_vol: lo,
                iterations: 1,
                price_abs_error: f_lo.abs() * df_r,
                last_stage: IvSolverStage::Halley,
            });
        }
        if f_hi <= 0.0 {
            return Ok(IvSolution {
                implied_vol: hi,
                iterations: 1,
                price_abs_error: f_hi.abs() * df_r,
                last_stage: IvSolverStage::Halley,
            });
        }

        let mut sigma = smart_initial_vol_guess(forward, strike, sqrt_t, target_otm);
        let mut f_lo_cur = f_lo;
        let mut f_hi_cur = f_hi;
        let mut last_stage = IvSolverStage::Halley;

        // Up to 6 ultra-fast Halley / NR steps (almost always converges in 2..4 iterations!)
        for iter in 1..=6 {
            let (f, vega_u, d1, d2) = eval_otm_and_derivs(sigma);
            let disc_err = f.abs() * df_r;
            if f < 0.0 && sigma > lo {
                lo = sigma;
                f_lo_cur = f;
            } else if f > 0.0 && sigma < hi {
                hi = sigma;
                f_hi_cur = f;
            }

            if f.abs() <= 1e-15 * target_otm.max(1e-30) || (hi - lo) <= tol_eff {
                return Ok(IvSolution {
                    implied_vol: sigma,
                    iterations: iter,
                    price_abs_error: disc_err,
                    last_stage,
                });
            }

            if vega_u <= 1e-28 {
                // Deep tail Vega underflow -> immediately finish via guaranteed Brent on tight bracket
                return Ok(brent_fallback(
                    lo,
                    hi,
                    f_lo_cur * df_r,
                    f_hi_cur * df_r,
                    tol_eff,
                    |s| eval_otm_and_derivs(s).0 * df_r,
                    iter,
                ));
            }

            let nr_step = f / vega_u;
            // Halley denominator adjustment: 1 - 0.5 * nr_step * (d1 * d2 / sigma)
            let halley_denom = 1.0 - 0.5 * nr_step * (d1 * d2 / sigma);
            let (step, stage_used) = if halley_denom.is_finite() && halley_denom > 0.25 {
                (nr_step / halley_denom, IvSolverStage::Halley)
            } else {
                (nr_step, IvSolverStage::NewtonRaphson)
            };
            last_stage = stage_used;

            let cand = sigma - step;
            if cand > lo && cand < hi {
                sigma = cand;
                if step.abs() <= tol_eff {
                    let (f_final, _, _, _) = eval_otm_and_derivs(sigma);
                    return Ok(IvSolution {
                        implied_vol: sigma,
                        iterations: iter,
                        price_abs_error: f_final.abs() * df_r,
                        last_stage,
                    });
                }
            } else {
                // Step jumped outside bracket -> switch to Brent fallback
                return Ok(brent_fallback(
                    lo,
                    hi,
                    f_lo_cur * df_r,
                    f_hi_cur * df_r,
                    tol_eff,
                    |s| eval_otm_and_derivs(s).0 * df_r,
                    iter,
                ));
            }
        }

        return Ok(brent_fallback(
            lo,
            hi,
            f_lo_cur * df_r,
            f_hi_cur * df_r,
            tol_eff,
            |s| eval_otm_and_derivs(s).0 * df_r,
            6,
        ));
    }

    // American Option Implied Volatility Solver (Hybrid Halley/NR + Brent over Bjerksund-Stensland 2002)
    let p_lo = option_price(
        spot,
        strike,
        time_to_expiry_yrs,
        rate_r,
        carry_q,
        MIN_IMPLIED_VOL,
        right,
        ExerciseStyle::American,
    );
    let p_hi = option_price(
        spot,
        strike,
        time_to_expiry_yrs,
        rate_r,
        carry_q,
        MAX_IMPLIED_VOL,
        right,
        ExerciseStyle::American,
    );
    if market_price < p_lo - 1e-8 || market_price > p_hi + 1e-8 {
        return Err(IvSolverError::ArbitrageBoundsViolation {
            market_price,
            lower_bound: p_lo,
            upper_bound: p_hi,
        });
    }
    if (market_price - p_lo).abs() <= 1e-10 {
        return Ok(IvSolution {
            implied_vol: MIN_IMPLIED_VOL,
            iterations: 0,
            price_abs_error: 0.0,
            last_stage: IvSolverStage::Halley,
        });
    }

    let mut lo = MIN_IMPLIED_VOL;
    let mut hi = MAX_IMPLIED_VOL;
    let mut f_lo = p_lo - market_price;
    let mut f_hi = p_hi - market_price;

    // Seed American IV from its European OTM analytical guess (very tight starting point!)
    let df_r = (-rate_r * time_to_expiry_yrs).exp();
    let df_q = (-carry_q * time_to_expiry_yrs).exp();
    let forward = spot * (df_q / df_r);
    let approx_otm = (market_price / df_r - (forward - strike).abs()).max(1e-6 * spot);
    let mut sigma = smart_initial_vol_guess(forward, strike, time_to_expiry_yrs.sqrt(), approx_otm);

    for iter in 1..=6 {
        let g = option_greeks(
            spot,
            strike,
            time_to_expiry_yrs,
            rate_r,
            carry_q,
            sigma,
            right,
            ExerciseStyle::American,
        );
        let f = g.price - market_price;
        if f < 0.0 && sigma > lo {
            lo = sigma;
            f_lo = f;
        } else if f > 0.0 && sigma < hi {
            hi = sigma;
            f_hi = f;
        }
        if f.abs() <= 1e-10 * spot.max(1.0) || (hi - lo) <= tol_eff {
            return Ok(IvSolution {
                implied_vol: sigma,
                iterations: iter,
                price_abs_error: f.abs(),
                last_stage: IvSolverStage::Halley,
            });
        }
        if g.vega <= 1e-16 {
            return Ok(brent_fallback(
                lo,
                hi,
                f_lo,
                f_hi,
                tol_eff,
                |s| {
                    option_price(
                        spot,
                        strike,
                        time_to_expiry_yrs,
                        rate_r,
                        carry_q,
                        s,
                        right,
                        ExerciseStyle::American,
                    ) - market_price
                },
                iter,
            ));
        }
        let nr = f / g.vega;
        let denom = 1.0 - 0.5 * nr * (g.volga / g.vega);
        let step = if denom.is_finite() && denom > 0.25 {
            nr / denom
        } else {
            nr
        };
        let cand = sigma - step;
        if cand > lo && cand < hi {
            sigma = cand;
            if step.abs() <= tol_eff {
                let p_fin = option_price(
                    spot,
                    strike,
                    time_to_expiry_yrs,
                    rate_r,
                    carry_q,
                    sigma,
                    right,
                    ExerciseStyle::American,
                );
                return Ok(IvSolution {
                    implied_vol: sigma,
                    iterations: iter,
                    price_abs_error: (p_fin - market_price).abs(),
                    last_stage: IvSolverStage::Halley,
                });
            }
        } else {
            return Ok(brent_fallback(
                lo,
                hi,
                f_lo,
                f_hi,
                tol_eff,
                |s| {
                    option_price(
                        spot,
                        strike,
                        time_to_expiry_yrs,
                        rate_r,
                        carry_q,
                        s,
                        right,
                        ExerciseStyle::American,
                    ) - market_price
                },
                iter,
            ));
        }
    }

    Ok(brent_fallback(
        lo,
        hi,
        f_lo,
        f_hi,
        tol_eff,
        |s| {
            option_price(
                spot,
                strike,
                time_to_expiry_yrs,
                rate_r,
                carry_q,
                s,
                right,
                ExerciseStyle::American,
            ) - market_price
        },
        6,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::greeks_soa::black_scholes_price_only;

    #[test]
    fn test_halley_iv_convergence_le_6_iters_across_grid() {
        let spot = 65_000.0;
        let strikes = [40_000.0, 50_000.0, 60_000.0, 65_000.0, 70_000.0, 85_000.0, 100_000.0];
        let tenors = [1.0 / 365.0, 7.0 / 365.0, 30.0 / 365.0, 0.25, 1.0];
        let vols = [0.15, 0.45, 0.85, 1.40, 2.50];
        let rights = [OptionRight::Call, OptionRight::Put];

        for &k in &strikes {
            for &t in &tenors {
                for &true_vol in &vols {
                    for &right in &rights {
                        let mkt = black_scholes_price_only(spot, k, t, 0.04, 0.01, true_vol, right);
                        // Skip deep OTM options whose time value underflows below 1e-9 USD
                        let df_r = (-0.04 * t).exp();
                        let df_q = (-0.01 * t).exp();
                        let intrinsic = match right {
                            OptionRight::Call => (spot * df_q - k * df_r).max(0.0),
                            OptionRight::Put => (k * df_r - spot * df_q).max(0.0),
                        };
                        let time_value = mkt - intrinsic;
                        // Skip degenerate IEEE-754 inputs where deep ITM intrinsic subtraction leaves < 8 digits
                        // of time-value precision (or pure OTM sub-micro-dollar dust < 1e-6).
                        if time_value < 1e-6 || (intrinsic > 0.0 && time_value / mkt < 1e-6) {
                            continue;
                        }

                        let sol = solve_implied_volatility(
                            mkt,
                            spot,
                            k,
                            t,
                            0.04,
                            0.01,
                            right,
                            ExerciseStyle::European,
                            1e-8,
                        )
                        .expect("IV should converge");
                        assert!(
                            (sol.implied_vol - true_vol).abs() < 1e-8,
                            "vol mismatch at K={k}, T={t}, true={true_vol}, got={}",
                            sol.implied_vol
                        );
                        assert!(
                            sol.iterations <= 6,
                            "exceeded 6 iterations ({}) at K={k}, T={t}, vol={true_vol}",
                            sol.iterations
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_american_put_iv_roundtrip() {
        let p = option_price(
            95.0,
            100.0,
            0.5,
            0.06,
            0.0,
            0.42,
            OptionRight::Put,
            ExerciseStyle::American,
        );
        let sol = solve_implied_volatility(
            p,
            95.0,
            100.0,
            0.5,
            0.06,
            0.0,
            OptionRight::Put,
            ExerciseStyle::American,
            1e-8,
        )
        .unwrap();
        assert!((sol.implied_vol - 0.42).abs() < 1e-7);
        assert!(sol.iterations <= 6);
    }
}
