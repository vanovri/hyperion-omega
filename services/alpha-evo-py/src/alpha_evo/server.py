"""
FastAPI Telemetry, Artifact Inspection, & Prometheus Metrics Server for Channel 19 (`alpha-evo-py`).

Exposes read-only quantitative artifacts for Channel 15 (`quant-wasm-host`) and
Channel 20 (`apps/sovereign-studio` Symbolic AST & DSR Equity Curve Workbench).
Default HTTP port: `8098` (canonical CH11 `01-hyperion-omega.md` port; configurable via `ALPHA_EVO_HTTP_PORT` / `PORT`).
Redpanda topic: `hyperion.omega.alpha.formulas.v1` (key `gen:<generation_id>`).
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any
from fastapi import FastAPI, HTTPException
from fastapi.responses import PlainTextResponse
import uvicorn

DEFAULT_ARTIFACTS_DIR = Path(__file__).resolve().parents[2] / "artifacts"


def create_app(artifacts_dir: Path | None = None) -> FastAPI:
    art_dir = artifacts_dir or Path(os.environ.get("ALPHA_EVO_ARTIFACTS_DIR", str(DEFAULT_ARTIFACTS_DIR)))
    app = FastAPI(
        title="Hyperion Omega — Channel 19 Alpha-Evo Quantitative AI Service",
        version="1.0.0",
    )

    def _load_json(filename: str) -> dict[str, Any]:
        p = art_dir / filename
        if not p.exists():
            raise HTTPException(status_code=404, detail=f"Artifact {filename} not generated yet")
        return json.loads(p.read_text(encoding="utf-8"))

    @app.get("/healthz")
    def healthz() -> dict[str, Any]:
        onnx_exists = (art_dir / "ppo_execution_agent.onnx").exists()
        ast_exists = (art_dir / "best_alpha_ast.json").exists()
        return {
            "status": "ok",
            "service": "alpha-evo-py",
            "channel": 19,
            "artifacts_ready": bool(onnx_exists and ast_exists),
        }

    @app.get("/api/v1/alpha/best-ast")
    def get_best_ast() -> dict[str, Any]:
        return _load_json("best_alpha_ast.json")

    @app.get("/api/v1/alpha/wasm-source", response_class=PlainTextResponse)
    def get_wasm_source() -> str:
        p = art_dir / "generated_alpha_strategy.rs"
        if not p.exists():
            raise HTTPException(status_code=404, detail="Rust Wasm strategy source not found")
        return p.read_text(encoding="utf-8")

    @app.get("/api/v1/alpha/dsr-evaluation")
    def get_dsr_evaluation() -> dict[str, Any]:
        return _load_json("dsr_pbo_evaluation.json")

    @app.get("/api/v1/ppo/telemetry")
    def get_ppo_telemetry() -> dict[str, Any]:
        return _load_json("ppo_learning_curve.json")

    @app.get("/metrics", response_class=PlainTextResponse)
    def prometheus_metrics() -> str:
        lines = [
            "# HELP hyperion_alpha_evo_up Service health indicator.",
            "# TYPE hyperion_alpha_evo_up gauge",
            "hyperion_alpha_evo_up 1",
        ]
        dsr_file = art_dir / "dsr_pbo_evaluation.json"
        if dsr_file.exists():
            d = json.loads(dsr_file.read_text(encoding="utf-8"))
            dsr = d.get("dsr_report", {}).get("deflated_sharpe_ratio", 0.0)
            pbo = d.get("pbo_report", {}).get("pbo", 1.0)
            sr = d.get("dsr_report", {}).get("sharpe_annualized", 0.0)
            lines.extend(
                [
                    "# HELP hyperion_alpha_evo_dsr Deflated Sharpe Ratio of promoted GP alpha.",
                    "# TYPE hyperion_alpha_evo_dsr gauge",
                    f"hyperion_alpha_evo_dsr {dsr}",
                    "# HELP hyperion_alpha_evo_pbo CSCV Probability of Backtest Overfitting.",
                    "# TYPE hyperion_alpha_evo_pbo gauge",
                    f"hyperion_alpha_evo_pbo {pbo}",
                    "# HELP hyperion_alpha_evo_oos_sharpe Walk-forward OOS annualized Sharpe ratio.",
                    "# TYPE hyperion_alpha_evo_oos_sharpe gauge",
                    f"hyperion_alpha_evo_oos_sharpe {sr}",
                ]
            )
        return "\n".join(lines) + "\n"

    return app


def main() -> None:
    port = int(
        os.environ.get(
            "HTTP_PORT",
            os.environ.get("ALPHA_EVO_HTTP_PORT", os.environ.get("PORT", "8098")),
        )
    )
    uvicorn.run(create_app(), host="0.0.0.0", port=port)


if __name__ == "__main__":
    main()
