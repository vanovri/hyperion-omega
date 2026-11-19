//! Zero-dependency pure fixed-point 1e8 calculation core for HYPERION (`contracts/gen/rust`).

/// Global fixed-point scale factor (1.00000000 = 100_000_000 atomic ticks).
pub const FP_SCALE: u64 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const FP_SCALE_U128: u128 = 100_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;

/// Binary WebSocket little-endian framing constants.
pub const WS_FRAME_MAGIC: u16 = 0x4859;
pub const WS_FRAME_VERSION: u8 = 0x01;
pub const WS_HEADER_LEN: usize = 32;

/// WAL & Snapshot binary magic headers.
pub const WAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const SNAPSHOT_MAGIC: &[u8; 8] = b"HYPSNP01";

/// Cache-line aligned 8-lane batch input (2 x 64B cache lines, zero false sharing).
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignedNotionalInput8 {
    pub prices_fp: [u64; 8],
    pub qtys_fp: [u64; 8],
}

/// Cache-line aligned 8-lane batch output (2 x 64B cache lines).
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlignedNotionalOutput8 {
    pub notionals_fp: [u128; 8],
}

/// AoS reference record (Pass 1 Baseline layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotionalOrderRow {
    pub price_fp: u64,
    pub qty_fp: u64,
}

#[inline]
pub fn i128_to_parts(value: i128) -> (i64, u64) {
    ((value >> 64) as i64, value as u64)
}

#[inline]
pub fn parts_to_i128(hi: i64, lo: u64) -> i128 {
    ((((hi as u64) as u128) << 64) + (lo as u128)) as i128
}

#[inline]
pub fn u128_to_parts(value: u128) -> (u64, u64) {
    ((value >> 64) as u64, value as u64)
}

#[inline]
pub fn parts_to_u128(hi: u64, lo: u64) -> u128 {
    ((hi as u128) << 64) + (lo as u128)
}

/// Computes exact quote notional `(price_fp * qty_fp) / 1e8` without overflow.
#[inline]
pub fn checked_notional_fp(price_fp: u64, qty_fp: u64) -> Option<u128> {
    let product = (price_fp as u128).checked_mul(qty_fp as u128)?;
    Some(product / FP_SCALE_U128)
}

/// Fast-path exact notional avoiding 128-bit software division when product fits in u64.
#[inline]
pub fn fast_notional_fp(price_fp: u64, qty_fp: u64) -> u128 {
    let product = (price_fp as u128) * (qty_fp as u128);
    if product <= u64::MAX as u128 {
        ((product as u64) / FP_SCALE) as u128
    } else {
        product / FP_SCALE_U128
    }
}

/// Computes signed fee `(notional_fp * fee_ppm) / 1e6` rounded toward zero.
/// Negative `fee_ppm` represents a maker rebate.
#[inline]
pub fn checked_fee_fp(notional_fp: u128, fee_ppm: i64) -> Option<i64> {
    let notional_i128 = i128::try_from(notional_fp).ok()?;
    let raw = notional_i128.checked_mul(fee_ppm as i128)? / PPM_SCALE_I128;
    i64::try_from(raw).ok()
}

/// Pass 1 (Baseline): AoS slice -> heap-allocated Vec<u128> using generic 128-bit division.
pub fn compute_notional_batch_baseline(rows: &[NotionalOrderRow]) -> Vec<u128> {
    let mut out = Vec::new();
    for row in rows {
        let prod = (row.price_fp as u128) * (row.qty_fp as u128);
        out.push(prod / FP_SCALE_U128);
    }
    out
}

/// Pass 2 (Zero-Copy / SoA Refactor): Zero heap allocation into caller-owned output slice
/// with u64 hardware-reciprocal fast path.
#[inline]
pub fn compute_notional_batch_soa_zero_copy(
    prices_fp: &[u64],
    qtys_fp: &[u64],
    out_fp: &mut [u128],
) -> usize {
    let len = prices_fp.len().min(qtys_fp.len()).min(out_fp.len());
    for i in 0..len {
        out_fp[i] = fast_notional_fp(prices_fp[i], qtys_fp[i]);
    }
    len
}

/// Pass 3 (Cache-Line Aligned Final): 64-byte aligned 8-lane unrolled kernel with zero bounds checks.
#[inline]
pub fn compute_notional_batch_aligned8(
    input: &AlignedNotionalInput8,
    output: &mut AlignedNotionalOutput8,
) {
    let mut i = 0;
    while i < 8 {
        output.notionals_fp[i] = fast_notional_fp(input.prices_fp[i], input.qtys_fp[i]);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_i128_and_u128_parts_extremes() {
        for sample in [
            0_i128,
            1_i128,
            -1_i128,
            i64::MAX as i128,
            i64::MIN as i128,
            (1_i128 << 100) + 42,
            -((1_i128 << 100) + 99),
            i128::MAX,
            i128::MIN,
        ] {
            let (hi, lo) = i128_to_parts(sample);
            assert_eq!(parts_to_i128(hi, lo), sample);
        }

        for sample in [
            0_u128,
            1_u128,
            u64::MAX as u128,
            (1_u128 << 64) + 123_456_789,
            (1_u128 << 110) + 7,
            u128::MAX,
        ] {
            let (hi, lo) = u128_to_parts(sample);
            assert_eq!(parts_to_u128(hi, lo), sample);
        }
    }

    #[test]
    fn fixed_point_notional_and_rebate_math_boundaries() {
        let price_fp = 6_842_550_000_000_u64;
        let qty_fp = 125_000_000_u64;
        let notional = checked_notional_fp(price_fp, qty_fp).expect("notional");
        assert_eq!(notional, 8_553_187_500_000_u128);
        assert_eq!(fast_notional_fp(price_fp, qty_fp), notional);

        assert_eq!(checked_notional_fp(3, 33_333_333), Some(0));
        assert_eq!(checked_notional_fp(3, 33_333_334), Some(1));
        assert_eq!(fast_notional_fp(3, 33_333_333), 0);
        assert_eq!(fast_notional_fp(3, 33_333_334), 1);

        assert_eq!(
            fast_notional_fp(3, 6_148_914_691_236_517_205),
            checked_notional_fp(3, 6_148_914_691_236_517_205).unwrap()
        );
        assert_eq!(
            fast_notional_fp(1 << 32, 1 << 32),
            checked_notional_fp(1 << 32, 1 << 32).unwrap()
        );
        let max_prod = checked_notional_fp(u64::MAX, u64::MAX).expect("max notional");
        assert_eq!(fast_notional_fp(u64::MAX, u64::MAX), max_prod);
        assert_eq!(max_prod, 3_402_823_669_209_384_634_264_811_192_843_u128);

        let maker_fee = checked_fee_fp(notional, -20).expect("maker fee");
        let taker_fee = checked_fee_fp(notional, 100).expect("taker fee");
        assert_eq!(maker_fee, -171_063_750);
        assert_eq!(taker_fee, 855_318_750);

        assert_eq!(checked_fee_fp(u128::MAX, 1), None);
        assert_eq!(checked_fee_fp(i128::MAX as u128, 2), None);
        assert_eq!(checked_fee_fp((i64::MAX as u128) * 2_000_000, 1), None);
        assert_eq!(checked_fee_fp(999_999, -1), Some(0));
        assert_eq!(checked_fee_fp(1_000_000, -1), Some(-1));
    }

    #[test]
    fn batch_3pass_equivalence_and_alignment() {
        assert_eq!(core::mem::align_of::<AlignedNotionalInput8>(), 64);
        assert_eq!(core::mem::align_of::<AlignedNotionalOutput8>(), 64);
        let prices = [
            6_842_550_000_000_u64,
            350_120_000_000,
            18_450_000_000,
            2_450_000_000,
            3,
            3,
            1 << 32,
            u64::MAX,
        ];
        let qtys = [
            125_000_000_u64,
            500_000_000,
            1_250_000_000,
            10_000_000_000,
            33_333_333,
            33_333_334,
            1 << 32,
            u64::MAX,
        ];
        let rows: Vec<NotionalOrderRow> = (0..8)
            .map(|i| NotionalOrderRow {
                price_fp: prices[i],
                qty_fp: qtys[i],
            })
            .collect();

        let pass1 = compute_notional_batch_baseline(&rows);
        let mut pass2 = [0_u128; 8];
        let count = compute_notional_batch_soa_zero_copy(&prices, &qtys, &mut pass2);
        assert_eq!(count, 8);
        assert_eq!(pass1.as_slice(), &pass2);

        let aligned_in = AlignedNotionalInput8 {
            prices_fp: prices,
            qtys_fp: qtys,
        };
        let mut aligned_out = AlignedNotionalOutput8::default();
        compute_notional_batch_aligned8(&aligned_in, &mut aligned_out);
        assert_eq!(pass1.as_slice(), &aligned_out.notionals_fp);

        let mut short_out = [0_u128; 3];
        assert_eq!(
            compute_notional_batch_soa_zero_copy(&prices, &qtys, &mut short_out),
            3
        );
        assert_eq!(&short_out, &pass1[..3]);
    }
}
