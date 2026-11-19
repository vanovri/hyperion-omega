#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::cmp::Ordering;
use std::fmt;
use thiserror::Error;

use crate::pb::{I128, U128};

/// Canonical Hyperion fixed-point scaling factor: `1.00000000 == 100_000_000` (`10^8`).
pub const FP_SCALE_U64: u64 = 100_000_000;
pub const FP_SCALE_U128: u128 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;

/// Canonical parts-per-million fee scaling factor: `1_000_000 ppm == 100%` (`100 ppm == 1 bps`).
pub const PPM_SCALE_U128: u128 = 1_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;
pub const BPS_SCALE_U128: u128 = 10_000;
pub const MAX_ABS_FEE_PPM: i64 = 500_000; // Hard safety cap: 50%

pub const DEFAULT_MAKER_FEE_PPM: i64 = 100; // 1.0 bps
pub const DEFAULT_TAKER_FEE_PPM: i64 = 500; // 5.0 bps

#[derive(Debug, Clone, PartialEq, Eq, Error)]
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
    #[error("invalid decimal integer string: {0}")]
    InvalidDecimal(String),
}

/// Computes quote notional `(price_fp * qty_fp) / 1e8` using checked 128-bit integer math.
#[inline(always)]
pub const fn checked_notional_fp(price_fp: u64, qty_fp: u64) -> Result<u128, MathError> {
    match (price_fp as u128).checked_mul(qty_fp as u128) {
        Some(prod) => Ok(prod / FP_SCALE_U128),
        None => Err(MathError::Overflow),
    }
}

/// Computes signed fee in quote fixed-point `(notional_fp * fee_ppm) / 1_000_000` rounded toward zero.
/// Negative `fee_ppm` represents a maker rebate.
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

/// Computes upfront quote reserve a BUY order must lock so partial fills never exceed locked margin.
#[inline(always)]
pub fn checked_buyer_reserve_fp(
    limit_price_fp: u64,
    qty_fp: u64,
    maker_fee_ppm: i64,
    taker_fee_ppm: i64,
) -> Result<i128, MathError> {
    if !(-MAX_ABS_FEE_PPM..=MAX_ABS_FEE_PPM).contains(&maker_fee_ppm)
        || !(-MAX_ABS_FEE_PPM..=MAX_ABS_FEE_PPM).contains(&taker_fee_ppm)
    {
        return Err(MathError::InvalidFeeRate(maker_fee_ppm));
    }
    let notional = checked_notional_fp(limit_price_fp, qty_fp)?;
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

/// Slippage-bounded limit price for MARKET / slippage-guarded orders.
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

/// Exact integer floor square root `floor(sqrt(n))` without floating-point operations.
#[inline]
pub const fn isqrt_u128(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let shift = ((128 - n.leading_zeros()) + 1) >> 1;
    let mut x = 1u128 << shift;
    loop {
        let nx = (x + n / x) >> 1;
        if nx >= x {
            return x;
        }
        x = nx;
    }
}

/// Computes realized return standard deviation in integer basis points (`bps`, scale `10^4`)
/// across an ordered sequence of execution prices, using 100% deterministic integer arithmetic.
pub fn compute_realized_volatility_bps(prices_fp: &[u64]) -> u32 {
    if prices_fp.len() < 2 {
        return 0;
    }
    let mut returns_ppm = Vec::with_capacity(prices_fp.len() - 1);
    for pair in prices_fp.windows(2) {
        let prev = pair[0];
        let curr = pair[1];
        if prev == 0 {
            continue;
        }
        let diff = (curr as i128) - (prev as i128);
        let ret_ppm = diff.saturating_mul(PPM_SCALE_I128) / (prev as i128);
        returns_ppm.push(ret_ppm);
    }
    if returns_ppm.is_empty() {
        return 0;
    }
    let count = returns_ppm.len() as i128;
    let sum: i128 = returns_ppm.iter().copied().fold(0i128, i128::saturating_add);
    let mean = sum / count;

    let mut sq_sum: u128 = 0;
    for r in returns_ppm {
        let dev = r.saturating_sub(mean).unsigned_abs();
        sq_sum = sq_sum.saturating_add(dev.saturating_mul(dev));
    }
    let variance_ppm2 = sq_sum / (count as u128);
    let std_ppm = isqrt_u128(variance_ppm2);
    // 100 ppm == 1 bps
    let bps = std_ppm / 100;
    bps.min(u32::MAX as u128) as u32
}

/// Computes adverse execution slippage in integer basis points (`bps`) relative to pre-trade reference price.
#[inline]
pub const fn compute_slippage_bps(ref_price_fp: u64, exec_price_fp: u64, is_buy: bool) -> u32 {
    if ref_price_fp == 0 || exec_price_fp == 0 {
        return 0;
    }
    let adverse_diff = if is_buy {
        exec_price_fp.saturating_sub(ref_price_fp)
    } else {
        ref_price_fp.saturating_sub(exec_price_fp)
    };
    let bps = (adverse_diff as u128).saturating_mul(BPS_SCALE_U128) / (ref_price_fp as u128);
    if bps > u32::MAX as u128 {
        u32::MAX
    } else {
        bps as u32
    }
}

#[inline]
pub const fn i128_to_pb(value: i128) -> I128 {
    I128 {
        hi: (value >> 64) as i64,
        lo: value as u64,
    }
}

#[inline]
pub const fn pb_to_i128(value: Option<I128>) -> i128 {
    match value {
        Some(v) => ((v.hi as i128) << 64) | (v.lo as i128),
        None => 0,
    }
}

#[inline]
pub const fn u128_to_pb(value: u128) -> U128 {
    U128 {
        hi: (value >> 64) as u64,
        lo: value as u64,
    }
}

#[inline]
pub const fn pb_to_u128(value: Option<U128>) -> u128 {
    match value {
        Some(v) => ((v.hi as u128) << 64) | (v.lo as u128),
        None => 0,
    }
}

pub fn encode_hex_lower(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(LUT[(b >> 4) as usize] as char);
        out.push(LUT[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn decode_hex_32(hex_str: &str) -> Result<[u8; 32], MathError> {
    if hex_str.len() != 64 {
        return Err(MathError::InvalidDecimal(format!(
            "expected 64-char hex digest, got len {}",
            hex_str.len()
        )));
    }
    let mut out = [0u8; 32];
    let raw = hex_str.as_bytes();
    for i in 0..32 {
        let hi = hex_nibble(raw[2 * i]).ok_or_else(|| MathError::InvalidDecimal(hex_str.to_string()))?;
        let lo =
            hex_nibble(raw[2 * i + 1]).ok_or_else(|| MathError::InvalidDecimal(hex_str.to_string()))?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

#[inline]
const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Canonical Hyperion instrument metadata per `00-hyperion-contract.md` Section 4.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentSpec {
    pub instrument_id: u32,
    pub symbol: String,
    pub base_asset: AssetSymbol,
    pub quote_asset: AssetSymbol,
    pub tick_size_fp: u64,
    pub lot_size_fp: u64,
}

impl InstrumentSpec {
    pub fn from_symbol(symbol: &str) -> Self {
        match symbol {
            "BTC-USDT" => Self {
                instrument_id: 1,
                symbol: "BTC-USDT".to_string(),
                base_asset: AssetSymbol::from_str_trunc("BTC"),
                quote_asset: AssetSymbol::from_str_trunc("USDT"),
                tick_size_fp: 1_000_000,
                lot_size_fp: 1_000,
            },
            "ETH-USDT" => Self {
                instrument_id: 2,
                symbol: "ETH-USDT".to_string(),
                base_asset: AssetSymbol::from_str_trunc("ETH"),
                quote_asset: AssetSymbol::from_str_trunc("USDT"),
                tick_size_fp: 1_000_000,
                lot_size_fp: 10_000,
            },
            "SOL-USDT" => Self {
                instrument_id: 3,
                symbol: "SOL-USDT".to_string(),
                base_asset: AssetSymbol::from_str_trunc("SOL"),
                quote_asset: AssetSymbol::from_str_trunc("USDT"),
                tick_size_fp: 100_000,
                lot_size_fp: 100_000,
            },
            "HYPE-USDT" => Self {
                instrument_id: 4,
                symbol: "HYPE-USDT".to_string(),
                base_asset: AssetSymbol::from_str_trunc("HYPE"),
                quote_asset: AssetSymbol::from_str_trunc("USDT"),
                tick_size_fp: 10_000,
                lot_size_fp: 1_000_000,
            },
            other => {
                let (base, quote) = other.split_once('-').unwrap_or((other, "USDT"));
                Self {
                    instrument_id: 99,
                    symbol: other.to_string(),
                    base_asset: AssetSymbol::from_str_trunc(base),
                    quote_asset: AssetSymbol::from_str_trunc(quote),
                    tick_size_fp: 1,
                    lot_size_fp: 1,
                }
            }
        }
    }
}

/// Zero-allocation fixed-capacity UTF-8 inline string matching Channel 02's `AssetSymbol`.
#[derive(Clone, Copy)]
pub struct InlineStr<const N: usize> {
    buf: [u8; N],
    len: u8,
}

impl<const N: usize> serde::Serialize for InlineStr<N> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de, const N: usize> serde::Deserialize<'de> for InlineStr<N> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_str_trunc(&s))
    }
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
