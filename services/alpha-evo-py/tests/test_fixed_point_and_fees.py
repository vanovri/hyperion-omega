"""Tests for exact Hyperion 1e8 fixed-point arithmetic & Channel 02 maker/taker fee conservation."""

from __future__ import annotations

import numpy as np
import pytest

from alpha_evo.fixed_point import (
    CANONICAL_INSTRUMENTS,
    FP_SCALE,
    PPM_SCALE,
    compute_fee_fp_scalar,
    compute_fee_fp_vec,
    compute_notional_fp_scalar,
    compute_notional_fp_vec,
    decimal_str_fp,
    from_fp,
    settle_fill_double_entry_vec,
    to_fp,
    trunc_div_i128,
)


def test_fixed_point_scale_and_canonical_instruments() -> None:
    assert FP_SCALE == 100_000_000
    assert PPM_SCALE == 1_000_000
    assert to_fp("68425.50") == 6_842_550_000_000
    assert str(from_fp(6_842_550_000_000)) == "68425.5"
    assert decimal_str_fp(6_842_550_000_000) == "6842550000000"
    assert set(CANONICAL_INSTRUMENTS.keys()) == {"BTC-USDT", "ETH-USDT", "SOL-USDT", "HYPE-USDT"}
    assert CANONICAL_INSTRUMENTS["BTC-USDT"].tick_size_fp == 1_000_000
    assert CANONICAL_INSTRUMENTS["BTC-USDT"].lot_size_fp == 1_000


def test_trunc_toward_zero_vs_python_floor_on_negative_maker_rebate() -> None:
    # Critical Rust i128 parity: -15_500_000 / 1_000_000 == -15 (NOT -16 like Python //!)
    assert trunc_div_i128(-15_500_000, 1_000_000) == -15
    assert trunc_div_i128(15_500_000, 1_000_000) == 15
    assert trunc_div_i128(-999_999, 1_000_000) == 0
    assert trunc_div_i128(-1_000_000, 1_000_000) == -1
    with pytest.raises(ZeroDivisionError):
        trunc_div_i128(100, 0)


def test_vectorized_128bit_notional_and_fee_match_arbitrary_precision_scalar() -> None:
    # Large BTC orders ($98,765.43210987 * 25.43210987 BTC) would overflow signed int64 without split-word!
    prices = np.array([9_876_543_210_987, 6_842_550_000_000, 1_000_000, 0, 155_000], dtype=np.int64)
    qtys = np.array([2_543_210_987, 100_000_000, 1_000, 500_000_000, 100_000_000], dtype=np.int64)

    vec_notional = compute_notional_fp_vec(prices, qtys)
    expected_notional = np.array(
        [compute_notional_fp_scalar(int(p), int(q)) for p, q in zip(prices, qtys)],
        dtype=np.int64,
    )
    np.testing.assert_array_equal(vec_notional, expected_notional)

    for fee_ppm in (-150, -100, 0, 100, 500, -333):
        vec_fee = compute_fee_fp_vec(vec_notional, fee_ppm)
        exp_fee = np.array([compute_fee_fp_scalar(int(n), fee_ppm) for n in vec_notional], dtype=np.int64)
        np.testing.assert_array_equal(vec_fee, exp_fee)


def test_double_entry_conservation_exact_zero_residual() -> None:
    rng = np.random.default_rng(19)
    prices = rng.integers(1_000_000, 10_000_000_000_000, size=512, dtype=np.int64)
    qtys = rng.integers(1_000, 900_000_000, size=512, dtype=np.int64)
    maker_buy = rng.random(512) > 0.5

    for m_ppm, t_ppm in [(-100, 500), (100, 400), (-250, 250), (0, 0)]:
        notional, m_fee, t_fee, m_delta, t_delta, treas_delta = settle_fill_double_entry_vec(
            prices, qtys, maker_buy, maker_fee_ppm=m_ppm, taker_fee_ppm=t_ppm
        )
        assert np.all(notional >= 0)
        # Exact Double-Entry Conservation Law (00-hyperion-contract.md Section 5.2):
        np.testing.assert_array_equal(m_delta + t_delta + treas_delta, np.zeros(512, dtype=np.int64))
        # Verify direction of quote flows
        buy_idx = np.where(maker_buy)[0][0]
        sell_idx = np.where(~maker_buy)[0][0]
        assert m_delta[buy_idx] == -notional[buy_idx] - m_fee[buy_idx]
        assert m_delta[sell_idx] == notional[sell_idx] - m_fee[sell_idx]


def test_negative_inputs_rejected() -> None:
    with pytest.raises(ValueError):
        compute_notional_fp_scalar(-1, 100)
    with pytest.raises(ValueError):
        compute_notional_fp_vec(np.array([-1], dtype=np.int64), np.array([10], dtype=np.int64))
    with pytest.raises(ValueError):
        compute_fee_fp_scalar(-5, 100)
    with pytest.raises(ValueError):
        compute_fee_fp_vec(np.array([-5], dtype=np.int64), 100)
