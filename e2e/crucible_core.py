#!/usr/bin/env python3
"""
HYPERION Channel 10 — Deterministic Crucible Cryptographic & Fixed-Point Invariant Core.

Implements:
  1. Exact 128-bit Fixed-Point (scale = 1e8) conservation, notional, fee, and uncrossed-book verifiers.
  2. Canonical HTTP mutation signing (HMAC-SHA256 preimage) & nanosecond anti-replay window check.
  3. Sub-millisecond Little-Endian Binary WebSocket (0x4859 v1) frame codec + Castagnoli CRC-32C verifier.
  4. Cryptographic Blake3 / SHA-256 Merkle inclusion branch & Chronos Counterfactual Diff invariant verifier.
"""

from __future__ import annotations

import hashlib
import hmac
import struct
from dataclasses import dataclass

FP_SCALE: int = 100_000_000
PPM_SCALE: int = 1_000_000
BPS_SCALE: int = 10_000
MAX_CLOCK_SKEW_NS: int = 5_000_000_000  # 5 seconds strict anti-replay window

WS_MAGIC: int = 0x4859
WS_VERSION: int = 0x01
WS_HEADER_LEN: int = 32
WS_FLAG_CRC32C: int = 0x01
WS_FLAG_SNAPSHOT_RESYNC: int = 0x02
WS_FLAG_SLOW_CONSUMER_DROP: int = 0x04
WS_FLAG_REPLAY_TIMELINE: int = 0x08

# Pre-compiled C-struct layout for 32-byte Little-Endian Binary WS Header (Pass 3 optimization)
_WS_HDR_STRUCT = struct.Struct("<HBBBBHIIQQ")
_L2_TOP_STRUCT = struct.Struct("<QQHHI32s")
_L2_LVL_STRUCT = struct.Struct("<QQI")
_FILL_STRUCT = struct.Struct("<QQQQQQQB7s")
_CANDLE_STRUCT = struct.Struct("<BB6sQQQQQQQqqqQ")


def _build_crc32c_table() -> tuple[int, ...]:
    """Precompute Castagnoli CRC-32C (0x82F63B78 reflected polynomial) 256-entry lookup table."""
    poly = 0x82F63B78
    table: list[int] = []
    for i in range(256):
        crc = i
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ poly
            else:
                crc >>= 1
        table.append(crc & 0xFFFFFFFF)
    return tuple(table)


_CRC32C_TABLE: tuple[int, ...] = _build_crc32c_table()


def _build_crc32c_slice4_tables() -> tuple[tuple[int, ...], tuple[int, ...], tuple[int, ...], tuple[int, ...]]:
    """Build Slice-by-4 Castagnoli CRC-32C tables (T0..T3) for 4-byte word-at-a-time throughput."""
    t0 = list(_CRC32C_TABLE)
    t1 = [0] * 256
    t2 = [0] * 256
    t3 = [0] * 256
    for i in range(256):
        c1 = (t0[i] >> 8) ^ t0[t0[i] & 0xFF]
        c2 = (c1 >> 8) ^ t0[c1 & 0xFF]
        c3 = (c2 >> 8) ^ t0[c2 & 0xFF]
        t1[i] = c1 & 0xFFFFFFFF
        t2[i] = c2 & 0xFFFFFFFF
        t3[i] = c3 & 0xFFFFFFFF
    return tuple(t0), tuple(t1), tuple(t2), tuple(t3)


_CRC_T0, _CRC_T1, _CRC_T2, _CRC_T3 = _build_crc32c_slice4_tables()
_U32_STRUCT = struct.Struct("<I")


def crc32c(data: bytes | bytearray | memoryview) -> int:
    """Compute exact Castagnoli CRC-32C over byte-like buffer."""
    crc = 0xFFFFFFFF
    table = _CRC32C_TABLE
    for b in data:
        crc = table[(crc ^ b) & 0xFF] ^ (crc >> 8)
    return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF


def crc32c_slice4(
    buf: bytes | bytearray | memoryview,
    offset: int,
    length: int,
    *,
    t0=_CRC_T0,
    t1=_CRC_T1,
    t2=_CRC_T2,
    t3=_CRC_T3,
    unpack_u32=_U32_STRUCT.unpack_from,
) -> int:
    """Zero-copy Slice-by-4 Castagnoli CRC-32C processing 4 bytes per iteration with zero slicing."""
    crc = 0xFFFFFFFF
    pos = offset
    end4 = offset + (length & ~3)
    while pos < end4:
        word = unpack_u32(buf, pos)[0] ^ crc
        crc = (
            t3[word & 0xFF]
            ^ t2[(word >> 8) & 0xFF]
            ^ t1[(word >> 16) & 0xFF]
            ^ t0[(word >> 24) & 0xFF]
        )
        pos += 4
    end = offset + length
    while pos < end:
        crc = t0[(crc ^ buf[pos]) & 0xFF] ^ (crc >> 8)
        pos += 1
    return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF


def compute_notional_fp(price_fp: int, qty_fp: int) -> int:
    """Canonical floor((u128(price_fp) * u128(qty_fp)) / 100_000_000) with strict domain checks."""
    if price_fp < 0 or qty_fp <= 0:
        raise ValueError("price_fp must be >= 0 and qty_fp must be > 0")
    return (price_fp * qty_fp) // FP_SCALE


def compute_fee_fp(notional_fp: int, fee_ppm: int) -> int:
    """
    Canonical round_toward_zero((i128(notional_fp) * i128(fee_ppm)) / 1_000_000).
    Negative fee_ppm represents maker rebate; truncation is strictly toward zero.
    """
    if notional_fp < 0:
        raise ValueError("notional_fp must be >= 0")
    if fee_ppm < -PPM_SCALE or fee_ppm > PPM_SCALE:
        raise ValueError("fee_ppm out of valid [-1_000_000, +1_000_000] bounds")
    prod = notional_fp * abs(fee_ppm)
    mag = prod // PPM_SCALE
    return -mag if fee_ppm < 0 else mag


def verify_double_entry_conservation(
    *,
    account_balances: list[tuple[int, int]],
    treasury_fee_pool_fp: int,
    total_deposits_fp: int,
    total_withdrawals_fp: int = 0,
) -> bool:
    """
    Verify exact global conservation law down to 1e-8 atomic unit:
      Sum(available_fp + locked_fp) + treasury_fee_pool_fp == total_deposits_fp - total_withdrawals_fp
    Rejects any negative available or negative locked balance on user accounts.
    """
    if total_deposits_fp < 0 or total_withdrawals_fp < 0:
        return False
    if total_withdrawals_fp > total_deposits_fp:
        return False

    user_equity_sum = 0
    for avail_fp, locked_fp in account_balances:
        if avail_fp < 0 or locked_fp < 0:
            return False
        user_equity_sum += avail_fp + locked_fp

    lhs = user_equity_sum + treasury_fee_pool_fp
    rhs = total_deposits_fp - total_withdrawals_fp
    return lhs == rhs


def verify_uncrossed_l2_book(bids_fp: list[tuple[int, int]], asks_fp: list[tuple[int, int]]) -> bool:
    """
    Verify Level-2 book invariants:
      1. Bids strictly descending by price_fp, all quantities > 0, prices > 0.
      2. Asks strictly ascending by price_fp, all quantities > 0, prices > 0.
      3. BestBid < BestAsk whenever both sides are non-empty (Uncrossed Book Law).
    """
    prev_bid: int | None = None
    for price_fp, qty_fp in bids_fp:
        if price_fp <= 0 or qty_fp <= 0:
            return False
        if prev_bid is not None and price_fp >= prev_bid:
            return False
        prev_bid = price_fp

    prev_ask: int | None = None
    for price_fp, qty_fp in asks_fp:
        if price_fp <= 0 or qty_fp <= 0:
            return False
        if prev_ask is not None and price_fp <= prev_ask:
            return False
        prev_ask = price_fp

    if bids_fp and asks_fp:
        if bids_fp[0][0] >= asks_fp[0][0]:
            return False
    return True


def build_canonical_preimage(
    timestamp_ns: int,
    nonce: int,
    method: str,
    request_path: str,
    raw_body: bytes,
) -> bytes:
    """Construct canonical UTF-8 signing preimage per 00-hyperion-contract.md §6.2."""
    if timestamp_ns <= 0 or nonce < 0:
        raise ValueError("timestamp_ns must be > 0 and nonce >= 0")
    body_hex = hashlib.sha256(raw_body).hexdigest()
    preimage = f"{timestamp_ns}\n{nonce}\n{method.upper()}\n{request_path}\n{body_hex}"
    return preimage.encode("utf-8")


def sign_hmac_sha256_hex(
    secret: bytes,
    timestamp_ns: int,
    nonce: int,
    method: str,
    request_path: str,
    raw_body: bytes,
) -> str:
    preimage = build_canonical_preimage(timestamp_ns, nonce, method, request_path, raw_body)
    return hmac.new(secret, preimage, hashlib.sha256).hexdigest()


def verify_timestamp_within_window(client_ts_ns: int, server_now_ns: int, max_skew_ns: int = MAX_CLOCK_SKEW_NS) -> bool:
    """Strict inclusive anti-replay window check: |server_now_ns - client_ts_ns| <= max_skew_ns."""
    if client_ts_ns <= 0 or server_now_ns <= 0 or max_skew_ns < 0:
        return False
    return abs(server_now_ns - client_ts_ns) <= max_skew_ns


@dataclass(slots=True, frozen=True)
class DecodedWsHeader:
    magic: int
    version: int
    msg_type: int
    flags: int
    instrument_id: int
    payload_len: int
    crc32c_digest: int
    engine_seq: int
    timestamp_ns: int


def encode_ws_binary_frame(
    msg_type: int,
    instrument_id: int,
    engine_seq: int,
    timestamp_ns: int,
    payload: bytes,
    *,
    with_crc32c: bool = True,
    extra_flags: int = 0,
) -> bytes:
    flags = extra_flags | (WS_FLAG_CRC32C if with_crc32c else 0)
    digest = crc32c(payload) if with_crc32c else 0
    hdr = _WS_HDR_STRUCT.pack(
        WS_MAGIC,
        WS_VERSION,
        msg_type & 0xFF,
        flags & 0xFF,
        0,
        instrument_id & 0xFFFF,
        len(payload),
        digest,
        engine_seq,
        timestamp_ns,
    )
    return hdr + payload


def decode_ws_binary_frame(frame: bytes | bytearray | memoryview) -> tuple[DecodedWsHeader, memoryview]:
    """Zero-copy Little-Endian Binary WebSocket v1 frame decoder & CRC-32C validator."""
    mv = frame if isinstance(frame, memoryview) else memoryview(frame)
    total_len = len(mv)
    if total_len < WS_HEADER_LEN:
        raise ValueError(f"Frame shorter than 32-byte header: {total_len}")

    (
        magic,
        version,
        msg_type,
        flags,
        reserved,
        instrument_id,
        payload_len,
        crc_digest,
        engine_seq,
        timestamp_ns,
    ) = _WS_HDR_STRUCT.unpack_from(mv, 0)

    if magic != WS_MAGIC:
        raise ValueError(f"Invalid WS magic: 0x{magic:04x}")
    if version != WS_VERSION:
        raise ValueError(f"Unsupported WS version: {version}")
    if reserved != 0:
        raise ValueError(f"Non-zero reserved header byte: {reserved}")
    if total_len != WS_HEADER_LEN + payload_len:
        raise ValueError(f"Frame length mismatch: got {total_len}, expected {WS_HEADER_LEN + payload_len}")

    payload_view = mv[WS_HEADER_LEN:]
    if flags & WS_FLAG_CRC32C:
        computed = crc32c(payload_view)
        if computed != crc_digest:
            raise ValueError(f"CRC32C mismatch: header=0x{crc_digest:08x} computed=0x{computed:08x}")

    header = DecodedWsHeader(
        magic=magic,
        version=version,
        msg_type=msg_type,
        flags=flags,
        instrument_id=instrument_id,
        payload_len=payload_len,
        crc32c_digest=crc_digest,
        engine_seq=engine_seq,
        timestamp_ns=timestamp_ns,
    )
    return header, payload_view


def verify_counterfactual_diff_invariants(
    *,
    is_noop_override: bool,
    baseline_root_hex: str,
    forked_root_hex: str,
    state_identical: bool,
    clearing_price_delta_fp: int,
    prevented_wash_volume_fp: int,
    account_deltas_fp: list[int],
    treasury_fee_delta_fp: int,
) -> bool:
    """
    Validate Chronos Counterfactual Fork mathematical invariants:
      1. Both roots must be valid 64-char hex digests.
      2. If noop override: roots MUST match, state_identical MUST be True, and all deltas == 0.
      3. Closed-system conservation across fork: Sum(account_deltas_fp) + treasury_fee_delta_fp == 0
         when no external deposit/withdrawal override was injected.
      4. prevented_wash_volume_fp must never be negative.
    """
    if len(baseline_root_hex) != 64 or len(forked_root_hex) != 64:
        return False
    if prevented_wash_volume_fp < 0:
        return False

    roots_equal = baseline_root_hex.lower() == forked_root_hex.lower()
    if state_identical != roots_equal:
        return False

    if is_noop_override:
        if not roots_equal or not state_identical:
            return False
        if clearing_price_delta_fp != 0 or prevented_wash_volume_fp != 0 or treasury_fee_delta_fp != 0:
            return False
        if any(d != 0 for d in account_deltas_fp):
            return False

    if sum(account_deltas_fp) + treasury_fee_delta_fp != 0:
        return False

    return True


# -----------------------------------------------------------------------------
# 3-Pass Micro-Optimization Implementations for Hot-Path Frame + Invariant Loop
# -----------------------------------------------------------------------------

def hotpath_pass1_baseline(raw_frame: bytes, balances: list[tuple[int, int]], treasury_fp: int, deposits_fp: int) -> bool:
    """Pass 1 (Baseline): Byte slicing copies + int.from_bytes + generator comprehension."""
    if len(raw_frame) < 32:
        return False
    magic = int.from_bytes(raw_frame[0:2], "little")
    version = int.from_bytes(raw_frame[2:3], "little")
    flags = int.from_bytes(raw_frame[4:5], "little")
    reserved = int.from_bytes(raw_frame[5:6], "little")
    payload_len = int.from_bytes(raw_frame[8:12], "little")
    crc_hdr = int.from_bytes(raw_frame[12:16], "little")
    if magic != WS_MAGIC or version != WS_VERSION or reserved != 0:
        return False
    payload_copy = bytes(raw_frame[32:])
    if len(payload_copy) != payload_len:
        return False
    if (flags & WS_FLAG_CRC32C) and crc32c(payload_copy) != crc_hdr:
        return False
    total_eq = sum(a + l for a, l in balances)
    return (total_eq + treasury_fp) == deposits_fp


def hotpath_pass2_zerocopy(mv_frame: memoryview, balances: list[tuple[int, int]], treasury_fp: int, deposits_fp: int) -> bool:
    """Pass 2 (Zero-Copy Slice): struct.unpack_from + zero-copy memoryview slice."""
    magic, version, _mt, flags, reserved, _inst, payload_len, crc_hdr, _seq, _ts = struct.unpack_from(
        "<HBBBBHIIQQ", mv_frame, 0
    )
    if magic != WS_MAGIC or version != WS_VERSION or reserved != 0:
        return False
    if len(mv_frame) - 32 != payload_len:
        return False
    if (flags & WS_FLAG_CRC32C) and crc32c(mv_frame[32:]) != crc_hdr:
        return False
    acc = 0
    for a, l in balances:
        acc += a + l
    return (acc + treasury_fp) == deposits_fp


def hotpath_pass3_precompiled_flat(
    mv_frame: memoryview,
    flat_balances: tuple[int, ...],
    treasury_fp: int,
    deposits_fp: int,
    *,
    unpack_hdr=_WS_HDR_STRUCT.unpack_from,
    crc_fast=crc32c_slice4,
    _sum=sum,
) -> bool:
    """Pass 3 (Final): Bound precompiled Struct.unpack_from + Slice-by-4 zero-slice CRC32C + C-level flat tuple sum."""
    magic, version, _mt, flags, reserved, _inst, payload_len, crc_hdr, _seq, _ts = unpack_hdr(mv_frame, 0)
    if magic != 0x4859 or version != 1 or reserved != 0 or (len(mv_frame) - 32) != payload_len:
        return False
    if (flags & 1) and crc_fast(mv_frame, 32, payload_len) != crc_hdr:
        return False
    return (_sum(flat_balances) + treasury_fp) == deposits_fp


# -----------------------------------------------------------------------------
# High-Level Crucible Orchestrator Helpers (used by e2e/run_crucible.py)
# -----------------------------------------------------------------------------

def check_conservation_law(
    total_deposits_fp: int,
    total_withdrawals_fp: int,
    total_available_fp: int,
    total_locked_fp: int,
    treasury_fee_pool_fp: int,
) -> bool:
    """Scalar convenience wrapper over verify_double_entry_conservation for system-wide proof."""
    return verify_double_entry_conservation(
        account_balances=[(total_available_fp, total_locked_fp)],
        treasury_fee_pool_fp=treasury_fee_pool_fp,
        total_deposits_fp=total_deposits_fp,
        total_withdrawals_fp=total_withdrawals_fp,
    )


def sign_canonical_hmac(
    secret: bytes,
    timestamp_ns: int,
    nonce: int,
    method: str,
    request_path: str,
    raw_body: bytes,
) -> str:
    """Canonical HMAC-SHA256 signature hex for gateway-go authenticated mutations."""
    return sign_hmac_sha256_hex(secret, timestamp_ns, nonce, method, request_path, raw_body)


def verify_binary_ws_header(frame: bytes | bytearray | memoryview) -> bool:
    """Return True iff frame passes full 32-byte 0x4859 v1 header and CRC-32C validation."""
    try:
        decode_ws_binary_frame(frame)
        return True
    except ValueError:
        return False


def compute_percentile(samples: list[float], pct: float) -> float:
    """Deterministic nearest-rank / linear interpolated percentile over positive latency samples."""
    if not samples:
        return 0.0
    ordered = sorted(samples)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = (max(0.0, min(100.0, pct)) / 100.0) * (len(ordered) - 1)
    lo = int(rank)
    hi = min(len(ordered) - 1, lo + 1)
    frac = rank - lo
    return float(ordered[lo] + (ordered[hi] - ordered[lo]) * frac)


def format_fp_human(value_fp: int, decimals: int = 4) -> str:
    """Format 1e8 fixed-point integer deterministically without IEEE-754 float conversion."""
    sign = "-" if value_fp < 0 else ""
    mag = abs(value_fp)
    whole = mag // FP_SCALE
    rem = mag % FP_SCALE
    frac_full = f"{rem:08d}"
    if decimals <= 0:
        return f"{sign}{whole:,}"
    return f"{sign}{whole:,}.{frac_full[:decimals]}"
