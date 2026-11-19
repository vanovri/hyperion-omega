#![deny(clippy::float_arithmetic)]

use std::cmp::Ordering;
use std::fmt;
use thiserror::Error;

/// Canonical Hyperion fixed-point scaling factor: 1.00000000 == 100_000_000 (10^8).
pub const FP_SCALE_U64: u64 = 100_000_000;
pub const FP_SCALE_U128: u128 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;

/// Canonical parts-per-million fee scaling factor: 1_000_000 ppm == 100% (100 ppm == 1 bps).
pub const PPM_SCALE_U128: u128 = 1_000_000;
pub const BPS_SCALE_U128: u128 = 10_000;
pub const MAX_ABS_FEE_PPM: i64 = 500_000; // Hard safety cap: 50%

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MathError {
    #[error("fixed-point arithmetic overflow")]
    Overflow,
    #[error("fixed-point arithmetic underflow")]
    Underflow,
    #[error("division by zero")]
    DivisionByZero,
    #[error("invalid fee rate ppm: {0}")]
    InvalidFeeRate(i64),
    #[error("value out of target integer range")]
    OutOfRange,
}

/// Computes quote notional `(price_fp * qty_fp) / 1e8` using checked 128-bit integer math.
/// Floor rounding guarantees superadditivity across partial fills:
/// `sum(floor(p_i * q_i / 1e8)) <= floor(P_limit * sum(q_i) / 1e8)`.
#[inline(always)]
pub const fn checked_notional_fp(price_fp: u64, qty_fp: u64) -> Result<u128, MathError> {
    match (price_fp as u128).checked_mul(qty_fp as u128) {
        Some(prod) => Ok(prod / FP_SCALE_U128),
        None => Err(MathError::Overflow),
    }
}

/// Computes signed fee in quote fixed-point `(notional_fp * fee_ppm) / 1_000_000`.
/// Negative `fee_ppm` represents a maker rebate; magnitude is floored toward zero so
/// `|rebate| <= notional_fp` and partial fills never exceed upfront buyer reserves.
#[inline(always)]
pub const fn checked_fee_fp(notional_fp: u128, fee_ppm: i64) -> Result<i64, MathError> {
    if fee_ppm < -MAX_ABS_FEE_PPM || fee_ppm > MAX_ABS_FEE_PPM {
        return Err(MathError::InvalidFeeRate(fee_ppm));
    }
    let abs_ppm = fee_ppm.unsigned_abs() as u128;
    let raw_fee = match notional_fp.checked_mul(abs_ppm) {
        Some(prod) => prod / PPM_SCALE_U128,
        None => return Err(MathError::Overflow),
    };
    if raw_fee > i64::MAX as u128 {
        return Err(MathError::OutOfRange);
    }
    let signed = raw_fee as i64;
    if fee_ppm < 0 {
        Ok(-signed)
    } else {
        Ok(signed)
    }
}

/// Computes the exact upfront quote collateral a BUY order must lock so that any
/// combination of maker/taker partial fills at `exec_price <= limit_price_fp` is
/// guaranteed never to exceed the locked amount.
#[inline(always)]
pub const fn checked_buyer_reserve_fp(
    limit_price_fp: u64,
    qty_fp: u64,
    maker_fee_ppm: i64,
    taker_fee_ppm: i64,
) -> Result<i128, MathError> {
    if maker_fee_ppm < -MAX_ABS_FEE_PPM
        || maker_fee_ppm > MAX_ABS_FEE_PPM
        || taker_fee_ppm < -MAX_ABS_FEE_PPM
        || taker_fee_ppm > MAX_ABS_FEE_PPM
    {
        return Err(MathError::InvalidFeeRate(maker_fee_ppm));
    }
    let notional = match checked_notional_fp(limit_price_fp, qty_fp) {
        Ok(n) => n,
        Err(e) => return Err(e),
    };
    let worst_pos_ppm = if maker_fee_ppm > taker_fee_ppm {
        if maker_fee_ppm > 0 {
            maker_fee_ppm as u128
        } else {
            0
        }
    } else if taker_fee_ppm > 0 {
        taker_fee_ppm as u128
    } else {
        0
    };
    let max_fee = match notional.checked_mul(worst_pos_ppm) {
        Some(prod) => prod / PPM_SCALE_U128,
        None => return Err(MathError::Overflow),
    };
    let total = match notional.checked_add(max_fee) {
        Some(t) => t,
        None => return Err(MathError::Overflow),
    };
    if total > i128::MAX as u128 {
        return Err(MathError::OutOfRange);
    }
    Ok(total as i128)
}

/// Computes slippage-bounded price cap/floor in fixed-point given `max_slippage_bps`.
#[inline(always)]
pub const fn checked_slippage_limit_fp(
    ref_price_fp: u64,
    max_slippage_bps: u32,
    is_buy: bool,
) -> Result<u64, MathError> {
    let bps = max_slippage_bps as u128;
    let factor = if is_buy {
        match BPS_SCALE_U128.checked_add(bps) {
            Some(f) => f,
            None => return Err(MathError::Overflow),
        }
    } else {
        BPS_SCALE_U128.saturating_sub(bps)
    };
    let scaled = match (ref_price_fp as u128).checked_mul(factor) {
        Some(v) => v / BPS_SCALE_U128,
        None => return Err(MathError::Overflow),
    };
    if scaled > u64::MAX as u128 {
        return Err(MathError::OutOfRange);
    }
    Ok(scaled as u64)
}

/// Zero-allocation fixed-capacity UTF-8 inline string used on the hot matching path.
#[derive(Clone, Copy)]
pub struct InlineStr<const N: usize> {
    buf: [u8; N],
    len: u8,
}

impl<const N: usize> Default for InlineStr<N> {
    #[inline(always)]
    fn default() -> Self {
        Self::empty()
    }
}

impl<const N: usize> InlineStr<N> {
    #[inline(always)]
    pub const fn empty() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    #[inline(always)]
    pub fn from_str_trunc(s: &str) -> Self {
        let bytes = s.as_bytes();
        let mut copy_len = bytes.len().min(N);
        while copy_len > 0 && !s.is_char_boundary(copy_len) {
            copy_len -= 1;
        }
        let mut buf = [0u8; N];
        buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
        Self {
            buf,
            len: copy_len as u8,
        }
    }

    #[inline(always)]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }

    #[inline(always)]
    pub fn as_str(&self) -> &str {
        // Guaranteed valid UTF-8 by construction in `from_str_trunc` / `try_from_bytes`.
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > N {
            return Err("inline string exceeds capacity");
        }
        let s = std::str::from_utf8(bytes).map_err(|_| "invalid utf-8 in inline string")?;
        Ok(Self::from_str_trunc(s))
    }
}

impl<const N: usize> PartialEq for InlineStr<N> {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl<const N: usize> Eq for InlineStr<N> {}

impl<const N: usize> PartialOrd for InlineStr<N> {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<const N: usize> Ord for InlineStr<N> {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

impl<const N: usize> std::hash::Hash for InlineStr<N> {
    #[inline(always)]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state);
    }
}

impl<const N: usize> fmt::Debug for InlineStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl<const N: usize> fmt::Display for InlineStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub type AssetSymbol = InlineStr<16>;
pub type InstrumentSymbol = InlineStr<24>;
pub type ClientOrderId = InlineStr<48>;
