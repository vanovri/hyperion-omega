"""Vectorized Sub-Millisecond Local KernelSHAP Explainer for Sentinel-ML.

Computes game-theoretic Shapley feature attributions over the 16 streaming microstructure
and temporal graph features using a precomputed constrained weighted-least-squares
antithetic coalition solver:
  - Evaluates K=32 paired antithetic coalitions + full sample in ONE vectorized ONNX batch
    call using pre-allocated buffers (~0.4-0.8 ms on CPU).
  - Enforces exact Shapley Efficiency (Local Accuracy):
        baseline_prob + sum_j phi_j == observed_prob
  - Emits ranked attributions scaled to fixed-point integers (1e8 feature values, 1e6 ppm SHAP).
"""

from __future__ import annotations

from dataclasses import dataclass
import math

import numpy as np
import numpy.typing as npt

from sentinel_ml.fixed_point import PPM_SCALE, scalar_to_fp_int64
from sentinel_ml.microstructure import (
    FEATURE_NAMES,
    NUM_FEATURES,
    TEMPORAL_SEQ_LEN,
    alloc_aligned64_f32,
)
from sentinel_ml.models_onnx import SentinelOnnxRuntime


@dataclass(frozen=True, slots=True)
class FeatureAttributionItem:
    """Single ranked SHAP attribution aligned 1:1 with hyperion.v1.SHAPFeatureAttribution."""

    feature_name: str
    feature_value_fp: int
    baseline_value_fp: int
    shap_contribution_ppm: int
    importance_rank: int
    raw_shap: float


__all__ = ["FeatureAttributionItem", "FastVectorizedKernelSHAP", "alloc_aligned64_f32"]


class FastVectorizedKernelSHAP:
    """Precomputed antithetic KernelSHAP projection engine satisfying exact local accuracy."""

    def __init__(
        self,
        runtime: SentinelOnnxRuntime,
        *,
        num_antithetic_pairs: int = 16,
        seed: int = 42,
    ) -> None:
        self.runtime = runtime
        self.num_features = NUM_FEATURES
        self.bg_seq = runtime.bg_seq_mean
        self.baseline_prob: float = float(runtime.score_probabilities_only(self.bg_seq[None, :, :])[0])

        rng = np.random.default_rng(seed)
        m = self.num_features
        coalitions: list[npt.NDArray[np.float32]] = []

        eye = np.eye(m, dtype=np.float32)
        for j in range(m):
            coalitions.append(eye[j])
            coalitions.append(1.0 - eye[j])

        extra_pairs = max(0, num_antithetic_pairs - m)
        for _ in range(extra_pairs):
            k = int(rng.integers(2, m // 2 + 1))
            z = np.zeros(m, dtype=np.float32)
            z[rng.choice(m, size=k, replace=False)] = 1.0
            coalitions.append(z)
            coalitions.append(1.0 - z)

        self.Z: npt.NDArray[np.float32] = np.stack(coalitions, axis=0)
        s_counts = np.sum(self.Z, axis=1)

        weights = np.zeros(self.Z.shape[0], dtype=np.float64)
        for idx, s_val in enumerate(s_counts):
            s_int = int(round(float(s_val)))
            cmd_binom = math.comb(m, s_int)
            weights[idx] = (m - 1.0) / (cmd_binom * s_int * (m - s_int))

        z64 = self.Z.astype(np.float64)
        w_diag = np.diag(weights)
        ztwz = z64.T @ w_diag @ z64 + 1e-8 * np.eye(m, dtype=np.float64)
        ztwz_inv = np.linalg.inv(ztwz)
        self._proj_matrix: npt.NDArray[np.float64] = ztwz_inv @ z64.T @ w_diag
        ones = np.ones((m, 1), dtype=np.float64)
        denom = float((ones.T @ ztwz_inv @ ones)[0, 0])
        self._constraint_vec: npt.NDArray[np.float64] = (ztwz_inv @ ones / denom).squeeze(1)

        full_row = np.ones((1, m), dtype=np.float32)
        mask_raw = np.concatenate([full_row, self.Z], axis=0)[:, None, :]
        self._mask_3d: npt.NDArray[np.float32] = alloc_aligned64_f32(mask_raw.shape)
        np.copyto(self._mask_3d, mask_raw)
        bg_raw = ((1.0 - self._mask_3d) * self.bg_seq[None, :, :]).astype(np.float32)
        self._bg_contrib_3d: npt.NDArray[np.float32] = alloc_aligned64_f32(bg_raw.shape)
        np.copyto(self._bg_contrib_3d, bg_raw)
        self._coal_buf: npt.NDArray[np.float32] = alloc_aligned64_f32(
            (self._mask_3d.shape[0], TEMPORAL_SEQ_LEN, m)
        )
        self._x_seq_3d: npt.NDArray[np.float32] = alloc_aligned64_f32((1, TEMPORAL_SEQ_LEN, m))
        self._diff_buf: npt.NDArray[np.float32] = alloc_aligned64_f32((TEMPORAL_SEQ_LEN, m))
        self._f_coal_buf: npt.NDArray[np.float64] = np.empty(self.Z.shape[0], dtype=np.float64)
        self._phi_buf: npt.NDArray[np.float64] = np.empty(m, dtype=np.float64)
        self._bg_last_fp: tuple[int, ...] = tuple(
            scalar_to_fp_int64(float(self.bg_seq[-1, j])) for j in range(m)
        )

    def explain(
        self,
        sequence: npt.NDArray[np.float32],
        *,
        top_k: int = NUM_FEATURES,
    ) -> tuple[float, float, tuple[FeatureAttributionItem, ...]]:
        """Compute exact-efficiency local SHAP attributions in a single ONNX batch call."""
        x_seq = (
            sequence
            if (sequence.dtype == np.float32 and sequence.flags["C_CONTIGUOUS"])
            else np.ascontiguousarray(sequence, dtype=np.float32)
        )
        np.copyto(self._x_seq_3d[0], x_seq)
        self._coal_buf[:] = self._x_seq_3d
        np.multiply(self._coal_buf, self._mask_3d, out=self._coal_buf)
        np.add(self._coal_buf, self._bg_contrib_3d, out=self._coal_buf)
        batch_probs = self.runtime.score_probabilities_only(self._coal_buf)

        fx = float(batch_probs[0])
        self._f_coal_buf[:] = batch_probs[1:]
        self._f_coal_buf -= self.baseline_prob
        total_delta = fx - self.baseline_prob

        np.matmul(self._proj_matrix, self._f_coal_buf, out=self._phi_buf)
        residual = total_delta - float(self._phi_buf.sum())
        phi = self._phi_buf
        phi += residual * self._constraint_vec

        np.subtract(x_seq, self.bg_seq, out=self._diff_buf)
        np.abs(self._diff_buf, out=self._diff_buf)
        identical_mask = np.all(self._diff_buf <= 1e-7, axis=0)
        if np.any(identical_mask) and not np.all(identical_mask):
            phi[identical_mask] = 0.0
            active = ~identical_mask
            rem_sum = float(np.sum(phi[active]))
            if abs(rem_sum) > 1e-12:
                phi[active] *= total_delta / rem_sum
            else:
                phi[active] = total_delta / float(np.sum(active))

        obs_last = x_seq[-1]
        bg_last_fp = self._bg_last_fp
        order = np.argsort(-np.abs(phi))[: max(1, min(top_k, self.num_features))]

        items: list[FeatureAttributionItem] = []
        for rank_idx, feat_idx in enumerate(order, start=1):
            j = int(feat_idx)
            phi_j = float(phi[j])
            items.append(
                FeatureAttributionItem(
                    feature_name=FEATURE_NAMES[j],
                    feature_value_fp=scalar_to_fp_int64(float(obs_last[j])),
                    baseline_value_fp=bg_last_fp[j],
                    shap_contribution_ppm=int(round(phi_j * PPM_SCALE)),
                    importance_rank=rank_idx,
                    raw_shap=phi_j,
                )
            )

        return fx, self.baseline_prob, tuple(items)
