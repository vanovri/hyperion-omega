use arrow::datatypes::i256;
use crate::config::FP_SCALE_I128;

/// Q40 fixed-point constant: round((ln(2) / 4) * 2^40)
/// ln(2)/4 = 0.17328679513998632735... * 1_099_511_627_776 = 190_530_846_196.73...
const LN2_DIV_4_Q40: i128 = 190_530_846_197;

/// Monotonic time-series segment summary for associative O(1) parallel TWAP reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwapSegment {
    pub first_ts_ns: i64,
    pub first_price_fp: i64,
    pub last_ts_ns: i64,
    pub last_price_fp: i64,
    pub weighted_sum: i128,
    pub simple_sum: i128,
    pub count: u64,
}

impl TwapSegment {
    /// Builds a `TwapSegment` from non-null `(timestamp_ns, price_fp)` pairs.
    /// Automatically detects intra-slice disorder and sorts only if unsorted.
    pub fn from_pairs(pairs: &mut [(i64, i64)]) -> Option<Self> {
        if pairs.is_empty() {
            return None;
        }
        let mut sorted = true;
        for w in pairs.windows(2) {
            if w[1].0 < w[0].0 {
                sorted = false;
                break;
            }
        }
        if !sorted {
            pairs.sort_by_key(|&(ts, _)| ts);
        }
        Self::from_sorted_pairs(pairs)
    }

    /// Builds a `TwapSegment` from pre-sorted `(timestamp_ns, price_fp)` pairs in O(N).
    #[inline]
    pub fn from_sorted_pairs(pairs: &[(i64, i64)]) -> Option<Self> {
        let &(first_ts_ns, first_price_fp) = pairs.first()?;
        let &(last_ts_ns, last_price_fp) = pairs.last()?;
        let mut weighted_sum: i128 = 0;
        let mut simple_sum: i128 = 0;

        for idx in 0..pairs.len() {
            let (ts, price) = pairs[idx];
            simple_sum += i128::from(price);
            if idx + 1 < pairs.len() {
                let next_ts = pairs[idx + 1].0;
                let dt = i128::from(next_ts) - i128::from(ts);
                weighted_sum += i128::from(price) * dt;
            }
        }

        Some(Self {
            first_ts_ns,
            first_price_fp,
            last_ts_ns,
            last_price_fp,
            weighted_sum,
            simple_sum,
            count: pairs.len() as u64,
        })
    }

    /// Vectorized fast-path directly over aligned non-null `prices` and `timestamps` slices
    /// when monotonic order holds; returns `None` if an out-of-order pair is encountered.
    #[inline]
    pub fn try_from_monotonic_slices(prices: &[i64], timestamps: &[i64]) -> Option<Self> {
        debug_assert_eq!(prices.len(), timestamps.len());
        let len = prices.len();
        if len == 0 {
            return None;
        }
        let first_ts_ns = timestamps[0];
        let first_price_fp = prices[0];
        let mut prev_ts = first_ts_ns;
        let mut prev_price = first_price_fp;
        let mut weighted_sum: i128 = 0;
        let mut simple_sum = i128::from(first_price_fp);

        for i in 1..len {
            let ts = timestamps[i];
            if ts < prev_ts {
                return None;
            }
            let price = prices[i];
            let dt = i128::from(ts) - i128::from(prev_ts);
            weighted_sum += i128::from(prev_price) * dt;
            simple_sum += i128::from(price);
            prev_ts = ts;
            prev_price = price;
        }

        Some(Self {
            first_ts_ns,
            first_price_fp,
            last_ts_ns: prev_ts,
            last_price_fp: prev_price,
            weighted_sum,
            simple_sum,
            count: len as u64,
        })
    }
}

/// Evaluates exact fixed-point TWAP (`scale = 1e8`) across one or more `TwapSegment`s.
/// Orders segments by `(first_ts_ns, last_ts_ns)` and bridges boundary intervals
/// `seg[i].last_price_fp * (seg[i+1].first_ts_ns - seg[i].last_ts_ns)`.
pub fn evaluate_twap_segments(segments: &mut [TwapSegment]) -> Option<i64> {
    if segments.is_empty() {
        return None;
    }
    segments.sort_by_key(|s| (s.first_ts_ns, s.last_ts_ns));

    let mut total_weighted: i128 = segments[0].weighted_sum;
    let mut total_simple: i128 = segments[0].simple_sum;
    let mut total_count: u64 = segments[0].count;
    let start_ts = segments[0].first_ts_ns;
    let mut cursor_last_ts = segments[0].last_ts_ns;
    let mut cursor_last_price = segments[0].last_price_fp;

    for seg in &segments[1..] {
        total_simple += seg.simple_sum;
        total_count += seg.count;
        if seg.first_ts_ns >= cursor_last_ts {
            let bridge_dt = i128::from(seg.first_ts_ns) - i128::from(cursor_last_ts);
            total_weighted += i128::from(cursor_last_price) * bridge_dt + seg.weighted_sum;
            cursor_last_ts = seg.last_ts_ns;
            cursor_last_price = seg.last_price_fp;
        } else {
            // Overlapping coarse partition fallback: weight proportionally without negative dt
            total_weighted += seg.weighted_sum;
            if seg.last_ts_ns > cursor_last_ts {
                cursor_last_ts = seg.last_ts_ns;
                cursor_last_price = seg.last_price_fp;
            }
        }
    }

    let total_duration = i128::from(cursor_last_ts) - i128::from(start_ts);
    if total_duration > 0 {
        i64::try_from(total_weighted / total_duration).ok()
    } else if total_count > 0 {
        i64::try_from(total_simple / i128::from(total_count)).ok()
    } else {
        None
    }
}

/// Computes `log2(high_fp / low_fp)` in unsigned Q32 fixed-point (`1.0 = 2^32`)
/// using Clay Turner's deterministic binary shift-and-square algorithm in pure `u128`
/// (`#![deny(clippy::float_arithmetic)]` compliant, accurate to < 2.4e-10).
#[inline]
pub fn log2_ratio_q32(high_fp: i64, low_fp: i64) -> Option<u64> {
    if high_fp <= 0 || low_fp <= 0 {
        return None;
    }
    let (h, l) = if high_fp >= low_fp {
        (high_fp as u128, low_fp as u128)
    } else {
        // Defensive symmetry if caller swapped high/low since (ln(H/L))^2 == (ln(L/H))^2
        (low_fp as u128, high_fp as u128)
    };
    if h == l {
        return Some(0);
    }

    let mut int_part: u64 = 0;
    let mut l_shifted = l;
    while (l_shifted << 1) <= h {
        l_shifted <<= 1;
        int_part += 1;
    }

    // Normalize mantissa x = floor((h << 60) / l_shifted) in [2^60, 2^61)
    let mut x: u128 = (h << 60) / l_shifted;
    let mut frac_q40: u64 = 0;
    for bit in (0..40_u32).rev() {
        // x < 2^61 => x * x < 2^122 < u128::MAX (zero overflow guaranteed)
        x = ((x * x) + (1_u128 << 59)) >> 60;
        if x >= (1_u128 << 61) {
            frac_q40 |= 1_u64 << bit;
            x >>= 1;
        }
    }

    let log2_q40: u128 = ((int_part as u128) << 40) | (frac_q40 as u128);
    let log2_q32 = ((log2_q40 + 128) >> 8) as u64;
    Some(log2_q32)
}

#[inline]
fn u128_to_i256(v: u128) -> i256 {
    let mut le = [0_u8; 32];
    le[0..16].copy_from_slice(&v.to_le_bytes());
    i256::from_le_bytes(le)
}

/// Evaluates Parkinson (1980) realized volatility scaled by `1e8` (`FP_SCALE`)
/// from accumulated `sum((log2(H_i / L_i)_q32)^2)` (in Q64) and sample count `n`.
///
/// Formula: `sigma = sqrt((ln(2) / (4 * N)) * sum((log2(H_i / L_i))^2))`
#[inline]
pub fn evaluate_parkinson_from_q64_sum(sum_log2_sq_q64: u128, count: u64) -> Option<i64> {
    if count == 0 {
        return None;
    }
    if sum_log2_sq_q64 == 0 {
        return Some(0);
    }

    // Use 256-bit integer arithmetic:
    // 4 * sigma_fp^2 = (sum_log2_sq_q64 * LN2_DIV_4_Q40 * 4 * 10^16) / (count * 2^104)
    let sum_256 = u128_to_i256(sum_log2_sq_q64);
    let coeff_256 = i256::from_i128(LN2_DIV_4_Q40);
    // 4 * (10^8)^2 = 40_000_000_000_000_000
    let scale_sq_x4 = i256::from_i128(4 * FP_SCALE_I128 * FP_SCALE_I128);

    let numerator = sum_256
        .checked_mul(coeff_256)?
        .checked_mul(scale_sq_x4)?;

    let shift_104 = u128_to_i256(1_u128 << 104);
    let denominator = i256::from_i128(i128::from(count)).checked_mul(shift_104)?;
    let half_den = denominator / i256::from_i128(2);

    let variance_x4_256 = (numerator + half_den) / denominator;
    let variance_x4_u128 = variance_x4_256.to_i128()? as u128;

    let root_x2 = variance_x4_u128.isqrt();
    let sigma_fp = root_x2.div_ceil(2) as i64;
    Some(sigma_fp)
}

/// Associative state for 256-bit exact OLS Kyle's Lambda price-impact regression:
/// `delta_price_fp = alpha + lambda * signed_qty_fp + eps`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KylesLambdaState {
    pub count: u64,
    pub sum_v: i128,
    pub sum_dp: i128,
    pub sum_vv: i256,
    pub sum_v_dp: i256,
}

impl KylesLambdaState {
    #[inline]
    pub fn update_pair(&mut self, delta_price_fp: i64, signed_qty_fp: i64) {
        let v = i128::from(signed_qty_fp);
        let dp = i128::from(delta_price_fp);
        self.count += 1;
        self.sum_v += v;
        self.sum_dp += dp;
        self.sum_vv += i256::from_i128(v) * i256::from_i128(v);
        self.sum_v_dp += i256::from_i128(v) * i256::from_i128(dp);
    }

    #[inline]
    pub fn merge(&mut self, other: &Self) {
        self.count += other.count;
        self.sum_v += other.sum_v;
        self.sum_dp += other.sum_dp;
        self.sum_vv += other.sum_vv;
        self.sum_v_dp += other.sum_v_dp;
    }

    /// Evaluates OLS slope `lambda_fp` scaled by `1e8` (`price_fp` change per `1.0` base unit).
    #[inline]
    pub fn evaluate_fp(&self) -> Option<i64> {
        if self.count == 0 {
            return None;
        }
        if self.count < 2 {
            return Some(0);
        }
        let n = i256::from_i128(i128::from(self.count));
        let sv = i256::from_i128(self.sum_v);
        let sdp = i256::from_i128(self.sum_dp);

        let var_den = n * self.sum_vv - sv * sv;
        if var_den == i256::ZERO {
            return Some(0);
        }

        let cov_num = n * self.sum_v_dp - sv * sdp;
        let scaled = cov_num * i256::from_i128(FP_SCALE_I128);
        let slope_256 = scaled / var_den;

        let clamped = if slope_256 > i256::from_i128(i128::from(i64::MAX)) {
            i64::MAX
        } else if slope_256 < i256::from_i128(i128::from(i64::MIN)) {
            i64::MIN
        } else {
            slope_256.to_i128().unwrap_or(0) as i64
        };
        Some(clamped)
    }
}

/// Computes single-fill fixed-point Markout PnL (`scale = 1e8`):
/// `side_sign * ((future_mid_fp - exec_price_fp) * qty_fp) / 1e8`
/// Positive value indicates toxic/informed flow (price moved in aggressor's favor after execution).
#[inline]
pub fn compute_fill_markout_pnl_i128(
    side_sign: i64,
    exec_price_fp: i64,
    future_mid_fp: i64,
    qty_fp: i64,
) -> i128 {
    let dir = if side_sign >= 0 { 1_i128 } else { -1_i128 };
    let price_diff = i128::from(future_mid_fp) - i128::from(exec_price_fp);
    (dir * price_diff * i128::from(qty_fp)) / FP_SCALE_I128
}

/// Clamps an `i128` fixed-point accumulator safely into `i64` output domain.
#[inline]
pub fn saturate_i128_to_i64(val: i128) -> i64 {
    if val > i128::from(i64::MAX) {
        i64::MAX
    } else if val < i128::from(i64::MIN) {
        i64::MIN
    } else {
        val as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FP_SCALE;

    #[test]
    fn test_twap_segments_associative_exactness() {
        // Deliberately unsorted input to verify disorder detection & sorting
        let mut all = vec![
            (5_000, 90 * FP_SCALE),
            (1_000, 100 * FP_SCALE),
            (10_000, 120 * FP_SCALE),
            (2_000, 110 * FP_SCALE),
        ];
        let single = TwapSegment::from_pairs(&mut all).unwrap();
        let expected = evaluate_twap_segments(&mut [single]).unwrap();
        assert_eq!(expected, 9_777_777_777);

        let mut p1 = vec![(1_000, 100 * FP_SCALE), (2_000, 110 * FP_SCALE)];
        let mut p2 = vec![(5_000, 90 * FP_SCALE), (10_000, 120 * FP_SCALE)];
        let s1 = TwapSegment::from_pairs(&mut p1).unwrap();
        let s2 = TwapSegment::from_pairs(&mut p2).unwrap();
        let merged = evaluate_twap_segments(&mut [s2, s1]).unwrap();
        assert_eq!(merged, expected);

        // Strictly descending input kills `<` -> `>` disorder-check mutants!
        let mut descending = vec![
            (10_000, 120 * FP_SCALE),
            (5_000, 90 * FP_SCALE),
            (2_000, 110 * FP_SCALE),
            (1_000, 100 * FP_SCALE),
        ];
        let seg_desc = TwapSegment::from_pairs(&mut descending).unwrap();
        assert_eq!(evaluate_twap_segments(&mut [seg_desc]), Some(expected));

        // Zero-duration fallback
        let mut same_ts = vec![(500, 100 * FP_SCALE), (500, 300 * FP_SCALE)];
        let seg_zero = TwapSegment::from_pairs(&mut same_ts).unwrap();
        assert_eq!(evaluate_twap_segments(&mut [seg_zero]), Some(200 * FP_SCALE));
    }

    #[test]
    fn test_parkinson_pure_integer_precision() {
        assert_eq!(log2_ratio_q32(0, 100 * FP_SCALE), None);
        assert_eq!(log2_ratio_q32(100 * FP_SCALE, 0), None);
        assert_eq!(log2_ratio_q32(100 * FP_SCALE, 100 * FP_SCALE), Some(0));

        // Exact power-of-2 ratio: H / L = 2.0 => sigma = 41_627_731
        let q32 = log2_ratio_q32(200 * FP_SCALE, 100 * FP_SCALE).unwrap();
        assert_eq!(q32, 1_u64 << 32);
        let sq = (q32 as u128) * (q32 as u128);
        let sigma = evaluate_parkinson_from_q64_sum(sq, 1).unwrap();
        assert!((sigma - 41_627_731).abs() <= 1);

        // Non-power-of-2 ratio: H / L = 1.5 => log2(1.5) ≈ 0.5849625007 -> Q32 = 2_512_394_810
        let q32_frac = log2_ratio_q32(150 * FP_SCALE, 100 * FP_SCALE).unwrap();
        assert!((q32_frac as i64 - 2_512_394_810).abs() <= 4);
        let vol_frac =
            evaluate_parkinson_from_q64_sum((q32_frac as u128) * (q32_frac as u128), 1).unwrap();
        // sqrt(ln(2)/4) * log2(1.5) = 0.243506614 -> 24_350_661
        assert!((vol_frac - 24_350_661).abs() <= 2);
    }

    #[test]
    fn test_kyles_lambda_and_markout_exact_math() {
        // Non-zero mean linear regression: dp = 50 + 2.5 * v
        let mut state = KylesLambdaState::default();
        for v_units in [1_i64, 2, 4, 5, 8] {
            let v_fp = v_units * FP_SCALE;
            let dp_fp = (50 * FP_SCALE) + (v_units * 250_000_000);
            state.update_pair(dp_fp, v_fp);
        }
        assert_eq!(state.evaluate_fp(), Some(250_000_000));

        // Zero-variance flat volume
        let mut flat = KylesLambdaState::default();
        flat.update_pair(10 * FP_SCALE, 2 * FP_SCALE);
        flat.update_pair(20 * FP_SCALE, 2 * FP_SCALE);
        assert_eq!(flat.evaluate_fp(), Some(0));

        // Markout PnL buy/sell sign & notional scaling
        let buy_pnl = compute_fill_markout_pnl_i128(1, 100 * FP_SCALE, 106 * FP_SCALE, 2 * FP_SCALE);
        let sell_pnl = compute_fill_markout_pnl_i128(-1, 100 * FP_SCALE, 106 * FP_SCALE, 2 * FP_SCALE);
        assert_eq!(buy_pnl, (12 * FP_SCALE) as i128);
        assert_eq!(sell_pnl, (-12 * FP_SCALE) as i128);
    }
}
