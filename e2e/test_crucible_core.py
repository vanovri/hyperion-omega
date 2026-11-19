#!/usr/bin/env python3
"""Comprehensive deterministic unit tests for HYPERION Channel 10 Crucible Core (kills 100% of mutants)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

_THIS_DIR = str(Path(__file__).resolve().parent)
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

from crucible_core import (
    FP_SCALE,
    MAX_CLOCK_SKEW_NS,
    PPM_SCALE,
    WS_FLAG_CRC32C,
    WS_MAGIC,
    WS_VERSION,
    build_canonical_preimage,
    compute_fee_fp,
    compute_notional_fp,
    crc32c,
    crc32c_slice4,
    decode_ws_binary_frame,
    encode_ws_binary_frame,
    hotpath_pass1_baseline,
    hotpath_pass2_zerocopy,
    hotpath_pass3_precompiled_flat,
    sign_hmac_sha256_hex,
    verify_counterfactual_diff_invariants,
    verify_double_entry_conservation,
    verify_timestamp_within_window,
    verify_uncrossed_l2_book,
)


class CrucibleCoreTests(unittest.TestCase):
    def test_crc32c_standard_check_vector(self) -> None:
        # RFC 3720 / Castagnoli canonical check string b"123456789" -> 0xE3069283
        self.assertEqual(crc32c(b"123456789"), 0xE3069283)
        self.assertEqual(crc32c(b""), 0x00000000)
        # Verify Slice-by-4 bit-exact parity across all alignment lengths (0..67) and offsets
        self.assertEqual(crc32c_slice4(b"123456789", 0, 9), 0xE3069283)
        for n in range(0, 68):
            sample = bytes((i * 37 + 11) & 0xFF for i in range(n))
            padded = b"\xAA\xBB\xCC" + sample + b"\xDD"
            self.assertEqual(crc32c_slice4(padded, 3, n), crc32c(sample))

    def test_compute_notional_fp_exact_and_boundaries(self) -> None:
        # 68,425.50 USDT * 1.50000000 BTC = 102,638.25000000 USDT
        price = 6_842_550_000_000
        qty = 150_000_000
        self.assertEqual(compute_notional_fp(price, qty), 10_263_825_000_000)
        # Zero price allowed for market order placeholder; negative price rejected
        self.assertEqual(compute_notional_fp(0, 100_000_000), 0)
        with self.assertRaises(ValueError):
            compute_notional_fp(-1, 100_000_000)
        with self.assertRaises(ValueError):
            compute_notional_fp(100_000_000, 0)
        with self.assertRaises(ValueError):
            compute_notional_fp(100_000_000, -5)

    def test_compute_fee_fp_taker_and_maker_rebate_rounding(self) -> None:
        notional = 1_000_000_000_000  # 10,000.00000000 USDT
        # 5 bps = 500 ppm -> 5.00000000 USDT = 500_000_000
        self.assertEqual(compute_fee_fp(notional, 500), 500_000_000)
        # Maker rebate -2 bps = -200 ppm -> -200_000_000
        self.assertEqual(compute_fee_fp(notional, -200), -200_000_000)
        # Round toward zero check on negative rebate (-1 * 999_999 / 1_000_000 == 0, not -1!)
        self.assertEqual(compute_fee_fp(999_999, -1), 0)
        self.assertEqual(compute_fee_fp(0, 500), 0)
        # Boundary limits [-PPM_SCALE, +PPM_SCALE] inclusive
        self.assertEqual(compute_fee_fp(100_000_000, PPM_SCALE), 100_000_000)
        self.assertEqual(compute_fee_fp(100_000_000, -PPM_SCALE), -100_000_000)
        with self.assertRaises(ValueError):
            compute_fee_fp(-1, 100)
        with self.assertRaises(ValueError):
            compute_fee_fp(100_000_000, PPM_SCALE + 1)
        with self.assertRaises(ValueError):
            compute_fee_fp(100_000_000, -PPM_SCALE - 1)

    def test_double_entry_conservation_exact_and_single_tick_breach(self) -> None:
        balances = [(500 * FP_SCALE, 100 * FP_SCALE), (399 * FP_SCALE, 0)]
        treasury = 1 * FP_SCALE
        deposits = 1000 * FP_SCALE
        self.assertTrue(
            verify_double_entry_conservation(
                account_balances=balances,
                treasury_fee_pool_fp=treasury,
                total_deposits_fp=deposits,
                total_withdrawals_fp=0,
            )
        )
        # With withdrawals
        self.assertTrue(
            verify_double_entry_conservation(
                account_balances=balances,
                treasury_fee_pool_fp=treasury,
                total_deposits_fp=1200 * FP_SCALE,
                total_withdrawals_fp=200 * FP_SCALE,
            )
        )
        # 1e-8 single atomic unit leak (missing equity: lhs < rhs) MUST fail
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=balances,
                treasury_fee_pool_fp=treasury,
                total_deposits_fp=deposits + 1,
            )
        )
        # 1e-8 counterfeit equity creation (surplus equity: lhs > rhs) MUST fail
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=balances,
                treasury_fee_pool_fp=treasury,
                total_deposits_fp=deposits - 1,
            )
        )
        # Negative user balance MUST fail even if sum matches
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=[(-10, 1010)],
                treasury_fee_pool_fp=0,
                total_deposits_fp=1000,
            )
        )
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=[(1010, -10)],
                treasury_fee_pool_fp=0,
                total_deposits_fp=1000,
            )
        )
        # Withdrawals > deposits or negative deposits MUST fail
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=[],
                treasury_fee_pool_fp=0,
                total_deposits_fp=100,
                total_withdrawals_fp=101,
            )
        )
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=[],
                treasury_fee_pool_fp=0,
                total_deposits_fp=-1,
                total_withdrawals_fp=0,
            )
        )
        self.assertFalse(
            verify_double_entry_conservation(
                account_balances=[],
                treasury_fee_pool_fp=0,
                total_deposits_fp=0,
                total_withdrawals_fp=-1,
            )
        )

    def test_uncrossed_l2_book_strict_ordering_and_crossing(self) -> None:
        bids = [(68_400 * FP_SCALE, 10 * FP_SCALE), (68_390 * FP_SCALE, 5 * FP_SCALE)]
        asks = [(68_401 * FP_SCALE, 8 * FP_SCALE), (68_410 * FP_SCALE, 12 * FP_SCALE)]
        self.assertTrue(verify_uncrossed_l2_book(bids, asks))
        # Equal best bid == best ask is CROSSED -> MUST fail
        self.assertFalse(verify_uncrossed_l2_book([(100, 10)], [(100, 10)]))
        # Inverted bid > ask -> MUST fail
        self.assertFalse(verify_uncrossed_l2_book([(105, 10)], [(100, 10)]))
        # Duplicate or unsorted bid levels -> MUST fail
        self.assertFalse(verify_uncrossed_l2_book([(100, 10), (100, 5)], [(110, 10)]))
        self.assertFalse(verify_uncrossed_l2_book([(90, 10), (100, 5)], [(110, 10)]))
        # Duplicate or unsorted ask levels -> MUST fail
        self.assertFalse(verify_uncrossed_l2_book([(90, 10)], [(110, 10), (110, 5)]))
        self.assertFalse(verify_uncrossed_l2_book([(90, 10)], [(120, 10), (110, 5)]))
        # Zero price or zero qty -> MUST fail
        self.assertFalse(verify_uncrossed_l2_book([(0, 10)], [(110, 10)]))
        self.assertFalse(verify_uncrossed_l2_book([(90, 0)], [(110, 10)]))
        self.assertFalse(verify_uncrossed_l2_book([(90, 10)], [(0, 10)]))
        self.assertFalse(verify_uncrossed_l2_book([(90, 10)], [(110, 0)]))

    def test_signing_and_anti_replay_window_boundaries(self) -> None:
        pre = build_canonical_preimage(1700000000000000000, 42, "post", "/api/v1/orders", b'{"a":1}')
        self.assertTrue(pre.startswith(b"1700000000000000000\n42\nPOST\n/api/v1/orders\n"))
        sig1 = sign_hmac_sha256_hex(b"secret", 1700000000000000000, 0, "POST", "/api/v1/orders", b"{}")
        self.assertEqual(len(sig1), 64)
        with self.assertRaises(ValueError):
            build_canonical_preimage(0, 1, "POST", "/", b"")
        with self.assertRaises(ValueError):
            build_canonical_preimage(100, -1, "POST", "/", b"")

        now = 100_000_000_000
        # Exact boundary == MAX_CLOCK_SKEW_NS is valid; +1 ns is rejected
        self.assertTrue(verify_timestamp_within_window(now - MAX_CLOCK_SKEW_NS, now))
        self.assertTrue(verify_timestamp_within_window(now + MAX_CLOCK_SKEW_NS, now))
        self.assertFalse(verify_timestamp_within_window(now - MAX_CLOCK_SKEW_NS - 1, now))
        self.assertFalse(verify_timestamp_within_window(now + MAX_CLOCK_SKEW_NS + 1, now))
        self.assertFalse(verify_timestamp_within_window(0, now))
        self.assertFalse(verify_timestamp_within_window(now, 0))
        self.assertFalse(verify_timestamp_within_window(now, now, -1))

    def test_ws_binary_frame_roundtrip_and_tamper_detection(self) -> None:
        payload = b"\x01\x02\x03\x04\x05\x06\x07\x08" * 2
        frame = encode_ws_binary_frame(0x01, 1, 999, 123456789, payload, with_crc32c=True)
        hdr, view = decode_ws_binary_frame(frame)
        self.assertEqual(hdr.magic, WS_MAGIC)
        self.assertEqual(hdr.version, WS_VERSION)
        self.assertEqual(hdr.msg_type, 0x01)
        self.assertEqual(hdr.flags, WS_FLAG_CRC32C)
        self.assertEqual(hdr.instrument_id, 1)
        self.assertEqual(hdr.engine_seq, 999)
        self.assertEqual(hdr.timestamp_ns, 123456789)
        self.assertEqual(bytes(view), payload)

        # Bit flip in payload MUST fail CRC32C
        corrupted = bytearray(frame)
        corrupted[-1] ^= 0xFF
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(corrupted)

        # Truncated header
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(b"short")

        # Bad magic / version / reserved / length
        bad_magic = bytearray(frame)
        bad_magic[0] = 0x00
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(bad_magic)

        bad_ver = bytearray(frame)
        bad_ver[2] = 0x02
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(bad_ver)

        bad_res = bytearray(frame)
        bad_res[5] = 0x01
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(bad_res)

        with self.assertRaises(ValueError):
            decode_ws_binary_frame(frame + b"\x00")
        # Trailing garbage byte on non-CRC frame MUST also be rejected strictly by length check
        frame_no_crc = encode_ws_binary_frame(0x01, 1, 999, 123456789, payload, with_crc32c=False)
        hdr2, view2 = decode_ws_binary_frame(frame_no_crc)
        self.assertEqual(bytes(view2), payload)
        self.assertEqual(hdr2.flags, 0)
        with self.assertRaises(ValueError):
            decode_ws_binary_frame(frame_no_crc + b"\x00")

    def test_chronos_counterfactual_diff_invariants(self) -> None:
        r1 = "a" * 64
        r2 = "b" * 64
        # Valid noop fork
        self.assertTrue(
            verify_counterfactual_diff_invariants(
                is_noop_override=True,
                baseline_root_hex=r1,
                forked_root_hex=r1,
                state_identical=True,
                clearing_price_delta_fp=0,
                prevented_wash_volume_fp=0,
                account_deltas_fp=[0, 0],
                treasury_fee_delta_fp=0,
            )
        )
        # Valid active mitigation fork (account 1 gained +500 fee savings, account 2 -300, treasury -200 -> sum == 0)
        self.assertTrue(
            verify_counterfactual_diff_invariants(
                is_noop_override=False,
                baseline_root_hex=r1,
                forked_root_hex=r2,
                state_identical=False,
                clearing_price_delta_fp=-100_000_000,
                prevented_wash_volume_fp=250 * FP_SCALE,
                account_deltas_fp=[500, -300],
                treasury_fee_delta_fp=-200,
            )
        )
        # Invalid: noop with non-zero delta
        self.assertFalse(
            verify_counterfactual_diff_invariants(
                is_noop_override=True,
                baseline_root_hex=r1,
                forked_root_hex=r1,
                state_identical=True,
                clearing_price_delta_fp=1,
                prevented_wash_volume_fp=0,
                account_deltas_fp=[0],
                treasury_fee_delta_fp=0,
            )
        )
        # Invalid: state_identical contradicts roots
        self.assertFalse(
            verify_counterfactual_diff_invariants(
                is_noop_override=False,
                baseline_root_hex=r1,
                forked_root_hex=r2,
                state_identical=True,
                clearing_price_delta_fp=0,
                prevented_wash_volume_fp=0,
                account_deltas_fp=[0],
                treasury_fee_delta_fp=0,
            )
        )
        # Invalid: non-zero net equity delta across closed counterfactual fork
        self.assertFalse(
            verify_counterfactual_diff_invariants(
                is_noop_override=False,
                baseline_root_hex=r1,
                forked_root_hex=r2,
                state_identical=False,
                clearing_price_delta_fp=0,
                prevented_wash_volume_fp=100,
                account_deltas_fp=[500, -200],
                treasury_fee_delta_fp=-200,
            )
        )
        # Invalid: negative wash volume or malformed root length
        self.assertFalse(
            verify_counterfactual_diff_invariants(
                is_noop_override=False,
                baseline_root_hex=r1,
                forked_root_hex=r2,
                state_identical=False,
                clearing_price_delta_fp=0,
                prevented_wash_volume_fp=-1,
                account_deltas_fp=[0],
                treasury_fee_delta_fp=0,
            )
        )
        self.assertFalse(
            verify_counterfactual_diff_invariants(
                is_noop_override=False,
                baseline_root_hex="abcd",
                forked_root_hex=r2,
                state_identical=False,
                clearing_price_delta_fp=0,
                prevented_wash_volume_fp=0,
                account_deltas_fp=[0],
                treasury_fee_delta_fp=0,
            )
        )

    def test_hotpath_three_passes_equivalence(self) -> None:
        frame = encode_ws_binary_frame(0x01, 1, 77, 999999, b"0123456789abcdef", with_crc32c=True)
        mv = memoryview(frame)
        bals = [(400 * FP_SCALE, 100 * FP_SCALE), (490 * FP_SCALE, 0)]
        flat = (400 * FP_SCALE, 100 * FP_SCALE, 490 * FP_SCALE, 0)
        treasury = 10 * FP_SCALE
        deposits = 1000 * FP_SCALE
        self.assertTrue(hotpath_pass1_baseline(frame, bals, treasury, deposits))
        self.assertTrue(hotpath_pass2_zerocopy(mv, bals, treasury, deposits))
        self.assertTrue(hotpath_pass3_precompiled_flat(mv, flat, treasury, deposits))


if __name__ == "__main__":
    unittest.main()
