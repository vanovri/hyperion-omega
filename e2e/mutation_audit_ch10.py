#!/usr/bin/env python3
"""
HYPERION Channel 10 — Gate 1 Programmatic Operator & Invariant Mutation Harness.
Injects 25 critical financial, cryptographic, endianness, and boundary mutations into
crucible_core.py in isolated in-memory module namespaces and verifies >= 95.0% (target 100.0%)
mutant kill rate deterministically without mutating source files on disk.
"""

from __future__ import annotations

import importlib
import io
import json
import sys
import time
import types
import unittest
from pathlib import Path

MUTANTS: list[tuple[str, str, str]] = [
    ("M01_FP_SCALE_CORRUPT", "FP_SCALE: int = 100_000_000", "FP_SCALE: int = 10_000_000"),
    ("M02_PPM_SCALE_CORRUPT", "PPM_SCALE: int = 1_000_000", "PPM_SCALE: int = 100_000"),
    ("M03_CRC32C_POLY_FLIP", "poly = 0x82F63B78", "poly = 0xEDB88320"),
    ("M04_NOTIONAL_PRICE_BOUND", "if price_fp < 0 or qty_fp <= 0:", "if price_fp <= 0 or qty_fp <= 0:"),
    ("M05_NOTIONAL_QTY_BOUND", "if price_fp < 0 or qty_fp <= 0:", "if price_fp < 0 or qty_fp < 0:"),
    ("M06_NOTIONAL_MUL_TO_ADD", "return (price_fp * qty_fp) // FP_SCALE", "return (price_fp + qty_fp) // FP_SCALE"),
    ("M07_FEE_MIN_BOUND_LT_TO_LE", "if fee_ppm < -PPM_SCALE or fee_ppm > PPM_SCALE:", "if fee_ppm <= -PPM_SCALE or fee_ppm > PPM_SCALE:"),
    ("M08_FEE_MAX_BOUND_GT_TO_GE", "if fee_ppm < -PPM_SCALE or fee_ppm > PPM_SCALE:", "if fee_ppm < -PPM_SCALE or fee_ppm >= PPM_SCALE:"),
    ("M09_REBATE_SIGN_INVERT", "return -mag if fee_ppm < 0 else mag", "return mag if fee_ppm < 0 else mag"),
    ("M10_CONSERVATION_WITHDRAWAL_SIGN", "rhs = total_deposits_fp - total_withdrawals_fp", "rhs = total_deposits_fp + total_withdrawals_fp"),
    ("M11_CONSERVATION_TREASURY_SIGN", "lhs = user_equity_sum + treasury_fee_pool_fp", "lhs = user_equity_sum - treasury_fee_pool_fp"),
    ("M12_CONSERVATION_NEG_AVAIL_ALLOW", "if avail_fp < 0 or locked_fp < 0:", "if avail_fp < -100 or locked_fp < 0:"),
    ("M13_CONSERVATION_EQ_TO_GE", "return lhs == rhs", "return lhs >= rhs"),
    ("M14_BOOK_BID_ORDER_GE_TO_GT", "if prev_bid is not None and price_fp >= prev_bid:", "if prev_bid is not None and price_fp > prev_bid:"),
    ("M15_BOOK_ASK_ORDER_LE_TO_LT", "if prev_ask is not None and price_fp <= prev_ask:", "if prev_ask is not None and price_fp < prev_ask:"),
    ("M16_BOOK_CROSS_GE_TO_GT", "if bids_fp[0][0] >= asks_fp[0][0]:", "if bids_fp[0][0] > asks_fp[0][0]:"),
    ("M17_PREIMAGE_METHOD_CASE", "preimage = f\"{timestamp_ns}\\n{nonce}\\n{method.upper()}\\n{request_path}\\n{body_hex}\"", "preimage = f\"{timestamp_ns}\\n{nonce}\\n{method.lower()}\\n{request_path}\\n{body_hex}\""),
    ("M18_WINDOW_LE_TO_LT", "return abs(server_now_ns - client_ts_ns) <= max_skew_ns", "return abs(server_now_ns - client_ts_ns) < max_skew_ns"),
    ("M19_WS_MAGIC_MUTATE", "if magic != WS_MAGIC:", "if magic == WS_MAGIC:"),
    ("M20_WS_VERSION_SKIP", "if version != WS_VERSION:", "if version > 99:"),
    ("M21_WS_RESERVED_SKIP", "if reserved != 0:", "if reserved > 99:"),
    ("M22_WS_LEN_CHECK_LE", "if total_len != WS_HEADER_LEN + payload_len:", "if total_len < WS_HEADER_LEN + payload_len:"),
    ("M23_WS_CRC_CHECK_INVERT", "if computed != crc_digest:", "if computed == crc_digest and False:"),
    ("M24_CHRONOS_NOOP_DELTA_SKIP", "if clearing_price_delta_fp != 0 or prevented_wash_volume_fp != 0 or treasury_fee_delta_fp != 0:", "if prevented_wash_volume_fp != 0 or treasury_fee_delta_fp != 0:"),
    ("M25_CHRONOS_CLOSED_EQUITY_SIGN", "if sum(account_deltas_fp) + treasury_fee_delta_fp != 0:", "if sum(account_deltas_fp) - treasury_fee_delta_fp != 0:"),
]


def _run_suite_against_source(core_src: str, test_file: Path) -> bool:
    """Compile core_src into fresh crucible_core module, reload test suite, and return True iff all tests pass."""
    mod = types.ModuleType("crucible_core")
    mod.__file__ = "<mutated_crucible_core>"
    sys.modules["crucible_core"] = mod
    exec(compile(core_src, "<mutated_crucible_core>", "exec"), mod.__dict__)

    test_mod = types.ModuleType("test_crucible_core_runner")
    test_mod.__file__ = str(test_file)
    exec(compile(test_file.read_text(encoding="utf-8"), str(test_file), "exec"), test_mod.__dict__)

    suite = unittest.defaultTestLoader.loadTestsFromModule(test_mod)
    buf = io.StringIO()
    runner = unittest.TextTestRunner(stream=buf, verbosity=0)
    res = runner.run(suite)
    return res.wasSuccessful() and res.testsRun > 0


def main() -> int:
    here = Path(__file__).resolve().parent
    if str(here) not in sys.path:
        sys.path.insert(0, str(here))
    target_file = here / "crucible_core.py"
    test_file = here / "test_crucible_core.py"
    original_src = target_file.read_text(encoding="utf-8")

    # Baseline check: unmutated tests MUST pass first
    if not _run_suite_against_source(original_src, test_file):
        print("FATAL: Baseline test suite failed before mutation testing")
        return 1

    killed = 0
    survived: list[str] = []
    details: list[dict] = []
    t0 = time.perf_counter()

    for mid, target_str, replacement_str in MUTANTS:
        if target_str not in original_src:
            raise RuntimeError(f"Mutant {mid} target string not found in crucible_core.py")
        mutated_src = original_src.replace(target_str, replacement_str, 1)
        try:
            passed = _run_suite_against_source(mutated_src, test_file)
            is_killed = not passed
        except Exception:
            is_killed = True

        if is_killed:
            killed += 1
        else:
            survived.append(mid)
        details.append({"mutant_id": mid, "status": "KILLED" if is_killed else "SURVIVED"})

    # Restore clean module reference
    importlib.invalidate_caches()
    elapsed = time.perf_counter() - t0
    total = len(MUTANTS)
    score = (killed / total) * 100.0
    summary = {
        "total_mutants": total,
        "killed": killed,
        "survived": len(survived),
        "survived_ids": survived,
        "kill_score_pct": round(score, 2),
        "elapsed_sec": round(elapsed, 4),
        "details": details,
    }
    out_dir = here / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "ch10_mutation_report.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")

    print(f"[CH10 MUTATION GATE] Killed {killed}/{total} = {score:.2f}% in {elapsed:.4f}s (Survived={survived})")
    return 0 if score >= 95.0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
