"""HYPERION canonical Protobuf v3 bindings and 1e8 fixed-point helpers."""

from __future__ import annotations

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


__all__ = [
    "FP_SCALE",
    "PPM_SCALE",
    "WS_FRAME_MAGIC",
    "WS_FRAME_VERSION",
    "chronos_pb2",
    "chronos_pb2_grpc",
    "common_pb2",
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
