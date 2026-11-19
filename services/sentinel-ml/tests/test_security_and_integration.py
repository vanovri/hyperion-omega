"""Security & Cross-Service Integration Regression Tests (CH10: INT-01, SEC-04)."""

from __future__ import annotations

from pathlib import Path
from fastapi.testclient import TestClient
import pytest

from sentinel_ml.engine import SentinelDetectionEngine
from sentinel_ml.server import create_http_app


def _make_client() -> TestClient:
    models_dir = Path(__file__).resolve().parents[1] / "models"
    engine = SentinelDetectionEngine(models_dir, use_int8=True)
    app = create_http_app(engine)
    return TestClient(app)


def test_subgraph_supports_both_path_and_query_routes_up_to_6_hops() -> None:
    """INT-01: Both /api/v1/fraud/subgraph/{id} and /api/v1/fraud/subgraph?center_account_id=... work."""
    client = _make_client()
    r1 = client.get("/api/v1/fraud/subgraph/9001?symbol=BTC-USDT&max_hops=6")
    assert r1.status_code == 200
    r2 = client.get("/api/v1/fraud/subgraph?center_account_id=9001&symbol=BTC-USDT&max_hops=6")
    assert r2.status_code == 200
    b1 = r1.json()
    b2 = r2.json()
    expected_keys = {"nodes", "edges", "related_alerts", "top_attributions", "snapshot_ts_ns"}
    assert set(b1.keys()) == expected_keys
    assert set(b2.keys()) == expected_keys
    assert b1["nodes"] == b2["nodes"]
    assert b1["edges"] == b2["edges"]


def test_internal_ingest_endpoints_require_token_when_configured(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """SEC-04: Constant-time X-Hyperion-Internal-Token blocks unauthenticated ingest mutations."""
    monkeypatch.setenv("HYPERION_INTERNAL_AUTH_TOKEN", "sentinel-internal-secret-2026")
    monkeypatch.setenv("HYPERION_ENV", "production")
    client = _make_client()

    # /healthz remains open for kubelet probes
    assert client.get("/healthz").status_code == 200

    fill_payload = {
        "fill_id": 1,
        "engine_seq": 1,
        "symbol": "BTC-USDT",
        "maker_account_id": 9001,
        "taker_account_id": 9002,
        "aggressor_side": "BUY",
        "price_fp": "6845000000000",
        "qty_fp": "100000000",
        "exec_ts_ns": 1_700_000_000_000_000_000,
    }
    assert client.post("/api/v1/fraud/ingest/fill", json=fill_payload).status_code == 401
    assert (
        client.post(
            "/api/v1/fraud/ingest/fill",
            json=fill_payload,
            headers={"X-Hyperion-Internal-Token": "wrong"},
        ).status_code
        == 401
    )
    ok = client.post(
        "/api/v1/fraud/ingest/fill",
        json=fill_payload,
        headers={"X-Hyperion-Internal-Token": "sentinel-internal-secret-2026"},
    )
    assert ok.status_code == 200
