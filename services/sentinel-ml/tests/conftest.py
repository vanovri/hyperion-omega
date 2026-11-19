"""Shared pytest fixtures for Sentinel-ML test suite."""

from __future__ import annotations

from pathlib import Path

import pytest

from sentinel_ml.models_onnx import train_and_export_ensemble


@pytest.fixture(scope="session")
def models_dir() -> Path:
    repo_models = Path(__file__).resolve().parents[1] / "models"
    if not (repo_models / "ensemble_calibration.json").exists():
        train_and_export_ensemble(repo_models)
    return repo_models
