"""Tests for Channel 19 FastAPI Sovereign Studio Artifact & Prometheus Telemetry Endpoints."""

from __future__ import annotations

import json
from fastapi.testclient import TestClient

from alpha_evo.server import create_app


def test_server_healthz_metrics_and_artifact_routes(tmp_path) -> None:
    (tmp_path / "ppo_execution_agent.onnx").write_bytes(b"onnx-placeholder")
    (tmp_path / "best_alpha_ast.json").write_text(
        json.dumps({"formula_id": "alpha_gp_test", "sexpr": "microprice_skew(bid_px, bid_sz, ask_px, ask_sz)"}),
        encoding="utf-8",
    )
    (tmp_path / "generated_alpha_strategy.rs").write_text("#![no_std]\npub fn ok() {}\n", encoding="utf-8")
    (tmp_path / "dsr_pbo_evaluation.json").write_text(
        json.dumps(
            {
                "dsr_report": {"deflated_sharpe_ratio": 0.9991, "sharpe_annualized": 18.42},
                "pbo_report": {"pbo": 0.04},
            }
        ),
        encoding="utf-8",
    )
    (tmp_path / "ppo_learning_curve.json").write_text(
        json.dumps({"learning_curve": [{"iteration": 1, "mean_episode_reward": 12.5}]}),
        encoding="utf-8",
    )

    client = TestClient(create_app(artifacts_dir=tmp_path))

    r_health = client.get("/healthz")
    assert r_health.status_code == 200
    assert r_health.json()["status"] == "ok"
    assert r_health.json()["channel"] == 19
    assert r_health.json()["artifacts_ready"] is True

    assert client.get("/api/v1/alpha/best-ast").json()["formula_id"] == "alpha_gp_test"
    assert "#![no_std]" in client.get("/api/v1/alpha/wasm-source").text
    assert client.get("/api/v1/alpha/dsr-evaluation").json()["dsr_report"]["deflated_sharpe_ratio"] == 0.9991
    assert len(client.get("/api/v1/ppo/telemetry").json()["learning_curve"]) == 1

    metrics_txt = client.get("/metrics").text
    assert "hyperion_alpha_evo_up 1" in metrics_txt
    assert "hyperion_alpha_evo_dsr 0.9991" in metrics_txt
    assert "hyperion_alpha_evo_pbo 0.04" in metrics_txt
