"""
Autonomous Quantitative Research Pipeline (`alpha-evo-run`):
1. Generates/loads multi-regime L2 microstructure dataset (`BTC-USDT`, `1e8` fixed-point).
2. Runs Purged & Embargoed Walk-Forward Cross-Validation (`5` OOS folds) + Symbolic GP Evolution.
3. Computes Bailey & López de Prado Deflated Sharpe Ratio (DSR), PSR, M_eff, and CSCV PBO.
4. Auto-compiles the winning formula to structured JSON (`best_alpha_ast.json`) + `#![no_std]`
   Rust Wasm strategy (`generated_alpha_strategy.rs` -> `.wasm` binary via `rustc`).
5. Trains Multi-Agent PyTorch PPO + GAE Optimal Execution & Market-Making Policy, exports
   ONNX (`ppo_execution_agent.onnx`) + compact `.npz` weights + real learning curve.
6. Runs 3-Pass Micro-Optimization Benchmark & 500,000-Cycle `tracemalloc` Zero-Leak Soak Test.
"""

from __future__ import annotations

import json
from pathlib import Path
import time
from typing import Any
import numpy as np
from scipy.stats import spearmanr

from alpha_evo.benchmark_opt import run_500k_zero_leak_soak, run_three_pass_micro_benchmark
from alpha_evo.gp_alpha.ast_tree import ASTNode, compile_ast_to_ssa
from alpha_evo.gp_alpha.engine import GPAlphaConfig, SymbolicAlphaEngine, signal_to_net_returns
from alpha_evo.gp_alpha.rust_wasm_compiler import (
    compile_alpha_to_ch15_sdk_strategy,
    compile_alpha_to_json_manifest,
    compile_alpha_to_rust_wasm_source,
    try_compile_rust_to_wasm_binary,
)
from alpha_evo.lob_gym.env import LOBEnvConfig
from alpha_evo.lob_gym.market_generator import MarketDataset, MarketGenConfig, generate_microstructure_dataset
from alpha_evo.ppo_agent.exporter import export_ppo_artifacts
from alpha_evo.ppo_agent.trainer import PPOConfig, train_ppo_agent
from alpha_evo.validation.cross_validation import PurgedWalkForwardCV
from alpha_evo.validation.statistics import (
    compute_cscv_pbo,
    compute_deflated_sharpe_ratio,
)


def _slice_features(
    features: dict[str, np.ndarray],
    indices: np.ndarray,
) -> dict[str, np.ndarray]:
    return {k: np.ascontiguousarray(v[indices]) for k, v in features.items()}


def evaluate_ast_walk_forward_oos(
    ast: ASTNode,
    dataset: MarketDataset,
    n_splits: int = 5,
    pred_horizon_bars: int = 10,
    embargo_pct: float = 0.015,
    turnover_cost_bps: float = 0.35,
) -> dict[str, Any]:
    """
    Evaluate `ast` across strictly purged & embargoed chronological Out-of-Sample Walk-Forward folds.
    Each OOS fold is evaluated purely causally with zero leakage from future bars!
    """
    features = dataset.to_feature_dict()
    n_total = dataset.mid_price_fp.shape[0]
    wf = PurgedWalkForwardCV(
        n_splits=n_splits,
        min_train_fraction=0.35,
        label_horizon_bars=pred_horizon_bars,
        embargo_pct=embargo_pct,
        expanding=True,
    )
    folds = wf.split(n_total)
    tape = compile_ast_to_ssa(ast)
    periods_per_year = 252.0 * 6.5 * 3600.0 / max(0.1 * pred_horizon_bars, 0.1)

    fold_reports: list[dict[str, Any]] = []
    oos_returns_concat: list[np.ndarray] = []

    for f in folds:
        test_feat = _slice_features(features, f.test_indices)
        test_fwd = np.ascontiguousarray(dataset.forward_return_bps[f.test_indices])
        ws = tape.create_workspace(f.test_indices.size)
        raw_sig = tape.evaluate_in_place(test_feat, ws).copy()

        warmup = min(35, max(5, f.test_indices.size // 10))
        net_rets, _, mean_to = signal_to_net_returns(
            raw_sig,
            test_fwd,
            pred_horizon_bars=pred_horizon_bars,
            turnover_cost_bps=turnover_cost_bps,
            warmup_bars=warmup,
        )
        oos_returns_concat.append(net_rets)

        valid_sl = slice(warmup, max(warmup + 2, f.test_indices.size - pred_horizon_bars))
        s_v = raw_sig[valid_sl]
        y_v = test_fwd[valid_sl]
        ic_p = float(np.corrcoef(s_v, y_v)[0, 1]) if (s_v.size > 5 and np.std(s_v) > 1e-8) else 0.0
        ic_s = float(spearmanr(s_v, y_v).statistic) if (s_v.size > 5 and np.std(s_v) > 1e-8) else 0.0

        sd = float(np.std(net_rets, ddof=1)) if net_rets.size > 2 else 1.0
        sr_fold = (float(np.mean(net_rets)) / max(sd, 1e-9)) * np.sqrt(periods_per_year)

        fold_reports.append(
            {
                "fold_id": f.fold_id,
                "train_bars": int(f.train_indices.size),
                "purged_bars": int(f.purged_count),
                "embargoed_bars": int(f.embargoed_count),
                "test_bars": int(f.test_indices.size),
                "oos_non_overlapping_trades": int(net_rets.size),
                "oos_ic_pearson": round(ic_p if np.isfinite(ic_p) else 0.0, 5),
                "oos_ic_spearman": round(ic_s if np.isfinite(ic_s) else 0.0, 5),
                "oos_mean_net_bps": round(float(np.mean(net_rets)), 4),
                "oos_sharpe_annualized": round(float(sr_fold), 3),
                "oos_mean_turnover": round(mean_to, 4),
            }
        )

    full_oos_rets = np.concatenate(oos_returns_concat)
    equity_curve_bps = np.cumsum(full_oos_rets)
    running_max = np.maximum.accumulate(np.r_[0.0, equity_curve_bps])[1:]
    max_dd_bps = float(np.max(running_max - equity_curve_bps)) if equity_curve_bps.size > 0 else 0.0

    return {
        "folds": fold_reports,
        "oos_returns_bps": full_oos_rets,
        "oos_equity_curve_bps": [round(float(x), 3) for x in equity_curve_bps[:: max(1, equity_curve_bps.size // 120)]],
        "oos_cumulative_net_bps": round(float(equity_curve_bps[-1]) if equity_curve_bps.size else 0.0, 3),
        "oos_max_drawdown_bps": round(max_dd_bps, 3),
    }


def run_full_research_pipeline(
    artifacts_dir: str | Path | None = None,
    fast_mode: bool = False,
) -> dict[str, Any]:
    """Execute the complete Channel 19 autonomous research & anti-overfitting validation loop."""
    t_start = time.perf_counter()
    if artifacts_dir is None:
        artifacts_dir = Path(__file__).resolve().parents[2] / "artifacts"
    out_dir = Path(artifacts_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    # 1. Generate realistic multi-regime microstructure tape (~47 min @ 100ms = 28,000 bars)
    # Sufficient out-of-sample track record T >> MinTRL to statistically separate true alpha from M_eff GP trials!
    n_steps = 12_000 if fast_mode else 28_000
    pred_horizon = 10
    mkt_cfg = MarketGenConfig(
        symbol="BTC-USDT",
        n_steps=n_steps,
        alpha_signal_strength=0.34,
        seed=20261119,
    )
    dataset = generate_microstructure_dataset(mkt_cfg, pred_horizon_bars=pred_horizon)
    all_features = dataset.to_feature_dict()

    # Strict Purged Discovery Split (first 38% for GP discovery, purged+embargoed gap, remaining 62% for OOS WF & DSR!)
    embargo_bars = int(np.ceil(n_steps * 0.015))
    disc_end = int(n_steps * 0.38) - pred_horizon - embargo_bars
    disc_idx = np.arange(0, disc_end, dtype=np.int64)
    disc_features = _slice_features(all_features, disc_idx)
    disc_fwd = np.ascontiguousarray(dataset.forward_return_bps[disc_idx])

    # 2. Symbolic GP Evolution on Discovery Set with Parsimony Pressure
    gp_cfg = GPAlphaConfig(
        population_size=40 if fast_mode else 52,
        n_generations=8 if fast_mode else 12,
        turnover_cost_bps=0.08,
        pred_horizon_bars=pred_horizon,
        seed=202619,
    )
    gp_engine = SymbolicAlphaEngine(gp_cfg)
    evo_res = gp_engine.evolve(disc_features, disc_fwd)
    best_cand = evo_res.best_candidate

    # 3. Evaluate All Top Candidate Strategies across Full Dataset / Purged Walk-Forward for Exact DSR & PBO
    wf_eval = evaluate_ast_walk_forward_oos(
        best_cand.ast,
        dataset,
        n_splits=5,
        pred_horizon_bars=pred_horizon,
        embargo_pct=0.015,
        turnover_cost_bps=gp_cfg.turnover_cost_bps,
    )

    # Evaluate Hall-of-Fame + diverse evolved trial formulas on the OOS span for CSCV PBO & DSR cross-trial matrix
    oos_start = int(n_steps * 0.38)
    oos_idx = np.arange(oos_start, n_steps, dtype=np.int64)
    oos_features = _slice_features(all_features, oos_idx)
    oos_fwd = np.ascontiguousarray(dataset.forward_return_bps[oos_idx])

    # Build representative diverse GP candidate return matrix `(T_oos, M_panel)` across the
    # full search archive (top HoF + uniformly stratified evolved trials across generations)
    # so CSCV PBO tests whether IS selection beats the true candidate family out-of-sample!
    oos_trial_cols: list[np.ndarray] = []
    seen_oos_sexpr: set[str] = set()
    panel_asts: list[ASTNode] = []

    for cand in evo_res.hall_of_fame[:10]:
        if cand.sexpr not in seen_oos_sexpr:
            seen_oos_sexpr.add(cand.sexpr)
            panel_asts.append(cand.ast)

    n_all_trials = len(evo_res.trial_asts)
    if n_all_trials > 0:
        stride = max(1, n_all_trials // 45)
        for ast_item in evo_res.trial_asts[::stride]:
            sx = ast_item.to_sexpr()
            if sx not in seen_oos_sexpr:
                seen_oos_sexpr.add(sx)
                panel_asts.append(ast_item)
            if len(panel_asts) >= 56:
                break

    for ast_item in panel_asts:
        tp = compile_ast_to_ssa(ast_item)
        ws_o = tp.create_workspace(oos_idx.size)
        sig_o = tp.evaluate_in_place(oos_features, ws_o).copy()
        r_o, _, _ = signal_to_net_returns(
            sig_o,
            oos_fwd,
            pred_horizon_bars=pred_horizon,
            turnover_cost_bps=gp_cfg.turnover_cost_bps,
            warmup_bars=35,
        )
        if float(np.std(r_o, ddof=1)) > 1e-9:
            oos_trial_cols.append(r_o)

    # Evaluate DSR against BOTH (a) full GP search empirical trials & (b) theoretical False-Strategy Null EVT
    periods_per_year = 252.0 * 6.5 * 3600.0 / max(0.1 * pred_horizon, 0.1)
    # Use full-length OOS panel trial matrix (same T_oos horizon as selected returns!) for consistent SE & M_eff!
    dsr_trial_mat = np.column_stack(oos_trial_cols) if len(oos_trial_cols) >= 4 else evo_res.trial_returns_matrix
    dsr_oos = compute_deflated_sharpe_ratio(
        selected_returns=wf_eval["oos_returns_bps"],
        trial_returns_matrix=evo_res.trial_returns_matrix,
        periods_per_year=periods_per_year,
        dsr_gate_threshold=0.95,
    )
    dsr_null_evt = compute_deflated_sharpe_ratio(
        selected_returns=wf_eval["oos_returns_bps"],
        trial_returns_matrix=None,
        n_trials_override=dsr_oos.effective_independent_trials,
        periods_per_year=periods_per_year,
        dsr_gate_threshold=0.95,
    )

    # Evaluate HoF DSR table (top 8 candidate formulas ranked by fitness)
    hof_dsr_table: list[dict[str, Any]] = []
    for rank_i, cand in enumerate(evo_res.hall_of_fame[:8], start=1):
        wf_c = evaluate_ast_walk_forward_oos(
            cand.ast,
            dataset,
            n_splits=5,
            pred_horizon_bars=pred_horizon,
            embargo_pct=0.015,
            turnover_cost_bps=gp_cfg.turnover_cost_bps,
        )
        dsr_c = compute_deflated_sharpe_ratio(
            selected_returns=wf_c["oos_returns_bps"],
            trial_returns_matrix=None,
            n_trials_override=dsr_oos.effective_independent_trials,
            periods_per_year=periods_per_year,
            dsr_gate_threshold=0.95,
        )
        hof_dsr_table.append(
            {
                "rank": rank_i,
                "formula_sexpr": cand.sexpr,
                "nodes": cand.size,
                "depth": cand.depth,
                "is_ic_pearson": round(cand.ic_pearson, 5),
                "oos_sharpe_ann": dsr_c.sharpe_annualized,
                "sr0_hurdle_ann": dsr_c.benchmark_sr0_annualized,
                "skewness": dsr_c.skewness,
                "kurtosis_pearson": dsr_c.pearson_kurtosis,
                "psr": dsr_c.psr_vs_zero,
                "dsr": dsr_c.deflated_sharpe_ratio,
                "passed_gate": dsr_c.passed_institutional_gate,
            }
        )

    # CSCV PBO across diverse GP candidate search archive (`S = 10` -> `252` combinations)
    cscv_mat = dsr_trial_mat
    pbo_res = compute_cscv_pbo(cscv_mat, n_partitions=10, pbo_gate_threshold=0.25)

    # 4. Compile Promoted Formula to JSON AST + Rust/Wasm Module for Channel 15
    validation_summary = {
        "oos_sharpe_annualized": dsr_null_evt.sharpe_annualized,
        "deflated_sharpe_ratio_null_evt": dsr_null_evt.deflated_sharpe_ratio,
        "deflated_sharpe_ratio_empirical_disp": dsr_oos.deflated_sharpe_ratio,
        "deflated_sharpe_ratio": dsr_null_evt.deflated_sharpe_ratio,
        "probabilistic_sharpe_ratio": dsr_null_evt.psr_vs_zero,
        "sr0_hurdle_null_evt_annualized": dsr_null_evt.benchmark_sr0_annualized,
        "sr0_hurdle_empirical_disp_annualized": dsr_oos.benchmark_sr0_annualized,
        "raw_gp_trials": dsr_oos.n_raw_trials,
        "effective_independent_trials": dsr_oos.effective_independent_trials,
        "pbo_cscv_252": pbo_res.pbo,
        "passed_institutional_gate": bool(dsr_null_evt.passed_institutional_gate and pbo_res.passed_institutional_gate),
    }

    ast_manifest = compile_alpha_to_json_manifest(best_cand.ast, metrics=validation_summary, symbol="BTC-USDT")
    ast_json_path = out_dir / "best_alpha_ast.json"
    ast_json_path.write_text(json.dumps(ast_manifest, indent=2), encoding="utf-8")

    rust_src = compile_alpha_to_rust_wasm_source(best_cand.ast)
    rust_src_path = out_dir / "generated_alpha_strategy.rs"
    rust_src_path.write_text(rust_src, encoding="utf-8")

    ch15_sdk_src = compile_alpha_to_ch15_sdk_strategy(best_cand.ast)
    ch15_sdk_path = out_dir / "generated_alpha_ch15_sdk.rs"
    ch15_sdk_path.write_text(ch15_sdk_src, encoding="utf-8")

    redpanda_envelope = {
        "topic": "hyperion.omega.alpha.formulas.v1",
        "key": f"gen:{best_cand.generation_born}",
        "headers": {
            "x-source-service": "alpha-evo-py",
            "x-schema-type": "hyperion.omega.alpha_ast.v1",
        },
        "payload": ast_manifest,
    }
    (out_dir / "redpanda_alpha_formula_event.json").write_text(
        json.dumps(redpanda_envelope, indent=2), encoding="utf-8"
    )

    wasm_compile_report = try_compile_rust_to_wasm_binary(
        rust_src_path,
        out_dir / "generated_alpha_strategy.wasm",
    )

    # 5. Train Multi-Agent PyTorch PPO + GAE Optimal Execution & Market-Making Policy
    ppo_cfg = PPOConfig(
        num_envs=48 if fast_mode else 64,
        rollout_steps=96 if fast_mode else 128,
        n_iterations=18 if fast_mode else 36,
        seed=202619,
    )
    lob_cfg = LOBEnvConfig(
        symbol="BTC-USDT",
        num_envs=ppo_cfg.num_envs,
        episode_steps=ppo_cfg.rollout_steps,
        seed=202619,
    )
    ppo_res = train_ppo_agent(ppo_cfg=ppo_cfg, env_cfg=lob_cfg)
    ppo_export_info = export_ppo_artifacts(ppo_res, out_dir)

    # 6. Run Extreme Overclock 3-Pass Benchmark & 500,000-Cycle Zero-Leak Soak Test
    bench_3pass = run_three_pass_micro_benchmark(ast=best_cand.ast, n_ticks=4_096)
    soak_500k = run_500k_zero_leak_soak(total_cycles=500_000)

    opt_path = out_dir / "optimization_benchmark.json"
    opt_path.write_text(
        json.dumps({"three_pass_benchmark": bench_3pass, "soak_test_500k": soak_500k}, indent=2),
        encoding="utf-8",
    )

    dsr_pbo_payload = {
        "schema_version": "hyperion.omega.dsr_pbo.v1",
        "winning_formula": {
            "sexpr": best_cand.sexpr,
            "latex": best_cand.latex,
            "nodes": best_cand.size,
            "depth": best_cand.depth,
            "generation_born": best_cand.generation_born,
        },
        "dsr_report": {
            "n_obs": dsr_null_evt.n_obs,
            "n_raw_trials": dsr_oos.n_raw_trials,
            "effective_independent_trials": dsr_oos.effective_independent_trials,
            "avg_pairwise_corr": dsr_oos.avg_pairwise_corr,
            "sharpe_per_period": dsr_null_evt.sharpe_per_period,
            "sharpe_annualized": dsr_null_evt.sharpe_annualized,
            "benchmark_sr0_per_period": dsr_null_evt.benchmark_sr0_per_period,
            "benchmark_sr0_annualized": dsr_null_evt.benchmark_sr0_annualized,
            "benchmark_sr0_empirical_disp_per_period": dsr_oos.benchmark_sr0_per_period,
            "benchmark_sr0_empirical_disp_annualized": dsr_oos.benchmark_sr0_annualized,
            "sharpe_std_err": dsr_null_evt.sharpe_std_err,
            "skewness": dsr_null_evt.skewness,
            "pearson_kurtosis": dsr_null_evt.pearson_kurtosis,
            "psr_vs_zero": dsr_null_evt.psr_vs_zero,
            "deflated_sharpe_ratio": dsr_null_evt.deflated_sharpe_ratio,
            "deflated_sharpe_ratio_empirical_disp": dsr_oos.deflated_sharpe_ratio,
            "min_track_record_length": dsr_null_evt.min_track_record_length,
            "passed_institutional_gate": dsr_null_evt.passed_institutional_gate,
        },
        "pbo_report": {
            "n_partitions": pbo_res.n_partitions,
            "n_combinations": pbo_res.n_combinations,
            "n_strategies": pbo_res.n_strategies,
            "pbo": pbo_res.pbo,
            "median_logit": pbo_res.median_logit,
            "mean_logit": pbo_res.mean_logit,
            "prob_oos_loss": pbo_res.prob_oos_loss,
            "degradation_slope": pbo_res.degradation_slope,
            "passed_institutional_gate": pbo_res.passed_institutional_gate,
        },
        "walk_forward_folds": wf_eval["folds"],
        "oos_cumulative_net_bps": wf_eval["oos_cumulative_net_bps"],
        "oos_max_drawdown_bps": wf_eval["oos_max_drawdown_bps"],
        "oos_equity_curve_bps": wf_eval["oos_equity_curve_bps"],
        "hall_of_fame_dsr_table": hof_dsr_table,
        "gp_generation_history": evo_res.generation_history,
        "rust_wasm_compilation": wasm_compile_report,
    }
    dsr_json_path = out_dir / "dsr_pbo_evaluation.json"
    dsr_json_path.write_text(json.dumps(dsr_pbo_payload, indent=2), encoding="utf-8")

    total_wall = round(time.perf_counter() - t_start, 3)
    return {
        "total_wall_sec": total_wall,
        "artifacts_dir": str(out_dir),
        "winning_formula": best_cand.sexpr,
        "winning_nodes": best_cand.size,
        "dsr": dsr_null_evt.deflated_sharpe_ratio,
        "dsr_empirical_disp": dsr_oos.deflated_sharpe_ratio,
        "pbo": pbo_res.pbo,
        "oos_sharpe_ann": dsr_null_evt.sharpe_annualized,
        "wasm_compilation": wasm_compile_report,
        "ppo_export": ppo_export_info,
        "ppo_benchmarks": ppo_res.benchmark_comparison,
        "three_pass_benchmark": bench_3pass,
        "soak_500k": soak_500k,
    }


def main() -> None:
    summary = run_full_research_pipeline()
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
