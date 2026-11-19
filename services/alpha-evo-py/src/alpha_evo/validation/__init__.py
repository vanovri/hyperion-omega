"""Institutional Anti-Overfitting Validation Suite: Purged/Embargoed CV, DSR, PSR, MinTRL, & PBO."""

from alpha_evo.validation.cross_validation import (
    CombinatorialPurgedCV,
    CVFold,
    PurgedWalkForwardCV,
    verify_no_label_leakage,
)
from alpha_evo.validation.statistics import (
    DSRResult,
    PBOResult,
    compute_cscv_pbo,
    compute_deflated_sharpe_ratio,
    compute_effective_trials,
    compute_probabilistic_sharpe_ratio,
)

__all__ = [
    "CVFold",
    "PurgedWalkForwardCV",
    "CombinatorialPurgedCV",
    "verify_no_label_leakage",
    "DSRResult",
    "PBOResult",
    "compute_effective_trials",
    "compute_probabilistic_sharpe_ratio",
    "compute_deflated_sharpe_ratio",
    "compute_cscv_pbo",
]
