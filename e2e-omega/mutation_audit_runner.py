#!/usr/bin/env python3
"""
HYPERION OMEGA Channel 20 — Targeted Financial, Consensus & Cryptographic Mutation Testing Harness
Implements Untch-Offutt-Harrold Mutant Schemata (single compiled supermutant + 26 isolated test runs)
to enforce .agents/rules/02-extreme-overclock-protocol.md Gate #1:
Mutation Kill Score >= 95.0% (Target: 100.0%, 0 surviving critical mutants).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any


# Each mutant is a surgical expression-level or condition-level fault injected into src/lib.rs
MUTANTS: list[dict[str, str]] = [
    {
        "id": "M01_FP_NOTIONAL_SCALE_SHIFT",
        "subsystem": "CH01/CH02 Fixed-Point Notional",
        "kind": "expr",
        "target": "((price_fp as i128) * (qty_fp as i128)) / FP_SCALE",
        "replacement": "((price_fp as i128) * (qty_fp as i128)) / (FP_SCALE / 10)",
    },
    {
        "id": "M02_FEE_TRUNC_ARITHMETIC",
        "subsystem": "CH01/CH02 Signed Fee/Rebate",
        "kind": "expr",
        "target": "(notional_fp * (fee_ppm as i128)) / PPM_SCALE",
        "replacement": "(notional_fp + (fee_ppm as i128)) / PPM_SCALE",
    },
    {
        "id": "M03_BYPASS_SELF_TRADE_PREVENTION",
        "subsystem": "CH02 LOB Self-Trade Prevention",
        "kind": "cond",
        "target": "maker.account_id == taker.account_id",
        "replacement": "false",
    },
    {
        "id": "M04_WEAKEN_FROZEN_ACCOUNT_GATE",
        "subsystem": "CH02/CH06 Aegis Account Freeze",
        "kind": "cond",
        "target": "maker.frozen || taker.frozen",
        "replacement": "maker.frozen && taker.frozen",
    },
    {
        "id": "M05_TAKER_SOLVENCY_BOUNDARY",
        "subsystem": "CH02 Pre-Trade Balance Check",
        "kind": "cond",
        "target": "taker.available_fp < taker_required_fp",
        "replacement": "taker.available_fp < 0",
    },
    {
        "id": "M06_INVERT_MAKER_REBATE_SIGN",
        "subsystem": "CH02 Maker Rebate Settlement",
        "kind": "expr",
        "target": "notional_fp - maker_fee_fp",
        "replacement": "notional_fp + maker_fee_fp",
    },
    {
        "id": "M07_DROP_MAKER_FEE_FROM_TREASURY",
        "subsystem": "CH02 Double-Entry Treasury Pool",
        "kind": "expr",
        "target": "maker_fee_fp + taker_fee_fp",
        "replacement": "taker_fee_fp",
    },
    {
        "id": "M08_RELAX_DOUBLE_ENTRY_EQUALITY",
        "subsystem": "CH02/CH11 Conservation Invariant",
        "kind": "expr",
        "target": "lhs == rhs",
        "replacement": "lhs >= rhs",
    },
    {
        "id": "M09_IGNORE_WAL_CRC32C_BITROT",
        "subsystem": "CH02/CH08 HYPWAL01 CRC32C Check",
        "kind": "cond",
        "target": "recomputed.crc32c_checksum != frame.crc32c_checksum",
        "replacement": "false",
    },
    {
        "id": "M10_IGNORE_WAL_SEQ_GAP",
        "subsystem": "CH02/CH08 WAL Monotonic Sequence",
        "kind": "cond",
        "target": "frame.engine_seq != expected_seq || frame.prev_digest != expected_prev",
        "replacement": "frame.prev_digest != expected_prev",
    },
    {
        "id": "M11_MINORITY_RAFT_QUORUM_SPLITBRAIN",
        "subsystem": "CH12 5-Node Raft Quorum Formula",
        "kind": "expr",
        "target": "(cluster_size / 2) + 1",
        "replacement": "cluster_size / 2",
    },
    {
        "id": "M12_BYPASS_RAFT_PREVOTE_SHIELD",
        "subsystem": "CH12 Raft PreVote Protocol",
        "kind": "cond",
        "target": "pre_votes < needed_votes",
        "replacement": "pre_votes == 0",
    },
    {
        "id": "M13_RAFT_VOTE_STALE_LOG_CANDIDATE",
        "subsystem": "CH12 Raft Log Completeness",
        "kind": "cond",
        "target": "granted_votes >= needed_votes",
        "replacement": "granted_votes > needed_votes + 2",
    },
    {
        "id": "M14_RAFT_STALE_TERM_NOT_INCREMENTED",
        "subsystem": "CH12 Raft Election Term Monotonicity",
        "kind": "expr",
        "target": "highest_term + 1",
        "replacement": "highest_term",
    },
    {
        "id": "M15_CROSS_MARGIN_BLINDSPOT",
        "subsystem": "CH14 Maintenance Margin Gate",
        "kind": "cond",
        "target": "*eq >= mm",
        "replacement": "*eq >= -100 * FP_SCALE",
    },
    {
        "id": "M16_INSURANCE_FUND_SIGN_FLIP",
        "subsystem": "CH14 Stage-3 Insurance Absorption",
        "kind": "stmt",
        "target": "*insurance_fund_fp -= deficit;",
        "replacement": "*insurance_fund_fp += deficit;",
    },
    {
        "id": "M17_OMIT_STAGE4_QUANTILE_ADL",
        "subsystem": "CH14 Stage-4 Quantile ADL",
        "kind": "stmt",
        "target": "*profitable_adl_pool_fp -= rem;\n                    adl_used_fp += rem;",
        "replacement": "unbacked_deficit_fp += rem;",
    },
    {
        "id": "M18_BYPASS_EBPF_CIDR_BLOCKLIST",
        "subsystem": "CH16 eBPF XDP LPM Blocklist",
        "kind": "cond",
        "target": "blocked_ips.binary_search(&src_ip_u32).is_ok()",
        "replacement": "false",
    },
    {
        "id": "M19_EBPF_OFF_BY_ONE_RATE_LIMIT",
        "subsystem": "CH16 eBPF Token-Bucket Limiter",
        "kind": "cond",
        "target": "current_ip_pps > max_pps_per_ip",
        "replacement": "current_ip_pps >= max_pps_per_ip",
    },
    {
        "id": "M20_EBPF_ACCEPT_BAD_WIRE_MAGIC",
        "subsystem": "CH16 0x4859 Wire Magic Guard",
        "kind": "cond",
        "target": "magic != WS_MAGIC_LE || version != WS_VERSION",
        "replacement": "version != WS_VERSION",
    },
    {
        "id": "M21_DISABLE_WASH_RING_DETECTOR",
        "subsystem": "CH05 Temporal Multigraph Cycle",
        "kind": "cond",
        "target": "v == target && path.len() >= 2",
        "replacement": "v == target && path.len() >= 99",
    },
    {
        "id": "M22_BYPASS_WASM_FUEL_METERING",
        "subsystem": "CH15 Wasmtime Fuel Exhaustion Trap",
        "kind": "cond",
        "target": "instructions_requested > fuel_budget",
        "replacement": "false",
    },
    {
        "id": "M23_BYPASS_WASM_16MB_MEMORY_CAP",
        "subsystem": "CH15 Wasmtime 16MiB Linear Cap",
        "kind": "cond",
        "target": "memory_requested_kb > memory_limit_kb",
        "replacement": "false",
    },
    {
        "id": "M24_ALLOW_NEGATIVE_GHOST_LEAF_MSST",
        "subsystem": "CH13 ZK Merkle-Sum Range Proof",
        "kind": "expr",
        "target": "range_proof_ok && reserves_cover",
        "replacement": "reserves_cover",
    },
    {
        "id": "M25_ALLOW_INSOLVENT_VAULT_RESERVES",
        "subsystem": "CH13/CH18 Solvency Inequality",
        "kind": "expr",
        "target": "vault_reserves_fp >= root_sum",
        "replacement": "vault_reserves_fp > 0",
    },
    {
        "id": "M26_IGNORE_L2_4BLOCK_CHAIN_REORG",
        "subsystem": "CH18 Chain Relayer Reorg Detector",
        "kind": "expr",
        "target": "c.block_hash != blk.block_hash",
        "replacement": "false",
    },
]

SCHEMATA_HELPER = """
fn __hyperion_active_mutant() -> u32 {
    std::env::var("HYPERION_MUTANT_ID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}
"""


def find_rustc() -> str:
    explicit = Path.home() / ".cargo" / "bin" / ("rustc.exe" if os.name == "nt" else "rustc")
    if explicit.exists():
        return str(explicit)
    found = shutil.which("rustc")
    if not found:
        raise RuntimeError("rustc not found in PATH or ~/.cargo/bin")
    return found


def build_schemata_source(original_src: str) -> str:
    src = original_src + "\n" + SCHEMATA_HELPER
    for idx, m in enumerate(MUTANTS, start=1):
        target = m["target"]
        rep = m["replacement"]
        if target not in src:
            raise RuntimeError(f"Target pattern not found for mutant {m['id']}: {target}")
        if m["kind"] == "stmt":
            wrapped = f"if __hyperion_active_mutant() == {idx} {{ {rep} }} else {{ {target} }}"
        else:
            wrapped = f"(if __hyperion_active_mutant() == {idx} {{ {rep} }} else {{ {target} }})"
        src = src.replace(target, wrapped, 1)
    return src


def run_mutation_campaign(crate_dir: Path, out_json: Path, out_md: Path) -> dict[str, Any]:
    t_start = time.perf_counter()
    rustc_bin = find_rustc()
    sandbox = crate_dir / "bin_out" / "schemata_sandbox"
    sandbox.mkdir(parents=True, exist_ok=True)

    original_src = (crate_dir / "src" / "lib.rs").read_text(encoding="utf-8")
    schemata_src = build_schemata_source(original_src)
    schemata_rs = sandbox / "lib_schemata.rs"
    schemata_rs.write_text(schemata_src, encoding="utf-8")

    exe_name = "schemata_tests.exe" if os.name == "nt" else "schemata_tests"
    exe_path = sandbox / exe_name

    # Single fast debug compilation of the supermutant schemata binary
    comp = subprocess.run(
        [
            rustc_bin,
            "--edition",
            "2021",
            "-A",
            "unused_parens",
            "-C",
            "opt-level=0",
            "--test",
            str(schemata_rs),
            "-o",
            str(exe_path),
        ],
        cwd=sandbox,
        capture_output=True,
        text=True,
        timeout=240,
    )
    if comp.returncode != 0:
        raise RuntimeError(f"Mutant schemata compilation failed:\n{comp.stderr}")

    # 1. Verify baseline (HYPERION_MUTANT_ID=0) passes 100%
    env_base = os.environ.copy()
    env_base["HYPERION_MUTANT_ID"] = "0"
    base_run = subprocess.run(
        [str(exe_path), "--quiet"],
        cwd=sandbox,
        env=env_base,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if base_run.returncode != 0:
        raise RuntimeError(
            f"Unmutated schemata baseline failed!\n{base_run.stdout}\n{base_run.stderr}"
        )

    # 2. Execute each mutant (1..26) in isolated subprocess
    results: list[dict[str, Any]] = []
    killed_count = 0
    for idx, m in enumerate(MUTANTS, start=1):
        env_m = os.environ.copy()
        env_m["HYPERION_MUTANT_ID"] = str(idx)
        m_t0 = time.perf_counter()
        proc = subprocess.run(
            [str(exe_path), "--quiet"],
            cwd=sandbox,
            env=env_m,
            capture_output=True,
            text=True,
            timeout=60,
        )
        elapsed_ms = round((time.perf_counter() - m_t0) * 1000, 1)
        killed = proc.returncode != 0
        if killed:
            killed_count += 1
        results.append(
            {
                "id": m["id"],
                "subsystem": m["subsystem"],
                "status": "KILLED" if killed else "SURVIVED",
                "exit_code": proc.returncode,
                "elapsed_ms": elapsed_ms,
            }
        )

    total = len(MUTANTS)
    kill_score_pct = round((killed_count / max(total, 1)) * 100.0, 2)
    total_elapsed_s = round(time.perf_counter() - t_start, 2)

    summary: dict[str, Any] = {
        "suite": "HYPERION_OMEGA_CH20_DIRECTED_MUTATION_AUDIT",
        "technique": "Untch-Offutt-Harrold Mutant Schemata (1x rustc + 26 isolated runs)",
        "total_mutants": total,
        "killed_mutants": killed_count,
        "survived_mutants": total - killed_count,
        "mutation_kill_score_pct": kill_score_pct,
        "threshold_pct": 95.0,
        "gate_passed": kill_score_pct >= 95.0 and killed_count == total,
        "total_elapsed_sec": total_elapsed_s,
        "mutants": results,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(summary, indent=2), encoding="utf-8")

    md_lines = [
        "# HYPERION OMEGA — Targeted Financial & Cryptographic Mutation Testing Audit (`CH20`)",
        "",
        f"- **Technique**: `{summary['technique']}`",
        f"- **Total Mutants Injected**: `{total}`",
        f"- **Mutants Killed**: `{killed_count} / {total}`",
        f"- **Surviving Mutants**: `{total - killed_count}`",
        f"- **Mutation Kill Score**: **`{kill_score_pct:.2f}%`** (Mandatory Gate: `>= 95.00%`)",
        f"- **Execution Time**: `{total_elapsed_s:.2f}s`",
        "",
        "| Mutant ID | Target Subsystem / Invariant | Verdict | Time (ms) |",
        "|---|---|---|---:|",
    ]
    for r in results:
        md_lines.append(
            f"| `{r['id']}` | {r['subsystem']} | **{r['status']}** | `{r['elapsed_ms']}` |"
        )
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_md.write_text("\n".join(md_lines) + "\n", encoding="utf-8")
    return summary


def main() -> None:
    here = Path(__file__).resolve().parent
    crate_dir = here / "singularity-harness-rs"
    repo_root = here.parent
    out_json = here / "artifacts" / "mutation_audit_report.json"
    out_md = repo_root / "whitepaper" / "MUTATION_TESTING_AUDIT.md"
    if (repo_root / "docs" / "whitepaper").exists():
        out_md = repo_root / "docs" / "whitepaper" / "MUTATION_TESTING_AUDIT.md"

    summary = run_mutation_campaign(crate_dir, out_json, out_md)
    print(
        f"[MUTATION-AUDIT] Killed {summary['killed_mutants']}/{summary['total_mutants']} "
        f"({summary['mutation_kill_score_pct']}%) in {summary['total_elapsed_sec']}s -> "
        f"{'PASS' if summary['gate_passed'] else 'FAIL'}"
    )
    if not summary["gate_passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
