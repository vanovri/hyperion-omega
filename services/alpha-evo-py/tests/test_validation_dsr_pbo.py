"""Tests for Purged/Embargoed CV, Bailey & López de Prado DSR, and CSCV PBO."""

from __future__ import annotations

import numpy as np

from alpha_evo.validation.cross_validation import (
    CombinatorialPurgedCV,
    PurgedWalkForwardCV,
    verify_no_label_leakage,
)
from alpha_evo.validation.statistics import (
    compute_cscv_pbo,
    compute_deflated_sharpe_ratio,
    compute_effective_trials,
    compute_probabilistic_sharpe_ratio,
)


def test_purged_walk_forward_and_cpcv_zero_leakage() -> None:
    wf = PurgedWalkForwardCV(n_splits=5, label_horizon_bars=12, embargo_pct=0.02)
    folds = wf.split(1_000)
    assert len(folds) == 5
    for f in folds:
        assert f.purged_count == 12
        assert f.embargoed_count >= 20
        assert verify_no_label_leakage(f.train_indices, f.test_indices, 12, 20)

    cpcv = CombinatorialPurgedCV(n_groups=6, n_test_groups=2, label_horizon_bars=10, embargo_pct=0.02)
    c_folds = cpcv.split(600)
    assert len(c_folds) == 15  # C(6, 2) == 15
    for cf in c_folds:
        assert verify_no_label_leakage(cf.train_indices, cf.test_indices, 10, 12)

    # Verify detector catches intentional leakage!
    bad_train = np.array([0, 1, 95], dtype=np.int64)
    bad_test = np.array([100, 101, 102], dtype=np.int64)
    assert verify_no_label_leakage(bad_train, bad_test, label_horizon_bars=10, embargo_bars=5) is False
    bad_embargo = np.array([105], dtype=np.int64)
    assert verify_no_label_leakage(bad_embargo, bad_test, label_horizon_bars=10, embargo_bars=5) is False


def test_deflated_sharpe_ratio_econometric_properties() -> None:
    rng = np.random.default_rng(77)
    n = 600
    # 1. Pure noise multiple-testing trap: pick the best of 200 pure random Gaussian walks!
    noise_mat = rng.normal(0.0, 1.0, size=(n, 200))
    best_noise_idx = int(np.argmax(np.mean(noise_mat, axis=0) / np.std(noise_mat, axis=0, ddof=1)))
    lucky_noise = noise_mat[:, best_noise_idx]

    psr_naive, _, _ = compute_probabilistic_sharpe_ratio(lucky_noise, benchmark_sr_per_period=0.0)
    dsr_noise = compute_deflated_sharpe_ratio(lucky_noise, trial_returns_matrix=noise_mat)

    # Naive PSR looks significant (> 0.99), whereas DSR deflates multiple-testing bias and REJECTS it!
    assert psr_naive > 0.95
    assert dsr_noise.deflated_sharpe_ratio < 0.90
    assert dsr_noise.passed_institutional_gate is False
    assert 150.0 < dsr_noise.effective_independent_trials <= 200.0

    # 2. Genuine high-SNR alpha easily clears DSR >= 0.95 even against 200 trials!
    true_alpha = rng.normal(0.24, 1.0, size=n)
    dsr_true = compute_deflated_sharpe_ratio(
        true_alpha,
        trial_returns_matrix=np.column_stack([noise_mat, true_alpha]),
    )
    assert dsr_true.deflated_sharpe_ratio > 0.99
    assert dsr_true.passed_institutional_gate is True

    # 3. Negative skewness & excess kurtosis increase Sharpe standard error and reduce PSR!
    # Construct two series with IDENTICAL sample mean & sample std, one positively skewed and one negatively skewed
    base = np.array([-3.5] * 8 + [0.60] * 92, dtype=np.float64)   # Strong negative skew
    mirror = -base + 2.0 * np.mean(base)                          # Same mean & std, positive skew!
    psr_neg, se_neg, _ = compute_probabilistic_sharpe_ratio(base, benchmark_sr_per_period=0.0)
    psr_pos, se_pos, _ = compute_probabilistic_sharpe_ratio(mirror, benchmark_sr_per_period=0.0)
    assert se_neg > se_pos
    assert psr_neg < psr_pos

    # 4. Identical correlated strategies collapse M_eff -> ~1.0
    base_col = rng.normal(0.05, 1.0, size=(300, 1))
    dup_mat = np.repeat(base_col, 25, axis=1)
    m_eff_dup, rho_dup = compute_effective_trials(dup_mat)
    assert np.isclose(rho_dup, 1.0, atol=1e-6)
    assert np.isclose(m_eff_dup, 1.0, atol=1e-6)


def test_cscv_pbo_distinguishes_overfit_noise_from_structural_alpha() -> None:
    rng = np.random.default_rng(404)
    # 1. 60 pure zero-mean noise strategies -> PBO should be ~0.50 (>> 0.25 gate -> REJECTED!)
    pure_noise = rng.normal(0.0, 1.0, size=(800, 60))
    pbo_noise = compute_cscv_pbo(pure_noise, n_partitions=10, pbo_gate_threshold=0.25)
    assert pbo_noise.n_combinations == 252
    assert pbo_noise.pbo > 0.35
    assert pbo_noise.passed_institutional_gate is False

    # 2. Family with genuine persistent alpha strategies -> IS winner stays above median OOS -> PBO near 0!
    persistent = np.column_stack(
        [
            rng.normal(0.28, 1.0, size=(800, 4)),
            rng.normal(0.02, 1.0, size=(800, 20)),
        ]
    )
    pbo_good = compute_cscv_pbo(persistent, n_partitions=10, pbo_gate_threshold=0.25)
    assert pbo_good.pbo <= 0.10
    assert pbo_good.passed_institutional_gate is True
