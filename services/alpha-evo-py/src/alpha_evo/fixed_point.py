"""
Exact Fixed-Point Financial Arithmetic aligned 100% with `.agents/rules/00-hyperion-contract.md`
and Channel 02 (`services/ledger-rs`).

Invariants (Section 5.1 & 5.2 of `00-hyperion-contract.md`):
- Monetary prices, base quantities, quote notionals, fees, and balances use integer atomic
  ticks scaled by `FP_SCALE = 100_000_000` (1e8).
- Quote notional:
      notional_fp = floor((u128(price_fp) * u128(qty_fp)) / 100_000_000)
- Signed maker/taker fee (`fee_ppm` scaled by 1e6; negative = maker rebate, positive = fee):
      fee_fp = round_toward_zero((i128(notional_fp) * i128(fee_ppm)) / 1_000_000)
- Vectorized NumPy operations use exact split-word Euclidean decomposition `(q * S + r)` so
  64-bit integer arrays never overflow `int64` (even for multi-BTC orders at >$100k) and
  remain bit-for-bit identical to 128-bit Rust `u128`/`i128` arithmetic.
"""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal, ROUND_DOWN
import numpy as np
from numpy.typing import NDArray

FP_SCALE: int = 100_000_000
PPM_SCALE: int = 1_000_000
BPS_SCALE: int = 10_000

# Default Hyperion institutional Tier-0 schedule (matches Channel 02 / Chronos overrides):
# -100 ppm = -1.0 bps maker rebate; +500 ppm = +5.0 bps taker fee.
DEFAULT_MAKER_FEE_PPM: int = -100
DEFAULT_TAKER_FEE_PPM: int = 500


@dataclass(frozen=True, slots=True)
class InstrumentSpec:
    """Canonical Hyperion exchange instrument specification (Section 4.3)."""

    instrument_id: int
    symbol: str
    tick_size_fp: int
    lot_size_fp: int
    reference_price_fp: int


CANONICAL_INSTRUMENTS: dict[str, InstrumentSpec] = {
    "BTC-USDT": InstrumentSpec(
        instrument_id=1,
        symbol="BTC-USDT",
        tick_size_fp=1_000_000,          # 0.01 USDT
        lot_size_fp=1_000,               # 0.00001 BTC
        reference_price_fp=68_500_00000000,
    ),
    "ETH-USDT": InstrumentSpec(
        instrument_id=2,
        symbol="ETH-USDT",
        tick_size_fp=1_000_000,          # 0.01 USDT
        lot_size_fp=10_000,              # 0.0001 ETH
        reference_price_fp=3_450_00000000,
    ),
    "SOL-USDT": InstrumentSpec(
        instrument_id=3,
        symbol="SOL-USDT",
        tick_size_fp=100_000,            # 0.001 USDT
        lot_size_fp=100_000,             # 0.001 SOL
        reference_price_fp=185_00000000,
    ),
    "HYPE-USDT": InstrumentSpec(
        instrument_id=4,
        symbol="HYPE-USDT",
        tick_size_fp=10_000,             # 0.0001 USDT
        lot_size_fp=1_000_000,           # 0.01 HYPE
        reference_price_fp=28_50000000,
    ),
}


def to_fp(value: str | int | Decimal) -> int:
    """Convert exact string/int/Decimal representation to integer fixed-point (scale 1e8)."""
    if isinstance(value, int):
        return value * FP_SCALE
    dec = Decimal(str(value))
    scaled = (dec * Decimal(FP_SCALE)).quantize(Decimal("1"), rounding=ROUND_DOWN)
    return int(scaled)


def from_fp(value_fp: int) -> Decimal:
    """Convert integer fixed-point (scale 1e8) to exact Decimal for reporting."""
    return Decimal(int(value_fp)) / Decimal(FP_SCALE)


def decimal_str_fp(value_fp: int) -> str:
    """Serialize `_fp` integer as canonical decimal integer string per Section 5.1.3."""
    return str(int(value_fp))


def trunc_div_i128(numerator: int, denominator: int) -> int:
    """
    Exact Rust `i128` division rounding toward zero (`round_toward_zero`).
    Note: Python's `//` rounds toward -infinity; Rust `/` truncates toward zero.
    """
    if denominator == 0:
        raise ZeroDivisionError("Fixed-point denominator cannot be zero")
    sign = -1 if ((numerator < 0) ^ (denominator < 0)) else 1
    q = abs(int(numerator)) // abs(int(denominator))
    return sign * q if q != 0 else 0


def compute_notional_fp_scalar(price_fp: int, qty_fp: int) -> int:
    """
    Exact scalar 128-bit quote notional:
        floor((u128(price_fp) * u128(qty_fp)) / 100_000_000)
    """
    if price_fp < 0 or qty_fp < 0:
        raise ValueError(f"price_fp ({price_fp}) and qty_fp ({qty_fp}) must be non-negative")
    return (int(price_fp) * int(qty_fp)) // FP_SCALE


def compute_fee_fp_scalar(notional_fp: int, fee_ppm: int) -> int:
    """
    Exact scalar 128-bit fee in quote fixed-point:
        round_toward_zero((i128(notional_fp) * i128(fee_ppm)) / 1_000_000)
    Negative `fee_ppm` represents a maker rebate credited to the maker.
    """
    if notional_fp < 0:
        raise ValueError(f"notional_fp ({notional_fp}) must be non-negative")
    return trunc_div_i128(int(notional_fp) * int(fee_ppm), PPM_SCALE)


def compute_notional_fp_vec(
    price_fp: NDArray[np.int64],
    qty_fp: NDArray[np.int64],
) -> NDArray[np.int64]:
    """
    Overflow-free 128-bit exact vectorized quote notional on `int64` NumPy arrays!

    Uses Euclidean split `p = p_q * S + p_r`, `q = q_q * S + q_r` (`S = 100_000_000`):
        floor((p * q) / S) = p_q * q_q * S + p_q * q_r + p_r * q_q + (p_r * q_r) // S
    Since `p_r, q_r < 1e8`, `p_r * q_r < 1e16 << INT64_MAX (9.22e18)`, preventing overflow!
    """
    p = np.asarray(price_fp, dtype=np.int64)
    q = np.asarray(qty_fp, dtype=np.int64)
    if np.any(p < 0) or np.any(q < 0):
        raise ValueError("price_fp and qty_fp arrays must be non-negative")

    p_q = p // FP_SCALE
    p_r = p % FP_SCALE
    q_q = q // FP_SCALE
    q_r = q % FP_SCALE

    return (
        p_q * q_q * FP_SCALE
        + p_q * q_r
        + p_r * q_q
        + (p_r * q_r) // FP_SCALE
    )


def compute_fee_fp_vec(
    notional_fp: NDArray[np.int64],
    fee_ppm: int | NDArray[np.int64],
) -> NDArray[np.int64]:
    """
    Overflow-free 128-bit exact vectorized fee computation truncating toward zero!

    Matches `compute_fee_fp_scalar` bit-for-bit across arbitrary `notional_fp >= 0`
    and positive or negative `fee_ppm` (maker rebate / taker fee).
    """
    n = np.asarray(notional_fp, dtype=np.int64)
    if np.any(n < 0):
        raise ValueError("notional_fp array must be non-negative")

    f = np.asarray(fee_ppm, dtype=np.int64)
    sign = np.sign(f)
    abs_f = np.abs(f)

    n_q = n // PPM_SCALE
    n_r = n % PPM_SCALE

    mag = n_q * abs_f + (n_r * abs_f) // PPM_SCALE
    return sign * mag


def settle_fill_double_entry_vec(
    price_fp: NDArray[np.int64],
    qty_fp: NDArray[np.int64],
    maker_is_buy: NDArray[np.bool_],
    maker_fee_ppm: int = DEFAULT_MAKER_FEE_PPM,
    taker_fee_ppm: int = DEFAULT_TAKER_FEE_PPM,
) -> tuple[
    NDArray[np.int64],
    NDArray[np.int64],
    NDArray[np.int64],
    NDArray[np.int64],
    NDArray[np.int64],
    NDArray[np.int64],
]:
    """
    Settle matched trade fills with exact Double-Entry Conservation Law (Section 5.2):
        maker_quote_delta + taker_quote_delta + treasury_fee_delta == 0
        maker_base_delta + taker_base_delta == 0

    Returns:
        (notional_fp, maker_fee_fp, taker_fee_fp,
         maker_quote_delta_fp, taker_quote_delta_fp, treasury_fee_delta_fp)
    """
    notional_fp = compute_notional_fp_vec(price_fp, qty_fp)
    maker_fee_fp = compute_fee_fp_vec(notional_fp, maker_fee_ppm)
    taker_fee_fp = compute_fee_fp_vec(notional_fp, taker_fee_ppm)

    # If maker bought base, maker pays notional + maker_fee; if maker sold, receives notional - maker_fee
    maker_quote_gross = np.where(maker_is_buy, -notional_fp, notional_fp)
    taker_quote_gross = -maker_quote_gross

    maker_quote_delta_fp = maker_quote_gross - maker_fee_fp
    taker_quote_delta_fp = taker_quote_gross - taker_fee_fp
    treasury_fee_delta_fp = maker_fee_fp + taker_fee_fp

    return (
        notional_fp,
        maker_fee_fp,
        taker_fee_fp,
        maker_quote_delta_fp,
        taker_quote_delta_fp,
        treasury_fee_delta_fp,
    )
