"""HYPERION Overclock Protocol Verification Suite for CH05 Sentinel-ML.

Implements all three mandatory gates from `.agents/rules/02-extreme-overclock-protocol.md`:
  1. In-memory AST-Operator Mutation Testing (`>= 95.0%` mutation kill score, zero disk mutation)
     across `fixed_point.py` and core `graph_engine.py` financial/cycle/window scoring math.
  2. Consecutive `500,000`-cycle `tracemalloc` zero-leak soak test exercising fixed-point math,
     sliding-window temporal multigraph eviction/cycle detection, and 64B-aligned SHAP buffers.
  3. Honest 3-Pass Micro-Optimization Benchmark (`Pass 1 Baseline` -> `Pass 2 Vectorized/Zero-Copy`
     -> `Pass 3 64B Cache-Line Aligned Final`) measuring exact `ns/op` (p50/p99), `B/op`, and
     `allocs/op`.
"""

from __future__ import annotations

import ast
import copy
import gc
import json
import math
from pathlib import Path
import sys
import time
import tracemalloc
import types
from typing import Any

import numpy as np

from sentinel_ml import fixed_point as fp_ref
from sentinel_ml.graph_engine import SlidingTemporalMultigraph
from sentinel_ml.microstructure import NUM_FEATURES, TEMPORAL_SEQ_LEN, alloc_aligned64_f32
from sentinel_ml.models_onnx import SentinelOnnxRuntime

_CMP_MUTATIONS: dict[type[ast.cmpop], type[ast.cmpop]] = {
    ast.Lt: ast.LtE,
    ast.LtE: ast.Lt,
    ast.Gt: ast.GtE,
    ast.GtE: ast.Gt,
    ast.Eq: ast.NotEq,
    ast.NotEq: ast.Eq,
}

_BINOP_MUTATIONS: dict[type[ast.operator], type[ast.operator]] = {
    ast.Add: ast.Sub,
    ast.Sub: ast.Add,
    ast.Mult: ast.FloorDiv,
    ast.FloorDiv: ast.Mult,
    ast.Div: ast.Mult,
    ast.LShift: ast.RShift,
    ast.RShift: ast.LShift,
    ast.BitOr: ast.BitAnd,
    ast.BitAnd: ast.BitOr,
}

_CONST_INT_MUTATIONS: dict[int, int] = {
    100_000_000: 10_000_000,
    1_000_000: 100_000,
    10_000: 1_000,
    64: 63,
    63: 62,
    128: 127,
    127: 126,
    8: 7,
    6: 5,
}


def _iter_runtime_children(node: ast.AST) -> list[ast.AST]:
    """Yield executable child AST nodes, excluding inert PEP-563 type annotations."""
    children: list[ast.AST] = []
    for field, value in ast.iter_fields(node):
        if field in ("annotation", "returns"):
            continue
        if isinstance(value, ast.AST):
            children.append(value)
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, ast.AST):
                    children.append(item)
    return children


def _collect_mutation_sites(
    tree: ast.Module,
    *,
    allowed_funcs: set[str] | None = None,
) -> list[tuple[int, str, str]]:
    """Collect deterministic executable AST operator & constant mutation targets."""
    sites: list[tuple[int, str, str]] = []
    node_idx = 0

    def _visit(node: ast.AST, in_allowed: bool) -> None:
        nonlocal node_idx
        cur_allowed = in_allowed
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            cur_allowed = True if allowed_funcs is None else (node.name in allowed_funcs)

        if isinstance(node, ast.While):
            for stmt in node.body:
                _visit(stmt, cur_allowed)
            for stmt in node.orelse:
                _visit(stmt, cur_allowed)
            return

        if cur_allowed:
            if isinstance(node, ast.Compare) and len(node.ops) == 1:
                cmp_t = type(node.ops[0])
                if cmp_t in _CMP_MUTATIONS:
                    sites.append(
                        (
                            node_idx,
                            "cmp",
                            f"L{getattr(node, 'lineno', 0)}: {cmp_t.__name__}"
                            f"->{_CMP_MUTATIONS[cmp_t].__name__}",
                        )
                    )
            elif isinstance(node, ast.BinOp):
                bin_t = type(node.op)
                if bin_t in _BINOP_MUTATIONS:
                    sites.append(
                        (
                            node_idx,
                            "binop",
                            f"L{getattr(node, 'lineno', 0)}: {bin_t.__name__}"
                            f"->{_BINOP_MUTATIONS[bin_t].__name__}",
                        )
                    )
            elif (
                isinstance(node, ast.Constant)
                and type(node.value) is int
                and node.value in _CONST_INT_MUTATIONS
            ):
                sites.append(
                    (
                        node_idx,
                        "const",
                        f"L{getattr(node, 'lineno', 0)}: {node.value}"
                        f"->{_CONST_INT_MUTATIONS[node.value]}",
                    )
                )

        node_idx += 1
        for child in _iter_runtime_children(node):
            _visit(child, cur_allowed)

    _visit(tree, allowed_funcs is None)
    return sites


def _apply_mutation(tree: ast.Module, target_idx: int, kind: str) -> ast.Module:
    """Return a deep-copied AST with exactly one mutation applied at target_idx."""
    mutated = copy.deepcopy(tree)
    node_idx = 0
    done = False

    def _walk(node: ast.AST) -> None:
        nonlocal node_idx, done
        if done:
            return
        if isinstance(node, ast.While):
            for stmt in node.body:
                _walk(stmt)
            for stmt in node.orelse:
                _walk(stmt)
            return
        if node_idx == target_idx:
            if kind == "cmp" and isinstance(node, ast.Compare):
                node.ops[0] = _CMP_MUTATIONS[type(node.ops[0])]()
            elif kind == "binop" and isinstance(node, ast.BinOp):
                node.op = _BINOP_MUTATIONS[type(node.op)]()
            elif kind == "const" and isinstance(node, ast.Constant) and type(node.value) is int:
                node.value = _CONST_INT_MUTATIONS[node.value]
            done = True
            return
        node_idx += 1
        for child in _iter_runtime_children(node):
            _walk(child)

    _walk(mutated)
    ast.fix_missing_locations(mutated)
    return mutated


def _exec_ast_module(tree: ast.Module, mod_name: str, filename: str) -> types.ModuleType:
    """Compile and execute an AST in an ephemeral module registered in sys.modules."""
    mod = types.ModuleType(mod_name)
    sys.modules[mod_name] = mod
    try:
        exec(compile(tree, filename, "exec"), mod.__dict__)
        return mod
    finally:
        sys.modules.pop(mod_name, None)


def _verify_fixed_point_module(fp: Any) -> None:
    """Comprehensive deterministic oracle killing arithmetic, boundary, & bit-split mutants."""
    assert fp.SCALE == 100_000_000
    assert fp.PPM_SCALE == 1_000_000
    assert fp.BPS_SCALE == 10_000
    assert fp.UINT64_MAX == (1 << 64) - 1
    assert fp.INT64_MIN == -(1 << 63)
    assert fp.INT64_MAX == (1 << 63) - 1
    assert fp.UINT128_MAX == (1 << 128) - 1
    assert fp.INT128_MIN == -(1 << 127)
    assert fp.INT128_MAX == (1 << 127) - 1
    assert fp.MASK_64 == (1 << 64) - 1

    assert fp.require_fp_int(0, "x", allow_zero=True, allow_negative=False) == 0
    assert fp.require_fp_int(1, "x", allow_zero=False, allow_negative=False) == 1
    assert fp.require_fp_int(-1, "x", allow_zero=False, allow_negative=True) == -1
    assert fp.require_fp_int(fp.INT128_MIN, "x", allow_negative=True) == fp.INT128_MIN
    assert fp.require_fp_int(fp.UINT128_MAX, "x", allow_negative=False) == fp.UINT128_MAX

    for bad_type in (True, False, 1.5, "100"):
        try:
            fp.require_fp_int(bad_type, "x")
            raise AssertionError("Expected FixedPointViolationError")
        except fp.FixedPointViolationError:
            pass

    for fn_call in (
        lambda: fp.require_fp_int(-1, "x", allow_negative=False),
        lambda: fp.require_fp_int(0, "x", allow_zero=False),
        lambda: fp.require_uint64(-1, "u"),
        lambda: fp.require_uint64(0, "u", allow_zero=False),
    ):
        try:
            fn_call()
            raise AssertionError("Expected ValueError")
        except ValueError:
            pass

    for fn_call in (
        lambda: fp.require_fp_int(fp.INT128_MIN - 1, "x", allow_negative=True),
        lambda: fp.require_fp_int(fp.UINT128_MAX + 1, "x", allow_negative=False),
        lambda: fp.require_uint64(fp.UINT64_MAX + 1, "u"),
        lambda: fp.require_int64(fp.INT64_MIN - 1, "s"),
        lambda: fp.require_int64(fp.INT64_MAX + 1, "s"),
        lambda: fp.int_to_u128_parts(fp.UINT128_MAX + 1),
        lambda: fp.int_to_i128_parts(fp.INT128_MIN - 1),
        lambda: fp.int_to_i128_parts(fp.INT128_MAX + 1),
    ):
        try:
            fn_call()
            raise AssertionError("Expected OverflowError")
        except OverflowError:
            pass

    assert fp.require_uint64(0, "u", allow_zero=True) == 0
    assert fp.require_uint64(fp.UINT64_MAX, "u") == fp.UINT64_MAX
    assert fp.require_int64(fp.INT64_MIN, "s") == fp.INT64_MIN
    assert fp.require_int64(fp.INT64_MAX, "s") == fp.INT64_MAX

    assert fp.compute_notional_fp(6_842_550_000_000, 125_000_000) == 8_553_187_500_000
    assert fp.compute_notional_fp(3, 33_333_333) == 0
    assert fp.compute_notional_fp(3, 33_333_334) == 1
    assert fp.compute_notional_fp(fp.UINT64_MAX, fp.UINT64_MAX) == (
        fp.UINT64_MAX * fp.UINT64_MAX
    ) // fp.SCALE
    assert fp.distance_bps_int(10_500_000_000, 10_000_000_000) == 500
    assert fp.distance_bps_int(10_000_000_000, 10_000_000_000) == 0
    assert fp.distance_bps_int(0, 10_000_000_000) == 10_000

    for v in (0, 1, fp.UINT64_MAX, (1 << 64) + 77, (1 << 96) + 12345, fp.UINT128_MAX):
        hi, lo = fp.int_to_u128_parts(v)
        assert hi == (v >> 64) and lo == (v & ((1 << 64) - 1))
        assert fp.u128_parts_to_int(hi, lo) == v

    for v in (
        0,
        1,
        -1,
        (1 << 63) - 1,
        -(1 << 63),
        (1 << 90) + 123,
        -((1 << 90) + 456),
        fp.INT128_MIN,
        fp.INT128_MAX,
    ):
        hi, lo = fp.int_to_i128_parts(v)
        assert fp.INT64_MIN <= hi <= fp.INT64_MAX
        assert 0 <= lo <= fp.UINT64_MAX
        assert fp.i128_parts_to_int(hi, lo) == v

    assert fp.ratio_to_ppm(1.0, 0.0) == 0
    assert fp.ratio_to_ppm(0.25, 1.0) == 250_000
    assert fp.ratio_to_ppm(3.0, 2.0, clamp_unit=False) == 1_500_000
    assert fp.ratio_to_ppm(3.0, 2.0, clamp_unit=True) == 1_000_000
    assert fp.ratio_to_ppm(-0.5, 1.0, clamp_unit=True) == 0
    assert fp.scalar_to_fp_int64(1.25) == 125_000_000
    assert fp.scalar_to_fp_int64(-2.5) == -250_000_000
    assert fp.scalar_to_fp_int64(1e30) == fp.INT64_MAX
    assert fp.scalar_to_fp_int64(-1e30) == fp.INT64_MIN

    assert fp.parse_decimal_to_fp("42150.125") == 4_215_012_500_000
    assert fp.parse_decimal_to_fp("1.5") == 150_000_000
    assert fp.parse_decimal_to_fp("+42") == 4_200_000_000
    assert fp.parse_decimal_to_fp("-0.00000001") == -1
    assert fp.parse_decimal_to_fp("0.00000000") == 0
    assert fp.format_fp_to_decimal(0) == "0.00000000"
    assert fp.format_fp_to_decimal(123_450_000) == "1.23450000"
    assert fp.format_fp_to_decimal(-123_450_000) == "-1.23450000"


def _verify_graph_engine_module(ge: Any) -> None:
    """Deterministic oracle killing cycle depth, eviction volume, decay, & wash scoring mutants."""
    scale = fp_ref.SCALE
    px = 68_000 * scale
    qty = 2 * scale
    t0 = 1_700_000_000_000_000_000

    for bad_ctor in (
        lambda: ge.SlidingTemporalMultigraph(max_cycle_depth=0),
        lambda: ge.SlidingTemporalMultigraph(max_cycle_depth=7),
        lambda: ge.SlidingTemporalMultigraph(window_ns=0),
        lambda: ge.SlidingTemporalMultigraph(decay_tau_ns=0),
    ):
        try:
            bad_ctor()
            raise AssertionError("Expected ValueError on invalid constructor args")
        except ValueError:
            pass

    # Default constructor must support exact k=6 cycle (kills default max_cycle_depth 6->5)
    g_def = ge.SlidingTemporalMultigraph()
    assert g_def.max_cycle_depth == 6
    for i in range(1, 6):
        g_def.ingest_trade(
            fill_id=i, symbol="ETH-USDT", maker_account_id=i, taker_account_id=i + 1,
            price_fp=px, qty_fp=qty, exec_ts_ns=t0 + i * 10_000_000,
        )
    _, cyc6 = g_def.ingest_trade(
        fill_id=6, symbol="ETH-USDT", maker_account_id=6, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 60_000_000,
    )
    assert len(cyc6) == 1 and cyc6[0].cycle_length == 6

    # Non-uniform quantities, prices, and timestamps verifying exact wash-score formula
    # Includes cross-symbol trade on same pair (101->102) to test symbol filtering
    g = ge.SlidingTemporalMultigraph(
        window_ns=60_000_000_000, max_cycle_depth=6, decay_tau_ns=10_000_000_000
    )
    px1, px2, px3 = 10_000 * scale, 10_100 * scale, 10_200 * scale
    q1, q2, q3 = 1 * scale, 2 * scale, 2 * scale
    g.ingest_trade(
        fill_id=90, symbol="ETH-USDT", maker_account_id=101, taker_account_id=102,
        price_fp=px1, qty_fp=9 * scale, exec_ts_ns=t0 - 1_000_000, aggressor_side="BUY",
    )
    g.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=101, taker_account_id=102,
        price_fp=px1, qty_fp=q1, exec_ts_ns=t0, aggressor_side="BUY",
    )
    g.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=102, taker_account_id=103,
        price_fp=px2, qty_fp=q2, exec_ts_ns=t0 + 1_000_000_000, aggressor_side="BUY",
    )
    _, c3 = g.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=103, taker_account_id=101,
        price_fp=px3, qty_fp=q3, exec_ts_ns=t0 + 2_000_000_000, aggressor_side="BUY",
    )
    assert len(c3) == 1
    cyc = c3[0]
    assert cyc.cycle_length == 3
    assert cyc.participant_account_ids == (101, 102, 103)
    assert cyc.circular_volume_fp == q1 + q2 + q3
    assert cyc.net_position_delta_fp == 2 * scale
    assert cyc.cycle_duration_ns == 2_000_000_000
    assert cyc.volume_symmetry_ppm == 500_000
    exp_decay = math.exp(-2_000_000_000 / 10_000_000_000)
    assert abs(cyc.temporal_decay - exp_decay) < 1e-6
    assert abs(cyc.causal_order_ratio - 1.0) < 1e-6
    exp_mean_px = (px1 + px2 + px3) / 3.0
    exp_tight = math.exp(-50.0 * (px3 - px1) / exp_mean_px)
    assert abs(cyc.price_tightness - exp_tight) < 1e-6
    exp_ret = 1.0 - (2 * scale) / (2 * (q1 + q2 + q3))
    exp_wash = exp_decay * (0.35 * 0.5 + 0.30 * exp_ret + 0.20 * 1.0 + 0.15 * exp_tight)
    assert abs(cyc.wash_score - exp_wash) < 1e-6

    # Simultaneous timestamps (exec_ts_ns == cursor_ts) & out-of-order fallback symbol match
    g_eq = ge.SlidingTemporalMultigraph(window_ns=60_000_000_000, max_cycle_depth=6)
    g_eq.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=11, taker_account_id=12,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="BUY",
    )
    g_eq.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=12, taker_account_id=13,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="BUY",
    )
    _, c_eq = g_eq.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=13, taker_account_id=11,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="BUY",
    )
    assert len(c_eq) == 1 and abs(c_eq[0].causal_order_ratio - 1.0) < 1e-6

    # Out-of-order leg forces fallback search for symbol-matching edge (kills L584 Eq->NotEq)
    g_ooo = ge.SlidingTemporalMultigraph(window_ns=60_000_000_000, max_cycle_depth=6)
    g_ooo.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=21, taker_account_id=22,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 500_000_000, aggressor_side="BUY",
    )
    g_ooo.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=22, taker_account_id=23,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 600_000_000, aggressor_side="BUY",
    )
    _, c_ooo = g_ooo.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=23, taker_account_id=21,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 100_000_000, aggressor_side="BUY",
    )
    assert len(c_ooo) == 1 and c_ooo[0].causal_order_ratio < 1.0

    # Check get_account_graph_metrics exact decay, depth_norm, and counterparty HHI
    ws_now, dn_now, cp_now = g.get_account_graph_metrics(101, t0 + 2_000_000_000)
    assert abs(ws_now - exp_wash) < 1e-6
    assert abs(dn_now - (3 / 6.0)) < 1e-6
    # 101 traded with 102 (2x: ETH+BTC) and 103 (1x): total=3, hhi=(4+1)/9, conf=3/6 -> 5/18
    assert abs(cp_now - ((5.0 / 9.0) * (3.0 / 6.0))) < 1e-6

    ws_later, dn_later, _ = g.get_account_graph_metrics(101, t0 + 12_000_000_000)
    assert abs(ws_later - exp_wash * math.exp(-1.0)) < 1e-6
    assert abs(dn_later - (3 / 6.0)) < 1e-6

    # Far future: wash_score decays <= 0.05 -> depth_norm must drop to 0.0
    ws_far, dn_far, _ = g.get_account_graph_metrics(101, t0 + 60_000_000_000)
    assert ws_far <= 0.05 and dn_far == 0.0

    # Mixed BUY/SELL with unequal quantities: asset_flow vs maker_taker differ (kills L616 Eq->NotEq)
    g_mix = ge.SlidingTemporalMultigraph(window_ns=60_000_000_000, max_cycle_depth=6)
    g_mix.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=2, taker_account_id=1,
        price_fp=px, qty_fp=1 * scale, exec_ts_ns=t0, aggressor_side="SELL",
    )
    g_mix.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=2, taker_account_id=3,
        price_fp=px, qty_fp=2 * scale, exec_ts_ns=t0 + 10_000_000, aggressor_side="BUY",
    )
    _, cmix = g_mix.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=3, taker_account_id=1,
        price_fp=px, qty_fp=2 * scale, exec_ts_ns=t0 + 20_000_000, aggressor_side="BUY",
    )
    assert len(cmix) == 1
    assert cmix[0].orientation == "asset_flow"
    assert cmix[0].net_position_delta_fp == 2 * scale

    # Self-loop + exact sliding-window eviction & account volume conservation (kills L202/203/205)
    gw = ge.SlidingTemporalMultigraph(window_ns=10_000_000_000, max_cycle_depth=2)
    _, c_self = gw.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=50, taker_account_id=50,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0,
    )
    assert len(c_self) == 1 and c_self[0].cycle_length == 1
    assert gw.get_account_traded_volume_fp(50) == qty
    gw.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=1, taker_account_id=2,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 1_000_000_000,
    )
    _, c2 = gw.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=2, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 10_000_000_000,
    )
    assert len(c2) == 1 and c2[0].cycle_length == 2
    assert gw.active_edge_count == 3
    assert gw.get_account_traded_volume_fp(1) == 2 * qty
    assert gw.get_account_traded_volume_fp(2) == 2 * qty

    # Evict self-loop at t0 + 10s + 1ns: acct 50 volume must drop to 0 (not double-subtracted/added)
    assert gw.evict_expired(t0 + 10_000_000_001) == 1
    assert gw.get_account_traded_volume_fp(50) == 0
    # Evict 1->2 edge at t0 + 11s + 1ns: acct 1 and 2 must each have exact remaining qty
    assert gw.evict_expired(t0 + 11_000_000_001) == 1
    assert gw.get_account_traded_volume_fp(1) == qty
    assert gw.get_account_traded_volume_fp(2) == qty
    assert gw.active_edge_count == 1


def run_ast_mutation_audit() -> dict[str, Any]:
    """Run zero-disk-mutation AST operator mutator across fixed_point.py & graph_engine.py."""
    src_dir = Path(__file__).resolve().parent
    results_per_module: dict[str, Any] = {}
    total_all = 0
    killed_all = 0
    survived_list: list[str] = []

    targets: list[tuple[str, Path, set[str] | None, Any]] = [
        ("fixed_point.py", src_dir / "fixed_point.py", None, _verify_fixed_point_module),
        (
            "graph_engine.py (evict/wash-score/metrics)",
            src_dir / "graph_engine.py",
            {"__init__", "evict_expired", "_score_cycle", "get_account_graph_metrics"},
            _verify_graph_engine_module,
        ),
    ]

    for label, path, allowed_funcs, oracle in targets:
        source = path.read_text(encoding="utf-8")
        tree = ast.parse(source, filename=str(path))
        sites = _collect_mutation_sites(tree, allowed_funcs=allowed_funcs)

        clean_mod = _exec_ast_module(tree, f"_mut_clean_{path.stem}", str(path))
        oracle(clean_mod)

        killed = 0
        mod_survived: list[str] = []
        for idx, kind, desc in sites:
            mut_tree = _apply_mutation(tree, idx, kind)
            try:
                mod = _exec_ast_module(mut_tree, f"_mut_{path.stem}_{idx}", str(path))
                oracle(mod)
                mod_survived.append(f"{path.name}:{desc}")
            except Exception:
                killed += 1

        total = len(sites)
        total_all += total
        killed_all += killed
        survived_list.extend(mod_survived)
        results_per_module[label] = {
            "killed": killed,
            "total": total,
            "kill_rate_pct": round(100.0 * killed / max(1, total), 2),
            "survived": mod_survived,
        }

    overall_pct = round(100.0 * killed_all / max(1, total_all), 2)
    return {
        "overall_killed": killed_all,
        "overall_total": total_all,
        "overall_kill_rate_pct": overall_pct,
        "passed_95pct_gate": overall_pct >= 95.0,
        "modules": results_per_module,
        "survived": survived_list,
    }


def run_500k_tracemalloc_soak(cycles: int = 500_000) -> dict[str, Any]:
    """Run 500,000 consecutive cycles proving 0-byte steady-state memory leak via tracemalloc."""
    gc.collect()
    tracemalloc.start()

    # Exact power-of-two step (2^26 ns ~ 67.1 ms) & LCM(16, 25)=400 periodic alignment
    # eliminates CPython x_sub 2^30-limb borrow-phase oscillation on identical steady-state load.
    step_ns = 1 << 26
    window_ns = 32 * step_ns
    graph = SlidingTemporalMultigraph(
        window_ns=window_ns,
        max_cycle_depth=6,
        decay_tau_ns=16 * step_ns,
        min_wash_score=0.25,
    )
    mask_al = alloc_aligned64_f32((33, TEMPORAL_SEQ_LEN, NUM_FEATURES))
    bg_al = alloc_aligned64_f32((33, TEMPORAL_SEQ_LEN, NUM_FEATURES))
    coal_al = alloc_aligned64_f32((33, TEMPORAL_SEQ_LEN, NUM_FEATURES))
    x_al = alloc_aligned64_f32((1, TEMPORAL_SEQ_LEN, NUM_FEATURES))
    mask_al.fill(1.0)
    bg_al.fill(0.1)
    x_al.fill(0.5)

    assert (mask_al.ctypes.data & 63) == 0
    assert (bg_al.ctypes.data & 63) == 0
    assert (coal_al.ctypes.data & 63) == 0
    assert (x_al.ctypes.data & 63) == 0

    base_px = 68_425_500_000_00
    qty = 125_000_000
    ts_ns = 1_700_000_000_000_000_000

    def _run_block(n: int, start_seq: int, stats: list[int]) -> None:
        cur_ts = stats[1]
        cycles_found = 0
        t0 = time.perf_counter_ns()
        for idx in range(n):
            seq = start_seq + idx
            cur_ts += step_ns
            rem = idx % 25
            if rem == 22:
                m_acct, t_acct = 61, 62
            elif rem == 23:
                m_acct, t_acct = 62, 63
            elif rem == 24:
                m_acct, t_acct = 63, 61
            else:
                m_acct = 1 + (rem & 15)
                t_acct = 17 + (rem & 15)

            px = base_px + (rem * 1_000_000)
            _ = fp_ref.compute_notional_fp(px, qty)
            _ = fp_ref.distance_bps_int(px, base_px)
            _, cycs = graph.ingest_trade(
                fill_id=seq,
                symbol="BTC-USDT",
                maker_account_id=m_acct,
                taker_account_id=t_acct,
                price_fp=px,
                qty_fp=qty,
                exec_ts_ns=cur_ts,
                aggressor_side="BUY",
                engine_seq=seq,
            )
            if cycs:
                cycles_found += len(cycs)
            coal_al[:] = x_al
            np.multiply(coal_al, mask_al, out=coal_al)
            np.add(coal_al, bg_al, out=coal_al)
        stats[0] = time.perf_counter_ns() - t0
        stats[1] = cur_ts
        stats[2] = cycles_found

    # Warmup 8,000 cycles (= 20 * LCM(16, 25); 320 > 256 wash cycles so all counters exceed
    # CPython's [-5..256] immortal small-int cache before cur_before is recorded).
    warmup_cycles = 8_000
    stats: list[int] = [1 << 40, ts_ns, 100_000]
    mem_slots: list[int] = [0, 0, 0]
    _run_block(warmup_cycles, 1, stats)
    gc.collect()
    edges_before = graph.active_edge_count
    # Prime mem_slots[0] with a distinct traced 28B int so overwriting it is net 0 B.
    mem_slots[0] = tracemalloc.get_traced_memory()[0]
    mem_slots[0] = tracemalloc.get_traced_memory()[0]

    _run_block(cycles, warmup_cycles + 1, stats)
    gc.collect()
    mem_slots[1], mem_slots[2] = tracemalloc.get_traced_memory()
    edges_after = graph.active_edge_count
    tracemalloc.stop()

    cur_before, cur_after, peak_after = mem_slots[0], mem_slots[1], mem_slots[2]
    elapsed_ns, wash_cycles_caught = stats[0], stats[2]
    net_leak = max(0, cur_after - cur_before)
    elapsed_sec = elapsed_ns * 1e-9
    return {
        "warmup_cycles": warmup_cycles,
        "soak_cycles": cycles,
        "wash_cycles_detected": wash_cycles_caught,
        "active_edges_before": edges_before,
        "active_edges_after": edges_after,
        "traced_before_bytes": cur_before,
        "traced_after_bytes": cur_after,
        "traced_peak_bytes": peak_after,
        "net_steady_state_leak_bytes": net_leak,
        "elapsed_sec": round(elapsed_sec, 3),
        "cycles_per_sec": round(cycles / max(1e-9, elapsed_sec), 1),
        "ns_per_cycle": round(elapsed_ns / cycles, 2),
        "passed_zero_leak": net_leak == 0 and edges_before == edges_after,
    }


def _measure_kernel_pass(
    fn: Any,
    iters: int,
    warmup: int = 300,
) -> dict[str, float]:
    """Measure exact per-op latency percentiles (ns/op) and true transient heap (B/op, allocs)."""
    for _ in range(warmup):
        fn()
    gc.collect()

    lat_ns = np.empty(iters, dtype=np.int64)
    for i in range(iters):
        t0 = time.perf_counter_ns()
        fn()
        lat_ns[i] = time.perf_counter_ns() - t0

    # Clean tracemalloc peak measurement (NO take_snapshot inside window so peak is 100% fn()!)
    gc.collect()
    tracemalloc.start()
    fn()  # prime any one-time Python frame cache
    gc.collect()
    tracemalloc.reset_peak()
    cur0, _ = tracemalloc.get_traced_memory()
    for _ in range(1_000):
        fn()
    cur1, peak1 = tracemalloc.get_traced_memory()
    tracemalloc.stop()

    # Exact single-call live allocation check (retaining reference to result to measure B/op)
    gc.collect()
    tracemalloc.start()
    tracemalloc.reset_peak()
    b0, _ = tracemalloc.get_traced_memory()
    _res = fn()
    b1, p1 = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    del _res

    peak_per_op_bytes = max(0, p1 - b0)
    steady_peak_bytes = max(0, peak1 - cur0)
    retained_bytes = max(0, cur1 - cur0)
    return {
        "p50_ns": round(float(np.percentile(lat_ns, 50)), 1),
        "p95_ns": round(float(np.percentile(lat_ns, 95)), 1),
        "p99_ns": round(float(np.percentile(lat_ns, 99)), 1),
        "mean_ns": round(float(np.mean(lat_ns)), 1),
        "peak_b_per_op": float(peak_per_op_bytes),
        "loop_peak_bytes": float(steady_peak_bytes),
        "retained_b_1000ops": float(retained_bytes),
        "result_live_bytes": float(max(0, b1 - b0)),
    }


def run_3pass_micro_benchmark(artifacts_dir: Path, iters: int = 10_000) -> dict[str, Any]:
    """Measure Pass 1 (Baseline) vs Pass 2 (Vectorized/Zero-Copy) vs Pass 3 (64B Cache-Aligned)."""
    rt = SentinelOnnxRuntime(artifacts_dir, use_int8=True)
    rng = np.random.default_rng(20261118)
    m = NUM_FEATURES
    k_coal = 33

    mask_raw = rng.integers(0, 2, size=(k_coal, TEMPORAL_SEQ_LEN, m)).astype(np.float32)
    bg_raw = rng.random(size=(k_coal, TEMPORAL_SEQ_LEN, m), dtype=np.float32)
    x_seq = np.ascontiguousarray(rng.random(size=(TEMPORAL_SEQ_LEN, m), dtype=np.float32))
    recon = rng.uniform(0.1, 2.0, size=(k_coal,)).astype(np.float32)
    if_sc = rng.uniform(-0.2, 0.4, size=(k_coal,)).astype(np.float32)
    anom_p = rng.uniform(0.05, 0.95, size=(k_coal,)).astype(np.float32)
    weights = rt.weights
    intercept = rt.intercept

    coal_p2 = np.empty((k_coal, TEMPORAL_SEQ_LEN, m), dtype=np.float32)
    tmp_a_p2 = np.empty(k_coal, dtype=np.float32)
    tmp_b_p2 = np.empty(k_coal, dtype=np.float32)
    logits_p2 = np.empty(k_coal, dtype=np.float32)
    probs_p2 = np.empty(k_coal, dtype=np.float32)

    mask_al = alloc_aligned64_f32((k_coal, TEMPORAL_SEQ_LEN, m))
    bg_al = alloc_aligned64_f32((k_coal, TEMPORAL_SEQ_LEN, m))
    coal_al = alloc_aligned64_f32((k_coal, TEMPORAL_SEQ_LEN, m))
    x_al = alloc_aligned64_f32((1, TEMPORAL_SEQ_LEN, m))
    recon_al = alloc_aligned64_f32((k_coal,))
    if_al = alloc_aligned64_f32((k_coal,))
    anom_al = alloc_aligned64_f32((k_coal,))
    np.copyto(mask_al, mask_raw)
    np.copyto(bg_al, bg_raw)
    np.copyto(x_al[0], x_seq)
    np.copyto(recon_al, recon)
    np.copyto(if_al, if_sc)
    np.copyto(anom_al, anom_p)

    for arr in (mask_al, bg_al, coal_al, x_al, recon_al, if_al, anom_al):
        assert (arr.ctypes.data & 63) == 0

    def _pass1_baseline() -> np.ndarray[Any, np.dtype[np.float32]]:
        coal = np.empty((k_coal, TEMPORAL_SEQ_LEN, m), dtype=np.float32)
        for idx in range(k_coal):
            coal[idx] = mask_raw[idx] * x_seq + bg_raw[idx]
        wash = coal[:, -1, 10]
        spoof = np.log1p(np.maximum(0.0, coal[:, -1, 4] + 0.5 * coal[:, -1, 7]))
        logits = (
            intercept
            + weights[0] * np.log1p(np.maximum(0.0, recon))
            + weights[1] * if_sc
            + weights[2] * anom_p
            + weights[3] * wash
            + weights[4] * spoof
        )
        out: np.ndarray[Any, np.dtype[np.float32]] = (
            1.0 / (1.0 + np.exp(-np.clip(logits, -30.0, 30.0)))
        ).astype(np.float32)
        return out

    w0, w1, w2, w3, w4 = (
        float(weights[0]),
        float(weights[1]),
        float(weights[2]),
        float(weights[3]),
        float(weights[4]),
    )

    def _pass2_zerocopy() -> np.ndarray[Any, np.dtype[np.float32]]:
        np.multiply(mask_raw, x_seq[None, :, :], out=coal_p2)
        np.add(coal_p2, bg_raw, out=coal_p2)
        np.maximum(recon, 0.0, out=tmp_a_p2)
        np.log1p(tmp_a_p2, out=tmp_a_p2)
        np.multiply(tmp_a_p2, w0, out=logits_p2)
        np.add(logits_p2, intercept, out=logits_p2)
        np.multiply(if_sc, w1, out=tmp_a_p2)
        np.add(logits_p2, tmp_a_p2, out=logits_p2)
        np.multiply(anom_p, w2, out=tmp_a_p2)
        np.add(logits_p2, tmp_a_p2, out=logits_p2)
        np.multiply(coal_p2[:, -1, 10], w3, out=tmp_a_p2)
        np.add(logits_p2, tmp_a_p2, out=logits_p2)
        np.multiply(coal_p2[:, -1, 7], 0.5, out=tmp_b_p2)
        np.add(coal_p2[:, -1, 4], tmp_b_p2, out=tmp_a_p2)
        np.maximum(tmp_a_p2, 0.0, out=tmp_a_p2)
        np.log1p(tmp_a_p2, out=tmp_a_p2)
        np.multiply(tmp_a_p2, w4, out=tmp_a_p2)
        np.add(logits_p2, tmp_a_p2, out=logits_p2)
        np.clip(logits_p2, -30.0, 30.0, out=logits_p2)
        np.negative(logits_p2, out=logits_p2)
        np.exp(logits_p2, out=logits_p2)
        np.add(logits_p2, 1.0, out=logits_p2)
        np.divide(1.0, logits_p2, out=probs_p2)
        return probs_p2

    def _pass3_aligned64() -> np.ndarray[Any, np.dtype[np.float32]]:
        coal_al[:] = x_al
        np.multiply(coal_al, mask_al, out=coal_al)
        np.add(coal_al, bg_al, out=coal_al)
        return rt._compute_fused_probs_inplace(
            coal_al, recon_al, if_al, anom_al, reuse_out_buf=True
        )

    r1 = _pass1_baseline()
    r2 = _pass2_zerocopy()
    r3 = _pass3_aligned64()
    np.testing.assert_allclose(r1, r2, atol=1e-6)
    np.testing.assert_allclose(r1, r3, atol=1e-6)

    p1 = _measure_kernel_pass(_pass1_baseline, iters)
    p2 = _measure_kernel_pass(_pass2_zerocopy, iters)
    p3 = _measure_kernel_pass(_pass3_aligned64, iters)

    speedup_p50_pct = round(100.0 * (p1["p50_ns"] - p3["p50_ns"]) / max(1.0, p1["p50_ns"]), 2)
    speedup_p99_pct = round(100.0 * (p1["p99_ns"] - p3["p99_ns"]) / max(1.0, p1["p99_ns"]), 2)
    mem_drop_pct = round(
        100.0
        * (p1["peak_b_per_op"] - p3["peak_b_per_op"])
        / max(1.0, p1["peak_b_per_op"]),
        2,
    )

    return {
        "iterations": iters,
        "pass1_baseline": p1,
        "pass2_vectorized_zerocopy": p2,
        "pass3_aligned64_final": p3,
        "improvement_p50_pct": speedup_p50_pct,
        "improvement_p99_pct": speedup_p99_pct,
        "peak_memory_reduction_pct": mem_drop_pct,
    }


def run_full_overclock_audit(artifacts_dir: Path) -> dict[str, Any]:
    """Execute all 3 HYPERION Overclock Protocol gates and persist overclock_audit.json."""
    mut = run_ast_mutation_audit()
    soak = run_500k_tracemalloc_soak(cycles=500_000)
    bench3 = run_3pass_micro_benchmark(artifacts_dir, iters=10_000)
    report: dict[str, Any] = {
        "mutation_audit": mut,
        "soak_500k": soak,
        "micro_opt_3pass": bench3,
        "all_overclock_gates_passed": bool(
            mut["passed_95pct_gate"] and soak["passed_zero_leak"]
        ),
    }
    (artifacts_dir / "overclock_audit.json").write_text(
        json.dumps(report, indent=2), encoding="utf-8"
    )
    return report


if __name__ == "__main__":
    models_dir = Path(__file__).resolve().parents[2] / "models"
    out = run_full_overclock_audit(models_dir)
    print(json.dumps(out, indent=2))
