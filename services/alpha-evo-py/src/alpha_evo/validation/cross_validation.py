"""
Purged & Embargoed Walk-Forward Cross-Validation & Combinatorial Purged K-Fold (CPCV).

Reference:
    Marcos López de Prado (2018), *Advances in Financial Machine Learning*,
    Chapter 7 (Cross-Validation in Finance) & Chapter 12 (Backtesting through Cross-Validation).
"""

from __future__ import annotations

from dataclasses import dataclass
from itertools import combinations
import numpy as np
from numpy.typing import NDArray


@dataclass(frozen=True, slots=True)
class CVFold:
    """Single purged & embargoed cross-validation fold."""

    fold_id: int
    train_indices: NDArray[np.int64]
    test_indices: NDArray[np.int64]
    purged_count: int
    embargoed_count: int


def verify_no_label_leakage(
    train_indices: NDArray[np.int64],
    test_indices: NDArray[np.int64],
    label_horizon_bars: int,
    embargo_bars: int,
) -> bool:
    """
    Formally verify that NO training observation `i` has a label span `[i, i + H]` overlapping
    any test observation interval `[j, j + H]` (or `[test_block_start, test_block_end]`), AND
    that no training observation falls inside the post-test embargo zone `(test_end, test_end + E]`.
    """
    if train_indices.size == 0 or test_indices.size == 0:
        return True
    if np.intersect1d(train_indices, test_indices).size > 0:
        return False

    h = max(0, int(label_horizon_bars))
    e = max(0, int(embargo_bars))

    # Decompose sorted test_indices into contiguous [a, b] segments
    t_sorted = np.sort(test_indices)
    breaks = np.where(np.diff(t_sorted) > 1)[0]
    seg_starts = np.r_[t_sorted[0], t_sorted[breaks + 1]]
    seg_ends = np.r_[t_sorted[breaks], t_sorted[-1]]

    for a, b in zip(seg_starts, seg_ends):
        # Training sample `i` has label interval `[i, i + h]`.
        # Overlap with test `[a, b]` occurs iff `i <= b` and `i + h >= a` -> `a - h <= i <= b`.
        overlap = (train_indices >= (a - h)) & (train_indices <= b)
        if np.any(overlap):
            return False
        # Post-test embargo zone `(b, b + e]`
        in_embargo = (train_indices > b) & (train_indices <= (b + e))
        if np.any(in_embargo):
            return False

    return True


class PurgedWalkForwardCV:
    """
    Chronological Walk-Forward Cross-Validation with strict Purge & Embargo buffers
    between training end and out-of-sample test start.
    """

    def __init__(
        self,
        n_splits: int = 5,
        min_train_fraction: float = 0.35,
        label_horizon_bars: int = 10,
        embargo_pct: float = 0.015,
        expanding: bool = True,
    ) -> None:
        if n_splits < 2:
            raise ValueError("n_splits must be >= 2")
        self.n_splits = int(n_splits)
        self.min_train_fraction = float(min_train_fraction)
        self.label_horizon_bars = max(0, int(label_horizon_bars))
        self.embargo_pct = max(0.0, float(embargo_pct))
        self.expanding = bool(expanding)

    def split(self, n_samples: int) -> list[CVFold]:
        if n_samples < 100:
            raise ValueError(f"n_samples ({n_samples}) too small for PurgedWalkForwardCV")

        embargo_bars = max(1, int(np.ceil(n_samples * self.embargo_pct)))
        init_train_end = int(n_samples * self.min_train_fraction)
        rem = n_samples - init_train_end
        test_len = rem // self.n_splits
        if test_len <= self.label_horizon_bars + 5:
            raise ValueError("Insufficient samples per walk-forward fold relative to label horizon")

        folds: list[CVFold] = []
        for k in range(self.n_splits):
            test_start = init_train_end + k * test_len
            test_end = n_samples - 1 if k == self.n_splits - 1 else (test_start + test_len - 1)

            # Purge all training samples whose forward label `[i, i + H]` touches `test_start`
            # PLUS apply explicit safety embargo gap before `test_start` in walk-forward mode
            raw_train_end = test_start - 1
            purged_train_end = test_start - self.label_horizon_bars - embargo_bars - 1
            train_start = 0 if self.expanding else max(0, k * test_len)

            if purged_train_end <= train_start:
                raise ValueError("Purge + embargo exhausted training window; increase n_samples")

            train_idx = np.arange(train_start, purged_train_end + 1, dtype=np.int64)
            test_idx = np.arange(test_start, test_end + 1, dtype=np.int64)
            purged_cnt = min(self.label_horizon_bars, max(0, raw_train_end - purged_train_end))
            emb_cnt = max(0, (raw_train_end - purged_train_end) - purged_cnt)

            assert verify_no_label_leakage(train_idx, test_idx, self.label_horizon_bars, embargo_bars)
            folds.append(
                CVFold(
                    fold_id=k,
                    train_indices=train_idx,
                    test_indices=test_idx,
                    purged_count=int(purged_cnt),
                    embargoed_count=int(emb_cnt),
                )
            )
        return folds


class CombinatorialPurgedCV:
    """
    Combinatorial Purged Cross-Validation (`CPCV(N, k)` — López de Prado 2018, Chapter 12).

    Partitions `[0..T-1]` into `n_groups` contiguous chronological shards and tests all
    `C(n_groups, n_test_groups)` subsets while purging overlapping label spans `[a - H, b]`
    and embargoing post-test bars `(b, b + embargo_bars]` on every boundary!
    """

    def __init__(
        self,
        n_groups: int = 6,
        n_test_groups: int = 2,
        label_horizon_bars: int = 10,
        embargo_pct: float = 0.015,
    ) -> None:
        if n_groups < 3 or n_test_groups < 1 or n_test_groups >= n_groups:
            raise ValueError("Require n_groups >= 3 and 1 <= n_test_groups < n_groups")
        self.n_groups = int(n_groups)
        self.n_test_groups = int(n_test_groups)
        self.label_horizon_bars = max(0, int(label_horizon_bars))
        self.embargo_pct = max(0.0, float(embargo_pct))

    def split(self, n_samples: int) -> list[CVFold]:
        embargo_bars = max(1, int(np.ceil(n_samples * self.embargo_pct)))
        all_idx = np.arange(n_samples, dtype=np.int64)
        groups = np.array_split(all_idx, self.n_groups)

        folds: list[CVFold] = []
        for fold_id, test_group_ids in enumerate(combinations(range(self.n_groups), self.n_test_groups)):
            test_idx = np.concatenate([groups[g] for g in test_group_ids])
            keep_mask = np.ones(n_samples, dtype=np.bool_)
            keep_mask[test_idx] = False

            purged_cnt = 0
            emb_cnt = 0

            # Merge adjacent test groups into contiguous intervals [a, b]
            t_sorted = np.sort(test_idx)
            breaks = np.where(np.diff(t_sorted) > 1)[0]
            seg_starts = np.r_[t_sorted[0], t_sorted[breaks + 1]]
            seg_ends = np.r_[t_sorted[breaks], t_sorted[-1]]

            for a, b in zip(seg_starts, seg_ends):
                p_lo = max(0, int(a) - self.label_horizon_bars)
                p_hi = int(a) - 1
                if p_hi >= p_lo:
                    newly_purged = int(np.sum(keep_mask[p_lo : p_hi + 1]))
                    keep_mask[p_lo : p_hi + 1] = False
                    purged_cnt += newly_purged

                e_lo = int(b) + 1
                e_hi = min(n_samples - 1, int(b) + embargo_bars)
                if e_hi >= e_lo:
                    newly_emb = int(np.sum(keep_mask[e_lo : e_hi + 1]))
                    keep_mask[e_lo : e_hi + 1] = False
                    emb_cnt += newly_emb

            train_idx = all_idx[keep_mask]
            assert verify_no_label_leakage(train_idx, test_idx, self.label_horizon_bars, embargo_bars)
            folds.append(
                CVFold(
                    fold_id=fold_id,
                    train_indices=train_idx,
                    test_indices=test_idx,
                    purged_count=purged_cnt,
                    embargoed_count=emb_cnt,
                )
            )
        return folds
