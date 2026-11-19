"""Tests for GP time-series operators: strict zero-lookahead causality, parsimony, & Rust/Wasm compilation."""

from __future__ import annotations

import numpy as np

from alpha_evo.gp_alpha.ast_tree import (
    ASTNode,
    NodeKind,
    compile_ast_to_ssa,
    evaluate_ast_baseline_loop,
    evaluate_ast_vectorized,
    make_const,
    make_feature,
    make_microprice_skew,
    simplify_ast,
)
from alpha_evo.gp_alpha.engine import GPAlphaConfig, SymbolicAlphaEngine
from alpha_evo.gp_alpha.operators import (
    aligned_empty_64,
    compute_cks_ofi_from_quotes,
    microprice_skew,
    ofi_delta,
    protected_div,
    ts_corr,
    ts_decay_linear,
    ts_rank,
    ts_zscore,
)
from alpha_evo.gp_alpha.rust_wasm_compiler import (
    compile_alpha_to_json_manifest,
    compile_alpha_to_rust_wasm_source,
    try_compile_rust_to_wasm_binary,
)
from alpha_evo.lob_gym.market_generator import MarketGenConfig, generate_microstructure_dataset


def test_strict_causality_zero_lookahead_on_all_domain_operators() -> None:
    """
    CRITICAL ANTI-LOOKAHEAD PROOF:
    Modifying future observations `X[t+1:]` to wild extreme values MUST NOT alter
    operator output at index `<= t` by even 1e-12!
    """
    rng = np.random.default_rng(1901)
    n = 160
    x = rng.normal(50.0, 3.0, size=n)
    y = 0.6 * x + rng.normal(0.0, 2.0, size=n)
    bp = 68500.0 + np.cumsum(rng.normal(0.0, 0.5, size=n))
    ap = bp + 0.02 + np.abs(rng.normal(0.0, 0.01, size=n))
    bs = np.abs(rng.normal(2.0, 0.5, size=n)) + 0.1
    az = np.abs(rng.normal(2.0, 0.5, size=n)) + 0.1

    full_rank = ts_rank(x, 15)
    full_z = ts_zscore(x, 20)
    full_dec = ts_decay_linear(x, 12)
    full_corr = ts_corr(x, y, 18)
    full_ofi_d = ofi_delta(x, 10)
    full_cks = compute_cks_ofi_from_quotes(bp, bs, ap, az)
    full_mskew = microprice_skew(bp, bs, ap, az)

    for cut in (1, 14, 25, 87, 159):
        # Corrupt all future data after `cut` with extreme spikes
        x_tampered = x.copy()
        y_tampered = y.copy()
        bp_tampered = bp.copy()
        bs_tampered = bs.copy()
        if cut + 1 < n:
            x_tampered[cut + 1 :] = 999_999.0
            y_tampered[cut + 1 :] = -888_888.0
            bp_tampered[cut + 1 :] = 1.0
            bs_tampered[cut + 1 :] = 50_000.0

        np.testing.assert_allclose(ts_rank(x_tampered, 15)[: cut + 1], full_rank[: cut + 1], atol=1e-12)
        np.testing.assert_allclose(ts_zscore(x_tampered, 20)[: cut + 1], full_z[: cut + 1], atol=1e-12)
        np.testing.assert_allclose(ts_decay_linear(x_tampered, 12)[: cut + 1], full_dec[: cut + 1], atol=1e-12)
        np.testing.assert_allclose(ts_corr(x_tampered, y_tampered, 18)[: cut + 1], full_corr[: cut + 1], atol=1e-12)
        np.testing.assert_allclose(ofi_delta(x_tampered, 10)[: cut + 1], full_ofi_d[: cut + 1], atol=1e-12)
        np.testing.assert_allclose(
            compute_cks_ofi_from_quotes(bp_tampered, bs_tampered, ap, az)[: cut + 1],
            full_cks[: cut + 1],
            atol=1e-12,
        )
        np.testing.assert_allclose(
            microprice_skew(bp_tampered, bs_tampered, ap, az)[: cut + 1],
            full_mskew[: cut + 1],
            atol=1e-12,
        )


def test_operator_exact_mathematical_values() -> None:
    # 1. Linear decay on [1, 2, 3, 4] with window=4: (1*1 + 2*2 + 3*3 + 4*4) / 10 = 30 / 10 = 3.0
    x = np.array([1.0, 2.0, 3.0, 4.0], dtype=np.float64)
    dec = ts_decay_linear(x, window=4)
    assert np.isclose(dec[-1], 3.0, atol=1e-12)

    # 2. Rank on strictly increasing sequence -> +1.0; strictly decreasing -> -1.0
    assert np.isclose(ts_rank(np.array([1.0, 2.0, 3.0, 4.0, 5.0]), window=5)[-1], 1.0, atol=1e-12)
    assert np.isclose(ts_rank(np.array([5.0, 4.0, 3.0, 2.0, 1.0]), window=5)[-1], -1.0, atol=1e-12)

    # 3. Microprice skew: bid_sz=3, ask_sz=1 -> (3 - 1) / (3 + 1) = +0.5
    sk = microprice_skew(
        np.array([100.0]),
        np.array([3.0]),
        np.array([102.0]),
        np.array([1.0]),
    )
    assert np.isclose(sk[0], 0.5, atol=1e-12)

    # 4. Protected div returns 0 on near-zero denominator
    pd = protected_div(np.array([5.0, 6.0]), np.array([0.0, 2.0]))
    np.testing.assert_allclose(pd, np.array([0.0, 3.0]), atol=1e-12)

    # 5. Cache-line alignment 64B check
    buf = aligned_empty_64((4, 128), dtype=np.float64)
    assert buf.ctypes.data % 64 == 0


def test_three_evaluators_parity_and_algebraic_simplification() -> None:
    ds = generate_microstructure_dataset(MarketGenConfig(n_steps=300, seed=99))
    feats = ds.to_feature_dict()

    bloated = ASTNode(
        kind=NodeKind.BINARY_OP,
        op="add",
        children=(
            ASTNode(
                kind=NodeKind.WINDOW_OP,
                op="ts_zscore",
                children=(make_microprice_skew(),),
                window=10,
            ),
            ASTNode(
                kind=NodeKind.BINARY_OP,
                op="sub",
                children=(make_feature("ofi"), make_feature("ofi")),
            ),
        ),
    )
    clean = simplify_ast(bloated)
    assert clean.size() < bloated.size()
    assert simplify_ast(ASTNode(NodeKind.BINARY_OP, "mul", (make_feature("ofi"), make_const(0.0)))).value == 0.0

    p1 = evaluate_ast_baseline_loop(clean, feats)
    p2 = evaluate_ast_vectorized(clean, feats)
    tape = compile_ast_to_ssa(clean)
    ws = tape.create_workspace(300)
    p3 = tape.evaluate_in_place(feats, ws).copy()

    np.testing.assert_allclose(p1, p2, atol=1e-10)
    np.testing.assert_allclose(p2, p3, atol=1e-10)


def test_parsimony_pressure_controls_bloat_and_rust_wasm_compiles(tmp_path) -> None:
    ds = generate_microstructure_dataset(MarketGenConfig(n_steps=1_800, seed=42))
    feats = ds.to_feature_dict()

    # 1. Deterministic Parsimony Pressure check: bloated equivalent AST MUST score strictly lower!
    eng_pars = SymbolicAlphaEngine(GPAlphaConfig(parsimony_alpha=0.15, seed=101))
    compact_ast = ASTNode(NodeKind.WINDOW_OP, "ts_decay_linear", (make_feature("ofi"),), window=10)
    bloated_ast = ASTNode(
        NodeKind.BINARY_OP,
        "add",
        (
            compact_ast,
            ASTNode(
                NodeKind.BINARY_OP,
                "mul",
                (make_const(1e-7), make_feature("spread_bps")),
            ),
        ),
    )
    rec_compact, _ = eng_pars._evaluate_candidate(compact_ast, feats, ds.forward_return_bps, generation=0)
    rec_bloated, _ = eng_pars._evaluate_candidate(bloated_ast, feats, ds.forward_return_bps, generation=0)
    assert bloated_ast.size() > compact_ast.size()
    assert rec_compact.fitness > rec_bloated.fitness

    # 2. Full evolution under Parsimony Pressure keeps mean & best AST compact
    evo_pars = SymbolicAlphaEngine(
        GPAlphaConfig(population_size=24, n_generations=5, parsimony_alpha=0.15, seed=101)
    ).evolve(feats, ds.forward_return_bps)
    assert evo_pars.best_candidate.size <= 15

    manifest = compile_alpha_to_json_manifest(evo_pars.best_candidate.ast, {"dsr": 0.99})
    assert manifest["target_channel"] == "ch15-quant-wasm"
    assert len(manifest["ssa_bytecode"]["instructions"]) >= 1

    rs_code = compile_alpha_to_rust_wasm_source(evo_pars.best_candidate.ast)
    rs_path = tmp_path / "test_alpha.rs"
    wasm_path = tmp_path / "test_alpha.wasm"
    rs_path.write_text(rs_code, encoding="utf-8")

    comp = try_compile_rust_to_wasm_binary(rs_path, wasm_path)
    assert comp["compiled"] is True, f"Rust compilation failed: {comp}"
    if comp["target"] == "wasm32-unknown-unknown":
        assert comp["wasm_magic_valid"] is True
        assert comp["wasm_size_bytes"] > 1_000
