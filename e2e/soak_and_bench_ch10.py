#!/usr/bin/env python3
"""
HYPERION Channel 10 — Gate 2 (500,000-Cycle tracemalloc Zero-Leak Soak)
& Gate 3 (3-Pass Hot-Path Micro-Optimization Benchmark).
"""

from __future__ import annotations

import gc
import json
import statistics
import sys
import time
import tracemalloc
from pathlib import Path

_THIS_DIR = str(Path(__file__).resolve().parent)
if _THIS_DIR not in sys.path:
    sys.path.insert(0, _THIS_DIR)

from crucible_core import (
    FP_SCALE,
    encode_ws_binary_frame,
    hotpath_pass1_baseline,
    hotpath_pass2_zerocopy,
    hotpath_pass3_precompiled_flat,
)


def _execute_soak_cycles(
    mv: memoryview,
    flat_balances: tuple[int, ...],
    treasury: int,
    deposits: int,
    cycles: int,
) -> float:
    t0 = time.perf_counter()
    fn = hotpath_pass3_precompiled_flat
    for _ in range(cycles):
        if not fn(mv, flat_balances, treasury, deposits):
            raise RuntimeError("Soak invariant failed")
    return time.perf_counter() - t0


def run_500k_soak() -> dict:
    # 64-byte realistic L2/Fill payload (typical binary market-data frame size)
    payload = bytes((i * 19 + 7) & 0xFF for i in range(64))
    frame = encode_ws_binary_frame(0x01, 1, 1001, 1700000000000000000, payload, with_crc32c=True)
    mv = memoryview(frame)
    flat_balances = (400 * FP_SCALE, 100 * FP_SCALE, 490 * FP_SCALE, 0)
    treasury = 10 * FP_SCALE
    deposits = 1000 * FP_SCALE
    cycles = 500_000

    # Warmup outside tracemalloc to stabilize bytecode specialization & inline caches
    _execute_soak_cycles(mv, flat_balances, treasury, deposits, 10_000)

    elapsed_box = [0.0]
    gc.collect()
    tracemalloc.start()
    elapsed_box[0] = _execute_soak_cycles(mv, flat_balances, treasury, deposits, 5_000)
    gc.collect()
    mem_after_warmup, _ = tracemalloc.get_traced_memory()

    elapsed_box[0] = _execute_soak_cycles(mv, flat_balances, treasury, deposits, cycles)

    gc.collect()
    mem_after_soak, peak_mem = tracemalloc.get_traced_memory()
    tracemalloc.stop()

    elapsed = elapsed_box[0]
    steady_state_delta = max(0, mem_after_soak - mem_after_warmup)
    return {
        "cycles": cycles,
        "elapsed_sec": round(elapsed, 4),
        "ops_per_sec": round(cycles / elapsed, 1),
        "mem_after_warmup_bytes": mem_after_warmup,
        "mem_after_500k_bytes": mem_after_soak,
        "steady_state_leak_bytes": steady_state_delta,
        "peak_traced_bytes": peak_mem,
    }


def bench_pass(name: str, fn, iterations: int = 30_000, batches: int = 30) -> dict:
    batch_size = iterations // batches
    per_op_ns_samples: list[float] = []

    # Warmup
    for _ in range(2_000):
        fn()

    # Measure transient allocation delta via tracemalloc over 2,000 ops
    gc.collect()
    tracemalloc.start()
    before_cur, _ = tracemalloc.get_traced_memory()
    for _ in range(2_000):
        fn()
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    bytes_per_op = max(0.0, (peak - before_cur) / 2_000.0)

    # Timing batches for p50 / p99
    for _ in range(batches):
        t0 = time.perf_counter_ns()
        for _ in range(batch_size):
            if not fn():
                raise RuntimeError(f"Benchmark {name} returned False")
        dt_ns = time.perf_counter_ns() - t0
        per_op_ns_samples.append(dt_ns / batch_size)

    per_op_ns_samples.sort()
    p50_ns = statistics.median(per_op_ns_samples)
    p99_idx = min(len(per_op_ns_samples) - 1, int(len(per_op_ns_samples) * 0.99))
    p99_ns = per_op_ns_samples[p99_idx]

    return {
        "pass": name,
        "p50_ns_per_op": round(p50_ns, 1),
        "p99_ns_per_op": round(p99_ns, 1),
        "bytes_per_op": round(bytes_per_op, 2),
    }


def main() -> int:
    here = Path(__file__).resolve().parent
    payload = bytes((i * 19 + 7) & 0xFF for i in range(64))
    frame = encode_ws_binary_frame(0x01, 1, 1001, 1700000000000000000, payload, with_crc32c=True)
    mv = memoryview(frame)
    bals = [(400 * FP_SCALE, 100 * FP_SCALE), (490 * FP_SCALE, 0)]
    flat_bals = (400 * FP_SCALE, 100 * FP_SCALE, 490 * FP_SCALE, 0)
    treasury = 10 * FP_SCALE
    deposits = 1000 * FP_SCALE

    soak = run_500k_soak()
    p1 = bench_pass("Pass 1: Baseline (Slice Copy + Byte-CRC + Generator)", lambda: hotpath_pass1_baseline(frame, bals, treasury, deposits))
    p2 = bench_pass("Pass 2: Zero-Copy (memoryview + struct.unpack_from)", lambda: hotpath_pass2_zerocopy(mv, bals, treasury, deposits))
    p3 = bench_pass("Pass 3: Precompiled Struct + Slice-by-4 CRC32C + Flat Sum", lambda: hotpath_pass3_precompiled_flat(mv, flat_bals, treasury, deposits))

    speedup_pct = round((1.0 - (p3["p50_ns_per_op"] / p1["p50_ns_per_op"])) * 100.0, 2)
    report = {
        "soak_500k": soak,
        "optimization_passes": [p1, p2, p3],
        "p50_latency_reduction_pct": speedup_pct,
    }
    out_dir = here / "results"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "ch10_overclock_report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
