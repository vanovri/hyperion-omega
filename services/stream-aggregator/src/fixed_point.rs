use bytes::{BufMut, BytesMut};
use hyperion_proto::{FP_SCALE_U128, PPM_SCALE_I128};
use postgres_types::{FromSql, IsNull, ToSql, Type};
use std::error::Error;

/// Scale constants (strict integer arithmetic; zero IEEE-754 floats).
pub const FP_SCALE_U64: u64 = 100_000_000;
pub const FP_SCALE_I64: i64 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const BPS_SCALE_U128: u128 = 10_000;
pub const PPM_MAX_I64: i64 = 1_000_000;

/// Exact quote notional: `floor((price_fp * qty_fp) / 1e8)`.
#[inline(always)]
pub fn calc_notional_fp(price_fp: u64, qty_fp: u64) -> u128 {
    ((price_fp as u128) * (qty_fp as u128)) / FP_SCALE_U128
}

/// Exact volume-weighted average price: `floor(sum(price_fp * qty_fp) / sum(qty_fp))`.
#[inline(always)]
pub fn calc_vwap_fp(raw_px_qty_sum: u128, total_qty_fp: u128, fallback_price_fp: u64) -> u64 {
    if total_qty_fp == 0 {
        return fallback_price_fp;
    }
    let v = raw_px_qty_sum / total_qty_fp;
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Exact mid-price `floor((best_bid_fp + best_ask_fp) / 2)`.
#[inline(always)]
pub fn calc_mid_price_fp(best_bid_fp: u64, best_ask_fp: u64, last_trade_fp: u64) -> u64 {
    if best_bid_fp > 0 && best_ask_fp > 0 {
        let sum = (best_bid_fp as u128) + (best_ask_fp as u128);
        (sum / 2) as u64
    } else if best_bid_fp > 0 {
        best_bid_fp
    } else if best_ask_fp > 0 {
        best_ask_fp
    } else {
        last_trade_fp
    }
}

/// Exact bid-ask spread and spread in basis points (`1 bps = 0.01% = 1/10_000`).
#[inline(always)]
pub fn calc_spread(best_bid_fp: u64, best_ask_fp: u64, mid_price_fp: u64) -> (u64, u32) {
    if best_bid_fp == 0 || best_ask_fp <= best_bid_fp || mid_price_fp == 0 {
        return (0, 0);
    }
    let spread_fp = best_ask_fp - best_bid_fp;
    let bps = ((spread_fp as u128) * BPS_SCALE_U128) / (mid_price_fp as u128);
    let bps_u32 = u32::try_from(bps).unwrap_or(u32::MAX);
    (spread_fp, bps_u32)
}

/// Signed Book Imbalance Ratio `((bid_sum - ask_sum) * 1_000_000) / (bid_sum + ask_sum)`
/// bounded strictly within `[-1_000_000, +1_000_000]` ppm.
#[inline(always)]
pub fn calc_imbalance_ppm(bid_qty_sum_fp: u128, ask_qty_sum_fp: u128) -> i64 {
    let denom = bid_qty_sum_fp.saturating_add(ask_qty_sum_fp);
    if denom == 0 {
        return 0;
    }
    let diff = (bid_qty_sum_fp as i128) - (ask_qty_sum_fp as i128);
    let denom_i128 = denom as i128;
    let raw = (diff * PPM_SCALE_I128) / denom_i128;
    raw.clamp(-PPM_MAX_I64 as i128, PPM_MAX_I64 as i128) as i64
}

/// Aggressive Taker Buy Ratio in parts-per-million `[0, 1_000_000]`.
#[inline(always)]
pub fn calc_taker_buy_ratio_ppm(taker_buy_vol_fp: u128, total_vol_fp: u128) -> u32 {
    if total_vol_fp == 0 {
        return 0;
    }
    let ppm = (taker_buy_vol_fp.saturating_mul(1_000_000)) / total_vol_fp;
    u32::try_from(ppm.min(1_000_000)).unwrap_or(1_000_000)
}

/// Quantizes `price_fp` down to canonical bucket floor `(price_fp / bucket_size_fp) * bucket_size_fp`,
/// ensuring positive (> 0) bucket floor for PostgreSQL `CHECK (price_bucket_fp > 0)`.
#[inline(always)]
pub fn quantize_price_bucket(price_fp: u64, bucket_size_fp: u64) -> u64 {
    let step = bucket_size_fp.max(1);
    let floored = (price_fp / step) * step;
    floored.max(step)
}

/// PostgreSQL Binary Wire Protocol wrapper for `NUMERIC(38, 0)` exact 128-bit integers!
/// Enables zero-float, high-throughput `BinaryCopyInWriter` streaming directly into
/// TimescaleDB `NUMERIC(38, 0)` columns (`notional_fp`, `base_volume_fp`, `ofi_fp`, etc.).
///
/// PostgreSQL binary `numeric` format (`src/backend/utils/adt/numeric.c`):
/// - `ndigits`: `i16` (number of base-10000 digits, big-endian)
/// - `weight`: `i16` (`ndigits - 1` for pure integers with `dscale = 0`)
/// - `sign`: `u16` (`0x0000` = `NUMERIC_POS`, `0x4000` = `NUMERIC_NEG`)
/// - `dscale`: `u16` (`0` fractional decimal digits)
/// - `digits`: `[i16; ndigits]` in base `10_000`, most-significant group first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct PgNumericI128(pub i128);

impl From<u128> for PgNumericI128 {
    #[inline]
    fn from(value: u128) -> Self {
        Self(i128::try_from(value).unwrap_or(i128::MAX))
    }
}

impl From<i128> for PgNumericI128 {
    #[inline]
    fn from(value: i128) -> Self {
        Self(value)
    }
}

impl ToSql for PgNumericI128 {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        let val = self.0;
        if val == 0 {
            out.put_i16(0); // ndigits
            out.put_i16(0); // weight
            out.put_u16(0x0000); // NUMERIC_POS
            out.put_u16(0); // dscale
            return Ok(IsNull::No);
        }

        let (sign, mut mag): (u16, u128) = if val < 0 {
            (0x4000, val.unsigned_abs())
        } else {
            (0x0000, val as u128)
        };

        // Max u128 is 3.4e38 -> at most 10 base-10000 digits (stack-only, zero heap alloc!).
        let mut groups = [0_i16; 12];
        let mut count = 0_usize;
        while mag > 0 {
            groups[count] = (mag % 10_000) as i16;
            mag /= 10_000;
            count += 1;
        }

        let ndigits = count as i16;
        let weight = ndigits - 1;
        out.put_i16(ndigits);
        out.put_i16(weight);
        out.put_u16(sign);
        out.put_u16(0); // dscale = 0
        for idx in (0..count).rev() {
            out.put_i16(groups[idx]);
        }
        Ok(IsNull::No)
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }

    postgres_types::to_sql_checked!();
}

impl<'a> FromSql<'a> for PgNumericI128 {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        if *ty != Type::NUMERIC {
            return Err("expected PostgreSQL NUMERIC type".into());
        }
        if raw.len() < 8 {
            return Err("invalid binary NUMERIC header length".into());
        }
        let ndigits = i16::from_be_bytes([raw[0], raw[1]]);
        let weight = i16::from_be_bytes([raw[2], raw[3]]);
        let sign = u16::from_be_bytes([raw[4], raw[5]]);
        let _dscale = u16::from_be_bytes([raw[6], raw[7]]);

        if ndigits <= 0 {
            return Ok(Self(0));
        }
        let expected_len = 8 + (ndigits as usize) * 2;
        if raw.len() < expected_len {
            return Err("truncated binary NUMERIC payload".into());
        }

        let mut acc: i128 = 0;
        let total_integer_groups = (weight + 1).max(0) as usize;
        for i in 0..total_integer_groups {
            let digit = if i < (ndigits as usize) {
                let off = 8 + i * 2;
                i16::from_be_bytes([raw[off], raw[off + 1]]) as i128
            } else {
                0
            };
            acc = acc
                .checked_mul(10_000)
                .and_then(|v| v.checked_add(digit))
                .ok_or("NUMERIC(38,0) overflowed i128")?;
        }

        if sign == 0x4000 {
            Ok(Self(-acc))
        } else {
            Ok(Self(acc))
        }
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_numeric_binary_roundtrip_extremes() {
        let samples = [
            0_i128,
            1_i128,
            -1_i128,
            9_999_i128,
            10_000_i128,
            -10_000_i128,
            8_553_187_500_000_i128,
            -987_654_321_000_123_456_789_i128,
            (1_i128 << 110) + 12345,
            -((1_i128 << 110) + 54321),
        ];
        for s in samples {
            let mut buf = BytesMut::new();
            PgNumericI128(s)
                .to_sql(&Type::NUMERIC, &mut buf)
                .expect("encode numeric");
            let decoded =
                PgNumericI128::from_sql(&Type::NUMERIC, &buf).expect("decode numeric");
            assert_eq!(decoded.0, s);
        }
    }

    #[test]
    fn imbalance_and_vwap_exactness() {
        assert_eq!(calc_notional_fp(6_842_550_000_000, 125_000_000), 8_553_187_500_000);
        assert_eq!(calc_mid_price_fp(100_000_000, 102_000_000, 99_000_000), 101_000_000);
        assert_eq!(calc_spread(100_000_000, 102_000_000, 101_000_000), (2_000_000, 198));
        assert_eq!(calc_taker_buy_ratio_ppm(750_000_000, 1_000_000_000), 750_000);
        assert_eq!(quantize_price_bucket(6_842_550_000_000, 100_000_000), 6_842_500_000_000);

        assert_eq!(calc_imbalance_ppm(100, 100), 0);
        assert_eq!(calc_imbalance_ppm(300, 100), 500_000);
        assert_eq!(calc_imbalance_ppm(100, 300), -500_000);
        assert_eq!(calc_imbalance_ppm(0, 0), 0);

        let p1 = 6_800_000_000_000_u64;
        let q1 = 100_000_000_u64;
        let p2 = 6_900_000_000_000_u64;
        let q2 = 300_000_000_u64;
        let raw_pq = (p1 as u128) * (q1 as u128) + (p2 as u128) * (q2 as u128);
        let vwap = calc_vwap_fp(raw_pq, (q1 + q2) as u128, 0);
        assert_eq!(vwap, 6_875_000_000_000_u64);
    }
}
