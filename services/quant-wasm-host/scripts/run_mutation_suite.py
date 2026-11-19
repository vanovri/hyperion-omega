#!/usr/bin/env python3
"""
Hyperion Channel 15 — Programmatic Mutation Survival Zero Harness (`02-extreme-overclock-protocol.md` §1)

Applies surgical AST/operator mutations across core financial fixed-point math (`hyperion-quant-sdk`),
Pre-Trade Risk Gate (`host/src/risk_gate.rs`), and Spatial Bounds Checker (`host/src/shm_ring.rs`),
running `cargo test --test mutation_and_microopt_gate` against each mutant to verify >= 95% kill score.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import time
from dataclasses import dataclass


@dataclass(frozen=True)
class MutantSpec:
    mutant_id: str
    rel_file: str
    description: str
    target_snippet: str
    mutated_snippet: str


MUTANTS: list[MutantSpec] = [
    # --- Module 1: hyperion-quant-sdk fixed-point & rolling stats ---
    MutantSpec(
        "M01_ISQRT_RESULT_OFF_BY_ONE",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate Newton-Raphson return `return x;` -> `return x.saturating_add(1);`",
        "if nx >= x {\n                return x;\n            }",
        "if nx >= x {\n                return x.saturating_add(1);\n            }",
    ),
    MutantSpec(
        "M02_MUL_FP_SCALE",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate fixed-point product scale `/ FP_SCALE_I128` -> `/ 10_000_000`",
        "(prod / FP_SCALE_I128) as i64",
        "(prod / 10_000_000) as i64",
    ),
    MutantSpec(
        "M03_NOTIONAL_FP_SCALE",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate quote notional divisor `/ FP_SCALE_U128` -> `/ 1_000_000`",
        "((price_fp as u128) * (qty_fp as u128)) / FP_SCALE_U128",
        "((price_fp as u128) * (qty_fp as u128)) / 1_000_000",
    ),
    MutantSpec(
        "M04_RATIO_PPM_SIGN",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate ratio_ppm scaling `PPM_SCALE_I128` -> `100_000`",
        "let scaled = num.saturating_mul(PPM_SCALE_I128) / den;",
        "let scaled = num.saturating_mul(100_000) / den;",
    ),
    MutantSpec(
        "M05_ALIGN_ASK_TICK_DIRECTION",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate ask tick rounding upward `saturating_add` -> `saturating_sub`",
        "price_fp.saturating_add(tick_size_fp - rem)",
        "price_fp.saturating_sub(rem)",
    ),
    MutantSpec(
        "M06_ALIGN_LOT_QTY",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate lot rounding `(qty_fp / lot_size_fp) * lot_size_fp` -> `+ lot_size_fp`",
        "(qty_fp / lot_size_fp) * lot_size_fp",
        "((qty_fp / lot_size_fp) + 1) * lot_size_fp",
    ),
    MutantSpec(
        "M07_EWMA_WEIGHT_COMPLEMENT",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate EWMA complement `PPM_SCALE_I128 - a` -> `PPM_SCALE_I128 + a`",
        "let one_minus_a = PPM_SCALE_I128 - a;",
        "let one_minus_a = PPM_SCALE_I128 + a;",
    ),
    MutantSpec(
        "M08_ROLLING_WINDOW_EVICTION_SIGN",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate sliding window sum `(value_fp as i128) - (evicted as i128)` -> `+`",
        "self.sum += (value_fp as i128) - (evicted as i128);",
        "self.sum += (value_fp as i128) + (evicted as i128);",
    ),
    MutantSpec(
        "M09_ZSCORE_DIFF_SIGN",
        "crates/hyperion-quant-sdk/src/lib.rs",
        "Mutate Z-score numerator `(sample_fp - mean_fp)` -> `(mean_fp - sample_fp)`",
        "fp::ratio_ppm((sample_fp as i128) - (mean_fp as i128), std_fp as i128)",
        "fp::ratio_ppm((mean_fp as i128) - (sample_fp as i128), std_fp as i128)",
    ),
    MutantSpec(
        "M10_CRC32C_POLYNOMIAL",
        "crates/hyperion-quant-abi/src/lib.rs",
        "Mutate Castagnoli CRC-32C polynomial `0x82F6_3B78` -> `0xEDB8_8320`",
        "crc = (crc >> 1) ^ (0x82F6_3B78 & mask);",
        "crc = (crc >> 1) ^ (0xEDB8_8320 & mask);",
    ),
    # --- Module 2: Pre-Trade Risk Gate (`host/src/risk_gate.rs`) ---
    MutantSpec(
        "M11_ORDER_SPAM_OFF_BY_ONE",
        "host/src/risk_gate.rs",
        "Mutate per-tick spam cap `self.tick_attempts > max_per_tick` -> `>=`",
        "if self.tick_attempts > max_per_tick {",
        "if self.tick_attempts >= max_per_tick {",
    ),
    MutantSpec(
        "M12_ACCOUNT_HALT_LOGIC",
        "host/src/risk_gate.rs",
        "Mutate account/market halt guard `||` -> `&&`",
        "if !self.policy.account_active || self.policy.market_halted {",
        "if !self.policy.account_active && self.policy.market_halted {",
    ),
    MutantSpec(
        "M13_TICK_SIZE_DIVISIBILITY",
        "host/src/risk_gate.rs",
        "Mutate tick size alignment `!= 0` -> `== 0`",
        "if slot.price_fp % spec.tick_size_fp != 0 {",
        "if slot.price_fp % spec.tick_size_fp == 0 {",
    ),
    MutantSpec(
        "M14_LOT_SIZE_DIVISIBILITY",
        "host/src/risk_gate.rs",
        "Mutate lot size alignment `!= 0` -> `== 0`",
        "if slot.qty_fp % spec.lot_size_fp != 0 {",
        "if slot.qty_fp % spec.lot_size_fp == 0 {",
    ),
    MutantSpec(
        "M15_FAT_FINGER_COLLAR_OFF_BY_ONE",
        "host/src/risk_gate.rs",
        "Mutate fat-finger collar `bps > (self.policy.fat_finger_collar_bps as u128)` -> `>=`",
        "if bps > (self.policy.fat_finger_collar_bps as u128) {",
        "if bps >= (self.policy.fat_finger_collar_bps as u128) {",
    ),
    MutantSpec(
        "M16_POST_ONLY_BUY_CROSS_BOUNDARY",
        "host/src/risk_gate.rs",
        "Mutate POST_ONLY buy crossing `slot.price_fp >= self.ref_best_ask_fp` -> `>`",
        "if slot.side == SIDE_BUY && self.ref_best_ask_fp > 0 && slot.price_fp >= self.ref_best_ask_fp {",
        "if slot.side == SIDE_BUY && self.ref_best_ask_fp > 0 && slot.price_fp > self.ref_best_ask_fp {",
    ),
    MutantSpec(
        "M17_POST_ONLY_SELL_CROSS_BOUNDARY",
        "host/src/risk_gate.rs",
        "Mutate POST_ONLY sell crossing `slot.price_fp <= self.ref_best_bid_fp` -> `<`",
        "if slot.side == SIDE_SELL && self.ref_best_bid_fp > 0 && slot.price_fp <= self.ref_best_bid_fp {",
        "if slot.side == SIDE_SELL && self.ref_best_bid_fp > 0 && slot.price_fp < self.ref_best_bid_fp {",
    ),
    MutantSpec(
        "M18_MAX_ORDER_QTY_OFF_BY_ONE",
        "host/src/risk_gate.rs",
        "Mutate max order qty `slot.qty_fp > self.policy.max_order_qty_fp` -> `>=`",
        "if slot.qty_fp > self.policy.max_order_qty_fp {",
        "if slot.qty_fp >= self.policy.max_order_qty_fp {",
    ),
    MutantSpec(
        "M19_POSITION_DELTA_OFF_BY_ONE",
        "host/src/risk_gate.rs",
        "Mutate worst-case position cap `>` -> `>=`",
        "if worst_case_pos.unsigned_abs() > (self.policy.max_abs_position_qty_fp as u128) {",
        "if worst_case_pos.unsigned_abs() >= (self.policy.max_abs_position_qty_fp as u128) {",
    ),
    MutantSpec(
        "M20_DUPLICATE_CID_CHECK",
        "host/src/risk_gate.rs",
        "Disable duplicate client_order_id check `==` -> `&& false && ==`",
        "if o.client_order_id == slot.client_order_id {",
        "if false && o.client_order_id == slot.client_order_id {",
    ),
    # --- Module 3: Spatial Memory Bounds (`host/src/shm_ring.rs`) ---
    MutantSpec(
        "M21_SHM_REGION_BOUNDS_OFF_BY_ONE",
        "host/src/shm_ring.rs",
        "Mutate shared memory region end check `end > mem_bytes.len()` -> `end >= mem_bytes.len()`",
        "if end > mem_bytes.len() {\n        return Err(ShmBoundsError::OutOfBounds {\n            ptr: base_ptr,",
        "if end >= mem_bytes.len() {\n        return Err(ShmBoundsError::OutOfBounds {\n            ptr: base_ptr,",
    ),
    MutantSpec(
        "M22_EGRESS_SLOT_ALIGNMENT",
        "host/src/shm_ring.rs",
        "Mutate egress slot alignment check `start % 8 != 0` -> `start % 64 != 0 && false`",
        "if start % 8 != 0 {",
        "if false && start % 8 != 0 {",
    ),
]


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    manifest = root / "Cargo.toml"

    # Run all 3 fast deterministic boundary tests in release/offline mode (reusing cached deps!):
    print(f"[MUTATION] Running baseline test gate on {manifest} ...")
    test_args = [
        "cargo",
        "test",
        "--release",
        "--offline",
        "--manifest-path",
        str(manifest),
        "-p",
        "quant-wasm-host",
        "--test",
        "mutation_and_microopt_gate",
        "--",
        "--skip",
        "test_three_pass_micro_optimization_latency_and_allocation_table",
    ]
    baseline = subprocess.run(test_args, capture_output=True, text=True)
    if baseline.returncode != 0:
        print("[MUTATION] Baseline failed!\n", baseline.stdout, baseline.stderr)
        return 1

    results = []
    killed = 0
    t0_all = time.perf_counter()

    for m in MUTANTS:
        fpath = root / m.rel_file
        original = fpath.read_text(encoding="utf-8")
        if m.target_snippet not in original:
            print(f"[MUTATION] ERROR: snippet not found for {m.mutant_id} in {m.rel_file}")
            return 2

        try:
            mutated = original.replace(m.target_snippet, m.mutated_snippet, 1)
            fpath.write_text(mutated, encoding="utf-8")
            t0 = time.perf_counter()
            try:
                proc = subprocess.run(test_args, capture_output=True, text=True, timeout=180)
                dt = time.perf_counter() - t0
                was_killed = proc.returncode != 0
                status = "KILLED" if was_killed else "SURVIVED"
            except subprocess.TimeoutExpired:
                dt = time.perf_counter() - t0
                was_killed = True
                status = "KILLED_TIMEOUT"
            if was_killed:
                killed += 1
            print(f"  [{status}] {m.mutant_id:<34} ({dt:.2f}s) - {m.description}", flush=True)
            results.append(
                {
                    "mutant_id": m.mutant_id,
                    "file": m.rel_file,
                    "description": m.description,
                    "status": status,
                    "elapsed_sec": round(dt, 3),
                }
            )
        finally:
            fpath.write_text(original, encoding="utf-8")

    # Restore clean incremental artifact state
    subprocess.run(test_args, capture_output=True, text=True)

    total = len(MUTANTS)
    score = (killed / total) * 100.0
    total_dt = time.perf_counter() - t0_all
    report = {
        "killed": killed,
        "total": total,
        "mutation_score_pct": round(score, 2),
        "total_elapsed_sec": round(total_dt, 2),
        "mutants": results,
    }
    out_path = root / "artifacts" / "mutation_report.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(f"\n[MUTATION SUMMARY] Killed {killed}/{total} ({score:.2f}%) in {total_dt:.1f}s -> {out_path}")
    return 0 if score >= 95.0 else 3


if __name__ == "__main__":
    sys.exit(main())
