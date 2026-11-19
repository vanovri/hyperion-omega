"""Fixed-point integer arithmetic invariants for HYPERION Sentinel-ML.

All monetary prices, asset quantities, and notional values strictly use 64-bit / 128-bit
fixed-point integers scaled by 10^8 (100_000_000). Floating-point types are strictly
prohibited for representing prices, quantities, or balances.
Dimensionless ratios/probabilities in Protobuf use integer parts-per-million (10^6, _ppm)
or basis points (10^4, _bps).
"""

from __future__ import annotations

SCALE: int = 100_000_000  # 1e8 fixed-point scale
PPM_SCALE: int = 1_000_000  # 1e6 parts-per-million scale
BPS_SCALE: int = 10_000  # 1e4 basis-points scale

UINT64_MAX: int = (1 << 64) - 1
INT64_MIN: int = -(1 << 63)
INT64_MAX: int = (1 << 63) - 1
UINT128_MAX: int = (1 << 128) - 1
INT128_MIN: int = -(1 << 127)
INT128_MAX: int = (1 << 127) - 1
MASK_64: int = (1 << 64) - 1


class FixedPointViolationError(TypeError):
    """Raised when a float or invalid type is passed for a fixed-point monetary field."""


def require_fp_int(
    value: int,
    field_name: str,
    *,
    allow_zero: bool = True,
    allow_negative: bool = False,
) -> int:
    """Validate that a monetary or quantity field is a strict Python int (never float/bool)."""
    if isinstance(value, bool) or not isinstance(value, int):
        raise FixedPointViolationError(
            f"{field_name} must be fixed-point int (scale 1e8), "
            f"got {type(value).__name__}: {value!r}"
        )
    if not allow_negative and value < 0:
        raise ValueError(f"{field_name} cannot be negative: {value}")
    if not allow_zero and value == 0:
        raise ValueError(f"{field_name} must be strictly positive (> 0), got 0")
    if value < INT128_MIN or value > UINT128_MAX:
        raise OverflowError(f"{field_name} exceeds 128-bit fixed-point bounds: {value}")
    return value


def require_uint64(value: int, field_name: str, *, allow_zero: bool = True) -> int:
    """Validate unsigned 64-bit integer bounds."""
    require_fp_int(value, field_name, allow_zero=allow_zero, allow_negative=False)
    if value > UINT64_MAX:
        raise OverflowError(f"{field_name} exceeds uint64 bounds: {value}")
    return value


def require_int64(
    value: int,
    field_name: str,
    *,
    allow_zero: bool = True,
    allow_negative: bool = True,
) -> int:
    """Validate signed 64-bit integer bounds."""
    require_fp_int(value, field_name, allow_zero=allow_zero, allow_negative=allow_negative)
    if value < INT64_MIN or value > INT64_MAX:
        raise OverflowError(f"{field_name} exceeds int64 bounds: {value}")
    return value


def compute_notional_fp(price_fp: int, qty_fp: int) -> int:
    """Compute exact 128-bit unsigned fixed-point notional: (price_fp * qty_fp) // 10^8."""
    require_uint64(price_fp, "price_fp", allow_zero=False)
    require_uint64(qty_fp, "qty_fp", allow_zero=False)
    return (price_fp * qty_fp) // SCALE


def distance_bps_int(price_a_fp: int, price_b_fp: int) -> int:
    """Compute integer basis-point distance |a - b| * 10_000 // b without floats."""
    require_uint64(price_a_fp, "price_a_fp", allow_zero=True)
    require_uint64(price_b_fp, "price_b_fp", allow_zero=False)
    return (abs(price_a_fp - price_b_fp) * BPS_SCALE) // price_b_fp


def int_to_u128_parts(value: int) -> tuple[int, int]:
    """Split an unsigned 128-bit Python int into (hi: uint64, lo: uint64)."""
    require_fp_int(value, "u128_value", allow_zero=True, allow_negative=False)
    if value > UINT128_MAX:
        raise OverflowError(f"Value exceeds U128 max: {value}")
    hi = (value >> 64) & MASK_64
    lo = value & MASK_64
    return hi, lo


def u128_parts_to_int(hi: int, lo: int) -> int:
    """Reconstruct an unsigned 128-bit Python int from (hi: uint64, lo: uint64)."""
    require_uint64(hi, "hi", allow_zero=True)
    require_uint64(lo, "lo", allow_zero=True)
    return (hi << 64) | lo


def int_to_i128_parts(value: int) -> tuple[int, int]:
    """Split a signed two's-complement 128-bit Python int into (hi: int64, lo: uint64)."""
    require_fp_int(value, "i128_value", allow_zero=True, allow_negative=True)
    if value < INT128_MIN or value > INT128_MAX:
        raise OverflowError(f"Value exceeds I128 bounds: {value}")
    raw_hi = (value >> 64) & MASK_64
    hi = raw_hi - (1 << 64) if raw_hi >= (1 << 63) else raw_hi
    lo = value & MASK_64
    return hi, lo


def i128_parts_to_int(hi: int, lo: int) -> int:
    """Reconstruct a signed 128-bit Python int from (hi: int64, lo: uint64)."""
    require_int64(hi, "hi", allow_zero=True, allow_negative=True)
    require_uint64(lo, "lo", allow_zero=True)
    return (hi << 64) + lo


def ratio_to_ppm(numerator: float, denominator: float = 1.0, *, clamp_unit: bool = False) -> int:
    """Convert a dimensionless ratio into integer parts-per-million (scale 1e6)."""
    val = 0.0 if denominator == 0.0 else numerator / denominator
    if clamp_unit:
        val = max(0.0, min(1.0, val))
    return int(round(val * PPM_SCALE))


def scalar_to_fp_int64(val: float) -> int:
    """Convert a dimensionless ML feature scalar to signed scale-1e8 int64 for SHAP audit."""
    scaled = int(round(val * SCALE))
    return max(INT64_MIN, min(INT64_MAX, scaled))


def parse_decimal_to_fp(decimal_str: str) -> int:
    """Deterministically parse a decimal string (e.g. '42150.12500000') into scale-1e8 int."""
    if not isinstance(decimal_str, str) or not decimal_str.strip():
        raise ValueError(f"Invalid decimal string: {decimal_str!r}")
    s = decimal_str.strip()
    sign = -1 if s.startswith("-") else 1
    if s[0] in ("+", "-"):
        s = s[1:]
    whole_part, frac_part = s.split(".", 1) if "." in s else (s, "")
    if not whole_part.isdigit() or (frac_part and not frac_part.isdigit()):
        raise ValueError(f"Non-numeric decimal string: {decimal_str!r}")
    frac_padded = frac_part.ljust(8, "0")[:8]
    return sign * (int(whole_part) * SCALE + int(frac_padded))


def format_fp_to_decimal(value_fp: int) -> str:
    """Format a scale-1e8 integer back into an exact 8-decimal string."""
    require_fp_int(value_fp, "value_fp", allow_zero=True, allow_negative=True)
    sign = "-" if value_fp < 0 else ""
    abs_val = abs(value_fp)
    whole = abs_val // SCALE
    frac = abs_val % SCALE
    return f"{sign}{whole}.{frac:08d}"
