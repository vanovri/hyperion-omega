"""
HYPERION OMEGA — Channel 19 (`services/alpha-evo-py`)
Autonomous Quantitative Microstructure Alpha Factory & RL Optimal Execution Lab.
"""

from __future__ import annotations

__version__ = "1.0.0"

from alpha_evo.fixed_point import (
    BPS_SCALE,
    CANONICAL_INSTRUMENTS,
    FP_SCALE,
    PPM_SCALE,
    InstrumentSpec,
    compute_fee_fp_scalar,
    compute_fee_fp_vec,
    compute_notional_fp_scalar,
    compute_notional_fp_vec,
    decimal_str_fp,
    from_fp,
    to_fp,
)

__all__ = [
    "__version__",
    "FP_SCALE",
    "PPM_SCALE",
    "BPS_SCALE",
    "InstrumentSpec",
    "CANONICAL_INSTRUMENTS",
    "to_fp",
    "from_fp",
    "decimal_str_fp",
    "compute_notional_fp_scalar",
    "compute_notional_fp_vec",
    "compute_fee_fp_scalar",
    "compute_fee_fp_vec",
]
