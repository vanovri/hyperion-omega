"""
ONNX Model Exporter, ONNXRuntime Numerical Parity Verifier, & Compact Matrix Serializer.
"""

from __future__ import annotations

import csv
import json
from pathlib import Path
from typing import Any
import warnings
import numpy as np
import onnx
import onnxruntime as ort
import torch

from alpha_evo.lob_gym.env import OBS_DIM
from alpha_evo.ppo_agent.network import ActorCriticNetwork
from alpha_evo.ppo_agent.trainer import PPOTrainingResult


def export_ppo_artifacts(
    result: PPOTrainingResult,
    output_dir: str | Path,
) -> dict[str, Any]:
    """
    Export trained PyTorch Actor-Critic network to:
    1. `ppo_execution_agent.onnx` (verified via `onnx.checker` + `onnxruntime` numerical parity!)
    2. `ppo_weights_compact.npz` (compact FP32/FP16 matrix archive for zero-framework inference)
    3. `ppo_learning_curve.json` & `ppo_learning_curve.csv`
    """
    out_dir = Path(output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    model = result.model.cpu().eval()
    onnx_path = out_dir / "ppo_execution_agent.onnx"
    npz_path = out_dir / "ppo_weights_compact.npz"
    curve_json_path = out_dir / "ppo_learning_curve.json"
    curve_csv_path = out_dir / "ppo_learning_curve.csv"

    dummy_obs = torch.randn(8, OBS_DIM, dtype=torch.float32)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        torch.onnx.export(
            model,
            dummy_obs,
            str(onnx_path),
            export_params=True,
            opset_version=17,
            do_constant_folding=True,
            input_names=["obs"],
            output_names=["bounded_action", "state_value"],
            dynamic_axes={
                "obs": {0: "batch_size"},
                "bounded_action": {0: "batch_size"},
                "state_value": {0: "batch_size"},
            },
            dynamo=False,
        )

    onnx_model = onnx.load(str(onnx_path))
    onnx.checker.check_model(onnx_model)

    # Verify numerical parity between PyTorch and ONNXRuntime across 64 random observations
    rng = np.random.default_rng(20261119)
    test_obs = rng.normal(0.0, 1.5, size=(64, OBS_DIM)).astype(np.float32)
    with torch.no_grad():
        pt_act, pt_val = model(torch.from_numpy(test_obs))
        pt_act_np = pt_act.numpy()
        pt_val_np = pt_val.numpy()

    sess = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    ort_act_np, ort_val_np = sess.run(None, {"obs": test_obs})

    max_act_diff = float(np.max(np.abs(pt_act_np - ort_act_np)))
    max_val_diff = float(np.max(np.abs(pt_val_np - ort_val_np)))
    if max_act_diff > 1e-5 or max_val_diff > 1e-5:
        raise RuntimeError(
            f"ONNX parity check failed: max_act_diff={max_act_diff}, max_val_diff={max_val_diff}"
        )

    # Export compact matrix weights (.npz)
    state_np: dict[str, np.ndarray] = {}
    for k, tensor in model.state_dict().items():
        arr = tensor.detach().cpu().numpy()
        state_np[k] = arr.astype(np.float32)
        if arr.ndim >= 2:
            state_np[f"{k}_fp16"] = arr.astype(np.float16)
    np.savez_compressed(str(npz_path), **state_np)

    # Export real learning curve + baseline benchmarks
    payload = {
        "schema_version": "hyperion.omega.ppo_telemetry.v1",
        "wall_time_sec": result.wall_time_sec,
        "onnx_verification": {
            "opset_version": 17,
            "onnx_size_bytes": onnx_path.stat().st_size,
            "compact_npz_size_bytes": npz_path.stat().st_size,
            "ort_max_action_abs_diff": max_act_diff,
            "ort_max_value_abs_diff": max_val_diff,
            "parity_verified": True,
        },
        "out_of_sample_benchmarks": result.benchmark_comparison,
        "learning_curve": result.learning_curve,
    }
    curve_json_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    if result.learning_curve:
        keys = list(result.learning_curve[0].keys())
        with curve_csv_path.open("w", newline="", encoding="utf-8") as fh:
            writer = csv.DictWriter(fh, fieldnames=keys)
            writer.writeheader()
            writer.writerows(result.learning_curve)

    return {
        "onnx_path": str(onnx_path),
        "npz_path": str(npz_path),
        "curve_json_path": str(curve_json_path),
        "curve_csv_path": str(curve_csv_path),
        "onnx_size_bytes": onnx_path.stat().st_size,
        "npz_size_bytes": npz_path.stat().st_size,
        "ort_max_action_abs_diff": max_act_diff,
        "ort_max_value_abs_diff": max_val_diff,
    }
