#!/usr/bin/env python3
"""
Master runner & pytest suite for HYPERION OMEGA Z3 SMT Formal Proofs (Theorems 1, 2, & 3).
Generates machine-verifiable certificate `smt_proof_certificates.json`.
"""

from __future__ import annotations

import json
import time
from dataclasses import asdict
from pathlib import Path
import z3

from theorem1_double_entry_conservation import run_theorem1_suite
from theorem2_margin_liquidation_adl import run_theorem2_suite
from theorem3_l2_bridge_no_double_spend import run_theorem3_suite


def execute_all_proofs(write_certificate: bool = True) -> dict:
    t0 = time.perf_counter()
    t1_results = run_theorem1_suite()
    t2_results = run_theorem2_suite()
    t3_results = run_theorem3_suite()
    all_results = t1_results + t2_results + t3_results
    total_ms = (time.perf_counter() - t0) * 1000.0

    unsat_invariants = [r for r in all_results if r.expected_status == "UNSAT"]
    sat_mutation_guards = [r for r in all_results if r.expected_status == "SAT"]

    report = {
        "suite": "HYPERION_OMEGA_CH11_Z3_SMT_PROOFS",
        "z3_version": z3.get_version_string(),
        "total_checks": len(all_results),
        "unsat_invariants_proven": sum(1 for r in unsat_invariants if r.verified),
        "unsat_invariants_expected": len(unsat_invariants),
        "adversarial_mutants_caught_sat": sum(1 for r in sat_mutation_guards if r.verified),
        "adversarial_mutants_expected": len(sat_mutation_guards),
        "all_verified": all(r.verified for r in all_results),
        "total_elapsed_ms": round(total_ms, 3),
        "proofs": [asdict(r) for r in all_results],
    }

    if write_certificate:
        cert_path = Path(__file__).resolve().parent / "smt_proof_certificates.json"
        cert_path.write_text(json.dumps(report, indent=2), encoding="utf-8")

    return report


def test_theorem_1_double_entry_and_fee_rounding() -> None:
    results = run_theorem1_suite()
    assert len(results) == 6
    for r in results:
        assert r.verified, f"{r.theorem_id} failed: got {r.status}, expected {r.expected_status}"


def test_theorem_2_cross_margin_liquidation_and_adl() -> None:
    results = run_theorem2_suite()
    assert len(results) == 4
    for r in results:
        assert r.verified, f"{r.theorem_id} failed: got {r.status}, expected {r.expected_status}"


def test_theorem_3_l2_bridge_double_spend_immunity() -> None:
    results = run_theorem3_suite()
    assert len(results) == 5
    for r in results:
        assert r.verified, f"{r.theorem_id} failed: got {r.status}, expected {r.expected_status}"


if __name__ == "__main__":
    summary = execute_all_proofs(write_certificate=True)
    print("=" * 88)
    print(f"HYPERION OMEGA Z3 SMT FORMAL VERIFICATION SUITE (Z3 v{summary['z3_version']})")
    print("=" * 88)
    for p in summary["proofs"]:
        badge = "PASS" if p["verified"] else "FAIL"
        print(
            f"[{badge}] {p['theorem_id']:<44} | {p['status']:<5} (exp {p['expected_status']:<5}) | {p['elapsed_ms']:>8.2f} ms"
        )
        print(f"       -> {p['details']}")
    print("-" * 88)
    print(
        f"SUMMARY: {summary['unsat_invariants_proven']}/{summary['unsat_invariants_expected']} Core Invariants UNSAT | "
        f"{summary['adversarial_mutants_caught_sat']}/{summary['adversarial_mutants_expected']} Adversarial Mutants Caught SAT | "
        f"Total Time: {summary['total_elapsed_ms']:.2f} ms"
    )
    if not summary["all_verified"]:
        raise SystemExit(1)
