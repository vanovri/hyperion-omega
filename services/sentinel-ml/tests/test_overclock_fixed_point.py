"""Overclock Mutation-Survival-Zero & 64B Cache-Line Alignment Tests for Sentinel-ML."""

from __future__ import annotations

from pathlib import Path

import pytest

from sentinel_ml import fixed_point as fp
from sentinel_ml.engine import SentinelDetectionEngine
from sentinel_ml.overclock_audit import run_ast_mutation_audit
from sentinel_ml.shap_explainer import alloc_aligned64_f32


def test_require_fp_int_u64_i64_all_boundaries() -> None:
    assert fp.require_fp_int(0, "x", allow_zero=True, allow_negative=False) == 0
    assert fp.require_fp_int(fp.INT128_MIN, "x", allow_negative=True) == fp.INT128_MIN
    assert fp.require_fp_int(fp.UINT128_MAX, "x", allow_negative=False) == fp.UINT128_MAX

    with pytest.raises(fp.FixedPointViolationError):
        fp.require_fp_int(True, "x")
    with pytest.raises(fp.FixedPointViolationError):
        fp.require_fp_int(1.5, "x")  # type: ignore[arg-type]
    with pytest.raises(ValueError):
        fp.require_fp_int(-1, "x", allow_negative=False)
    with pytest.raises(ValueError):
        fp.require_fp_int(0, "x", allow_zero=False)
    with pytest.raises(OverflowError):
        fp.require_fp_int(fp.INT128_MIN - 1, "x", allow_negative=True)
    with pytest.raises(OverflowError):
        fp.require_fp_int(fp.UINT128_MAX + 1, "x", allow_negative=False)

    assert fp.require_uint64(0, "u", allow_zero=True) == 0
    assert fp.require_uint64((1 << 64) - 1, "u") == (1 << 64) - 1
    with pytest.raises(ValueError):
        fp.require_uint64(0, "u", allow_zero=False)
    with pytest.raises(ValueError):
        fp.require_uint64(-1, "u")
    with pytest.raises(OverflowError):
        fp.require_uint64(1 << 64, "u")

    assert fp.require_int64(-(1 << 63), "s") == -(1 << 63)
    assert fp.require_int64((1 << 63) - 1, "s") == (1 << 63) - 1
    with pytest.raises(OverflowError):
        fp.require_int64(-(1 << 63) - 1, "s")
    with pytest.raises(OverflowError):
        fp.require_int64(1 << 63, "s")


def test_notional_distance_bps_and_128bit_bit_splits() -> None:
    assert fp.compute_notional_fp(68_425_500_000_000 // 10, 125_000_000) == 8_553_187_500_000
    n0 = fp.compute_notional_fp(3, 33_333_333)
    assert isinstance(n0, int) and n0 == 0
    assert fp.compute_notional_fp(3, 33_333_334) == 1
    assert fp.compute_notional_fp(fp.UINT64_MAX, fp.UINT64_MAX) == (
        fp.UINT64_MAX * fp.UINT64_MAX
    ) // fp.SCALE
    with pytest.raises(ValueError):
        fp.compute_notional_fp(0, 100)
    with pytest.raises(ValueError):
        fp.compute_notional_fp(100, 0)

    d = fp.distance_bps_int(10_500_000_000, 10_000_000_000)
    assert isinstance(d, int) and d == 500
    assert fp.distance_bps_int(10_000_000_000, 10_000_000_000) == 0
    assert fp.distance_bps_int(9_999_999_999, 10_000_000_000) == 0
    assert fp.distance_bps_int(0, 10_000_000_000) == 10_000
    with pytest.raises(ValueError):
        fp.distance_bps_int(10, 0)

    for v in (0, 1, fp.UINT64_MAX, (1 << 64) + 77, fp.UINT128_MAX):
        hi, lo = fp.int_to_u128_parts(v)
        assert fp.u128_parts_to_int(hi, lo) == v
    with pytest.raises(OverflowError):
        fp.int_to_u128_parts(fp.UINT128_MAX + 1)

    for v in (
        0,
        1,
        -1,
        (1 << 63) - 1,
        -(1 << 63),
        (1 << 90) + 123,
        -((1 << 90) + 456),
        fp.INT128_MIN,
        fp.INT128_MAX,
    ):
        hi, lo = fp.int_to_i128_parts(v)
        assert fp.INT64_MIN <= hi <= fp.INT64_MAX
        assert 0 <= lo <= fp.UINT64_MAX
        assert fp.i128_parts_to_int(hi, lo) == v
    with pytest.raises(OverflowError):
        fp.int_to_i128_parts(fp.INT128_MIN - 1)
    with pytest.raises(OverflowError):
        fp.int_to_i128_parts(fp.INT128_MAX + 1)


def test_ratio_scalar_and_decimal_parsing_formatting() -> None:
    assert fp.ratio_to_ppm(1.0, 0.0) == 0
    assert fp.ratio_to_ppm(0.25, 1.0) == 250_000
    assert fp.ratio_to_ppm(3.0, 2.0, clamp_unit=False) == 1_500_000
    assert fp.ratio_to_ppm(3.0, 2.0, clamp_unit=True) == 1_000_000
    assert fp.ratio_to_ppm(-0.5, 1.0, clamp_unit=True) == 0

    assert fp.scalar_to_fp_int64(1.25) == 125_000_000
    assert fp.scalar_to_fp_int64(-2.5) == -250_000_000
    assert fp.scalar_to_fp_int64(1e30) == fp.INT64_MAX
    assert fp.scalar_to_fp_int64(-1e30) == fp.INT64_MIN

    assert fp.parse_decimal_to_fp("42150.125") == 4_215_012_500_000
    assert fp.parse_decimal_to_fp("1.5") == 150_000_000
    assert fp.parse_decimal_to_fp("+42") == 4_200_000_000
    assert fp.parse_decimal_to_fp("-0.00000001") == -1
    assert fp.parse_decimal_to_fp("0.00000000") == 0
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("")
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("   ")
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("abc.12")
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("12.ab")
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("1_000.5")
    with pytest.raises(ValueError):
        fp.parse_decimal_to_fp("10.1_2")

    assert fp.format_fp_to_decimal(0) == "0.00000000"
    assert fp.format_fp_to_decimal(123_450_000) == "1.23450000"
    assert fp.format_fp_to_decimal(-123_450_000) == "-1.23450000"


def test_aligned64_float32_allocation(models_dir: Path) -> None:
    buf = alloc_aligned64_f32((33, 8, 16))
    assert buf.shape == (33, 8, 16)
    assert int(buf.ctypes.data) & 63 == 0

    engine = SentinelDetectionEngine(models_dir, use_int8=True)
    assert int(engine._single_buf.ctypes.data) & 63 == 0
    assert int(engine._pair_buf.ctypes.data) & 63 == 0
    assert int(engine.explainer._mask_3d.ctypes.data) & 63 == 0
    assert int(engine.explainer._bg_contrib_3d.ctypes.data) & 63 == 0
    assert int(engine.explainer._coal_buf.ctypes.data) & 63 == 0
    assert int(engine.explainer._x_seq_3d.ctypes.data) & 63 == 0
    assert int(engine.explainer._diff_buf.ctypes.data) & 63 == 0
    for bsz, arr in engine.runtime._if_bufs.items():
        assert arr.shape == (bsz, 32)
        assert int(arr.ctypes.data) & 63 == 0


def test_overclock_ast_mutation_kill_score_ge_95_pct() -> None:
    report = run_ast_mutation_audit()
    assert report["passed_95pct_gate"] is True, f"Survived: {report['survived']}"
    assert report["overall_kill_rate_pct"] >= 95.0
