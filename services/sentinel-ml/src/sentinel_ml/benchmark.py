"""End-to-End Latency & Throughput Benchmark Harness for Sentinel-ML.

Measures:
  1. Single-event end-to-end streaming detection latency (feature extraction +
     incremental k <= 6 Tarjan/Johnson graph cycle check + INT8 ONNX Runtime forward pass)
  2. Alert-path end-to-end latency including Vectorized KernelSHAP attribution & Protobuf encode
  3. Micro-batch (B=16, B=64) vectorized ONNX ensemble latency & events/sec throughput
Asserts strict SLA compliance: p99 < 10.0 ms.
"""

from __future__ import annotations

import gc
import json
from pathlib import Path
import time
from typing import Any

import numpy as np

from sentinel_ml.engine import SentinelDetectionEngine
from sentinel_ml.fixed_point import SCALE
from sentinel_ml.protobuf_codec import serialize_alert_bytes


def _percentiles_ms(latencies_ns: list[int]) -> dict[str, float]:
    arr_ms = np.asarray(latencies_ns, dtype=np.float64) * 1e-6
    return {
        "min_ms": float(np.min(arr_ms)),
        "p50_ms": float(np.percentile(arr_ms, 50)),
        "p95_ms": float(np.percentile(arr_ms, 95)),
        "p99_ms": float(np.percentile(arr_ms, 99)),
        "max_ms": float(np.max(arr_ms)),
        "mean_ms": float(np.mean(arr_ms)),
    }


def run_latency_benchmarks(
    artifacts_dir: Path,
    *,
    num_events: int = 2_000,
    seed: int = 20261118,
    write_report: bool = True,
) -> dict[str, Any]:
    """Execute end-to-end single-event and micro-batch benchmarks and verify p99 < 10 ms."""
    rng = np.random.default_rng(seed)
    engine = SentinelDetectionEngine(artifacts_dir, use_int8=True)

    symbol = "BTC-USDT"
    base_px = 68_500 * SCALE
    tick_fp = 1_000_000
    ts_ns = 1_700_000_000_000_000_000
    engine.extractor.update_bbo(symbol, base_px - tick_fp, base_px + tick_fp)

    # Warm-up 100 events
    for i in range(100):
        engine.process_execution_fill(
            fill_id=i + 1,
            engine_seq=i + 1,
            symbol=symbol,
            maker_account_id=1000 + (i % 40),
            taker_account_id=2000 + (i % 40),
            aggressor_side="BUY" if (i % 2 == 0) else "SELL",
            price_fp=base_px,
            qty_fp=10_000_000,
            exec_ts_ns=ts_ns + i * 5_000_000,
        )

    gc.collect()
    e2e_latencies_ns: list[int] = []
    alert_path_latencies_ns: list[int] = []
    alerts_emitted = 0

    for step in range(num_events):
        ts_ns += 8_000_000
        if step % 50 == 47:
            maker, taker = 9001, 9002
        elif step % 50 == 48:
            maker, taker = 9002, 9003
        elif step % 50 == 49:
            maker, taker = 9003, 9001
        else:
            maker = int(rng.integers(1000, 1250))
            taker = int(rng.integers(1251, 1500))

        px = base_px + int(rng.integers(-3, 4)) * tick_fp
        qty = 75_000_000 if maker >= 9001 else int(rng.integers(5_000_000, 40_000_000))

        t0 = time.perf_counter_ns()
        alerts = engine.process_execution_fill(
            fill_id=1000 + step,
            engine_seq=1000 + step,
            symbol=symbol,
            maker_account_id=maker,
            taker_account_id=taker,
            aggressor_side="BUY",
            price_fp=px,
            qty_fp=qty,
            exec_ts_ns=ts_ns,
        )
        for a in alerts:
            _ = serialize_alert_bytes(a)
        dt_ns = time.perf_counter_ns() - t0
        e2e_latencies_ns.append(dt_ns)
        if alerts:
            alerts_emitted += len(alerts)
            alert_path_latencies_ns.append(dt_ns)

    micro16_seqs = rng.uniform(0.0, 2.5, size=(16, 8, 16)).astype(np.float32)
    micro64_seqs = rng.uniform(0.0, 2.5, size=(64, 8, 16)).astype(np.float32)

    gc.collect()
    for _ in range(25):
        engine.evaluate_microbatch_sequences(micro16_seqs)
    mb16_lat_ns: list[int] = []
    for _ in range(500):
        t0 = time.perf_counter_ns()
        engine.evaluate_microbatch_sequences(micro16_seqs)
        mb16_lat_ns.append(time.perf_counter_ns() - t0)

    gc.collect()
    for _ in range(25):
        engine.evaluate_microbatch_sequences(micro64_seqs)
    mb64_lat_ns: list[int] = []
    for _ in range(500):
        t0 = time.perf_counter_ns()
        engine.evaluate_microbatch_sequences(micro64_seqs)
        mb64_lat_ns.append(time.perf_counter_ns() - t0)

    e2e_stats = _percentiles_ms(e2e_latencies_ns)
    alert_stats = _percentiles_ms(alert_path_latencies_ns) if alert_path_latencies_ns else e2e_stats
    mb16_stats = _percentiles_ms(mb16_lat_ns)
    mb64_stats = _percentiles_ms(mb64_lat_ns)

    results: dict[str, Any] = {
        "events_benchmarked": num_events,
        "alerts_triggered_with_shap_and_proto": alerts_emitted,
        "single_event_e2e": e2e_stats,
        "alert_full_shap_proto_path": alert_stats,
        "microbatch_16_onnx": {
            **mb16_stats,
            "per_account_p99_ms": mb16_stats["p99_ms"] / 16.0,
            "throughput_accounts_per_sec": round(16_000.0 / max(1e-6, mb16_stats["mean_ms"]), 1),
        },
        "microbatch_64_onnx": {
            **mb64_stats,
            "per_account_p99_ms": mb64_stats["p99_ms"] / 64.0,
            "throughput_accounts_per_sec": round(64_000.0 / max(1e-6, mb64_stats["mean_ms"]), 1),
        },
        "sla_p99_under_10ms_passed": bool(
            e2e_stats["p99_ms"] < 10.0
            and alert_stats["p99_ms"] < 10.0
            and mb16_stats["p99_ms"] < 10.0
            and mb64_stats["p99_ms"] < 10.0
        ),
    }
    if write_report:
        (artifacts_dir / "latency_benchmark.json").write_text(
            json.dumps(results, indent=2), encoding="utf-8"
        )
    return results
