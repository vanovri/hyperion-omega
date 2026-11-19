//! SIMD-friendly 64-byte aligned blocks & high-precision normal distribution kernels.
//!
//! Uses Cody (1969) rational Chebyshev approximations for `erfc` / `norm_cdf`,
//! achieving `< 5e-16` relative accuracy across the entire IEEE-754 `f64` domain
//! while keeping hot-loop branches minimal for AVX2 / AVX-512 auto-vectorization.

use std::f64::consts::{FRAC_1_SQRT_2, PI};

/// `1 / sqrt(2 * pi)` with full IEEE-754 53-bit significand precision.
pub const INV_SQRT_2PI: f64 = 0.3989422804014326779399460599343818684758586311649346576659258296;
/// `sqrt(2 * pi)`
pub const SQRT_2PI: f64 = 2.506628274631000502415765284811045253006986740609938316629923576;
/// `1 / sqrt(pi)`
const INV_SQRT_PI: f64 = 0.5641895835477562869480794515607725858440506293289988568440857217;

/// 64-byte cache-line / AVX-512 aligned 8-lane `f64` SIMD tile (`64 bytes` = `8 * f64`).
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, Default)]
pub struct F64x8(pub [f64; 8]);

impl F64x8 {
    #[inline(always)]
    pub const fn splat(v: f64) -> Self {
        Self([v; 8])
    }
}

/// Standard normal probability density function $\phi(x) = \frac{1}{\sqrt{2\pi}} e^{-x^2/2}$.
/// Underflows cleanly to `0.0` for $|x| > 38.5$ without NaN.
#[inline(always)]
pub fn norm_pdf(x: f64) -> f64 {
    if x.abs() > 38.5 {
        return 0.0;
    }
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

#[allow(clippy::excessive_precision)]
/// Complementary error function `erfc(x)` using W. J. Cody's (1969) rational Chebyshev
/// approximation (*Mathematics of Computation*, 23(107), 631–637).
/// Accurate to ~1e-16 across the full `f64` domain.
#[inline(always)]
pub fn erfc_cody(x: f64) -> f64 {
    let y = x.abs();

    if y <= 0.46875 {
        // Region 1: |x| <= 0.46875 -> erfc(x) = 1 - erf(x)
        let z = y * y;
        const A: [f64; 5] = [
            3.16112374387056560e00,
            1.13864154151050156e02,
            3.77485237685302021e02,
            3.20937758913846947e03,
            1.85777706184603153e-01,
        ];
        const B: [f64; 4] = [
            2.36012909523441209e01,
            2.44024637934444173e02,
            1.28261652607737228e03,
            2.84423683343917062e03,
        ];
        let mut num = A[4] * z + A[0];
        num = num * z + A[1];
        num = num * z + A[2];
        num = num * z + A[3];

        let mut den = z + B[0];
        den = den * z + B[1];
        den = den * z + B[2];
        den = den * z + B[3];

        let erf_val = x * (num / den);
        return 1.0 - erf_val;
    }

    let result_pos = if y <= 4.0 {
        // Region 2: 0.46875 < |x| <= 4.0
        const C: [f64; 9] = [
            5.64188496988670089e-01,
            8.88314979438837594e00,
            6.61191906371416295e01,
            2.98635138197400131e02,
            8.81952221241769090e02,
            1.71204761263407058e03,
            2.05107837782607147e03,
            1.23033935479799725e03,
            2.15311535474403846e-08,
        ];
        const D: [f64; 8] = [
            1.57449261107098347e01,
            1.17693950891312499e02,
            5.37181101862009858e02,
            1.62138957456669019e03,
            3.29079923573345963e03,
            4.36261909014324716e03,
            3.43936767414372164e03,
            1.23033935480374942e03,
        ];
        let mut num = C[8] * y + C[0];
        for &coeff in &C[1..8] {
            num = num * y + coeff;
        }
        let mut den = y + D[0];
        for &coeff in &D[1..8] {
            den = den * y + coeff;
        }
        let y_sq = (y * 16.0).floor() / 16.0;
        let del = (y - y_sq) * (y + y_sq);
        (-y_sq * y_sq).exp() * (-del).exp() * (num / den)
    } else if y < 26.55 {
        // Region 3: 4.0 < |x| < 26.55
        const P: [f64; 6] = [
            3.05326634961232344e-01,
            3.60344899949804439e-01,
            1.25781726111229246e-01,
            1.60837851487422766e-02,
            6.58749161529837803e-04,
            1.63153871373020978e-02,
        ];
        const Q: [f64; 5] = [
            2.56852019228982242e00,
            1.87295284992346047e00,
            5.27905102951428412e-01,
            6.05183413124413191e-02,
            2.33520497626869185e-03,
        ];
        let z = 1.0 / (y * y);
        let mut num = P[5] * z + P[0];
        for &coeff in &P[1..5] {
            num = num * z + coeff;
        }
        let mut den = z + Q[0];
        for &coeff in &Q[1..5] {
            den = den * z + coeff;
        }
        let corr = INV_SQRT_PI - z * (num / den);
        let y_sq = (y * 16.0).floor() / 16.0;
        let del = (y - y_sq) * (y + y_sq);
        (-y_sq * y_sq).exp() * (-del).exp() * (corr / y)
    } else {
        0.0
    };

    if x < 0.0 {
        2.0 - result_pos
    } else {
        result_pos
    }
}

/// High-precision standard normal cumulative distribution function $\Phi(x) = \frac{1}{2} \text{erfc}(-x/\sqrt{2})$.
/// Accurate to `< 5e-16` across the full `f64` range.
#[inline(always)]
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc_cody(-x * FRAC_1_SQRT_2)
}

/// Fast SIMD-friendly Abramowitz-Stegun / Hart hybrid `norm_cdf_fast` for ultra-hot SPAN scenario grids
/// where `1e-9` precision is more than sufficient (single rational evaluation, zero inner loops).
#[inline(always)]
pub fn norm_cdf_fast(x: f64) -> f64 {
    // Hart 1968 / West 2009 branch-minimal rational approximation (~1e-15 accuracy, loop-free!)
    let abs_x = x.abs();
    if abs_x > 37.0 {
        return if x > 0.0 { 1.0 } else { 0.0 };
    }
    let exp_half_sq = (-0.5 * abs_x * abs_x).exp();
    let c = if abs_x < 7.07106781186547 {
        let n = (((((3.52624965998911e-2 * abs_x + 0.700383064443688) * abs_x
            + 6.37396220353165)
            * abs_x
            + 33.912866078383)
            * abs_x
            + 112.079291497871)
            * abs_x
            + 221.213596169931)
            * abs_x
            + 220.206867912376;
        let d = ((((((8.83883476483184e-2 * abs_x + 1.75566716318264) * abs_x
            + 16.064177579207)
            * abs_x
            + 86.7807322029461)
            * abs_x
            + 296.564248779674)
            * abs_x
            + 637.333633378831)
            * abs_x
            + 793.826512519948)
            * abs_x
            + 440.413735824752;
        exp_half_sq * n / d
    } else {
        let denom = abs_x
            + 1.0
                / (abs_x
                    + 2.0
                        / (abs_x
                            + 3.0 / (abs_x + 4.0 / (abs_x + 0.65))));
        exp_half_sq / (SQRT_2PI * denom)
    };
    if x > 0.0 {
        1.0 - c
    } else {
        c
    }
}

#[allow(clippy::excessive_precision)]
/// Inverse standard normal CDF $\Phi^{-1}(p)$ using Peter Acklam's rational approximation
/// refined by one Halley step to full IEEE-754 `1e-16` precision.
pub fn norm_inv_cdf(p: f64) -> f64 {
    let p_clamped = p.clamp(1e-300, 1.0 - 1e-16);
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];

    let p_low = 0.02425;
    let p_high = 1.0 - p_low;

    let x0 = if p_clamped < p_low {
        let q = (-2.0 * p_clamped.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p_clamped <= p_high {
        let q = p_clamped - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p_clamped).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };

    // One Halley refinement step -> ~1e-16 accuracy
    let err = norm_cdf(x0) - p_clamped;
    let pdf = norm_pdf(x0);
    if pdf > 1e-300 {
        let u = err / pdf;
        x0 - u / (1.0 + 0.5 * x0 * u)
    } else {
        x0
    }
}

/// Branch-minimal `norm_cdf` given precomputed `exp_half_sq = exp(-0.5 * x^2)`.
/// Enables exact 50% `exp()` elimination in Black-Scholes via the fundamental identity:
/// $$S e^{-qT} \phi(d_1) \equiv K e^{-rT} \phi(d_2) \implies e^{-\frac{1}{2} d_2^2} \equiv e^{-\frac{1}{2} d_1^2} \cdot \frac{S e^{-qT}}{K e^{-rT}}$$
#[inline(always)]
pub fn norm_cdf_from_exp(x: f64, exp_half_sq: f64) -> f64 {
    let abs_x = x.abs();
    let tail = if abs_x < 7.07106781186547 {
        let n = (((((3.52624965998911e-2 * abs_x + 0.700383064443688) * abs_x
            + 6.37396220353165)
            * abs_x
            + 33.912866078383)
            * abs_x
            + 112.079291497871)
            * abs_x
            + 221.213596169931)
            * abs_x
            + 220.206867912376;
        let d = ((((((8.83883476483184e-2 * abs_x + 1.75566716318264) * abs_x
            + 16.064177579207)
            * abs_x
            + 86.7807322029461)
            * abs_x
            + 296.564248779674)
            * abs_x
            + 637.333633378831)
            * abs_x
            + 793.826512519948)
            * abs_x
            + 440.413735824752;
        exp_half_sq * (n / d)
    } else if abs_x < 37.0 {
        let denom = abs_x
            + 1.0 / (abs_x + 2.0 / (abs_x + 3.0 / (abs_x + 4.0 / (abs_x + 0.65))));
        exp_half_sq / (SQRT_2PI * denom)
    } else {
        0.0
    };
    if x > 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/// Branch-minimal 8-lane SIMD `norm_cdf_x8_fast` for 64-byte aligned SPAN scenario tiles.
/// Uses Hart (1968) / West (2009) rational approximation (`< 1e-14` error) over `F64x8`.
#[inline(always)]
pub fn norm_cdf_x8_fast(x: &F64x8) -> F64x8 {
    let mut out = F64x8([0.0; 8]);
    for lane in 0..8 {
        let v = x.0[lane];
        let abs_v = v.abs();
        let exp_half_sq = (-0.5 * abs_v * abs_v).exp();
        let tail = if abs_v < 7.07106781186547 {
            let n = (((((3.52624965998911e-2 * abs_v + 0.700383064443688) * abs_v
                + 6.37396220353165)
                * abs_v
                + 33.912866078383)
                * abs_v
                + 112.079291497871)
                * abs_v
                + 221.213596169931)
                * abs_v
                + 220.206867912376;
            let d = ((((((8.83883476483184e-2 * abs_v + 1.75566716318264) * abs_v
                + 16.064177579207)
                * abs_v
                + 86.7807322029461)
                * abs_v
                + 296.564248779674)
                * abs_v
                + 637.333633378831)
                * abs_v
                + 793.826512519948)
                * abs_v
                + 440.413735824752;
            exp_half_sq * n / d
        } else if abs_v < 37.0 {
            let denom = abs_v
                + 1.0 / (abs_v + 2.0 / (abs_v + 3.0 / (abs_v + 4.0 / (abs_v + 0.65))));
            exp_half_sq / (SQRT_2PI * denom)
        } else {
            0.0
        };
        out.0[lane] = if v > 0.0 { 1.0 - tail } else { tail };
    }
    out
}

/// Evaluate `norm_cdf_fast` and `norm_pdf` across an 8-wide aligned SIMD tile.
#[inline(always)]
pub fn norm_cdf_pdf_x8(x: &F64x8) -> (F64x8, F64x8) {
    let mut cdfs = F64x8([0.0; 8]);
    let mut pdfs = F64x8([0.0; 8]);
    for lane in 0..8 {
        let v = x.0[lane];
        let abs_v = v.abs();
        if abs_v > 37.0 {
            cdfs.0[lane] = if v > 0.0 { 1.0 } else { 0.0 };
            pdfs.0[lane] = 0.0;
        } else {
            let exp_half_sq = (-0.5 * abs_v * abs_v).exp();
            pdfs.0[lane] = INV_SQRT_2PI * exp_half_sq;
            let tail = if abs_v < 7.07106781186547 {
                let n = (((((3.52624965998911e-2 * abs_v + 0.700383064443688) * abs_v
                    + 6.37396220353165)
                    * abs_v
                    + 33.912866078383)
                    * abs_v
                    + 112.079291497871)
                    * abs_v
                    + 221.213596169931)
                    * abs_v
                    + 220.206867912376;
                let d = ((((((8.83883476483184e-2 * abs_v + 1.75566716318264) * abs_v
                    + 16.064177579207)
                    * abs_v
                    + 86.7807322029461)
                    * abs_v
                    + 296.564248779674)
                    * abs_v
                    + 637.333633378831)
                    * abs_v
                    + 793.826512519948)
                    * abs_v
                    + 440.413735824752;
                exp_half_sq * n / d
            } else {
                let denom = abs_v
                    + 1.0
                        / (abs_v
                            + 2.0 / (abs_v + 3.0 / (abs_v + 4.0 / (abs_v + 0.65))));
                exp_half_sq / (SQRT_2PI * denom)
            };
            cdfs.0[lane] = if v > 0.0 { 1.0 - tail } else { tail };
        }
    }
    (cdfs, pdfs)
}

#[allow(dead_code)]
const _PI_REF: f64 = PI;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_norm_cdf_symmetry_and_accuracy() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-16);
        for k in 1..=35 {
            let x = (k as f64) * 0.25;
            let sum = norm_cdf(x) + norm_cdf(-x);
            assert!((sum - 1.0).abs() < 1e-15, "symmetry failed at x={x}: {sum}");
            let diff_fast = (norm_cdf(x) - norm_cdf_fast(x)).abs();
            assert!(diff_fast < 1e-14, "fast cdf diff at x={x}: {diff_fast}");
        }
        // Standard reference values:
        // Phi(1.0) = 0.841344746068542948585
        assert!((norm_cdf(1.0) - 0.8413447460685429).abs() < 1e-15);
        // Phi(1.959963984540054) = 0.975
        assert!((norm_cdf(1.959963984540054) - 0.975).abs() < 1e-15);
        // Inverse roundtrip
        for p in [1e-8, 1e-4, 0.025, 0.25, 0.5, 0.75, 0.975, 0.999999] {
            let z = norm_inv_cdf(p);
            assert!((norm_cdf(z) - p).abs() < 1e-14);
        }
    }
}
