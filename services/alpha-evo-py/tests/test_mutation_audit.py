"""Extreme Overclock Gate 1, 2, & 3 Verification: >= 95% Mutation Kill Score, 500k Soak, & 3-Pass Optimization."""

from __future__ import annotations

from alpha_evo.benchmark_opt import run_500k_zero_leak_soak, run_three_pass_micro_benchmark
from alpha_evo.mutation_audit import run_mutation_survival_audit


def test_mutation_kill_score_exceeds_95_percent() -> None:
    report = run_mutation_survival_audit()
    assert report["total_mutants"] >= 50
    assert report["kill_score_pct"] >= 95.0, (
        f"Mutation score {report['kill_score_pct']}% < 95.0%! Survivors: {report['survived_details']}"
    )


def test_500k_soak_zero_steady_state_memory_leak_and_3pass_speedup() -> None:
    soak = run_500k_zero_leak_soak(total_cycles=500_000)
    assert soak["executed_cycles"] == 500_000
    assert soak["steady_state_leak_bytes"] == 0
    assert soak["zero_leak_verified"] is True

    bench = run_three_pass_micro_benchmark(n_ticks=2_048)
    assert bench["register_base_addr_mod_64"] == 0
    assert bench["max_numerical_parity_error"] < 1e-9
    assert bench["speedup_pass3_vs_pass1_x"] > 2.0
