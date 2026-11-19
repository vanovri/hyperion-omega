"""ONNX Artifact Validation, IsolationForest Parity, Holdout F1 > 0.92, SHAP & Latency Tests."""

from __future__ import annotations

import json
from pathlib import Path

from hypothesis import given, settings, strategies as st
import numpy as np
import onnx
import onnxruntime as ort
from sklearn.ensemble import IsolationForest

from sentinel_ml.benchmark import run_latency_benchmarks
from sentinel_ml.microstructure import NUM_FEATURES, TEMPORAL_SEQ_LEN
from sentinel_ml.models_onnx import SentinelOnnxRuntime, export_isolation_forest_to_onnx
from sentinel_ml.shap_explainer import FastVectorizedKernelSHAP


def test_onnx_artifacts_valid_and_holdout_f1_exceeds_092(models_dir: Path) -> None:
    ae_fp32 = models_dir / "temporal_autoencoder.onnx"
    ae_int8 = models_dir / "temporal_autoencoder.int8.onnx"
    if_onnx = models_dir / "isolation_forest.onnx"
    cal_json = models_dir / "ensemble_calibration.json"

    assert ae_fp32.exists() and ae_fp32.stat().st_size > 1_000
    assert ae_int8.exists() and ae_int8.stat().st_size > 1_000
    assert if_onnx.exists() and if_onnx.stat().st_size > 1_000
    assert cal_json.exists()

    onnx.checker.check_model(onnx.load(str(ae_fp32)))
    onnx.checker.check_model(onnx.load(str(ae_int8)))
    onnx.checker.check_model(onnx.load(str(if_onnx)))

    cal = json.loads(cal_json.read_text(encoding="utf-8"))
    metrics = cal["metrics"]
    assert metrics["holdout_f1"] > 0.92, f"Holdout F1 {metrics['holdout_f1']} <= 0.92"
    assert metrics["holdout_roc_auc"] > 0.95


def test_isolation_forest_onnx_matches_sklearn_exact_scores(tmp_path: Path) -> None:
    rng = np.random.default_rng(123)
    x_train = rng.normal(0.0, 1.0, size=(200, 32)).astype(np.float32)
    x_test = rng.normal(0.5, 2.0, size=(32, 32)).astype(np.float32)

    clf = IsolationForest(n_estimators=24, max_samples=64, random_state=123)
    clf.fit(x_train)
    out_onnx = tmp_path / "if_parity.onnx"
    export_isolation_forest_to_onnx(clf, out_onnx, n_features=32)

    sess = ort.InferenceSession(str(out_onnx), providers=["CPUExecutionProvider"])
    # sklearn decision_function = score_samples - offset_ = -2^(-E[h]/c) - offset_
    # Our ONNX outputs anomaly excess = 2^(-E[h]/c) + offset_ = -decision_function(X)
    expected = (-clf.decision_function(x_test)).astype(np.float32)
    got_batch = sess.run(None, {"X": x_test})[0]
    assert got_batch.shape == (32,)
    np.testing.assert_allclose(got_batch, expected, atol=1e-4)

    # Single-sample batch_size=1 must preserve shape (1,) with zero squeeze warnings
    got_single = sess.run(None, {"X": x_test[:1]})[0]
    assert got_single.shape == (1,)
    np.testing.assert_allclose(got_single, expected[:1], atol=1e-4)


def test_shap_null_player_and_efficiency_properties(models_dir: Path) -> None:
    runtime = SentinelOnnxRuntime(models_dir, use_int8=True)
    explainer = FastVectorizedKernelSHAP(runtime)

    fx_bg, f0_bg, bg_items = explainer.explain(runtime.bg_seq_mean)
    assert abs(fx_bg - f0_bg) < 1e-6
    assert all(item.shap_contribution_ppm == 0 for item in bg_items)

    @settings(max_examples=40, deadline=None)
    @given(
        raw_vals=st.lists(
            st.floats(min_value=0.0, max_value=6.0, allow_nan=False, allow_infinity=False),
            min_size=TEMPORAL_SEQ_LEN * NUM_FEATURES,
            max_size=TEMPORAL_SEQ_LEN * NUM_FEATURES,
        )
    )
    def _check_random_sequence(raw_vals: list[float]) -> None:
        seq = np.asarray(raw_vals, dtype=np.float32).reshape(TEMPORAL_SEQ_LEN, NUM_FEATURES)
        fx, f0, items = explainer.explain(seq)
        assert len(items) == NUM_FEATURES
        phi_sum = sum(i.raw_shap for i in items)
        assert abs((f0 + phi_sum) - fx) < 1e-4
        assert [i.importance_rank for i in items] == list(range(1, NUM_FEATURES + 1))
        abs_ppm = [abs(i.shap_contribution_ppm) for i in items]
        assert abs_ppm == sorted(abs_ppm, reverse=True)

    _check_random_sequence()


def test_end_to_end_latency_sla_p99_under_10ms(models_dir: Path) -> None:
    bench = run_latency_benchmarks(models_dir, num_events=600, write_report=False)
    assert bench["sla_p99_under_10ms_passed"] is True
    assert bench["single_event_e2e"]["p99_ms"] < 10.0
    assert bench["alert_full_shap_proto_path"]["p99_ms"] < 10.0
    assert bench["microbatch_16_onnx"]["p99_ms"] < 10.0
    assert bench["microbatch_64_onnx"]["p99_ms"] < 10.0
