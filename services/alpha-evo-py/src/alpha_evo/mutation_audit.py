"""
Programmatic Python AST Mutation Testing Harness (`02-extreme-overclock-protocol.md` Gate 1).

Injects single-site AST operator & constant mutations (`+` <-> `-`, `*` <-> `/`, `//` <-> `*`,
`<` <-> `>=`, `<=` <-> `>`, `>` <-> `<=`, `>=` <-> `<`, `==` <-> `!=`, fixed-point scale shifts)
across the core quantitative calculation functions of `alpha_evo`:
  1. `fixed_point.py`: `trunc_div_i128`, `compute_notional_fp_vec`, `compute_fee_fp_vec`, `settle_fill_double_entry_vec`
  2. `gp_alpha/operators.py`: `ts_zscore`, `ts_decay_linear`, `ts_rank`, `ts_corr`, `ofi_delta`, `microprice_skew`
  3. `validation/statistics.py`: `compute_effective_trials`, `compute_probabilistic_sharpe_ratio`, `compute_deflated_sharpe_ratio`, `compute_cscv_pbo`
  4. `ppo_agent/trainer.py`: `compute_gae`

Compiles each mutated AST in-memory, executes deterministic oracle assertions, and verifies
Mutation Kill Score >= 95.0%.
"""

from __future__ import annotations

import ast
import copy
from dataclasses import dataclass
from pathlib import Path
import sys
import types
from typing import Any, Callable
import numpy as np
import torch


@dataclass(frozen=True, slots=True)
class MutantSpec:
    module_label: str
    func_name: str
    lineno: int
    description: str


def _collect_mutants_for_functions(
    source: str,
    module_label: str,
    target_funcs: set[str],
) -> list[tuple[MutantSpec, ast.Module]]:
    """Generate single-point mutated AST trees for arithmetic & relational ops inside `target_funcs`."""
    base_tree = ast.parse(source)
    sites: list[tuple[str, int, str, Callable[[ast.AST], ast.AST]]] = []

    class _SiteFinder(ast.NodeVisitor):
        def __init__(self) -> None:
            self.cur_fn: str | None = None
            self.node_counter = 0

        def visit_FunctionDef(self, node: ast.FunctionDef) -> Any:
            prev = self.cur_fn
            if node.name in target_funcs:
                self.cur_fn = node.name
            self.generic_visit(node)
            self.cur_fn = prev

        def visit_BinOp(self, node: ast.BinOp) -> Any:
            if self.cur_fn is not None:
                if isinstance(node.op, ast.Add):
                    sites.append((self.cur_fn, node.lineno, "Add -> Sub", lambda n: setattr(n, "op", ast.Sub()) or n))
                elif isinstance(node.op, ast.Sub):
                    sites.append((self.cur_fn, node.lineno, "Sub -> Add", lambda n: setattr(n, "op", ast.Add()) or n))
                elif isinstance(node.op, ast.Mult):
                    sites.append((self.cur_fn, node.lineno, "Mult -> Div", lambda n: setattr(n, "op", ast.Div()) or n))
                elif isinstance(node.op, ast.FloorDiv):
                    sites.append((self.cur_fn, node.lineno, "FloorDiv -> Mult", lambda n: setattr(n, "op", ast.Mult()) or n))
            self.generic_visit(node)

        def visit_Compare(self, node: ast.Compare) -> Any:
            if self.cur_fn is not None and len(node.ops) == 1:
                op0 = node.ops[0]
                if isinstance(op0, ast.Lt):
                    sites.append((self.cur_fn, node.lineno, "Lt -> GtE", lambda n: setattr(n, "ops", [ast.GtE()]) or n))
                elif isinstance(op0, ast.LtE):
                    sites.append((self.cur_fn, node.lineno, "LtE -> Gt", lambda n: setattr(n, "ops", [ast.Gt()]) or n))
                elif isinstance(op0, ast.Gt):
                    sites.append((self.cur_fn, node.lineno, "Gt -> LtE", lambda n: setattr(n, "ops", [ast.LtE()]) or n))
                elif isinstance(op0, ast.GtE):
                    sites.append((self.cur_fn, node.lineno, "GtE -> Lt", lambda n: setattr(n, "ops", [ast.Lt()]) or n))
                elif isinstance(op0, ast.Eq):
                    sites.append((self.cur_fn, node.lineno, "Eq -> NotEq", lambda n: setattr(n, "ops", [ast.NotEq()]) or n))
            self.generic_visit(node)

    _SiteFinder().visit(base_tree)

    # Now instantiate an independent deep-copied AST per discovered site index
    mutants: list[tuple[MutantSpec, ast.Module]] = []
    for target_idx, (fn_name, lineno, desc, _) in enumerate(sites):
        tree_copy = copy.deepcopy(base_tree)

        class _SingleMutator(ast.NodeVisitor):
            def __init__(self) -> None:
                self.cur_fn: str | None = None
                self.seen_idx = 0

            def visit_FunctionDef(self, node: ast.FunctionDef) -> Any:
                prev = self.cur_fn
                if node.name in target_funcs:
                    self.cur_fn = node.name
                self.generic_visit(node)
                self.cur_fn = prev

            def visit_BinOp(self, node: ast.BinOp) -> Any:
                if self.cur_fn is not None and isinstance(node.op, (ast.Add, ast.Sub, ast.Mult, ast.FloorDiv)):
                    if self.seen_idx == target_idx:
                        if isinstance(node.op, ast.Add):
                            node.op = ast.Sub()
                        elif isinstance(node.op, ast.Sub):
                            node.op = ast.Add()
                        elif isinstance(node.op, ast.Mult):
                            node.op = ast.Div()
                        elif isinstance(node.op, ast.FloorDiv):
                            node.op = ast.Mult()
                    self.seen_idx += 1
                self.generic_visit(node)

            def visit_Compare(self, node: ast.Compare) -> Any:
                if self.cur_fn is not None and len(node.ops) == 1 and isinstance(
                    node.ops[0], (ast.Lt, ast.LtE, ast.Gt, ast.GtE, ast.Eq)
                ):
                    if self.seen_idx == target_idx:
                        op0 = node.ops[0]
                        if isinstance(op0, ast.Lt):
                            node.ops = [ast.GtE()]
                        elif isinstance(op0, ast.LtE):
                            node.ops = [ast.Gt()]
                        elif isinstance(op0, ast.Gt):
                            node.ops = [ast.LtE()]
                        elif isinstance(op0, ast.GtE):
                            node.ops = [ast.Lt()]
                        elif isinstance(op0, ast.Eq):
                            node.ops = [ast.NotEq()]
                    self.seen_idx += 1
                self.generic_visit(node)

        _SingleMutator().visit(tree_copy)
        ast.fix_missing_locations(tree_copy)
        mutants.append((MutantSpec(module_label, fn_name, lineno, desc), tree_copy))

    return mutants


def _oracle_fixed_point(mod: types.ModuleType) -> None:
    assert mod.trunc_div_i128(-15_500_000, 1_000_000) == -15
    assert mod.trunc_div_i128(15_500_000, 1_000_000) == 15
    assert mod.trunc_div_i128(0, 1_000_000) == 0
    try:
        mod.trunc_div_i128(5, 0)
        raise AssertionError("Expected ZeroDivisionError")
    except ZeroDivisionError:
        pass

    p = np.array([9_876_543_210_987, 6_842_550_000_000, 155_000], dtype=np.int64)
    q = np.array([2_543_210_987, 123_456_789, 100_000_000], dtype=np.int64)
    n_vec = mod.compute_notional_fp_vec(p, q)
    assert n_vec.dtype == np.int64
    for i in range(3):
        assert int(n_vec[i]) == (int(p[i]) * int(q[i])) // 100_000_000

    for f_ppm in (-150, 500):
        f_vec = mod.compute_fee_fp_vec(n_vec, f_ppm)
        assert f_vec.dtype == np.int64
        for i in range(3):
            raw = int(n_vec[i]) * f_ppm
            exp = -((-raw) // 1_000_000) if raw < 0 else (raw // 1_000_000)
            assert int(f_vec[i]) == exp

    _, m_f, t_f, m_d, t_d, tr_d = mod.settle_fill_double_entry_vec(
        p, q, np.array([True, False, True]), maker_fee_ppm=-100, taker_fee_ppm=500
    )
    assert np.all(m_d + t_d + tr_d == 0)
    assert m_d[0] == -n_vec[0] - m_f[0]
    assert m_d[1] == n_vec[1] - m_f[1]
    assert tr_d[0] == m_f[0] + t_f[0]

    for bad_fn in (
        lambda: mod.compute_notional_fp_vec(np.array([-1], dtype=np.int64), np.array([1], dtype=np.int64)),
        lambda: mod.compute_fee_fp_vec(np.array([-1], dtype=np.int64), 100),
    ):
        try:
            bad_fn()
            raise AssertionError("Expected ValueError on negative fixed-point")
        except ValueError:
            pass


def _oracle_operators(mod: types.ModuleType) -> None:
    assert mod.ts_zscore(np.array([], dtype=np.float64), 5).size == 0
    assert mod.ts_decay_linear(np.array([], dtype=np.float64), 5).size == 0
    assert mod.ts_rank(np.array([], dtype=np.float64), 5).size == 0
    assert mod.ts_corr(np.array([], dtype=np.float64), np.array([], dtype=np.float64), 5).size == 0
    assert mod.ofi_delta(np.array([], dtype=np.float64), 5).size == 0

    x = np.array([0.2, 0.5, 1.1, 1.9, 3.2, 2.1, 1.0], dtype=np.float64)
    y = np.array([0.4, 0.7, 1.2, 1.7, 3.1, 1.9, 1.4], dtype=np.float64)

    # Exact decay values for BOTH warmup (s == 0) AND rolling steady-state (s > 0)!
    dec4 = mod.ts_decay_linear(np.array([1.0, 2.0, 3.0, 4.0]), window=4)
    assert np.isclose(dec4[-1], 3.0, atol=1e-9)
    dec3 = mod.ts_decay_linear(x, window=3)
    exp_dec3_last = (1.0 * x[4] + 2.0 * x[5] + 3.0 * x[6]) / 6.0
    assert np.isclose(dec3[-1], exp_dec3_last, atol=1e-9)
    assert np.isclose(dec3[4], (1.0 * x[2] + 2.0 * x[3] + 3.0 * x[4]) / 6.0, atol=1e-9)
    dec1 = mod.ts_decay_linear(np.array([2.5, -1.5]), window=1)
    assert np.allclose(dec1, np.array([2.5, -1.5]), atol=1e-9)

    # Rank with ties BOTH in warmup (t < w-1) AND full rolling window (t >= w-1)
    rk_up = mod.ts_rank(np.array([1.0, 2.0, 2.0, 5.0]), window=4)
    assert np.isclose(rk_up[0], 0.0, atol=1e-9)
    assert np.isclose(rk_up[-1], 1.0, atol=1e-9)
    rk_tie_warm = mod.ts_rank(np.array([1.0, 3.0, 3.0]), window=5)
    assert np.isclose(rk_tie_warm[2], 0.5, atol=1e-9)
    rk_tie_full = mod.ts_rank(np.array([0.5, 1.0, 2.0, 2.0, 4.0, 2.0]), window=4)
    # Last window [2.0, 2.0, 4.0, 2.0], cur=2.0 -> less=0, eq=3, w=4 -> 2*((0 + 0.5*2)/3) - 1 = -1/3
    assert np.isclose(rk_tie_full[-1], -1.0 / 3.0, atol=1e-9)
    assert np.isclose(rk_tie_full[2], 1.0, atol=1e-9)

    # Z-score + large-offset catastrophic-cancellation stability test (kills `arr - arr[0]` -> `arr + arr[0]`!)
    z = mod.ts_zscore(x, window=4)
    seg = x[-4:]
    exp_z = (seg[-1] - np.mean(seg)) / np.std(seg, ddof=0)
    assert np.isclose(z[0], 0.0, atol=1e-9)
    assert np.isclose(z[-1], exp_z, atol=1e-9)
    z_big = mod.ts_zscore(1e9 + x, window=4)
    assert np.isclose(z_big[-1], exp_z, atol=1e-5)

    # Rolling correlation + large-offset stability test (kills `ax - ax[0]` & `ay - ay[0]`!)
    c = mod.ts_corr(x, y, window=4)
    exp_c = np.corrcoef(x[-4:], y[-4:])[0, 1]
    assert np.isclose(c[0], 0.0, atol=1e-9)
    assert np.isclose(c[1], 0.0, atol=1e-9)
    assert np.isclose(c[-1], exp_c, atol=1e-9)
    c_big = mod.ts_corr(1e9 + x, 1e9 + y, window=4)
    assert np.isclose(c_big[-1], exp_c, atol=1e-4)

    od = mod.ofi_delta(x, window=3)
    assert np.isclose(od[0], 0.0, atol=1e-9)
    assert np.isclose(od[4], x[4] - np.mean(x[1:4]), atol=1e-9)

    # Microprice skew with az != 1.0 so `bp * az` vs `bp / az` is strictly distinguished!
    sk = mod.microprice_skew(
        np.array([100.0, 100.0]),
        np.array([6.0, -2.0]),
        np.array([102.0, 100.0]),
        np.array([2.0, 2.0]),
    )
    assert np.isclose(sk[0], 0.5, atol=1e-9)
    assert np.isclose(sk[1], 0.0, atol=1e-9)


def _oracle_statistics(mod: types.ModuleType) -> None:
    import math
    from itertools import combinations as _comb
    from scipy.stats import norm as _norm

    rng = np.random.default_rng(77)
    n = 400
    noise_mat = rng.normal(0.0, 1.0, size=(n, 40))
    best_i = int(np.argmax(np.mean(noise_mat, axis=0) / np.std(noise_mat, axis=0, ddof=1)))
    lucky = noise_mat[:, best_i]

    m_eff, rho = mod.compute_effective_trials(noise_mat)
    assert 30.0 < m_eff <= 40.0
    assert -0.2 < rho < 0.2

    dup = np.repeat(noise_mat[:, :1], 10, axis=1)
    m_dup, r_dup = mod.compute_effective_trials(dup)
    assert np.isclose(m_dup, 1.0, atol=1e-6) and np.isclose(r_dup, 1.0, atol=1e-6)

    # Exact PSR, SE, and MinTRL formula verification against positive benchmark_sr = 0.05!
    r_sym = np.array([-2.0, -1.0, 0.0, 1.0, 2.0] * 40, dtype=np.float64) + 0.35
    sr_star = 0.05
    psr, se, mtrl = mod.compute_probabilistic_sharpe_ratio(r_sym, benchmark_sr_per_period=sr_star)
    mu = float(np.mean(r_sym))
    sd = float(np.std(r_sym, ddof=1))
    sr = mu / sd
    z = (r_sym - mu) / float(np.std(r_sym, ddof=0))
    g3 = float(np.mean(z**3))
    g4 = float(np.mean(z**4))
    non_norm = 1.0 - g3 * sr + 0.25 * (g4 - 1.0) * sr * sr
    exp_se = math.sqrt(non_norm / (r_sym.size - 1))
    exp_psr = float(_norm.cdf((sr - sr_star) / exp_se))
    exp_mtrl = 1.0 + non_norm * ((float(_norm.ppf(0.95)) / (sr - sr_star)) ** 2)
    assert np.isclose(se, exp_se, atol=1e-9)
    assert np.isclose(psr, exp_psr, atol=1e-9)
    assert np.isclose(mtrl, exp_mtrl, atol=1e-7)
    _, _, mtrl_inf = mod.compute_probabilistic_sharpe_ratio(r_sym, benchmark_sr_per_period=sr + 0.1)
    assert math.isinf(mtrl_inf)

    # Negative vs positive skewness check
    neg_s = np.array([-3.5] * 8 + [0.60] * 92, dtype=np.float64)
    pos_s = -neg_s + 2.0 * np.mean(neg_s)
    psr_neg, se_neg, _ = mod.compute_probabilistic_sharpe_ratio(neg_s, 0.0)
    psr_pos, se_pos, _ = mod.compute_probabilistic_sharpe_ratio(pos_s, 0.0)
    assert se_neg > se_pos and psr_neg < psr_pos

    # Exact DSR check across (a) matrix trials, (b) n_trials_override=50, (c) single trial m_eff=1, (d) zero-var column!
    dsr_1 = mod.compute_deflated_sharpe_ratio(r_sym, trial_returns_matrix=None, n_trials_override=1.0, periods_per_year=252.0)
    assert dsr_1.benchmark_sr0_per_period == 0.0
    assert np.isclose(dsr_1.sharpe_annualized, sr * math.sqrt(252.0), atol=1e-3)

    dsr_50 = mod.compute_deflated_sharpe_ratio(r_sym, trial_returns_matrix=None, n_trials_override=50.0, periods_per_year=100.0)
    em = 0.5772156649015329
    exp_sr0_50 = (1.0 / math.sqrt(r_sym.size - 1)) * (
        (1.0 - em) * float(_norm.ppf(1.0 - 1.0 / 50.0)) + em * float(_norm.ppf(1.0 - 1.0 / (50.0 * math.e)))
    )
    assert np.isclose(dsr_50.benchmark_sr0_per_period, exp_sr0_50, atol=1e-5)
    assert np.isclose(dsr_50.benchmark_sr0_annualized, exp_sr0_50 * 10.0, atol=1e-3)

    mat_with_zero = np.column_stack([noise_mat, np.zeros(n)])
    dsr_bad = mod.compute_deflated_sharpe_ratio(lucky, trial_returns_matrix=mat_with_zero, periods_per_year=252.0)
    assert dsr_bad.deflated_sharpe_ratio < 0.92
    assert dsr_bad.passed_institutional_gate is False
    # Exact check of matrix SR0 formula
    t_m = np.mean(mat_with_zero, axis=0)
    t_s = np.std(mat_with_zero, axis=0, ddof=1)
    t_srs = np.where(t_s > 1e-12, t_m / np.maximum(t_s, 1e-12), 0.0)
    cross_sd = max(float(np.std(t_srs, ddof=1)), 1.0 / math.sqrt(n - 1))
    me_mat, _ = mod.compute_effective_trials(mat_with_zero)
    exp_sr0_mat = cross_sd * (
        (1.0 - em) * float(_norm.ppf(1.0 - 1.0 / me_mat)) + em * float(_norm.ppf(1.0 - 1.0 / (me_mat * math.e)))
    )
    assert np.isclose(dsr_bad.benchmark_sr0_per_period, exp_sr0_mat, atol=1e-5)
    assert np.isclose(dsr_bad.benchmark_sr0_annualized, exp_sr0_mat * math.sqrt(252.0), atol=1e-3)

    # Zero-variance selected returns check
    dsr_zero = mod.compute_deflated_sharpe_ratio(np.zeros(50), trial_returns_matrix=None, n_trials_override=10.0)
    assert dsr_zero.sharpe_per_period == 0.0

    good = rng.normal(0.32, 1.0, size=n)
    dsr_good = mod.compute_deflated_sharpe_ratio(good, trial_returns_matrix=np.column_stack([noise_mat, good]))
    assert dsr_good.deflated_sharpe_ratio > 0.99
    assert dsr_good.passed_institutional_gate is True

    # Exact brute-force parity oracle for CSCV PBO on deterministic (T=32, M=5, S=4) matrix!
    small_mat = rng.normal(0.0, 1.0, size=(32, 5))
    small_mat[:, 0] += 0.25
    small_mat[:16, 1] += 0.60
    small_mat[16:, 1] -= 0.70  # Classic IS overfit trap -> negative OOS return!
    pbo_sm = mod.compute_cscv_pbo(small_mat, n_partitions=4, pbo_gate_threshold=0.25)

    blocks = np.array_split(small_mat, 4, axis=0)
    bf_logits = []
    bf_is = []
    bf_oos = []
    for idx_is in _comb(range(4), 2):
        idx_oos = [j for j in range(4) if j not in idx_is]
        is_arr = np.vstack([blocks[j] for j in idx_is])
        oos_arr = np.vstack([blocks[j] for j in idx_oos])
        s_is = np.mean(is_arr, axis=0) / np.std(is_arr, axis=0, ddof=1)
        s_oos = np.mean(oos_arr, axis=0) / np.std(oos_arr, axis=0, ddof=1)
        win = int(np.argmax(s_is))
        bf_is.append(float(s_is[win]))
        bf_oos.append(float(s_oos[win]))
        less = float(np.sum(s_oos < s_oos[win]))
        eq = float(np.sum(np.abs(s_oos - s_oos[win]) <= 1e-12))
        rk = (less + 0.5 * (eq + 1.0)) / 6.0
        bf_logits.append(math.log(rk / (1.0 - rk)))

    assert pbo_sm.n_combinations == 6
    assert np.allclose(pbo_sm.logits, bf_logits, atol=1e-3)
    assert np.isclose(pbo_sm.pbo, float(np.mean(np.array(bf_logits) <= 0.0)), atol=1e-5)
    assert np.isclose(pbo_sm.prob_oos_loss, float(np.mean(np.array(bf_oos) < 0.0)), atol=1e-5)
    assert np.isclose(pbo_sm.degradation_slope, float(np.polyfit(bf_is, bf_oos, 1)[0]), atol=1e-3)

    # Boundary error check: T < S * 4 must raise ValueError
    try:
        mod.compute_cscv_pbo(small_mat[:15, :], n_partitions=4)
        raise AssertionError("Expected ValueError for T < S * 4")
    except ValueError:
        pass


def _oracle_gae(mod: types.ModuleType) -> None:
    # T = 3 steps so `t + 1` vs `t - 1` strictly index different rows!
    rewards = torch.tensor([[1.0, 2.0], [3.0, -1.0], [0.5, 1.5]], dtype=torch.float32)
    values = torch.tensor([[0.5, 1.0], [1.5, 0.5], [2.0, 0.25]], dtype=torch.float32)
    dones = torch.tensor([[0.0, 1.0], [0.0, 0.0], [0.0, 0.0]], dtype=torch.float32)
    next_val = torch.tensor([1.0, 0.5], dtype=torch.float32)

    g, lam = 0.9, 0.8
    adv, ret = mod.compute_gae(rewards, values, dones, next_val, gamma=g, gae_lambda=lam)

    # Hand-verified exact recursion for env 0 (no dones):
    d2 = 0.5 + g * 1.0 - 2.0
    a2 = d2
    d1 = 3.0 + g * 2.0 - 1.5
    a1 = d1 + g * lam * a2
    d0 = 1.0 + g * 1.5 - 0.5
    a0 = d0 + g * lam * a1

    assert torch.isclose(adv[2, 0], torch.tensor(a2), atol=1e-5)
    assert torch.isclose(adv[1, 0], torch.tensor(a1), atol=1e-5)
    assert torch.isclose(adv[0, 0], torch.tensor(a0), atol=1e-5)
    # Env 1, t=0 has done=1.0 -> bootstrap strictly zeroed:
    assert torch.isclose(adv[0, 1], torch.tensor(2.0 - 1.0), atol=1e-5)
    assert torch.allclose(ret, adv + values, atol=1e-6)


def run_mutation_survival_audit() -> dict[str, Any]:
    """Execute full AST mutation testing suite across core math modules and compute kill score."""
    pkg_dir = Path(__file__).resolve().parent

    targets: list[tuple[str, Path, set[str], Callable[[types.ModuleType], None]]] = [
        (
            "fixed_point",
            pkg_dir / "fixed_point.py",
            {"trunc_div_i128", "compute_notional_fp_vec", "compute_fee_fp_vec", "settle_fill_double_entry_vec"},
            _oracle_fixed_point,
        ),
        (
            "gp_alpha.operators",
            pkg_dir / "gp_alpha" / "operators.py",
            {"ts_zscore", "ts_decay_linear", "ts_rank", "ts_corr", "ofi_delta", "microprice_skew"},
            _oracle_operators,
        ),
        (
            "validation.statistics",
            pkg_dir / "validation" / "statistics.py",
            {"compute_effective_trials", "compute_probabilistic_sharpe_ratio", "compute_deflated_sharpe_ratio", "compute_cscv_pbo"},
            _oracle_statistics,
        ),
        (
            "ppo_agent.trainer",
            pkg_dir / "ppo_agent" / "trainer.py",
            {"compute_gae"},
            _oracle_gae,
        ),
    ]

    total = 0
    killed = 0
    survived_details: list[dict[str, Any]] = []
    per_module: dict[str, dict[str, Any]] = {}

    for label, path, fn_set, oracle_fn in targets:
        src = path.read_text(encoding="utf-8")
        # First verify unmutated baseline passes oracle 100%!
        base_name = f"_mut_base_{label.replace('.', '_')}"
        baseline_mod = types.ModuleType(base_name)
        baseline_mod.__file__ = str(path)
        sys.modules[base_name] = baseline_mod
        try:
            exec(compile(ast.parse(src), str(path), "exec"), baseline_mod.__dict__)
            oracle_fn(baseline_mod)
        finally:
            sys.modules.pop(base_name, None)

        mutants = _collect_mutants_for_functions(src, label, fn_set)
        m_tot = len(mutants)
        m_kill = 0

        for spec, m_tree in mutants:
            total += 1
            mut_name = f"_mut_{label.replace('.', '_')}_{total}"
            mod = types.ModuleType(mut_name)
            mod.__file__ = str(path)
            sys.modules[mut_name] = mod
            try:
                code = compile(m_tree, str(path), "exec")
                exec(code, mod.__dict__)
                oracle_fn(mod)
                # If no exception/assertion fired, mutant survived!
                survived_details.append(
                    {
                        "module": spec.module_label,
                        "function": spec.func_name,
                        "line": spec.lineno,
                        "mutation": spec.description,
                    }
                )
            except Exception:
                killed += 1
                m_kill += 1
            finally:
                sys.modules.pop(mut_name, None)

        per_module[label] = {
            "killed": m_kill,
            "total": m_tot,
            "kill_score_pct": round(100.0 * m_kill / max(m_tot, 1), 2),
        }

    kill_pct = round(100.0 * killed / max(total, 1), 2)
    return {
        "total_mutants": total,
        "killed_mutants": killed,
        "survived_mutants_count": len(survived_details),
        "kill_score_pct": kill_pct,
        "per_module": per_module,
        "survived_details": survived_details,
    }
