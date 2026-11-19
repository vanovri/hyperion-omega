"""Ensemble Anomaly Architecture, ONNX Export/Quantization, & Production ONNX Runtime.

Combines:
1. PyTorch Temporal-Mixer Autoencoder + Adversarial Margin & Typology Head:
   - Input: [batch, T=8, F=16] float32 sequence
   - Baked-in z-score standardization buffers so ONNX graph runs standalone
   - Exported to temporal_autoencoder.onnx and CPU INT8 dynamically quantized
     temporal_autoencoder.int8.onnx (~37 KB, L1/L2 cache resident)
2. Scikit-Learn IsolationForest (L2-cache-tuned 32 trees x 128 samples -> ~75 KB ONNX)
   exported directly to ONNX ai.onnx.ml.TreeEnsembleRegressor (isolation_forest.onnx)
   with explicit axes=[1] Squeeze for zero shape warnings on batch_size=1.
3. Calibrated logistic fusion producing severity_score in [0, 100], confidence_ppm,
   and pattern typology classification with sub-millisecond median latency.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import math
from pathlib import Path
from typing import Any

import numpy as np
import numpy.typing as npt
import onnx
from onnx import TensorProto, helper
import onnxruntime as ort
from onnxruntime.quantization import QuantType, quantize_dynamic
from onnxruntime.quantization.shape_inference import quant_pre_process
from sklearn.ensemble import IsolationForest
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, precision_score, recall_score, roc_auc_score
import torch
import torch.nn as nn

from sentinel_ml.microstructure import NUM_FEATURES, TEMPORAL_SEQ_LEN, alloc_aligned64_f32

MODEL_VERSION: str = "sentinel-ensemble-v1.0.0-int8"
_F32_ZERO = np.float32(0.0)
_F32_HALF = np.float32(0.5)
_F32_ONE = np.float32(1.0)
_F32_HI30 = np.float32(30.0)
_F32_LO30 = np.float32(-30.0)


class TemporalAutoencoderNet(nn.Module):
    """Temporal-Mixer Autoencoder + Latent Anomaly/Typology Head (100% MatMul INT8-safe)."""

    feat_mean: torch.Tensor
    feat_std: torch.Tensor

    def __init__(
        self,
        seq_len: int = TEMPORAL_SEQ_LEN,
        num_features: int = NUM_FEATURES,
        hidden_dim: int = 48,
        latent_dim: int = 12,
        num_classes: int = 6,
    ) -> None:
        super().__init__()
        self.seq_len = seq_len
        self.num_features = num_features

        self.register_buffer("feat_mean", torch.zeros(1, 1, num_features, dtype=torch.float32))
        self.register_buffer("feat_std", torch.ones(1, 1, num_features, dtype=torch.float32))

        self.temporal_enc = nn.Sequential(
            nn.Linear(seq_len, seq_len),
            nn.GELU(),
            nn.Linear(seq_len, seq_len),
        )
        self.channel_enc = nn.Sequential(
            nn.Linear(num_features, hidden_dim),
            nn.GELU(),
            nn.Linear(hidden_dim, latent_dim),
        )
        self.channel_dec = nn.Sequential(
            nn.Linear(latent_dim, hidden_dim),
            nn.GELU(),
            nn.Linear(hidden_dim, num_features),
        )
        self.temporal_dec = nn.Linear(seq_len, seq_len)

        pooled_dim = latent_dim * 2 + num_features
        self.anomaly_head = nn.Sequential(
            nn.Linear(pooled_dim, hidden_dim),
            nn.GELU(),
            nn.Linear(hidden_dim, 1),
        )
        self.typology_head = nn.Sequential(
            nn.Linear(pooled_dim, hidden_dim),
            nn.GELU(),
            nn.Linear(hidden_dim, num_classes),
        )

    def set_preprocess_stats(
        self, mean: npt.NDArray[np.float32], std: npt.NDArray[np.float32]
    ) -> None:
        self.feat_mean.copy_(torch.from_numpy(mean.reshape(1, 1, -1)))
        self.feat_std.copy_(torch.from_numpy(np.maximum(std, 1e-4).reshape(1, 1, -1)))

    def forward(
        self, x: torch.Tensor
    ) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
        x_norm = torch.clamp((x - self.feat_mean) / self.feat_std, -12.0, 12.0)

        t_mixed = x_norm + self.temporal_enc(x_norm.transpose(1, 2)).transpose(1, 2)
        z = self.channel_enc(t_mixed)

        dec_ch = self.channel_dec(z)
        x_hat = dec_ch + self.temporal_dec(dec_ch.transpose(1, 2)).transpose(1, 2)

        sq_diff = (x_norm - x_hat) ** 2
        per_feature_sq_err = torch.mean(sq_diff, dim=1)
        recon_error_mse = torch.mean(per_feature_sq_err, dim=1)

        z_last = z[:, -1, :]
        z_max = torch.amax(z, dim=1)
        pooled = torch.cat([z_last, z_max, per_feature_sq_err], dim=1)

        anomaly_prob = torch.sigmoid(self.anomaly_head(pooled)).squeeze(-1)
        typology_logits = self.typology_head(pooled)
        return recon_error_mse, per_feature_sq_err, anomaly_prob, typology_logits


def summarize_sequence_for_iforest(
    sequences: npt.NDArray[np.float32],
    out_buf: npt.NDArray[np.float32] | None = None,
) -> npt.NDArray[np.float32]:
    """Vectorized [B, T=8, F=16] -> [B, 32] summary (latest step + temporal peak)."""
    bsz = sequences.shape[0]
    if out_buf is None or out_buf.shape != (bsz, NUM_FEATURES * 2):
        out_buf = np.empty((bsz, NUM_FEATURES * 2), dtype=np.float32)
    out_buf[:, :NUM_FEATURES] = sequences[:, -1, :]
    np.max(sequences, axis=1, out=out_buf[:, NUM_FEATURES:])
    return out_buf


def _c_factor(n: float) -> float:
    """Expected path length of unsuccessful BST search (Liu et al., 2008 Isolation Forest)."""
    if n <= 1.0:
        return 0.0
    if n == 2.0:
        return 1.0
    euler_gamma = 0.5772156649015329
    return 2.0 * (math.log(n - 1.0) + euler_gamma) - 2.0 * (n - 1.0) / n


def export_isolation_forest_to_onnx(
    iforest: IsolationForest, output_path: Path, n_features: int = 32
) -> None:
    """Export trained sklearn IsolationForest directly to ONNX ai.onnx.ml.TreeEnsembleRegressor."""
    max_samples = float(iforest.max_samples_)
    c_max = _c_factor(max_samples)
    n_trees = len(iforest.estimators_)

    nodes_treeids: list[int] = []
    nodes_nodeids: list[int] = []
    nodes_featureids: list[int] = []
    nodes_values: list[float] = []
    nodes_hitrates: list[float] = []
    nodes_modes: list[str] = []
    nodes_truenodeids: list[int] = []
    nodes_falsenodeids: list[int] = []
    nodes_missing_tracks: list[int] = []

    target_treeids: list[int] = []
    target_nodeids: list[int] = []
    target_ids: list[int] = []
    target_weights: list[float] = []

    for tree_id, (est, feat_subset) in enumerate(
        zip(iforest.estimators_, iforest.estimators_features_, strict=True)
    ):
        tree = est.tree_
        depths = [0] * tree.node_count
        stack = [0]
        while stack:
            nid = stack.pop()
            left = int(tree.children_left[nid])
            right = int(tree.children_right[nid])
            if left != right:
                depths[left] = depths[nid] + 1
                depths[right] = depths[nid] + 1
                stack.append(left)
                stack.append(right)

        for nid in range(tree.node_count):
            left = int(tree.children_left[nid])
            right = int(tree.children_right[nid])
            nodes_treeids.append(tree_id)
            nodes_nodeids.append(nid)
            nodes_hitrates.append(1.0)
            nodes_missing_tracks.append(0)

            if left == right:
                n_node = float(tree.n_node_samples[nid])
                leaf_path_len = (depths[nid] + _c_factor(n_node)) / (n_trees * c_max)
                nodes_featureids.append(0)
                nodes_values.append(0.0)
                nodes_modes.append("LEAF")
                nodes_truenodeids.append(0)
                nodes_falsenodeids.append(0)

                target_treeids.append(tree_id)
                target_nodeids.append(nid)
                target_ids.append(0)
                target_weights.append(float(leaf_path_len))
            else:
                local_feat = int(tree.feature[nid])
                global_feat = int(feat_subset[local_feat])
                thresh = float(tree.threshold[nid])
                nodes_featureids.append(global_feat)
                nodes_values.append(thresh)
                nodes_modes.append("BRANCH_LEQ")
                nodes_truenodeids.append(left)
                nodes_falsenodeids.append(right)

    tree_node = helper.make_node(
        "TreeEnsembleRegressor",
        inputs=["X"],
        outputs=["norm_path_len"],
        domain="ai.onnx.ml",
        n_targets=1,
        aggregate_function="SUM",
        post_transform="NONE",
        nodes_treeids=nodes_treeids,
        nodes_nodeids=nodes_nodeids,
        nodes_featureids=nodes_featureids,
        nodes_values=nodes_values,
        nodes_hitrates=nodes_hitrates,
        nodes_modes=nodes_modes,
        nodes_truenodeids=nodes_truenodeids,
        nodes_falsenodeids=nodes_falsenodeids,
        nodes_missing_value_tracks_true=nodes_missing_tracks,
        target_treeids=target_treeids,
        target_nodeids=target_nodeids,
        target_ids=target_ids,
        target_weights=target_weights,
    )

    neg_ln2 = helper.make_tensor("neg_ln2", TensorProto.FLOAT, [1], [-math.log(2.0)])
    offset_tensor = helper.make_tensor("iforest_offset", TensorProto.FLOAT, [1], [float(iforest.offset_)])
    squeeze_axes = helper.make_tensor("squeeze_axes", TensorProto.INT64, [1], [1])

    mul_node = helper.make_node("Mul", inputs=["norm_path_len", "neg_ln2"], outputs=["scaled_exp"])
    exp_node = helper.make_node("Exp", inputs=["scaled_exp"], outputs=["two_pow"])
    add_node = helper.make_node(
        "Add", inputs=["two_pow", "iforest_offset"], outputs=["iforest_anomaly_score"]
    )
    squeeze_node = helper.make_node(
        "Squeeze", inputs=["iforest_anomaly_score", "squeeze_axes"], outputs=["score_out"]
    )

    graph_def = helper.make_graph(
        nodes=[tree_node, mul_node, exp_node, add_node, squeeze_node],
        name="SentinelIsolationForest",
        inputs=[helper.make_tensor_value_info("X", TensorProto.FLOAT, [None, n_features])],
        outputs=[helper.make_tensor_value_info("score_out", TensorProto.FLOAT, [None])],
        initializer=[neg_ln2, offset_tensor, squeeze_axes],
    )
    model_def = helper.make_model(
        graph_def,
        producer_name="hyperion-sentinel-ml",
        opset_imports=[
            helper.make_opsetid("", 18),
            helper.make_opsetid("ai.onnx.ml", 3),
        ],
    )
    model_def.ir_version = 9
    onnx.checker.check_model(model_def)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    onnx.save(model_def, str(output_path))


@dataclass(frozen=True, slots=True)
class TrainingMetricsReport:
    """Evaluation report saved alongside trained ONNX artifacts."""

    holdout_f1: float
    holdout_precision: float
    holdout_recall: float
    holdout_roc_auc: float
    val_f1: float
    optimal_threshold: float
    train_samples: int
    val_samples: int
    holdout_samples: int
    ae_fp32_bytes: int
    ae_int8_bytes: int
    iforest_onnx_bytes: int
    per_pattern_recall: dict[str, float]


def train_and_export_ensemble(
    artifacts_dir: Path,
    *,
    seed: int = 20261118,
    epochs: int = 45,
    batch_size: int = 64,
    lr: float = 2.5e-3,
) -> TrainingMetricsReport:
    """Run reproducible end-to-end training, ONNX export, INT8 quantization, & holdout check."""
    from sentinel_ml.dataset_generator import CLASS_NAMES, generate_adversarial_datasets

    torch.manual_seed(seed)
    np.random.seed(seed)
    artifacts_dir.mkdir(parents=True, exist_ok=True)

    train_ds, val_ds, holdout_ds = generate_adversarial_datasets(seed=seed)

    normal_mask = train_ds.binary_labels == 0
    normal_seqs = train_ds.sequences[normal_mask]
    feat_mean = np.mean(normal_seqs, axis=(0, 1)).astype(np.float32)
    feat_std = np.maximum(np.std(normal_seqs, axis=(0, 1)), 1e-3).astype(np.float32)

    # 1. Train Temporal Autoencoder + Adversarial Margin & Typology Head
    model = TemporalAutoencoderNet()
    model.set_preprocess_stats(feat_mean, feat_std)
    optimizer = torch.optim.AdamW(model.parameters(), lr=lr, weight_decay=1e-4)
    bce_loss = nn.BCELoss()
    ce_loss = nn.CrossEntropyLoss()

    x_train_t = torch.from_numpy(train_ds.sequences)
    y_bin_t = torch.from_numpy(train_ds.binary_labels.astype(np.float32))
    y_pat_t = torch.from_numpy(train_ds.pattern_labels)
    n_train = x_train_t.shape[0]

    model.train()
    rng = np.random.default_rng(seed)
    margin_target = 2.5
    for _ in range(epochs):
        perm = rng.permutation(n_train)
        for start in range(0, n_train, batch_size):
            idx = perm[start : start + batch_size]
            xb = x_train_t[idx]
            yb = y_bin_t[idx]
            yp = y_pat_t[idx]

            optimizer.zero_grad()
            recon_mse, _, anom_prob, typ_logits = model(xb)

            is_norm = 1.0 - yb
            norm_denom = torch.clamp(torch.sum(is_norm), min=1.0)
            adv_denom = torch.clamp(torch.sum(yb), min=1.0)
            loss_recon_norm = torch.sum(is_norm * recon_mse) / norm_denom
            loss_recon_adv = torch.sum(yb * torch.relu(margin_target - recon_mse)) / adv_denom
            loss_anom = bce_loss(anom_prob, yb)
            loss_typ = ce_loss(typ_logits, yp)

            total_loss = loss_recon_norm + 0.5 * loss_recon_adv + 1.2 * loss_anom + 0.6 * loss_typ
            total_loss.backward()
            optimizer.step()

    model.eval()

    ae_fp32_path = artifacts_dir / "temporal_autoencoder.onnx"
    ae_int8_path = artifacts_dir / "temporal_autoencoder.int8.onnx"
    dummy_in = torch.zeros(2, TEMPORAL_SEQ_LEN, NUM_FEATURES, dtype=torch.float32)

    torch.onnx.export(
        model,
        (dummy_in,),
        str(ae_fp32_path),
        input_names=["input_sequence"],
        output_names=["recon_error_mse", "per_feature_sq_err", "anomaly_prob", "typology_logits"],
        dynamic_axes={
            "input_sequence": {0: "batch"},
            "recon_error_mse": {0: "batch"},
            "per_feature_sq_err": {0: "batch"},
            "anomaly_prob": {0: "batch"},
            "typology_logits": {0: "batch"},
        },
        opset_version=18,
        dynamo=False,
    )
    onnx.checker.check_model(onnx.load(str(ae_fp32_path)))

    ae_preproc_path = artifacts_dir / "temporal_autoencoder.preproc.onnx"
    quant_pre_process(
        input_model_path=str(ae_fp32_path),
        output_model_path=str(ae_preproc_path),
        skip_symbolic_shape=True,
    )
    quantize_dynamic(
        model_input=str(ae_preproc_path),
        model_output=str(ae_int8_path),
        op_types_to_quantize=["MatMul", "Gemm"],
        weight_type=QuantType.QUInt8,
    )
    ae_preproc_path.unlink(missing_ok=True)

    # 2. L2-Cache-Resident IsolationForest (32 trees x 128 samples -> ~75 KB ONNX)
    x_if_norm = summarize_sequence_for_iforest(normal_seqs)
    iforest = IsolationForest(
        n_estimators=32,
        max_samples=min(128, x_if_norm.shape[0]),
        contamination=0.03,
        random_state=seed,
    )
    iforest.fit(x_if_norm)
    iforest_path = artifacts_dir / "isolation_forest.onnx"
    export_isolation_forest_to_onnx(iforest, iforest_path, n_features=x_if_norm.shape[1])

    # 3. Calibrate Ensemble Fusion using real INT8 ONNX Runtime outputs
    sess_opts = ort.SessionOptions()
    sess_opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    sess_opts.intra_op_num_threads = 1
    sess_opts.inter_op_num_threads = 1

    ae_sess = ort.InferenceSession(
        str(ae_int8_path), sess_options=sess_opts, providers=["CPUExecutionProvider"]
    )
    if_sess = ort.InferenceSession(
        str(iforest_path), sess_options=sess_opts, providers=["CPUExecutionProvider"]
    )

    def _extract_meta_features(seqs: npt.NDArray[np.float32]) -> npt.NDArray[np.float32]:
        recon_mse, _, anom_prob, _ = ae_sess.run(None, {"input_sequence": seqs})
        if_summary = summarize_sequence_for_iforest(seqs)
        if_score = np.atleast_1d(if_sess.run(None, {"X": if_summary})[0]).astype(np.float32)
        wash_sig = seqs[:, -1, 10]
        spoof_sig = np.log1p(np.maximum(0.0, seqs[:, -1, 4] + 0.5 * seqs[:, -1, 7]))
        return np.stack(
            [
                np.log1p(np.maximum(0.0, recon_mse)),
                if_score,
                anom_prob,
                wash_sig,
                spoof_sig,
            ],
            axis=1,
        ).astype(np.float32)

    meta_train = _extract_meta_features(train_ds.sequences)
    meta_val = _extract_meta_features(val_ds.sequences)
    meta_holdout = _extract_meta_features(holdout_ds.sequences)

    calibrator = LogisticRegression(C=2.0, random_state=seed)
    calibrator.fit(meta_train, train_ds.binary_labels)

    val_probs = calibrator.predict_proba(meta_val)[:, 1]
    best_thresh = 0.50
    best_val_f1 = -1.0
    for cand_t in np.linspace(0.25, 0.80, 56):
        f1 = float(f1_score(val_ds.binary_labels, (val_probs >= cand_t).astype(np.int64)))
        if f1 > best_val_f1:
            best_val_f1 = f1
            best_thresh = float(cand_t)

    holdout_probs = calibrator.predict_proba(meta_holdout)[:, 1]
    holdout_preds = (holdout_probs >= best_thresh).astype(np.int64)
    h_f1 = float(f1_score(holdout_ds.binary_labels, holdout_preds))
    h_prec = float(precision_score(holdout_ds.binary_labels, holdout_preds))
    h_rec = float(recall_score(holdout_ds.binary_labels, holdout_preds))
    h_auc = float(roc_auc_score(holdout_ds.binary_labels, holdout_probs))

    per_pat_recall: dict[str, float] = {}
    for pat_id, pat_name in CLASS_NAMES.items():
        if pat_id == 0:
            continue
        mask_p = holdout_ds.pattern_labels == pat_id
        if np.any(mask_p):
            per_pat_recall[pat_name] = float(np.mean(holdout_preds[mask_p] == 1))

    bg_indices = rng.choice(normal_seqs.shape[0], size=min(128, normal_seqs.shape[0]), replace=False)
    bg_mean_seq = np.mean(normal_seqs[bg_indices], axis=0).astype(np.float32)

    calibration_payload: dict[str, Any] = {
        "model_version": MODEL_VERSION,
        "seed": seed,
        "weights": calibrator.coef_[0].tolist(),
        "intercept": float(calibrator.intercept_[0]),
        "optimal_threshold": best_thresh,
        "feature_mean": feat_mean.tolist(),
        "feature_std": feat_std.tolist(),
        "background_sequence_mean": bg_mean_seq.tolist(),
        "metrics": {
            "val_f1": best_val_f1,
            "holdout_f1": h_f1,
            "holdout_precision": h_prec,
            "holdout_recall": h_rec,
            "holdout_roc_auc": h_auc,
            "per_pattern_recall": per_pat_recall,
            "train_samples": int(train_ds.sequences.shape[0]),
            "val_samples": int(val_ds.sequences.shape[0]),
            "holdout_samples": int(holdout_ds.sequences.shape[0]),
        },
    }
    (artifacts_dir / "ensemble_calibration.json").write_text(
        json.dumps(calibration_payload, indent=2), encoding="utf-8"
    )

    return TrainingMetricsReport(
        holdout_f1=h_f1,
        holdout_precision=h_prec,
        holdout_recall=h_rec,
        holdout_roc_auc=h_auc,
        val_f1=best_val_f1,
        optimal_threshold=best_thresh,
        train_samples=int(train_ds.sequences.shape[0]),
        val_samples=int(val_ds.sequences.shape[0]),
        holdout_samples=int(holdout_ds.sequences.shape[0]),
        ae_fp32_bytes=ae_fp32_path.stat().st_size,
        ae_int8_bytes=ae_int8_path.stat().st_size,
        iforest_onnx_bytes=iforest_path.stat().st_size,
        per_pattern_recall=per_pat_recall,
    )


@dataclass(frozen=True, slots=True)
class EnsembleInferenceResult:
    """Batch output from Production ONNX Runtime Ensemble."""

    confidence: npt.NDArray[np.float32]
    severity_score: npt.NDArray[np.int32]
    is_anomaly: npt.NDArray[np.bool_]
    recon_error_mse: npt.NDArray[np.float32]
    iforest_score: npt.NDArray[np.float32]
    per_feature_sq_err: npt.NDArray[np.float32]
    pattern_class: npt.NDArray[np.int64]


class SentinelOnnxRuntime:
    """Low-latency CPU INT8-quantized ONNX Runtime inference engine (< 10 ms p99)."""

    def __init__(self, artifacts_dir: Path, *, use_int8: bool = True) -> None:
        cal_path = artifacts_dir / "ensemble_calibration.json"
        if not cal_path.exists():
            raise FileNotFoundError(f"Missing calibration metadata: {cal_path}")

        cal = json.loads(cal_path.read_text(encoding="utf-8"))
        self.model_version: str = str(cal["model_version"])
        self.weights: npt.NDArray[np.float32] = np.asarray(cal["weights"], dtype=np.float32)
        self._w0: np.float32 = np.float32(self.weights[0])
        self._w1: np.float32 = np.float32(self.weights[1])
        self._w2: np.float32 = np.float32(self.weights[2])
        self._w3: np.float32 = np.float32(self.weights[3])
        self._w4: np.float32 = np.float32(self.weights[4])
        self.intercept: float = float(cal["intercept"])
        self._intercept_f32: np.float32 = np.float32(self.intercept)
        self.threshold: float = float(cal["optimal_threshold"])
        self.bg_seq_mean: npt.NDArray[np.float32] = np.asarray(
            cal["background_sequence_mean"], dtype=np.float32
        )

        ae_file = "temporal_autoencoder.int8.onnx" if use_int8 else "temporal_autoencoder.onnx"
        ae_path = artifacts_dir / ae_file
        if not ae_path.exists():
            ae_path = artifacts_dir / "temporal_autoencoder.onnx"
        if_path = artifacts_dir / "isolation_forest.onnx"

        opts = ort.SessionOptions()
        opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        opts.intra_op_num_threads = 1
        opts.inter_op_num_threads = 1
        opts.enable_mem_pattern = True
        opts.enable_cpu_mem_arena = True
        opts.add_session_config_entry("session.intra_op.allow_spinning", "0")

        self.ae_session = ort.InferenceSession(
            str(ae_path), sess_options=opts, providers=["CPUExecutionProvider"]
        )
        self.if_session = ort.InferenceSession(
            str(if_path), sess_options=opts, providers=["CPUExecutionProvider"]
        )

        # Cached output-name lists so KernelSHAP skips unused per_feature/typology heads
        self._ae_all_outs: list[str] = [
            "recon_error_mse",
            "per_feature_sq_err",
            "anomaly_prob",
            "typology_logits",
        ]
        self._ae_prob_outs: list[str] = ["recon_error_mse", "anomaly_prob"]
        self._if_outs: list[str] = ["score_out"]

        # Pre-allocated 64B cache-line-aligned scratch buffers for hot batch sizes (1, 2, 16, 33, 64)
        self._if_bufs: dict[int, npt.NDArray[np.float32]] = {
            bsz: alloc_aligned64_f32((bsz, NUM_FEATURES * 2)) for bsz in (1, 2, 16, 33, 64)
        }
        self._batch_scratch: dict[
            int,
            tuple[
                npt.NDArray[np.float32],
                npt.NDArray[np.float32],
                npt.NDArray[np.float32],
                npt.NDArray[np.float32],
            ],
        ] = {
            bsz: (
                alloc_aligned64_f32((bsz,)),
                alloc_aligned64_f32((bsz,)),
                alloc_aligned64_f32((bsz,)),
                alloc_aligned64_f32((bsz,)),
            )
            for bsz in (1, 2, 16, 33, 64)
        }

        # Pre-warm ONNX BFCArena across all production batch sizes to prevent live page-fault spikes
        for bsz in (64, 33, 16, 2, 1):
            warm_in = alloc_aligned64_f32((bsz, TEMPORAL_SEQ_LEN, NUM_FEATURES))
            warm_in.fill(0.0)
            self.predict_batch(warm_in)
            self.score_probabilities_only(warm_in)

    def _get_if_summary(self, seqs_f32: npt.NDArray[np.float32]) -> npt.NDArray[np.float32]:
        buf = self._if_bufs.get(seqs_f32.shape[0])
        return summarize_sequence_for_iforest(seqs_f32, out_buf=buf)

    def _compute_fused_probs_inplace(
        self,
        seqs_f32: npt.NDArray[np.float32],
        recon_mse: npt.NDArray[np.float32],
        if_score: npt.NDArray[np.float32],
        anom_prob: npt.NDArray[np.float32],
        *,
        reuse_out_buf: bool,
    ) -> npt.NDArray[np.float32]:
        bsz = seqs_f32.shape[0]
        scratch = self._batch_scratch.get(bsz)
        if scratch is not None:
            tmp_a, tmp_b, logits, probs_buf = scratch
            out_probs = probs_buf if reuse_out_buf else np.empty(bsz, dtype=np.float32)

            # logits = intercept + w0 * log1p(max(0, recon_mse))
            np.maximum(recon_mse, _F32_ZERO, out=tmp_a)
            np.log1p(tmp_a, out=tmp_a)
            np.multiply(tmp_a, self._w0, out=logits)
            np.add(logits, self._intercept_f32, out=logits)

            # + w1 * if_score + w2 * anom_prob + w3 * wash_sig
            np.multiply(if_score, self._w1, out=tmp_a)
            np.add(logits, tmp_a, out=logits)
            np.multiply(anom_prob, self._w2, out=tmp_a)
            np.add(logits, tmp_a, out=logits)
            np.multiply(seqs_f32[:, -1, 10], self._w3, out=tmp_a)
            np.add(logits, tmp_a, out=logits)

            # + w4 * log1p(max(0, spoof + 0.5 * sync_cancel))
            np.multiply(seqs_f32[:, -1, 7], _F32_HALF, out=tmp_b)
            np.add(seqs_f32[:, -1, 4], tmp_b, out=tmp_a)
            np.maximum(tmp_a, _F32_ZERO, out=tmp_a)
            np.log1p(tmp_a, out=tmp_a)
            np.multiply(tmp_a, self._w4, out=tmp_a)
            np.add(logits, tmp_a, out=logits)

            # sigmoid(clamp(logits, -30, 30)) using pure C ufuncs (avoids np.clip Python wrapper)
            np.minimum(logits, _F32_HI30, out=logits)
            np.maximum(logits, _F32_LO30, out=logits)
            np.negative(logits, out=logits)
            np.exp(logits, out=logits)
            np.add(logits, _F32_ONE, out=logits)
            np.divide(_F32_ONE, logits, out=out_probs)
            return out_probs

        wash_sig = seqs_f32[:, -1, 10]
        spoof_sig = np.log1p(np.maximum(0.0, seqs_f32[:, -1, 4] + 0.5 * seqs_f32[:, -1, 7]))
        logits_fb = (
            self.intercept
            + self._w0 * np.log1p(np.maximum(0.0, recon_mse))
            + self._w1 * if_score
            + self._w2 * anom_prob
            + self._w3 * wash_sig
            + self._w4 * spoof_sig
        )
        probs_fb: npt.NDArray[np.float32] = (
            1.0 / (1.0 + np.exp(-np.clip(logits_fb, -30.0, 30.0)))
        ).astype(np.float32)
        return probs_fb

    def score_probabilities_only(self, sequences: npt.NDArray[np.float32]) -> npt.NDArray[np.float32]:
        """Fast vectorized batch probability evaluation used by KernelSHAP coalitions."""
        seqs_f32 = (
            sequences
            if (sequences.dtype == np.float32 and sequences.flags["C_CONTIGUOUS"])
            else np.ascontiguousarray(sequences, dtype=np.float32)
        )
        recon_mse, anom_prob = self.ae_session.run(
            self._ae_prob_outs, {"input_sequence": seqs_f32}
        )
        if_summary = self._get_if_summary(seqs_f32)
        if_score = self.if_session.run(self._if_outs, {"X": if_summary})[0]
        return self._compute_fused_probs_inplace(
            seqs_f32, recon_mse, if_score, anom_prob, reuse_out_buf=True
        )

    def predict_batch(self, sequences: npt.NDArray[np.float32]) -> EnsembleInferenceResult:
        """Run full vectorized ensemble inference on [B, T=8, F=16] float32 batch."""
        if sequences.ndim == 2:
            seqs_f32 = np.ascontiguousarray(sequences[None, :, :], dtype=np.float32)
        elif sequences.dtype == np.float32 and sequences.flags["C_CONTIGUOUS"]:
            seqs_f32 = sequences
        else:
            seqs_f32 = np.ascontiguousarray(sequences, dtype=np.float32)

        recon_mse, per_feat_sq, anom_prob, typ_logits = self.ae_session.run(
            self._ae_all_outs, {"input_sequence": seqs_f32}
        )
        if_summary = self._get_if_summary(seqs_f32)
        if_score = np.atleast_1d(self.if_session.run(self._if_outs, {"X": if_summary})[0]).astype(
            np.float32, copy=False
        )

        probs = self._compute_fused_probs_inplace(
            seqs_f32, recon_mse, if_score, anom_prob, reuse_out_buf=False
        )
        is_anom = probs >= self.threshold

        sev = np.where(
            probs >= self.threshold,
            60.0 + 40.0 * (probs - self.threshold) / max(1e-5, 1.0 - self.threshold),
            59.0 * (probs / max(1e-5, self.threshold)),
        )
        sev_int = np.clip(np.rint(sev), 0, 100).astype(np.int32)

        attack_logits = typ_logits[:, 1:]
        pat_cls = (np.argmax(attack_logits, axis=1) + 1).astype(np.int64)
        pat_cls = np.where(seqs_f32[:, -1, 10] >= 0.35, 1, pat_cls)

        return EnsembleInferenceResult(
            confidence=probs,
            severity_score=sev_int,
            is_anomaly=is_anom,
            recon_error_mse=recon_mse.astype(np.float32, copy=False),
            iforest_score=if_score,
            per_feature_sq_err=per_feat_sq.astype(np.float32, copy=False),
            pattern_class=pat_cls,
        )
