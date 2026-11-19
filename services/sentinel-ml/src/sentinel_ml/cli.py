"""CLI Entrypoint for HYPERION Channel 05 Sentinel-ML (`train`, `benchmark`, `serve`)."""

from __future__ import annotations

import argparse
from dataclasses import asdict
import json
from pathlib import Path
import sys

from sentinel_ml.benchmark import run_latency_benchmarks
from sentinel_ml.models_onnx import train_and_export_ensemble
from sentinel_ml.overclock_audit import run_full_overclock_audit


def _default_models_dir() -> Path:
    return Path(__file__).resolve().parents[2] / "models"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="sentinel-ml", description="HYPERION Sentinel-ML CLI")
    sub = parser.add_subparsers(dest="command", required=True)

    p_train = sub.add_parser(
        "train", help="Generate adversarial dataset, train ensemble, export ONNX"
    )
    p_train.add_argument("--models-dir", type=Path, default=_default_models_dir())
    p_train.add_argument("--epochs", type=int, default=45)
    p_train.add_argument("--seed", type=int, default=20261118)

    p_bench = sub.add_parser("benchmark", help="Run E2E and micro-batch latency benchmark")
    p_bench.add_argument("--models-dir", type=Path, default=_default_models_dir())
    p_bench.add_argument("--events", type=int, default=2000)

    p_oc = sub.add_parser(
        "overclock", help="Run AST mutation (>=95%), 500k tracemalloc soak, & 3-pass micro-opt"
    )
    p_oc.add_argument("--models-dir", type=Path, default=_default_models_dir())

    sub.add_parser("serve", help="Start gRPC (:50153), HTTP/REST (:8084), & Redpanda worker")

    args = parser.parse_args(argv)
    if args.command == "train":
        report = train_and_export_ensemble(args.models_dir, seed=args.seed, epochs=args.epochs)
        print(json.dumps(asdict(report), indent=2))
        return 0 if report.holdout_f1 > 0.92 else 1

    if args.command == "benchmark":
        bench = run_latency_benchmarks(args.models_dir, num_events=args.events)
        print(json.dumps(bench, indent=2))
        return 0 if bench["sla_p99_under_10ms_passed"] else 1

    if args.command == "overclock":
        oc = run_full_overclock_audit(args.models_dir)
        print(json.dumps(oc, indent=2))
        return 0 if oc["all_overclock_gates_passed"] else 1

    if args.command == "serve":
        from sentinel_ml.server import main as serve_main

        serve_main()
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
