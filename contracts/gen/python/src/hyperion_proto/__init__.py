"""HYPERION canonical Protobuf v3 bindings and 1e8 fixed-point helpers."""

from __future__ import annotations

from itertools import repeat
from operator import floordiv, mul
from typing import MutableSequence, Sequence

from . import (
    chronos_pb2,
    chronos_pb2_grpc,
    common_pb2,
    orderbook_pb2,
    orderbook_pb2_grpc,
    stream_analytics_pb2,
    stream_analytics_pb2_grpc,
    telemetry_fraud_pb2,
    telemetry_fraud_pb2_grpc,
)

FP_SCALE: int = 100_000_000
PPM_SCALE: int = 1_000_000
WS_FRAME_MAGIC: int = 0x4859
WS_FRAME_VERSION: int = 1
MASK_64: int = (1 << 64) - 1
MAX_U128: int = (1 << 128) - 1
MIN_I128: int = -(1 << 127)
MAX_I128: int = (1 << 127) - 1
MIN_I64: int = -(1 << 63)
MAX_I64: int = (1 << 63) - 1


def to_u128(value: int) -> common_pb2.U128:
    if value < 0 or value > MAX_U128:
        raise ValueError(f"u128 out of range: {value}")
    return common_pb2.U128(hi=(value >> 64) & MASK_64, lo=value & MASK_64)


def from_u128(msg: common_pb2.U128) -> int:
    return ((int(msg.hi) & MASK_64) << 64) | (int(msg.lo) & MASK_64)


def to_i128(value: int) -> common_pb2.I128:
    if value < MIN_I128 or value > MAX_I128:
        raise ValueError(f"i128 out of range: {value}")
    hi_raw = value >> 64
    lo_raw = value & MASK_64
    return common_pb2.I128(hi=hi_raw, lo=lo_raw)


def from_i128(msg: common_pb2.I128) -> int:
    return (int(msg.hi) << 64) | (int(msg.lo) & MASK_64)


def compute_notional_fp(price_fp: int, qty_fp: int) -> int:
    if price_fp < 0 or qty_fp < 0:
        raise ValueError("price_fp and qty_fp must be non-negative fixed-point integers")
    return (price_fp * qty_fp) // FP_SCALE


def checked_fee_fp(notional_fp: int, fee_ppm: int) -> int:
    """Computes exact round-toward-zero (notional_fp * fee_ppm) / 1e6 in signed int64 range."""
    if notional_fp < 0 or notional_fp > MAX_U128:
        raise ValueError(f"notional_fp out of u128 range: {notional_fp}")
    if fee_ppm < MIN_I64 or fee_ppm > MAX_I64:
        raise ValueError(f"fee_ppm out of i64 range: {fee_ppm}")
    prod = notional_fp * fee_ppm
    if prod < 0:
        fee = -((-prod) // PPM_SCALE)
    else:
        fee = prod // PPM_SCALE
    if fee < MIN_I64 or fee > MAX_I64:
        raise OverflowError(f"fee_fp overflows int64: {fee}")
    return fee


def compute_notional_batch_zero_copy(
    prices: Sequence[int], qtys: Sequence[int], out: MutableSequence[int]
) -> int:
    """Pass 2 (Vectorized C-Iterator / Zero-Copy): Writes exact floor((p*q)/1e8) into caller buffer."""
    n = min(len(prices), len(qtys), len(out))
    if n == 0:
        return 0
    if min(prices[:n]) < 0 or min(qtys[:n]) < 0:
        raise ValueError("negative fixed-point input")
    out[:n] = map(floordiv, map(mul, prices[:n], qtys[:n]), repeat(FP_SCALE, n))
    return n


def allocate_aligned64_u64(length: int):
    """Allocates a 64-byte cache-line aligned NumPy uint64 array (`ptr % 64 == 0`)."""
    import numpy as np

    if length < 0:
        raise ValueError("length must be non-negative")
    raw = np.empty(length * 8 + 64, dtype=np.uint8)
    offset = (-raw.ctypes.data) & 63
    view = raw[offset : offset + length * 8].view(np.uint64)
    return view


def compute_notional_batch_aligned64_u64(prices_u64, qtys_u64, out_u64) -> int:
    """Pass 3 (Cache-Line Aligned 64B SIMD In-Place): Exact integer SIMD u64 multiply & floor_divide."""
    import numpy as np

    n = min(len(prices_u64), len(qtys_u64), len(out_u64))
    if n == 0:
        return 0
    np.multiply(prices_u64[:n], qtys_u64[:n], out=out_u64[:n])
    np.floor_divide(out_u64[:n], np.uint64(FP_SCALE), out=out_u64[:n])
    return n


__all__ = [
    "FP_SCALE",
    "PPM_SCALE",
    "WS_FRAME_MAGIC",
    "WS_FRAME_VERSION",
    "allocate_aligned64_u64",
    "checked_fee_fp",
    "chronos_pb2",
    "chronos_pb2_grpc",
    "common_pb2",
    "compute_notional_batch_aligned64_u64",
    "compute_notional_batch_zero_copy",
    "compute_notional_fp",
    "from_i128",
    "from_u128",
    "orderbook_pb2",
    "orderbook_pb2_grpc",
    "stream_analytics_pb2",
    "stream_analytics_pb2_grpc",
    "telemetry_fraud_pb2",
    "telemetry_fraud_pb2_grpc",
    "to_i128",
    "to_u128",
]
