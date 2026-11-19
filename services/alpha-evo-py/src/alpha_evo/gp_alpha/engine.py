"""
Genetic Programming Symbolic Regression Alpha Factory (`SymbolicAlphaEngine`).

Evolves strictly causal microstructure alpha trees with:
- Koza Ramped Half-and-Half initialization (50% Grow, 50% Full)
- Tournament selection + Elitism + SHA-256 structural deduplication
- Subtree Crossover, Point Mutation, Subtree Mutation, and Hoist (anti-bloat) Mutation
- Parsimony Pressure + Turnover Penalty + Algebraic Simplification
- Full candidate return matrix recording `(T, M)` for downstream Bailey & López de Prado
  Deflated Sharpe Ratio (DSR) & Probability of Backtest Overfitting (PBO) verification.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import numpy as np
from numpy.typing import NDArray
from scipy.stats import spearmanr

from alpha_evo.gp_alpha.ast_tree import (
    ALLOWED_CONSTS,
    ALLOWED_WINDOWS,
    BINARY_OPS,
    BINARY_WINDOW_OPS,
    FEATURE_NAMES,
    UNARY_OPS,
    WINDOW_OPS,
    ASTNode,
    NodeKind,
    compile_ast_to_ssa,
    make_const,
    make_feature,
    make_microprice_skew,
    simplify_ast,
)
from alpha_evo.gp_alpha.operators import ts_decay_linear, ts_zscore


@dataclass(slots=True)
class GPAlphaConfig:
    """Hyperparameters for Symbolic Regression Alpha Evolution."""

    population_size: int = 72
    n_generations: int = 18
    tournament_size: int = 5
    elite_count: int = 4
    min_init_depth: int = 2
    max_init_depth: int = 4
    max_tree_depth: int = 5
    max_tree_nodes: int = 21
    p_crossover: float = 0.58
    p_point_mutation: float = 0.22
    p_hoist_mutation: float = 0.08
    p_subtree_mutation: float = 0.12
    # Parsimony Pressure & Realistic Friction Costs
    parsimony_alpha: float = 0.045       # Complexity penalty per AST node!
    depth_penalty_beta: float = 0.080    # Extra penalty for depth > 4
    turnover_cost_bps: float = 0.12      # Realistic passive maker/queue rebalance friction per unit turnover
    pred_horizon_bars: int = 10
    annualization_bars_per_year: float = 252.0 * 6.5 * 3600.0  # 100ms-1s high-freq scale normalized per-step
    seed: int = 202619


@dataclass(slots=True)
class CandidateRecord:
    """Evaluated symbolic formula candidate & metrics."""

    ast: ASTNode
    sexpr: str
    latex: str
    size: int
    depth: int
    fitness: float
    sharpe_is: float
    ic_pearson: float
    ic_spearman: float
    mean_turnover: float
    generation_born: int


@dataclass(slots=True)
class EvolutionResult:
    """Complete output of an autonomous GP alpha discovery run."""

    best_candidate: CandidateRecord
    hall_of_fame: list[CandidateRecord]
    generation_history: list[dict[str, float | int | str]]
    # Matrix of distinct candidate return streams (T_eval, M_trials) for DSR & PBO!
    trial_returns_matrix: NDArray[np.float64] = field(repr=False)
    trial_formulas: list[str] = field(default_factory=list)
    trial_asts: list[ASTNode] = field(default_factory=list, repr=False)


def signal_to_net_returns(
    raw_signal: NDArray[np.float64],
    forward_return_bps: NDArray[np.float64],
    pred_horizon_bars: int = 10,
    turnover_cost_bps: float = 0.35,
    warmup_bars: int = 50,
) -> tuple[NDArray[np.float64], NDArray[np.float64], float]:
    """
    Convert raw causal AST alpha signal into bounded [-1, +1] positions and compute
    non-overlapping (decimated by `pred_horizon_bars` or per-bar amortized) net strategy returns
    after realistic turnover transaction costs!

    Using non-overlapping `step = pred_horizon_bars` observations eliminates overlapping-return
    serial correlation inflation so Sharpe & DSR statistics are econometrically exact!
    """
    n = raw_signal.shape[0]
    # Strictly causal z-score standardization + 3-bar linear smoothing to curb microstructure whipsaw turnover
    z_sig = ts_zscore(raw_signal, window=30)
    norm_sig = np.tanh(ts_decay_linear(z_sig, window=3))
    norm_sig[:warmup_bars] = 0.0

    # Sample at non-overlapping horizon intervals `t = warmup_bars, warmup_bars + H, ...`
    end_valid = max(warmup_bars + 1, n - pred_horizon_bars)
    sample_idx = np.arange(warmup_bars, end_valid, max(1, pred_horizon_bars), dtype=np.int64)
    if sample_idx.size < 5:
        return np.zeros(5, dtype=np.float64), norm_sig, 0.0

    pos = norm_sig[sample_idx]
    prev_pos = np.empty_like(pos)
    prev_pos[0] = 0.0
    prev_pos[1:] = pos[:-1]
    turnover = np.abs(pos - prev_pos)

    gross_ret_bps = pos * forward_return_bps[sample_idx]
    net_ret_bps = gross_ret_bps - turnover_cost_bps * turnover
    mean_to = float(np.mean(turnover))
    return net_ret_bps, norm_sig, mean_to


class SymbolicAlphaEngine:
    """Autonomous AST Genetic Programming engine with Parsimony Pressure."""

    def __init__(self, cfg: GPAlphaConfig | None = None) -> None:
        self.cfg = cfg or GPAlphaConfig()
        self.rng = np.random.default_rng(self.cfg.seed)

    def random_terminal(self) -> ASTNode:
        u = float(self.rng.random())
        if u < 0.72:
            feat = str(self.rng.choice(FEATURE_NAMES))
            return make_feature(feat)
        if u < 0.88:
            return make_microprice_skew()
        c = float(self.rng.choice(ALLOWED_CONSTS))
        return make_const(c)

    def random_tree(self, max_depth: int, method: str = "grow") -> ASTNode:
        if max_depth <= 1 or (method == "grow" and float(self.rng.random()) < 0.30):
            return self.random_terminal()

        u = float(self.rng.random())
        if u < 0.36:
            op = str(self.rng.choice(WINDOW_OPS))
            w = int(self.rng.choice(ALLOWED_WINDOWS))
            return ASTNode(
                kind=NodeKind.WINDOW_OP,
                op=op,
                children=(self.random_tree(max_depth - 1, method),),
                window=w,
            )
        if u < 0.76:
            op = str(self.rng.choice(BINARY_OPS))
            return ASTNode(
                kind=NodeKind.BINARY_OP,
                op=op,
                children=(
                    self.random_tree(max_depth - 1, method),
                    self.random_tree(max_depth - 1, method),
                ),
            )
        if u < 0.90:
            op = str(self.rng.choice(UNARY_OPS))
            return ASTNode(
                kind=NodeKind.UNARY_OP,
                op=op,
                children=(self.random_tree(max_depth - 1, method),),
            )
        w = int(self.rng.choice(ALLOWED_WINDOWS))
        return ASTNode(
            kind=NodeKind.BINARY_WINDOW_OP,
            op=BINARY_WINDOW_OPS[0],
            children=(
                self.random_tree(max_depth - 1, method),
                self.random_tree(max_depth - 1, method),
            ),
            window=w,
        )

    @staticmethod
    def _enumerate_paths(node: ASTNode, prefix: tuple[int, ...] = ()) -> list[tuple[tuple[int, ...], ASTNode]]:
        items = [(prefix, node)]
        for idx, child in enumerate(node.children):
            items.extend(SymbolicAlphaEngine._enumerate_paths(child, prefix + (idx,)))
        return items

    @staticmethod
    def _replace_at_path(root: ASTNode, path: tuple[int, ...], replacement: ASTNode) -> ASTNode:
        if not path:
            return replacement
        head, tail = path[0], path[1:]
        new_children = list(root.children)
        new_children[head] = SymbolicAlphaEngine._replace_at_path(new_children[head], tail, replacement)
        return ASTNode(
            kind=root.kind,
            op=root.op,
            children=tuple(new_children),
            window=root.window,
            value=root.value,
        )

    def subtree_crossover(self, parent_a: ASTNode, parent_b: ASTNode) -> ASTNode:
        paths_a = self._enumerate_paths(parent_a)
        paths_b = self._enumerate_paths(parent_b)
        # Prefer internal nodes 75% of the time (Koza standard)
        internal_a = [p for p in paths_a if p[1].children]
        pool_a = internal_a if (internal_a and float(self.rng.random()) < 0.75) else paths_a
        path_a, _ = pool_a[int(self.rng.integers(0, len(pool_a)))]
        _, donor = paths_b[int(self.rng.integers(0, len(paths_b)))]

        offspring = simplify_ast(self._replace_at_path(parent_a, path_a, donor))
        if offspring.depth() > self.cfg.max_tree_depth or offspring.size() > self.cfg.max_tree_nodes:
            return parent_a
        return offspring

    def point_mutate(self, tree: ASTNode) -> ASTNode:
        paths = self._enumerate_paths(tree)
        path, target = paths[int(self.rng.integers(0, len(paths)))]

        if target.kind in (NodeKind.FEATURE, NodeKind.CONST, NodeKind.MICROPRICE_SKEW):
            mutated = self.random_terminal()
        elif target.kind == NodeKind.UNARY_OP:
            mutated = ASTNode(kind=NodeKind.UNARY_OP, op=str(self.rng.choice(UNARY_OPS)), children=target.children)
        elif target.kind == NodeKind.BINARY_OP:
            mutated = ASTNode(kind=NodeKind.BINARY_OP, op=str(self.rng.choice(BINARY_OPS)), children=target.children)
        elif target.kind == NodeKind.WINDOW_OP:
            if float(self.rng.random()) < 0.5:
                mutated = ASTNode(
                    kind=NodeKind.WINDOW_OP,
                    op=str(self.rng.choice(WINDOW_OPS)),
                    children=target.children,
                    window=target.window,
                )
            else:
                mutated = ASTNode(
                    kind=NodeKind.WINDOW_OP,
                    op=target.op,
                    children=target.children,
                    window=int(self.rng.choice(ALLOWED_WINDOWS)),
                )
        elif target.kind == NodeKind.BINARY_WINDOW_OP:
            mutated = ASTNode(
                kind=NodeKind.BINARY_WINDOW_OP,
                op=target.op,
                children=target.children,
                window=int(self.rng.choice(ALLOWED_WINDOWS)),
            )
        else:
            mutated = target

        out = simplify_ast(self._replace_at_path(tree, path, mutated))
        if out.depth() > self.cfg.max_tree_depth or out.size() > self.cfg.max_tree_nodes:
            return tree
        return out

    def hoist_mutate(self, tree: ASTNode) -> ASTNode:
        """Anti-bloat Hoist Mutation: promotes an internal subtree to become the new root."""
        paths = [p for p in self._enumerate_paths(tree) if p[0] and p[1].size() >= 2]
        if not paths:
            return tree
        _, sub = paths[int(self.rng.integers(0, len(paths)))]
        return simplify_ast(sub)

    def subtree_mutate(self, tree: ASTNode) -> ASTNode:
        paths = self._enumerate_paths(tree)
        path, _ = paths[int(self.rng.integers(0, len(paths)))]
        new_sub = self.random_tree(max_depth=2, method="grow")
        out = simplify_ast(self._replace_at_path(tree, path, new_sub))
        if out.depth() > self.cfg.max_tree_depth or out.size() > self.cfg.max_tree_nodes:
            return tree
        return out

    def _evaluate_candidate(
        self,
        ast: ASTNode,
        features: dict[str, NDArray[np.float64]],
        forward_return_bps: NDArray[np.float64],
        generation: int,
    ) -> tuple[CandidateRecord, NDArray[np.float64]]:
        tape = compile_ast_to_ssa(ast)
        n = forward_return_bps.shape[0]
        ws = tape.create_workspace(n)
        raw_sig = tape.evaluate_in_place(features, ws).copy()

        warmup = 50
        valid_slice = slice(warmup, max(warmup + 2, n - self.cfg.pred_horizon_bars))
        s_val = raw_sig[valid_slice]
        y_val = forward_return_bps[valid_slice]

        if s_val.size < 20 or float(np.std(s_val)) < 1e-6:
            rec = CandidateRecord(
                ast=ast,
                sexpr=ast.to_sexpr(),
                latex=ast.to_latex(),
                size=ast.size(),
                depth=ast.depth(),
                fitness=-1e6,
                sharpe_is=-5.0,
                ic_pearson=0.0,
                ic_spearman=0.0,
                mean_turnover=0.0,
                generation_born=generation,
            )
            return rec, np.zeros(max(5, (n - warmup - self.cfg.pred_horizon_bars) // max(1, self.cfg.pred_horizon_bars)))

        net_rets, norm_sig, mean_to = signal_to_net_returns(
            raw_sig,
            forward_return_bps,
            pred_horizon_bars=self.cfg.pred_horizon_bars,
            turnover_cost_bps=self.cfg.turnover_cost_bps,
            warmup_bars=warmup,
        )

        # Reject degenerate saturated/clipped expressions that avoid trading via zero variance in z-score!
        active_pos_mean = float(np.mean(np.abs(norm_sig[valid_slice])))
        if active_pos_mean < 0.18 or mean_to < 0.02:
            rec = CandidateRecord(
                ast=ast,
                sexpr=ast.to_sexpr(),
                latex=ast.to_latex(),
                size=ast.size(),
                depth=ast.depth(),
                fitness=-1e6,
                sharpe_is=-5.0,
                ic_pearson=0.0,
                ic_spearman=0.0,
                mean_turnover=float(mean_to),
                generation_born=generation,
            )
            return rec, np.zeros_like(net_rets)

        sd_ret = float(np.std(net_rets, ddof=1)) if net_rets.size > 2 else 0.0
        # Annualize assuming 100ms step * pred_horizon_bars (e.g., 1s period -> ~86,400 periods/day or 252 trading days * 23,400s)
        periods_per_year = 252.0 * 6.5 * 3600.0 / max(0.1 * self.cfg.pred_horizon_bars, 0.1)
        sharpe_ann = (float(np.mean(net_rets)) / sd_ret) * np.sqrt(periods_per_year) if sd_ret > 1e-9 else -5.0
        # Non-annualized sample t-stat equivalent (`SR_per_period * sqrt(T_periods)`) is smoother for GP fitness
        t_stat_sr = (float(np.mean(net_rets)) / sd_ret) * np.sqrt(float(net_rets.size)) if sd_ret > 1e-9 else -5.0

        ic_p = float(np.corrcoef(s_val, y_val)[0, 1]) if np.std(y_val) > 1e-9 else 0.0
        if not np.isfinite(ic_p):
            ic_p = 0.0
        sp_res = spearmanr(s_val, y_val)
        ic_s = float(sp_res.statistic) if np.isfinite(sp_res.statistic) else 0.0

        # Parsimony Pressure penalty (penalizes bloat in node count & excess depth!)
        node_pen = self.cfg.parsimony_alpha * float(ast.size())
        depth_pen = self.cfg.depth_penalty_beta * float(max(0, ast.depth() - 4))
        tarpeian_pen = 10.0 if (ast.size() > self.cfg.max_tree_nodes or ast.depth() > self.cfg.max_tree_depth) else 0.0

        fitness = t_stat_sr + 6.0 * (abs(ic_p) if ic_p > 0 else 2.0 * ic_p) - node_pen - depth_pen - tarpeian_pen

        # Flip negated tree automatically if strongly anti-correlated (sign-canonicalization)
        if ic_p < -0.025 and ast.size() + 1 <= self.cfg.max_tree_nodes:
            flipped = simplify_ast(ASTNode(kind=NodeKind.UNARY_OP, op="neg", children=(ast,)))
            return self._evaluate_candidate(flipped, features, forward_return_bps, generation)

        rec = CandidateRecord(
            ast=ast,
            sexpr=ast.to_sexpr(),
            latex=ast.to_latex(),
            size=ast.size(),
            depth=ast.depth(),
            fitness=float(fitness),
            sharpe_is=float(sharpe_ann),
            ic_pearson=float(ic_p),
            ic_spearman=float(ic_s),
            mean_turnover=float(mean_to),
            generation_born=generation,
        )
        return rec, net_rets

    def _tournament_select(self, pop: list[CandidateRecord]) -> CandidateRecord:
        idxs = self.rng.integers(0, len(pop), size=self.cfg.tournament_size)
        best = pop[int(idxs[0])]
        for i in idxs[1:]:
            cand = pop[int(i)]
            # Lexicographic parsimony tie-breaker: prefer smaller AST when fitness is close!
            if cand.fitness > best.fitness + 1e-4 or (
                abs(cand.fitness - best.fitness) <= 1e-4 and cand.size < best.size
            ):
                best = cand
        return best

    def evolve(
        self,
        train_features: dict[str, NDArray[np.float64]],
        train_forward_return_bps: NDArray[np.float64],
    ) -> EvolutionResult:
        """Run full multi-generation Symbolic Regression Alpha Evolution."""
        cache: dict[str, tuple[CandidateRecord, NDArray[np.float64]]] = {}
        trial_returns_cols: list[NDArray[np.float64]] = []
        trial_formulas: list[str] = []
        trial_asts: list[ASTNode] = []

        # 1. Ramped Half-and-Half initial population
        population_asts: list[ASTNode] = []
        seen_init: set[str] = set()
        depths = list(range(self.cfg.min_init_depth, self.cfg.max_init_depth + 1))
        attempts = 0
        while len(population_asts) < self.cfg.population_size and attempts < self.cfg.population_size * 10:
            attempts += 1
            d = depths[len(population_asts) % len(depths)]
            method = "full" if (len(population_asts) % 2 == 0) else "grow"
            cand_ast = simplify_ast(self.random_tree(max_depth=d, method=method))
            sexpr = cand_ast.to_sexpr()
            if sexpr not in seen_init:
                seen_init.add(sexpr)
                population_asts.append(cand_ast)

        gen_history: list[dict[str, float | int | str]] = []
        hall_of_fame_map: dict[str, CandidateRecord] = {}
        evaluated_pop: list[CandidateRecord] = []

        for gen in range(self.cfg.n_generations):
            evaluated_pop = []
            for ast in population_asts:
                sexpr = ast.to_sexpr()
                if sexpr not in cache:
                    rec, rets = self._evaluate_candidate(ast, train_features, train_forward_return_bps, gen)
                    cache[rec.sexpr] = (rec, rets)
                    if rec.fitness > -1e5 and np.std(rets) > 1e-9:
                        trial_returns_cols.append(rets)
                        trial_formulas.append(rec.sexpr)
                        trial_asts.append(rec.ast)
                else:
                    rec, _ = cache[sexpr]
                evaluated_pop.append(rec)
                if rec.fitness > -1e5:
                    hall_of_fame_map[rec.sexpr] = rec

            evaluated_pop.sort(key=lambda r: (r.fitness, -r.size), reverse=True)
            best_gen = evaluated_pop[0]
            valid_fits = [r.fitness for r in evaluated_pop if r.fitness > -1e5]
            mean_fit = float(np.mean(valid_fits)) if valid_fits else -1e6
            mean_size = float(np.mean([r.size for r in evaluated_pop]))

            gen_history.append(
                {
                    "generation": gen,
                    "best_fitness": round(best_gen.fitness, 4),
                    "mean_fitness": round(mean_fit, 4),
                    "best_ic_pearson": round(best_gen.ic_pearson, 5),
                    "best_ic_spearman": round(best_gen.ic_spearman, 5),
                    "best_sharpe_is": round(best_gen.sharpe_is, 3),
                    "best_ast_nodes": best_gen.size,
                    "mean_ast_nodes": round(mean_size, 2),
                    "unique_trials_evaluated": len(trial_formulas),
                    "best_formula": best_gen.sexpr,
                }
            )

            if gen == self.cfg.n_generations - 1:
                break

            # Breeding next generation with Elitism + Structural Deduplication
            next_asts: list[ASTNode] = [r.ast for r in evaluated_pop[: self.cfg.elite_count]]
            next_seen: set[str] = {a.to_sexpr() for a in next_asts}

            breed_tries = 0
            while len(next_asts) < self.cfg.population_size and breed_tries < self.cfg.population_size * 12:
                breed_tries += 1
                u = float(self.rng.random())
                if u < self.cfg.p_crossover:
                    pa = self._tournament_select(evaluated_pop).ast
                    pb = self._tournament_select(evaluated_pop).ast
                    child = self.subtree_crossover(pa, pb)
                elif u < self.cfg.p_crossover + self.cfg.p_point_mutation:
                    pa = self._tournament_select(evaluated_pop).ast
                    child = self.point_mutate(pa)
                elif u < self.cfg.p_crossover + self.cfg.p_point_mutation + self.cfg.p_hoist_mutation:
                    pa = self._tournament_select(evaluated_pop).ast
                    child = self.hoist_mutate(pa)
                else:
                    pa = self._tournament_select(evaluated_pop).ast
                    child = self.subtree_mutate(pa)

                sx = child.to_sexpr()
                if sx not in next_seen:
                    next_seen.add(sx)
                    next_asts.append(child)

            while len(next_asts) < self.cfg.population_size:
                next_asts.append(simplify_ast(self.random_tree(max_depth=3, method="grow")))

            population_asts = next_asts

        hof = sorted(hall_of_fame_map.values(), key=lambda r: (r.fitness, -r.size), reverse=True)[:15]
        best_overall = hof[0] if hof else evaluated_pop[0]

        if trial_returns_cols:
            min_len = min(col.shape[0] for col in trial_returns_cols)
            ret_mat = np.column_stack([col[:min_len] for col in trial_returns_cols])
        else:
            ret_mat = np.zeros((32, 1), dtype=np.float64)

        return EvolutionResult(
            best_candidate=best_overall,
            hall_of_fame=hof,
            generation_history=gen_history,
            trial_returns_matrix=ret_mat,
            trial_formulas=trial_formulas,
            trial_asts=trial_asts,
        )
