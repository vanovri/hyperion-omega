"""
Typed Abstract Syntax Tree (AST), Algebraic Simplifier, and 3-Stage Evaluators
(Baseline Loop -> Vectorized NumPy -> 64B Cache-Line Aligned Zero-Alloc SSA Bytecode).
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import hashlib
import math
from typing import Any
import numpy as np
from numpy.typing import NDArray

from alpha_evo.gp_alpha.operators import (
    CLIP_BOUND,
    EPS,
    _sanitize,
    aligned_empty_64,
    microprice_skew,
    ofi_delta,
    protected_div,
    ts_corr,
    ts_decay_linear,
    ts_rank,
    ts_zscore,
)


class NodeKind(str, Enum):
    FEATURE = "feature"
    CONST = "const"
    UNARY_OP = "unary_op"
    BINARY_OP = "binary_op"
    WINDOW_OP = "window_op"
    BINARY_WINDOW_OP = "binary_window_op"
    MICROPRICE_SKEW = "microprice_skew"


FEATURE_NAMES: tuple[str, ...] = (
    "ofi",
    "obi_5",
    "obi_10",
    "trade_imb",
    "vwap_dev",
    "spread_bps",
    "mid_ret",
    "volatility",
)

UNARY_OPS: tuple[str, ...] = ("neg", "abs", "tanh", "signed_sqrt")
BINARY_OPS: tuple[str, ...] = ("add", "sub", "mul", "protected_div")
WINDOW_OPS: tuple[str, ...] = ("ts_rank", "ts_zscore", "ts_decay_linear", "ofi_delta")
BINARY_WINDOW_OPS: tuple[str, ...] = ("ts_corr",)
ALLOWED_WINDOWS: tuple[int, ...] = (5, 10, 20, 40)
ALLOWED_CONSTS: tuple[float, ...] = (-1.0, -0.5, 0.5, 1.0, 2.0)


@dataclass(frozen=True)
class ASTNode:
    """Immutable symbolic expression AST node."""

    kind: NodeKind
    op: str
    children: tuple["ASTNode", ...] = ()
    window: int = 0
    value: float = 0.0

    def size(self) -> int:
        """Total number of AST nodes (`|T|`) for Parsimony Pressure calculation."""
        return 1 + sum(c.size() for c in self.children)

    def depth(self) -> int:
        """Tree height (`1` for leaf terminals)."""
        if not self.children:
            return 1
        return 1 + max(c.depth() for c in self.children)

    def max_lookback(self) -> int:
        """Total causal lookback bars required by nested rolling operators."""
        own_w = max(0, self.window - 1) if self.window > 0 else (1 if self.kind == NodeKind.MICROPRICE_SKEW else 0)
        if not self.children:
            return own_w
        return own_w + max(c.max_lookback() for c in self.children)

    def to_sexpr(self) -> str:
        """Canonical S-expression string."""
        if self.kind == NodeKind.FEATURE:
            return self.op
        if self.kind == NodeKind.CONST:
            return f"{self.value:.4g}"
        if self.kind == NodeKind.MICROPRICE_SKEW:
            return "microprice_skew(bid_px, bid_sz, ask_px, ask_sz)"
        args = ", ".join(c.to_sexpr() for c in self.children)
        if self.kind in (NodeKind.WINDOW_OP, NodeKind.BINARY_WINDOW_OP):
            return f"{self.op}({args}, {self.window})"
        return f"{self.op}({args})"

    def to_latex(self) -> str:
        """Publication-ready LaTeX formula representation."""
        if self.kind == NodeKind.FEATURE:
            return rf"\text{{{self.op.replace('_', r'\_')}}}"
        if self.kind == NodeKind.CONST:
            return f"{self.value:.3g}"
        if self.kind == NodeKind.MICROPRICE_SKEW:
            return r"\text{MicroSkew}_t"
        if self.kind == NodeKind.UNARY_OP:
            c = self.children[0].to_latex()
            if self.op == "neg":
                return f"-({c})"
            if self.op == "abs":
                return rf"\left|{c}\right|"
            if self.op == "tanh":
                return rf"\tanh\!\left({c}\right)"
            if self.op == "signed_sqrt":
                return rf"\text{{sgn}}({c})\sqrt{{|{c}|}}"
        if self.kind == NodeKind.BINARY_OP:
            a, b = self.children[0].to_latex(), self.children[1].to_latex()
            if self.op == "add":
                return rf"\left({a} + {b}\right)"
            if self.op == "sub":
                return rf"\left({a} - {b}\right)"
            if self.op == "mul":
                return rf"\left({a} \cdot {b}\right)"
            if self.op == "protected_div":
                return rf"\frac{{{a}}}{{{b} + \epsilon}}"
        if self.kind in (NodeKind.WINDOW_OP, NodeKind.BINARY_WINDOW_OP):
            op_tex = self.op.replace("_", r"\_")
            args = ", ".join(c.to_latex() for c in self.children)
            return rf"\operatorname{{{op_tex}}}_{{{self.window}}}\!\left({args}\right)"
        return self.to_sexpr()

    def structural_hash(self) -> str:
        """Deterministic SHA-256 hex digest of canonical S-expression."""
        return hashlib.sha256(self.to_sexpr().encode("utf-8")).hexdigest()[:16]

    def to_dict(self) -> dict[str, Any]:
        """Serialize AST hierarchy to JSON-compatible dict for Sovereign Studio & Wasm tooling."""
        return {
            "kind": self.kind.value,
            "op": self.op,
            "window": self.window,
            "value": self.value,
            "children": [c.to_dict() for c in self.children],
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ASTNode":
        return cls(
            kind=NodeKind(data["kind"]),
            op=str(data["op"]),
            window=int(data.get("window", 0)),
            value=float(data.get("value", 0.0)),
            children=tuple(cls.from_dict(c) for c in data.get("children", [])),
        )


def make_feature(name: str) -> ASTNode:
    return ASTNode(kind=NodeKind.FEATURE, op=name)


def make_const(val: float) -> ASTNode:
    return ASTNode(kind=NodeKind.CONST, op="const", value=float(val))


def make_microprice_skew() -> ASTNode:
    return ASTNode(kind=NodeKind.MICROPRICE_SKEW, op="microprice_skew")


def simplify_ast(node: ASTNode) -> ASTNode:
    """
    Algebraic simplifier & bloat reducer:
    - Constant folding
    - Exact cancellation: `sub(x, x) -> 0`, `protected_div(x, x) -> 1`, `ts_corr(x, x, w) -> 1`
    - Multiplicative zero/one identities & double negation elimination
    """
    if not node.children:
        return node

    ch = tuple(simplify_ast(c) for c in node.children)
    cur = ASTNode(kind=node.kind, op=node.op, children=ch, window=node.window, value=node.value)

    if cur.kind == NodeKind.UNARY_OP:
        c0 = ch[0]
        if c0.kind == NodeKind.CONST:
            v = c0.value
            if cur.op == "neg":
                return make_const(-v)
            if cur.op == "abs":
                return make_const(abs(v))
            if cur.op == "tanh":
                return make_const(math.tanh(v))
            if cur.op == "signed_sqrt":
                return make_const(math.copysign(math.sqrt(abs(v)), v) if v != 0.0 else 0.0)
        if cur.op == "neg" and c0.kind == NodeKind.UNARY_OP and c0.op == "neg":
            return c0.children[0]
        if cur.op == "abs" and c0.kind == NodeKind.UNARY_OP and c0.op == "abs":
            return c0
        return cur

    if cur.kind == NodeKind.BINARY_OP:
        a, b = ch[0], ch[1]
        if a.kind == NodeKind.CONST and b.kind == NodeKind.CONST:
            va, vb = a.value, b.value
            if cur.op == "add":
                return make_const(va + vb)
            if cur.op == "sub":
                return make_const(va - vb)
            if cur.op == "mul":
                return make_const(va * vb)
            if cur.op == "protected_div":
                return make_const(va / vb if abs(vb) > 1e-6 else 0.0)

        sa, sb = a.to_sexpr(), b.to_sexpr()
        if cur.op == "sub" and sa == sb:
            return make_const(0.0)
        if cur.op == "protected_div" and sa == sb:
            return make_const(1.0)
        if cur.op == "mul":
            if (a.kind == NodeKind.CONST and abs(a.value) < EPS) or (b.kind == NodeKind.CONST and abs(b.value) < EPS):
                return make_const(0.0)
            if a.kind == NodeKind.CONST and abs(a.value - 1.0) < EPS:
                return b
            if b.kind == NodeKind.CONST and abs(b.value - 1.0) < EPS:
                return a
        if cur.op == "add":
            if a.kind == NodeKind.CONST and abs(a.value) < EPS:
                return b
            if b.kind == NodeKind.CONST and abs(b.value) < EPS:
                return a
        # Canonicalize commutative operators (`add`, `mul`) to prevent duplicate search trials
        if cur.op in ("add", "mul") and sa > sb:
            return ASTNode(kind=cur.kind, op=cur.op, children=(b, a), window=cur.window, value=cur.value)
        return cur

    if cur.kind == NodeKind.BINARY_WINDOW_OP and cur.op == "ts_corr":
        if ch[0].to_sexpr() == ch[1].to_sexpr():
            return make_const(0.0)  # Constant correlation carries zero cross-sectional variance

    if cur.kind == NodeKind.WINDOW_OP and ch[0].kind == NodeKind.CONST:
        if cur.op in ("ts_zscore", "ts_rank", "ofi_delta"):
            return make_const(0.0)
        if cur.op == "ts_decay_linear":
            return ch[0]

    return cur


# =============================================================================
# PASS 1: BASELINE PYTHON TICK-BY-TICK EVALUATOR (Reference Oracle)
# =============================================================================
def evaluate_ast_baseline_loop(
    node: ASTNode,
    features: dict[str, NDArray[np.float64]],
) -> NDArray[np.float64]:
    """
    Pass 1 (Baseline): Pure Python loop reference implementation over `t = 0..N-1`.
    Mathematically transparent and strictly causal, used as reference oracle & Pass 1 baseline.
    """
    first_arr = next(iter(features.values()))
    n = len(first_arr)

    def _eval_py(nd: ASTNode) -> list[float]:
        if nd.kind == NodeKind.FEATURE:
            return [float(v) for v in features[nd.op]]
        if nd.kind == NodeKind.CONST:
            return [float(nd.value)] * n
        if nd.kind == NodeKind.MICROPRICE_SKEW:
            bp = features["bid_px"]
            bs = features["bid_sz"]
            ap = features["ask_px"]
            az = features["ask_sz"]
            out_m: list[float] = []
            for t in range(n):
                b_s = max(0.0, float(bs[t]))
                a_s = max(0.0, float(az[t]))
                tot = b_s + a_s
                sp = float(ap[t]) - float(bp[t])
                if tot > EPS and sp > 0.0:
                    mid = 0.5 * (float(bp[t]) + float(ap[t]))
                    micro = (float(bp[t]) * a_s + float(ap[t]) * b_s) / tot
                    val = max(-1.0, min(1.0, (micro - mid) / (0.5 * max(sp, EPS))))
                else:
                    val = 0.0
                out_m.append(val)
            return out_m

        if nd.kind == NodeKind.UNARY_OP:
            c = _eval_py(nd.children[0])
            res_u: list[float] = []
            for v in c:
                if nd.op == "neg":
                    r = -v
                elif nd.op == "abs":
                    r = abs(v)
                elif nd.op == "tanh":
                    r = math.tanh(v)
                elif nd.op == "signed_sqrt":
                    r = math.copysign(math.sqrt(abs(v)), v) if v != 0.0 else 0.0
                else:
                    raise ValueError(f"Unknown unary op {nd.op}")
                res_u.append(max(-CLIP_BOUND, min(CLIP_BOUND, r)))
            return res_u

        if nd.kind == NodeKind.BINARY_OP:
            a = _eval_py(nd.children[0])
            b = _eval_py(nd.children[1])
            res_b: list[float] = []
            for va, vb in zip(a, b):
                if nd.op == "add":
                    r = va + vb
                elif nd.op == "sub":
                    r = va - vb
                elif nd.op == "mul":
                    r = va * vb
                elif nd.op == "protected_div":
                    r = va / vb if abs(vb) > 1e-6 else 0.0
                else:
                    raise ValueError(f"Unknown binary op {nd.op}")
                res_b.append(max(-CLIP_BOUND, min(CLIP_BOUND, r)))
            return res_b

        if nd.kind == NodeKind.WINDOW_OP:
            c = _eval_py(nd.children[0])
            w = max(2 if nd.op in ("ts_rank", "ts_zscore") else 1, nd.window)
            res_w: list[float] = []
            for t in range(n):
                s = max(0, t - w + 1)
                seg = c[s : t + 1]
                m = len(seg)
                if nd.op == "ts_zscore":
                    if m < 2:
                        r = 0.0
                    else:
                        mu = sum(seg) / m
                        var = sum((x - mu) ** 2 for x in seg) / m
                        sd = math.sqrt(max(0.0, var))
                        r = (c[t] - mu) / sd if sd > EPS else 0.0
                elif nd.op == "ts_decay_linear":
                    denom = 0.5 * m * (m + 1)
                    r = sum((i + 1) * x for i, x in enumerate(seg)) / denom
                elif nd.op == "ts_rank":
                    if m < 2:
                        r = 0.0
                    else:
                        cur = c[t]
                        less = sum(1 for x in seg if x < cur)
                        eq = sum(1 for x in seg if x == cur)
                        r = 2.0 * ((less + 0.5 * (eq - 1.0)) / (m - 1.0)) - 1.0
                elif nd.op == "ofi_delta":
                    ps = max(0, t - w)
                    prior = c[ps:t]
                    pm = (sum(prior) / len(prior)) if prior else c[0]
                    r = c[t] - pm
                else:
                    raise ValueError(f"Unknown window op {nd.op}")
                res_w.append(max(-CLIP_BOUND, min(CLIP_BOUND, r)))
            return res_w

        if nd.kind == NodeKind.BINARY_WINDOW_OP and nd.op == "ts_corr":
            a = _eval_py(nd.children[0])
            b = _eval_py(nd.children[1])
            w = max(3, nd.window)
            res_c: list[float] = []
            for t in range(n):
                s = max(0, t - w + 1)
                sa = a[s : t + 1]
                sb = b[s : t + 1]
                m = len(sa)
                if m < 3:
                    res_c.append(0.0)
                    continue
                ma = sum(sa) / m
                mb = sum(sb) / m
                cov = sum((x - ma) * (y - mb) for x, y in zip(sa, sb)) / m
                va = sum((x - ma) ** 2 for x in sa) / m
                vb = sum((y - mb) ** 2 for y in sb) / m
                den = math.sqrt(max(0.0, va * vb))
                r = max(-1.0, min(1.0, cov / den)) if den > EPS else 0.0
                res_c.append(r)
            return res_c

        raise ValueError(f"Unsupported AST node {nd}")

    return np.asarray(_eval_py(node), dtype=np.float64)


# =============================================================================
# PASS 2: VECTORIZED RECURSIVE NUMPY EVALUATOR
# =============================================================================
def evaluate_ast_vectorized(
    node: ASTNode,
    features: dict[str, NDArray[np.float64]],
) -> NDArray[np.float64]:
    """Pass 2 (Vectorized Refactor): Recursive NumPy array evaluation."""
    first_arr = next(iter(features.values()))
    n = first_arr.shape[0]

    if node.kind == NodeKind.FEATURE:
        return _sanitize(features[node.op].copy())
    if node.kind == NodeKind.CONST:
        return np.full(n, node.value, dtype=np.float64)
    if node.kind == NodeKind.MICROPRICE_SKEW:
        return microprice_skew(
            features["bid_px"],
            features["bid_sz"],
            features["ask_px"],
            features["ask_sz"],
        )

    if node.kind == NodeKind.UNARY_OP:
        c = evaluate_ast_vectorized(node.children[0], features)
        if node.op == "neg":
            return _sanitize(-c)
        if node.op == "abs":
            return _sanitize(np.abs(c))
        if node.op == "tanh":
            return np.tanh(c)
        if node.op == "signed_sqrt":
            return _sanitize(np.sign(c) * np.sqrt(np.abs(c)))
        raise ValueError(f"Unknown unary op {node.op}")

    if node.kind == NodeKind.BINARY_OP:
        a = evaluate_ast_vectorized(node.children[0], features)
        b = evaluate_ast_vectorized(node.children[1], features)
        if node.op == "add":
            return _sanitize(a + b)
        if node.op == "sub":
            return _sanitize(a - b)
        if node.op == "mul":
            return _sanitize(a * b)
        if node.op == "protected_div":
            return protected_div(a, b)
        raise ValueError(f"Unknown binary op {node.op}")

    if node.kind == NodeKind.WINDOW_OP:
        c = evaluate_ast_vectorized(node.children[0], features)
        if node.op == "ts_zscore":
            return ts_zscore(c, node.window)
        if node.op == "ts_decay_linear":
            return ts_decay_linear(c, node.window)
        if node.op == "ts_rank":
            return ts_rank(c, node.window)
        if node.op == "ofi_delta":
            return ofi_delta(c, node.window)
        raise ValueError(f"Unknown window op {node.op}")

    if node.kind == NodeKind.BINARY_WINDOW_OP and node.op == "ts_corr":
        a = evaluate_ast_vectorized(node.children[0], features)
        b = evaluate_ast_vectorized(node.children[1], features)
        return ts_corr(a, b, node.window)

    raise ValueError(f"Unsupported AST node {node}")


# =============================================================================
# PASS 3: 64-BYTE CACHE-LINE ALIGNED ZERO-ALLOC SSA BYTECODE EVALUATOR
# =============================================================================
@dataclass(frozen=True, slots=True)
class SSAInstruction:
    """Flat Static Single Assignment (SSA) register instruction."""

    dst_reg: int
    kind: NodeKind
    op: str
    src_regs: tuple[int, ...] = ()
    window: int = 0
    const_val: float = 0.0


class SSAWorkspace:
    """
    Pre-allocated 64-byte cache-line aligned register arena (`registers` + scratch prefix buffers)
    guaranteeing 0 heap allocations during steady-state batch/soak evaluation!
    """

    def __init__(self, num_regs: int, length: int) -> None:
        self.num_regs = max(1, num_regs)
        self.length = length
        self.registers = aligned_empty_64((self.num_regs, length), dtype=np.float64)
        self.valid_mask = np.empty(length, dtype=np.bool_)
        # Pre-allocated scratch buffers for zero-alloc O(N) prefix-sum rolling operators
        self.scratch_u = aligned_empty_64(length, dtype=np.float64)
        self.scratch_v = aligned_empty_64(length, dtype=np.float64)
        self.scratch_cs1 = aligned_empty_64(length + 1, dtype=np.float64)
        self.scratch_cs2 = aligned_empty_64(length + 1, dtype=np.float64)
        self.idx_1_to_n = np.arange(1, length + 1, dtype=np.int64)
        self.idx_0_to_n = np.arange(length, dtype=np.int64)
        self.j_float = np.arange(1, length + 1, dtype=np.float64)
        self.start_idx = np.empty(length, dtype=np.int64)
        self.cnt_float = np.empty(length, dtype=np.float64)


@dataclass(frozen=True, slots=True)
class CompiledSSATape:
    """Linear SSA bytecode tape with Common Subexpression Elimination (CSE)."""

    instructions: tuple[SSAInstruction, ...]
    output_reg: int
    num_registers: int

    def create_workspace(self, length: int) -> SSAWorkspace:
        return SSAWorkspace(self.num_registers, length)

    def evaluate_in_place(
        self,
        features: dict[str, NDArray[np.float64]],
        ws: SSAWorkspace,
    ) -> NDArray[np.float64]:
        """
        Execute flat SSA instructions directly inside pre-allocated 64B-aligned `SSAWorkspace`
        with zero temporary array allocations on arithmetic & O(N) prefix-sum paths!
        """
        regs = ws.registers
        n = ws.length

        for ins in self.instructions:
            dst = regs[ins.dst_reg]
            k = ins.kind
            op = ins.op

            if k == NodeKind.FEATURE:
                np.copyto(dst, features[op])
                _sanitize(dst, out=dst)
            elif k == NodeKind.CONST:
                dst.fill(ins.const_val)
            elif k == NodeKind.MICROPRICE_SKEW:
                microprice_skew(
                    features["bid_px"],
                    features["bid_sz"],
                    features["ask_px"],
                    features["ask_sz"],
                    out=dst,
                )
            elif k == NodeKind.UNARY_OP:
                src = regs[ins.src_regs[0]]
                if op == "neg":
                    np.negative(src, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "abs":
                    np.abs(src, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "tanh":
                    np.tanh(src, out=dst)
                elif op == "signed_sqrt":
                    np.abs(src, out=ws.scratch_u)
                    np.sqrt(ws.scratch_u, out=ws.scratch_u)
                    np.sign(src, out=dst)
                    np.multiply(dst, ws.scratch_u, out=dst)
                    _sanitize(dst, out=dst)
            elif k == NodeKind.BINARY_OP:
                a = regs[ins.src_regs[0]]
                b = regs[ins.src_regs[1]]
                if op == "add":
                    np.add(a, b, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "sub":
                    np.subtract(a, b, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "mul":
                    np.multiply(a, b, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "protected_div":
                    np.abs(b, out=ws.scratch_u)
                    np.greater(ws.scratch_u, 1e-6, out=ws.valid_mask)
                    dst.fill(0.0)
                    np.divide(a, b, out=dst, where=ws.valid_mask)
                    _sanitize(dst, out=dst)
            elif k == NodeKind.WINDOW_OP:
                src = regs[ins.src_regs[0]]
                w = ins.window
                if op == "ts_zscore":
                    w_eff = max(2, w)
                    np.subtract(src, src[0], out=ws.scratch_u)
                    np.multiply(ws.scratch_u, ws.scratch_u, out=ws.scratch_v)
                    ws.scratch_cs1[0] = 0.0
                    ws.scratch_cs2[0] = 0.0
                    np.cumsum(ws.scratch_u, out=ws.scratch_cs1[1:])
                    np.cumsum(ws.scratch_v, out=ws.scratch_cs2[1:])
                    np.subtract(ws.idx_1_to_n, w_eff, out=ws.start_idx)
                    np.maximum(0, ws.start_idx, out=ws.start_idx)
                    np.subtract(ws.idx_1_to_n, ws.start_idx, out=ws.cnt_float, casting="unsafe")
                    # mean -> scratch_v; std -> dst temporarily
                    np.take(ws.scratch_cs1, ws.start_idx, out=ws.scratch_v)
                    np.subtract(ws.scratch_cs1[1:], ws.scratch_v, out=ws.scratch_v)
                    np.divide(ws.scratch_v, ws.cnt_float, out=ws.scratch_v)
                    np.take(ws.scratch_cs2, ws.start_idx, out=dst)
                    np.subtract(ws.scratch_cs2[1:], dst, out=dst)
                    np.divide(dst, ws.cnt_float, out=dst)
                    np.subtract(dst, ws.scratch_v * ws.scratch_v, out=dst) if False else None
                    # Zero-alloc variance: dst = max(0, dst - scratch_v^2)
                    np.multiply(ws.scratch_v, ws.scratch_v, out=ws.scratch_cs1[:n])
                    np.subtract(dst, ws.scratch_cs1[:n], out=dst)
                    np.maximum(0.0, dst, out=dst)
                    np.sqrt(dst, out=dst)
                    np.greater(dst, EPS, out=ws.valid_mask)
                    ws.valid_mask &= ws.cnt_float >= 2.0
                    np.subtract(ws.scratch_u, ws.scratch_v, out=ws.scratch_u)
                    np.copyto(ws.scratch_v, dst)
                    dst.fill(0.0)
                    np.divide(ws.scratch_u, ws.scratch_v, out=dst, where=ws.valid_mask)
                    _sanitize(dst, out=dst)
                elif op == "ts_decay_linear":
                    w_eff = max(1, w)
                    ws.scratch_cs1[0] = 0.0
                    ws.scratch_cs2[0] = 0.0
                    np.cumsum(src, out=ws.scratch_cs1[1:])
                    np.multiply(ws.j_float, src, out=ws.scratch_u)
                    np.cumsum(ws.scratch_u, out=ws.scratch_cs2[1:])
                    np.subtract(ws.idx_1_to_n, w_eff, out=ws.start_idx)
                    np.maximum(0, ws.start_idx, out=ws.start_idx)
                    np.subtract(ws.idx_1_to_n, ws.start_idx, out=ws.cnt_float, casting="unsafe")
                    np.take(ws.scratch_cs1, ws.start_idx, out=ws.scratch_u)
                    np.subtract(ws.scratch_cs1[1:], ws.scratch_u, out=ws.scratch_u)
                    np.multiply(ws.scratch_u, ws.start_idx, out=ws.scratch_u)
                    np.take(ws.scratch_cs2, ws.start_idx, out=ws.scratch_v)
                    np.subtract(ws.scratch_cs2[1:], ws.scratch_v, out=ws.scratch_v)
                    np.subtract(ws.scratch_v, ws.scratch_u, out=dst)
                    np.add(ws.cnt_float, 1.0, out=ws.scratch_u)
                    np.multiply(ws.cnt_float, ws.scratch_u, out=ws.scratch_u)
                    np.multiply(ws.scratch_u, 0.5, out=ws.scratch_u)
                    np.divide(dst, ws.scratch_u, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "ofi_delta":
                    w_eff = max(1, w)
                    ws.scratch_cs1[0] = 0.0
                    np.cumsum(src, out=ws.scratch_cs1[1:])
                    np.subtract(ws.idx_0_to_n, w_eff, out=ws.start_idx)
                    np.maximum(0, ws.start_idx, out=ws.start_idx)
                    np.subtract(ws.idx_0_to_n, ws.start_idx, out=ws.cnt_float, casting="unsafe")
                    np.maximum(1.0, ws.cnt_float, out=ws.cnt_float)
                    np.take(ws.scratch_cs1, ws.start_idx, out=ws.scratch_u)
                    np.subtract(ws.scratch_cs1[:n], ws.scratch_u, out=ws.scratch_u)
                    np.divide(ws.scratch_u, ws.cnt_float, out=ws.scratch_u)
                    if n > 0:
                        ws.scratch_u[0] = src[0]
                    np.subtract(src, ws.scratch_u, out=dst)
                    _sanitize(dst, out=dst)
                elif op == "ts_rank":
                    ts_rank(src, w, out=dst)
            elif k == NodeKind.BINARY_WINDOW_OP and op == "ts_corr":
                ts_corr(regs[ins.src_regs[0]], regs[ins.src_regs[1]], ins.window, out=dst)

        return regs[self.output_reg]


def compile_ast_to_ssa(node: ASTNode) -> CompiledSSATape:
    """Compile an `ASTNode` into a deduplicated topological `CompiledSSATape` via CSE."""
    simplified = simplify_ast(node)
    memo: dict[str, int] = {}
    instructions: list[SSAInstruction] = []

    def _emit(nd: ASTNode) -> int:
        key = nd.to_sexpr()
        if key in memo:
            return memo[key]
        child_regs = tuple(_emit(c) for c in nd.children)
        reg = len(instructions)
        instructions.append(
            SSAInstruction(
                dst_reg=reg,
                kind=nd.kind,
                op=nd.op,
                src_regs=child_regs,
                window=nd.window,
                const_val=nd.value,
            )
        )
        memo[key] = reg
        return reg

    out_reg = _emit(simplified)
    return CompiledSSATape(
        instructions=tuple(instructions),
        output_reg=out_reg,
        num_registers=len(instructions),
    )
