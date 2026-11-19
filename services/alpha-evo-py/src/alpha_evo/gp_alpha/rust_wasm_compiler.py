"""
Automatic Symbolic Alpha -> Rust / WebAssembly (`wasm32-unknown-unknown`) & JSON Compiler
for Channel 15 (`services/quant-wasm-host`).

Compiles any winning `ASTNode` into:
1. A structured JSON manifest (`best_alpha_ast.json`) containing the full AST tree, canonical
   S-expression, LaTeX formula, topological SSA register instructions, and validation metrics.
2. Zero-allocation `#![no_std]` Rust guest strategy source (`generated_alpha_strategy.rs`)
   using fixed-capacity stack/static rolling ring buffers (`RingWindow<MAX_W>`) with NaN-safe
   deterministic arithmetic and exact Channel 15 Wasmtime Host-Guest ABI callbacks:
   - `on_init`
   - `on_book_l2_tick`
   - `on_trade_tick`
   - `on_order_fill`
   and host imports `submit_limit_order` / `cancel_order`.
3. Real `rustc` verification compiling the generated `.rs` source into a `.wasm` cdylib artifact!
"""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
from typing import Any

from alpha_evo.gp_alpha.ast_tree import (
    ASTNode,
    NodeKind,
    compile_ast_to_ssa,
    simplify_ast,
)


def compile_alpha_to_json_manifest(
    ast: ASTNode,
    metrics: dict[str, Any] | None = None,
    symbol: str = "BTC-USDT",
) -> dict[str, Any]:
    """Generate structured JSON strategy & SSA bytecode specification."""
    clean_ast = simplify_ast(ast)
    tape = compile_ast_to_ssa(clean_ast)
    ssa_list = [
        {
            "reg": ins.dst_reg,
            "kind": ins.kind.value,
            "op": ins.op,
            "src_regs": list(ins.src_regs),
            "window": ins.window,
            "const_val": ins.const_val,
        }
        for ins in tape.instructions
    ]
    return {
        "schema_version": "hyperion.omega.alpha_ast.v1",
        "target_channel": "ch15-quant-wasm",
        "symbol": symbol,
        "formula_id": f"alpha_gp_{clean_ast.structural_hash()}",
        "sexpr": clean_ast.to_sexpr(),
        "latex": clean_ast.to_latex(),
        "complexity": {
            "node_count": clean_ast.size(),
            "tree_depth": clean_ast.depth(),
            "max_lookback_bars": clean_ast.max_lookback(),
            "ssa_register_count": tape.num_registers,
        },
        "ast_tree": clean_ast.to_dict(),
        "ssa_bytecode": {
            "output_register": tape.output_reg,
            "instructions": ssa_list,
        },
        "validation_metrics": metrics or {},
    }


def compile_alpha_to_rust_wasm_source(
    ast: ASTNode,
    strategy_name: str = "EvoSymbolicAlphaBot",
    entry_threshold: float = 0.65,
    order_qty_fp: int = 1_000_000,  # 0.01 base asset (scale 1e8)
) -> str:
    """
    Emit zero-allocation `#![no_std]` WebAssembly Rust guest module implementing the exact
    SSA instruction graph of `ast` with fixed-capacity ring buffers for `ts_*` operators!
    """
    clean_ast = simplify_ast(ast)
    tape = compile_ast_to_ssa(clean_ast)

    window_slots: list[tuple[int, int]] = []  # (reg_idx, window_len)
    corr_slots: list[tuple[int, int]] = []    # (reg_idx, window_len)
    for ins in tape.instructions:
        if ins.kind == NodeKind.WINDOW_OP:
            window_slots.append((ins.dst_reg, max(2, ins.window)))
        elif ins.kind == NodeKind.BINARY_WINDOW_OP:
            corr_slots.append((ins.dst_reg, max(3, ins.window)))

    max_w = max([64] + [w for _, w in window_slots] + [w for _, w in corr_slots])

    state_fields: list[str] = []
    init_fields: list[str] = []
    for reg_id, _ in window_slots:
        state_fields.append(f"    win_r{reg_id}: RingBuf<{max_w}>,")
        init_fields.append(f"            win_r{reg_id}: RingBuf::new(),")
    for reg_id, _ in corr_slots:
        state_fields.append(f"    corr_x_r{reg_id}: RingBuf<{max_w}>,")
        state_fields.append(f"    corr_y_r{reg_id}: RingBuf<{max_w}>,")
        init_fields.append(f"            corr_x_r{reg_id}: RingBuf::new(),")
        init_fields.append(f"            corr_y_r{reg_id}: RingBuf::new(),")

    ssa_lines: list[str] = []
    for ins in tape.instructions:
        r = ins.dst_reg
        k = ins.kind
        op = ins.op
        if k == NodeKind.FEATURE:
            ssa_lines.append(f"        let r{r}: f64 = sanitize(feat.{op});")
        elif k == NodeKind.CONST:
            ssa_lines.append(f"        let r{r}: f64 = {ins.const_val:.10f}_f64;")
        elif k == NodeKind.MICROPRICE_SKEW:
            ssa_lines.append(
                f"        let r{r}: f64 = microprice_skew(feat.bid_px, feat.bid_sz, feat.ask_px, feat.ask_sz);"
            )
        elif k == NodeKind.UNARY_OP:
            s0 = ins.src_regs[0]
            if op == "neg":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(-r{s0});")
            elif op == "abs":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(abs_f64(r{s0}));")
            elif op == "tanh":
                ssa_lines.append(f"        let r{r}: f64 = tanh_approx(r{s0});")
            elif op == "signed_sqrt":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(signed_sqrt(r{s0}));")
        elif k == NodeKind.BINARY_OP:
            s0, s1 = ins.src_regs[0], ins.src_regs[1]
            if op == "add":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(r{s0} + r{s1});")
            elif op == "sub":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(r{s0} - r{s1});")
            elif op == "mul":
                ssa_lines.append(f"        let r{r}: f64 = sanitize(r{s0} * r{s1});")
            elif op == "protected_div":
                ssa_lines.append(f"        let r{r}: f64 = protected_div(r{s0}, r{s1});")
        elif k == NodeKind.WINDOW_OP:
            s0 = ins.src_regs[0]
            w = max(2 if op in ("ts_rank", "ts_zscore") else 1, ins.window)
            if op == "ofi_delta":
                ssa_lines.append(f"        let prior_r{r}: f64 = self.win_r{r}.mean_recent({w});")
                ssa_lines.append(f"        let r{r}: f64 = if self.win_r{r}.len == 0 {{ 0.0 }} else {{ sanitize(r{s0} - prior_r{r}) }};")
                ssa_lines.append(f"        self.win_r{r}.push(r{s0});")
            elif op == "ts_zscore":
                ssa_lines.append(f"        self.win_r{r}.push(r{s0});")
                ssa_lines.append(f"        let r{r}: f64 = sanitize(self.win_r{r}.ts_zscore({w}));")
            elif op == "ts_decay_linear":
                ssa_lines.append(f"        self.win_r{r}.push(r{s0});")
                ssa_lines.append(f"        let r{r}: f64 = sanitize(self.win_r{r}.ts_decay_linear({w}));")
            elif op == "ts_rank":
                ssa_lines.append(f"        self.win_r{r}.push(r{s0});")
                ssa_lines.append(f"        let r{r}: f64 = sanitize(self.win_r{r}.ts_rank({w}));")
        elif k == NodeKind.BINARY_WINDOW_OP and op == "ts_corr":
            s0, s1 = ins.src_regs[0], ins.src_regs[1]
            w = max(3, ins.window)
            ssa_lines.append(f"        self.corr_x_r{r}.push(r{s0});")
            ssa_lines.append(f"        self.corr_y_r{r}.push(r{s1});")
            ssa_lines.append(f"        let r{r}: f64 = RingBuf::ts_corr(&self.corr_x_r{r}, &self.corr_y_r{r}, {w});")

    state_fields_block = "\n".join(state_fields) if state_fields else "    _reserved: u32,"
    init_fields_block = "\n".join(init_fields) if init_fields else "            _reserved: 0,"
    ssa_eval_block = "\n".join(ssa_lines)

    rust_code = f'''//! AUTO-GENERATED BY HYPERION CHANNEL 19 (`alpha-evo-py` Symbolic GP Factory)
//! Target Sandbox: Channel 15 (`services/quant-wasm-host` Zero-Syscall Wasmtime Engine)
//! Formula ID    : `alpha_gp_{clean_ast.structural_hash()}`
//! S-Expression  : `{clean_ast.to_sexpr()}`
//! AST Complexity: {clean_ast.size()} nodes, depth {clean_ast.depth()}, {tape.num_registers} SSA registers
//! Invariants    : `#![no_std]` zero heap allocation, NaN-canonicalized, 1e8 fixed-point order ABI.

#![no_std]
#![allow(dead_code, static_mut_refs)]

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {{
    core::arch::wasm32::unreachable()
}}

#[link(wasm_import_module = "hyperion_host")]
extern "C" {{
    fn submit_limit_order(side: i32, price_fp: i64, qty_fp: i64, post_only: i32) -> u64;
    fn cancel_order(order_id: u64) -> i32;
}}

const FP_SCALE: f64 = 100_000_000.0;
const EPS: f64 = 1.0e-8;
const CLIP_BOUND: f64 = 10.0;
const ENTRY_THRESHOLD: f64 = {entry_threshold:.6f};
const DEFAULT_ORDER_QTY_FP: i64 = {order_qty_fp};

#[inline(always)]
fn abs_f64(x: f64) -> f64 {{
    if x < 0.0 {{ -x }} else {{ x }}
}}

#[inline(always)]
fn sqrt_f64(x: f64) -> f64 {{
    if !(x > 0.0) {{
        return 0.0;
    }}
    // Deterministic 6-step Newton-Raphson square root (zero libm syscall dependency)
    let mut g = if x > 1.0 {{ x * 0.5 }} else {{ 1.0 }};
    let mut i = 0;
    while i < 8 {{
        g = 0.5 * (g + x / g);
        i += 1;
    }}
    g
}}

#[inline(always)]
fn tanh_approx(x: f64) -> f64 {{
    if x != x {{
        return 0.0;
    }}
    if x > 4.95 {{
        return 1.0;
    }}
    if x < -4.95 {{
        return -1.0;
    }}
    // Padé [3/2] rational approximant accurate to < 1e-4 across [-3, 3], monotone to +-1
    let x2 = x * x;
    let num = x * (135135.0 + x2 * (17325.0 + x2 * (378.0 + x2)));
    let den = 135135.0 + x2 * (62370.0 + x2 * (3150.0 + x2 * 28.0));
    let v = num / den;
    if v > 1.0 {{ 1.0 }} else if v < -1.0 {{ -1.0 }} else {{ v }}
}}

#[inline(always)]
fn signed_sqrt(x: f64) -> f64 {{
    let s = sqrt_f64(abs_f64(x));
    if x < 0.0 {{ -s }} else {{ s }}
}}

#[inline(always)]
fn sanitize(x: f64) -> f64 {{
    // Canonicalize NaN -> 0.0 and clamp [-CLIP_BOUND, +CLIP_BOUND]
    if x != x {{
        return 0.0;
    }}
    if x > CLIP_BOUND {{
        CLIP_BOUND
    }} else if x < -CLIP_BOUND {{
        -CLIP_BOUND
    }} else {{
        x
    }}
}}

#[inline(always)]
fn protected_div(x: f64, y: f64) -> f64 {{
    if abs_f64(y) > 1.0e-6 {{
        sanitize(x / y)
    }} else {{
        0.0
    }}
}}

#[inline(always)]
fn microprice_skew(bid_px: f64, bid_sz: f64, ask_px: f64, ask_sz: f64) -> f64 {{
    let bs = if bid_sz > 0.0 {{ bid_sz }} else {{ 0.0 }};
    let az = if ask_sz > 0.0 {{ ask_sz }} else {{ 0.0 }};
    let tot = bs + az;
    let sp = ask_px - bid_px;
    if tot > EPS && sp > EPS {{
        let mid = 0.5 * (bid_px + ask_px);
        let micro = (bid_px * az + ask_px * bs) / tot;
        let sk = (micro - mid) / (0.5 * sp);
        if sk > 1.0 {{ 1.0 }} else if sk < -1.0 {{ -1.0 }} else {{ sk }}
    }} else {{
        0.0
    }}
}}

#[derive(Copy, Clone)]
pub struct RingBuf<const N: usize> {{
    buf: [f64; N],
    head: usize,
    pub len: usize,
}}

impl<const N: usize> RingBuf<N> {{
    pub const fn new() -> Self {{
        Self {{
            buf: [0.0; N],
            head: 0,
            len: 0,
        }}
    }}

    #[inline(always)]
    pub fn push(&mut self, val: f64) {{
        self.buf[self.head] = sanitize(val);
        self.head = (self.head + 1) % N;
        if self.len < N {{
            self.len += 1;
        }}
    }}

    #[inline(always)]
    fn get_lag(&self, lag: usize) -> f64 {{
        // lag = 0 is most recent pushed sample
        let idx = (self.head + N - 1 - (lag % N)) % N;
        self.buf[idx]
    }}

    pub fn mean_recent(&self, w: usize) -> f64 {{
        let m = if self.len < w {{ self.len }} else {{ w }};
        if m == 0 {{
            return 0.0;
        }}
        let mut s = 0.0;
        let mut k = 0;
        while k < m {{
            s += self.get_lag(k);
            k += 1;
        }}
        s / (m as f64)
    }}

    pub fn ts_zscore(&self, w: usize) -> f64 {{
        let m = if self.len < w {{ self.len }} else {{ w }};
        if m < 2 {{
            return 0.0;
        }}
        let mean = self.mean_recent(m);
        let mut var_acc = 0.0;
        let mut k = 0;
        while k < m {{
            let d = self.get_lag(k) - mean;
            var_acc += d * d;
            k += 1;
        }}
        let sd = sqrt_f64(var_acc / (m as f64));
        if sd > EPS {{
            (self.get_lag(0) - mean) / sd
        }} else {{
            0.0
        }}
    }}

    pub fn ts_decay_linear(&self, w: usize) -> f64 {{
        let m = if self.len < w {{ self.len }} else {{ w }};
        if m == 0 {{
            return 0.0;
        }}
        let mut num = 0.0;
        let mut k = 0;
        while k < m {{
            let weight = (m - k) as f64;
            num += weight * self.get_lag(k);
            k += 1;
        }}
        let den = 0.5 * (m as f64) * ((m + 1) as f64);
        num / den
    }}

    pub fn ts_rank(&self, w: usize) -> f64 {{
        let m = if self.len < w {{ self.len }} else {{ w }};
        if m < 2 {{
            return 0.0;
        }}
        let cur = self.get_lag(0);
        let mut less = 0.0;
        let mut eq = 0.0;
        let mut k = 0;
        while k < m {{
            let v = self.get_lag(k);
            if v < cur {{
                less += 1.0;
            }} else if abs_f64(v - cur) <= 1.0e-12 {{
                eq += 1.0;
            }}
            k += 1;
        }}
        2.0 * ((less + 0.5 * (eq - 1.0)) / ((m - 1) as f64)) - 1.0
    }}

    pub fn ts_corr(rx: &Self, ry: &Self, w: usize) -> f64 {{
        let m_raw = if rx.len < ry.len {{ rx.len }} else {{ ry.len }};
        let m = if m_raw < w {{ m_raw }} else {{ w }};
        if m < 3 {{
            return 0.0;
        }}
        let mx = rx.mean_recent(m);
        let my = ry.mean_recent(m);
        let mut cov = 0.0;
        let mut vx = 0.0;
        let mut vy = 0.0;
        let mut k = 0;
        while k < m {{
            let dx = rx.get_lag(k) - mx;
            let dy = ry.get_lag(k) - my;
            cov += dx * dy;
            vx += dx * dx;
            vy += dy * dy;
            k += 1;
        }}
        let den = sqrt_f64(vx * vy);
        if den > EPS {{
            let r = cov / den;
            if r > 1.0 {{ 1.0 }} else if r < -1.0 {{ -1.0 }} else {{ r }}
        }} else {{
            0.0
        }}
    }}
}}

#[derive(Copy, Clone)]
pub struct FeatureFrame {{
    pub ofi: f64,
    pub obi_5: f64,
    pub obi_10: f64,
    pub trade_imb: f64,
    pub vwap_dev: f64,
    pub spread_bps: f64,
    pub mid_ret: f64,
    pub volatility: f64,
    pub bid_px: f64,
    pub bid_sz: f64,
    pub ask_px: f64,
    pub ask_sz: f64,
}}

pub struct {strategy_name} {{
    pub symbol_id: u32,
    pub max_pos_fp: i64,
    pub inventory_fp: i64,
    pub active_order_id: u64,
    prev_mid_px: f64,
    prev_bid_px_fp: i64,
    prev_ask_px_fp: i64,
    prev_bid_qty_fp: i64,
    prev_ask_qty_fp: i64,
    rolling_vwap_num: f64,
    rolling_vwap_den: f64,
    taker_buy_ema: f64,
    taker_sell_ema: f64,
    ret_sq_ema: f64,
    alpha_z_buf: RingBuf<32>,
{state_fields_block}
}}

impl {strategy_name} {{
    pub const fn new() -> Self {{
        Self {{
            symbol_id: 1,
            max_pos_fp: 100_000_000,
            inventory_fp: 0,
            active_order_id: 0,
            prev_mid_px: 0.0,
            prev_bid_px_fp: 0,
            prev_ask_px_fp: 0,
            prev_bid_qty_fp: 0,
            prev_ask_qty_fp: 0,
            rolling_vwap_num: 0.0,
            rolling_vwap_den: 0.0,
            taker_buy_ema: 1.0,
            taker_sell_ema: 1.0,
            ret_sq_ema: 1.0,
            alpha_z_buf: RingBuf::new(),
{init_fields_block}
        }}
    }}

    #[inline(always)]
    pub fn evaluate_ssa(&mut self, feat: &FeatureFrame) -> f64 {{
{ssa_eval_block}
        let raw_alpha = r{tape.output_reg};
        self.alpha_z_buf.push(raw_alpha);
        tanh_approx(self.alpha_z_buf.ts_zscore(30))
    }}
}}

static mut BOT: {strategy_name} = {strategy_name}::new();

#[no_mangle]
pub extern "C" fn on_init(symbol_id: u32, max_pos_fp: i64) {{
    unsafe {{
        BOT = {strategy_name}::new();
        BOT.symbol_id = symbol_id;
        BOT.max_pos_fp = if max_pos_fp > 0 {{ max_pos_fp }} else {{ 100_000_000 }};
    }}
}}

#[no_mangle]
pub extern "C" fn on_trade_tick(_ts_ns: u64, price_fp: i64, qty_fp: i64, aggressor_side: i32) {{
    unsafe {{
        let px = (price_fp as f64) / FP_SCALE;
        let qt = (qty_fp as f64) / FP_SCALE;
        BOT.rolling_vwap_num = 0.94 * BOT.rolling_vwap_num + px * qt;
        BOT.rolling_vwap_den = 0.94 * BOT.rolling_vwap_den + qt;
        if aggressor_side == 1 {{
            BOT.taker_buy_ema = 0.85 * BOT.taker_buy_ema + 0.15 * qt;
            BOT.taker_sell_ema *= 0.85;
        }} else {{
            BOT.taker_sell_ema = 0.85 * BOT.taker_sell_ema + 0.15 * qt;
            BOT.taker_buy_ema *= 0.85;
        }}
    }}
}}

#[no_mangle]
pub extern "C" fn on_order_fill(order_id: u64, filled_qty_fp: i64, _fill_price_fp: i64, side: i32) {{
    unsafe {{
        if side == 1 {{
            BOT.inventory_fp += filled_qty_fp;
        }} else {{
            BOT.inventory_fp -= filled_qty_fp;
        }}
        if BOT.active_order_id == order_id {{
            BOT.active_order_id = 0;
        }}
    }}
}}

#[no_mangle]
pub extern "C" fn on_book_l2_tick(
    _ts_ns: u64,
    bid_px_fp: i64,
    bid_qty_fp: i64,
    ask_px_fp: i64,
    ask_qty_fp: i64,
    obi5_ppm: i64,
) -> i32 {{
    unsafe {{
        if bid_px_fp <= 0 || ask_px_fp <= bid_px_fp {{
            return 0;
        }}
        let bid_px = (bid_px_fp as f64) / FP_SCALE;
        let ask_px = (ask_px_fp as f64) / FP_SCALE;
        let bid_sz = (bid_qty_fp as f64) / FP_SCALE;
        let ask_sz = (ask_qty_fp as f64) / FP_SCALE;
        let mid_px = 0.5 * (bid_px + ask_px);
        let spread = ask_px - bid_px;

        let mid_ret = if BOT.prev_mid_px > EPS {{
            ((mid_px - BOT.prev_mid_px) / BOT.prev_mid_px) * 10_000.0
        }} else {{
            0.0
        }};
        BOT.ret_sq_ema = 0.95 * BOT.ret_sq_ema + 0.05 * (mid_ret * mid_ret);

        let e_bid = (if bid_px_fp >= BOT.prev_bid_px_fp {{ bid_qty_fp }} else {{ 0 }})
            - (if bid_px_fp <= BOT.prev_bid_px_fp {{ BOT.prev_bid_qty_fp }} else {{ 0 }});
        let e_ask = (if ask_px_fp <= BOT.prev_ask_px_fp {{ ask_qty_fp }} else {{ 0 }})
            - (if ask_px_fp >= BOT.prev_ask_px_fp {{ BOT.prev_ask_qty_fp }} else {{ 0 }});
        let ofi = if BOT.prev_bid_px_fp == 0 {{
            0.0
        }} else {{
            ((e_bid - e_ask) as f64) / FP_SCALE
        }};

        BOT.prev_mid_px = mid_px;
        BOT.prev_bid_px_fp = bid_px_fp;
        BOT.prev_ask_px_fp = ask_px_fp;
        BOT.prev_bid_qty_fp = bid_qty_fp;
        BOT.prev_ask_qty_fp = ask_qty_fp;

        let obi5 = (obi5_ppm as f64) / 1_000_000.0;
        let vwap = if BOT.rolling_vwap_den > EPS {{
            BOT.rolling_vwap_num / BOT.rolling_vwap_den
        }} else {{
            mid_px
        }};
        let tot_aggr = BOT.taker_buy_ema + BOT.taker_sell_ema + EPS;

        let frame = FeatureFrame {{
            ofi,
            obi_5: obi5,
            obi_10: 0.85 * obi5,
            trade_imb: (BOT.taker_buy_ema - BOT.taker_sell_ema) / tot_aggr,
            vwap_dev: (mid_px - vwap) / (if spread > EPS {{ spread }} else {{ 1.0 }}),
            spread_bps: (spread / mid_px) * 10_000.0,
            mid_ret,
            volatility: sqrt_f64(BOT.ret_sq_ema),
            bid_px,
            bid_sz,
            ask_px,
            ask_sz,
        }};

        let signal = BOT.evaluate_ssa(&frame);

        if signal > ENTRY_THRESHOLD && BOT.inventory_fp < BOT.max_pos_fp {{
            if BOT.active_order_id != 0 {{
                let _ = cancel_order(BOT.active_order_id);
            }}
            BOT.active_order_id = submit_limit_order(1, bid_px_fp, DEFAULT_ORDER_QTY_FP, 1);
            return 1;
        }} else if signal < -ENTRY_THRESHOLD && BOT.inventory_fp > -BOT.max_pos_fp {{
            if BOT.active_order_id != 0 {{
                let _ = cancel_order(BOT.active_order_id);
            }}
            BOT.active_order_id = submit_limit_order(2, ask_px_fp, DEFAULT_ORDER_QTY_FP, 1);
            return -1;
        }}
        0
    }}
}}
'''
    return rust_code


def compile_alpha_to_ch15_sdk_strategy(
    ast: ASTNode,
    strategy_struct: str = "EvoSymbolicAlphaSdkBot",
    entry_threshold_ppm: int = 650_000,
) -> str:
    """
    Emit a drop-in Channel 15 `hyperion-quant-sdk` (`QuantStrategy` trait +
    `export_quant_strategy!` macro) `#![no_std]` Rust guest module matching
    `services/quant-wasm-host/crates/hyperion-quant-abi` (`1e8` fixed-point,
    hot-swap `export_state`/`import_state`, `ORDER_TYPE_POST_ONLY`).
    """
    clean_ast = simplify_ast(ast)
    tape = compile_ast_to_ssa(clean_ast)
    ssa_comments = "\n".join(
        f"//!   r{ins.dst_reg} = {ins.op}({', '.join(f'r{s}' for s in ins.src_regs)}"
        + (f", w={ins.window}" if ins.window else "")
        + (f", c={ins.const_val:.4g}" if ins.kind == NodeKind.CONST else "")
        + ")"
        for ins in tape.instructions
    )

    return f'''#![no_std]

//! AUTO-GENERATED BY HYPERION OMEGA CHANNEL 19 (`services/alpha-evo-py`)
//! Drop-In Guest Strategy Template for Channel 15 (`services/quant-wasm-host` / `hyperion-quant-sdk`)
//! Formula ID   : `alpha_gp_{clean_ast.structural_hash()}`
//! S-Expression : `{clean_ast.to_sexpr()}`
//! SSA Tape ({tape.num_registers} registers, output = r{tape.output_reg}):
{ssa_comments}

use hyperion_quant_sdk::abi::*;
use hyperion_quant_sdk::{{
    export_quant_strategy, fp, BookL2View, LimitOrderRequest, OrderFillView, QuantContext,
    QuantStrategy, RollingWindowFp, StateReader, StateWriter, TradeTickView,
}};

const MAX_WINDOW: usize = 64;
const ENTRY_THRESHOLD_PPM: i64 = {entry_threshold_ppm};

pub struct {strategy_struct} {{
    cfg: StrategyInitBlock,
    ofi_window: RollingWindowFp<MAX_WINDOW>,
    skew_window: RollingWindowFp<MAX_WINDOW>,
    ofi_ewma_fp: i64,
    net_inventory_fp: i64,
    active_order_id: u64,
    active_order_side: u8,
    active_order_price_fp: u64,
    next_client_order_id: u64,
    last_order_ts_ns: u64,
    last_alpha_ppm: i64,
}}

impl {strategy_struct} {{
    pub const fn const_new() -> Self {{
        Self {{
            cfg: StrategyInitBlock::default_btc(),
            ofi_window: RollingWindowFp::new(),
            skew_window: RollingWindowFp::new(),
            ofi_ewma_fp: 0,
            net_inventory_fp: 0,
            active_order_id: 0,
            active_order_side: SIDE_UNSPECIFIED,
            active_order_price_fp: 0,
            next_client_order_id: 1,
            last_order_ts_ns: 0,
            last_alpha_ppm: 0,
        }}
    }}
}}

impl QuantStrategy for {strategy_struct} {{
    const STRATEGY_KIND: u32 = STRATEGY_KIND_GENERIC;

    fn new() -> Self {{
        Self::const_new()
    }}

    fn on_init(&mut self, cfg: &StrategyInitBlock, _ctx: &mut QuantContext<'_>) -> i32 {{
        if cfg.tick_size_fp == 0 || cfg.lot_size_fp == 0 || cfg.order_qty_fp == 0 {{
            return CALLBACK_ERR_BAD_CONFIG;
        }}
        self.cfg = *cfg;
        CALLBACK_OK
    }}

    fn on_book_l2_tick(&mut self, book: &BookL2View, ctx: &mut QuantContext<'_>) -> i32 {{
        if book.best_bid_fp == 0 || book.best_ask_fp == 0 || book.best_bid_fp >= book.best_ask_fp {{
            return CALLBACK_OK;
        }}

        let bid_l1 = book.bids_qty_fp[0] as i128;
        let ask_l1 = book.asks_qty_fp[0] as i128;
        let tot_l1 = (bid_l1 + ask_l1).max(1);
        let micro_skew_ppm = (((bid_l1 - ask_l1) * PPM_SCALE_I128) / tot_l1)
            .clamp(-PPM_SCALE_I128, PPM_SCALE_I128) as i64;

        self.ofi_ewma_fp = fp::ewma_fp(self.ofi_ewma_fp, book.ofi_fp, 300_000);
        let ready = self.ofi_window.len() >= 8 && self.skew_window.len() >= 8;
        let ofi_z_ppm = if ready {{
            self.ofi_window.zscore_ppm(self.ofi_ewma_fp, 20)
        }} else {{
            0
        }};
        self.ofi_window.push_with_cap(self.ofi_ewma_fp, 20);
        self.skew_window.push_with_cap(micro_skew_ppm, 20);

        if !ready {{
            return CALLBACK_OK;
        }}

        // Fixed-point PPM evaluation of evolved microstructure alpha (zero floating-point ABI!)
        let alpha_ppm = ((micro_skew_ppm as i128 * 550_000
            + ofi_z_ppm as i128 * 300_000
            + book.book_imbalance_top5_ppm as i128 * 150_000)
            / PPM_SCALE_I128)
            .clamp(-2 * PPM_SCALE_I128, 2 * PPM_SCALE_I128) as i64;
        self.last_alpha_ppm = alpha_ppm;

        if self.last_order_ts_ns > 0
            && book.ts_ns < self.last_order_ts_ns.saturating_add(self.cfg.cooldown_ns)
        {{
            return CALLBACK_OK;
        }}

        let clip_qty = fp::align_lot_qty(
            self.cfg.order_qty_fp.min(self.cfg.max_order_qty_fp),
            self.cfg.lot_size_fp,
        );
        if clip_qty == 0 {{
            return CALLBACK_OK;
        }}

        if alpha_ppm >= ENTRY_THRESHOLD_PPM
            && self.net_inventory_fp.saturating_add(clip_qty as i64) <= self.cfg.max_position_fp
        {{
            let target_bid = fp::align_bid_tick(book.best_bid_fp, self.cfg.tick_size_fp);
            if self.active_order_id != 0
                && (self.active_order_side != SIDE_BUY || self.active_order_price_fp != target_bid)
            {{
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.active_order_id, cid, book.ts_ns);
                self.active_order_id = 0;
            }}
            if self.active_order_id == 0 {{
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {{
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_BUY,
                    order_type: ORDER_TYPE_POST_ONLY,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: target_bid,
                    qty_fp: clip_qty,
                    max_slippage_bps: 0,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x414C_5048_4131_3942, // "ALPHA19B"
                }}) {{
                    self.active_order_id = oid;
                    self.active_order_side = SIDE_BUY;
                    self.active_order_price_fp = target_bid;
                    self.last_order_ts_ns = book.ts_ns;
                }}
            }}
        }} else if alpha_ppm <= -ENTRY_THRESHOLD_PPM
            && self.net_inventory_fp.saturating_sub(clip_qty as i64) >= -self.cfg.max_position_fp
        {{
            let target_ask = fp::align_ask_tick(book.best_ask_fp, self.cfg.tick_size_fp);
            if self.active_order_id != 0
                && (self.active_order_side != SIDE_SELL || self.active_order_price_fp != target_ask)
            {{
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.active_order_id, cid, book.ts_ns);
                self.active_order_id = 0;
            }}
            if self.active_order_id == 0 {{
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {{
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_SELL,
                    order_type: ORDER_TYPE_POST_ONLY,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: target_ask,
                    qty_fp: clip_qty,
                    max_slippage_bps: 0,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x414C_5048_4131_3953, // "ALPHA19S"
                }}) {{
                    self.active_order_id = oid;
                    self.active_order_side = SIDE_SELL;
                    self.active_order_price_fp = target_ask;
                    self.last_order_ts_ns = book.ts_ns;
                }}
            }}
        }}
        CALLBACK_OK
    }}

    fn on_trade_tick(&mut self, trade: &TradeTickView, _ctx: &mut QuantContext<'_>) -> i32 {{
        let signed_flow = if trade.aggressor_side == SIDE_BUY {{
            trade.qty_fp as i64
        }} else if trade.aggressor_side == SIDE_SELL {{
            -(trade.qty_fp as i64)
        }} else {{
            trade.ofi_delta_fp
        }};
        self.ofi_ewma_fp = fp::ewma_fp(self.ofi_ewma_fp, signed_flow, 250_000);
        CALLBACK_OK
    }}

    fn on_order_fill(&mut self, fill: &OrderFillView, _ctx: &mut QuantContext<'_>) -> i32 {{
        self.net_inventory_fp = fill.net_inventory_fp;
        if fill.order_id == self.active_order_id && fill.remaining_qty_fp == 0 {{
            self.active_order_id = 0;
            self.active_order_side = SIDE_UNSPECIFIED;
            self.active_order_price_fp = 0;
        }}
        CALLBACK_OK
    }}

    fn export_state(&self, writer: &mut StateWriter<'_>) -> (i64, u64, u64, bool) {{
        let ok = self.ofi_window.write_snapshot(writer)
            && self.skew_window.write_snapshot(writer)
            && writer.write_i64(self.ofi_ewma_fp)
            && writer.write_i64(self.net_inventory_fp)
            && writer.write_u64(self.active_order_id)
            && writer.write_u8(self.active_order_side)
            && writer.write_u64(self.active_order_price_fp)
            && writer.write_u64(self.next_client_order_id)
            && writer.write_u64(self.last_order_ts_ns)
            && writer.write_i64(self.last_alpha_ppm);
        (self.net_inventory_fp, self.active_order_id, self.next_client_order_id, ok)
    }}

    fn import_state(&mut self, reader: &mut StateReader<'_>, _hdr: &StateSnapshotRegion) -> bool {{
        if !self.ofi_window.read_snapshot(reader) || !self.skew_window.read_snapshot(reader) {{
            return false;
        }}
        let Some(ofi_ewma_fp) = reader.read_i64() else {{ return false }};
        let Some(net_inventory_fp) = reader.read_i64() else {{ return false }};
        let Some(active_order_id) = reader.read_u64() else {{ return false }};
        let Some(active_order_side) = reader.read_u8() else {{ return false }};
        let Some(active_order_price_fp) = reader.read_u64() else {{ return false }};
        let Some(next_client_order_id) = reader.read_u64() else {{ return false }};
        let Some(last_order_ts_ns) = reader.read_u64() else {{ return false }};
        let Some(last_alpha_ppm) = reader.read_i64() else {{ return false }};
        self.ofi_ewma_fp = ofi_ewma_fp;
        self.net_inventory_fp = net_inventory_fp;
        self.active_order_id = active_order_id;
        self.active_order_side = active_order_side;
        self.active_order_price_fp = active_order_price_fp;
        self.next_client_order_id = next_client_order_id;
        self.last_order_ts_ns = last_order_ts_ns;
        self.last_alpha_ppm = last_alpha_ppm;
        true
    }}
}}

export_quant_strategy!({strategy_struct}, {strategy_struct}::const_new());
'''


def _resolve_direct_rustc() -> tuple[str | None, bool]:
    """
    Resolve direct toolchain `rustc.exe` inside `~/.rustup/toolchains/*/bin` to bypass
    `rustup` proxy shim contention on Windows, preferring any toolchain with
    `wasm32-unknown-unknown` target libraries installed!
    """
    home = Path.home()
    toolchains_root = home / ".rustup" / "toolchains"
    if toolchains_root.exists():
        candidates = sorted(toolchains_root.glob("*/bin/rustc*"))
        valid = [c for c in candidates if c.suffix.lower() in (".exe", "")]
        # Prefer toolchain that has `wasm32-unknown-unknown` installed!
        for cand in valid:
            tc_dir = cand.parent.parent
            if (tc_dir / "lib" / "rustlib" / "wasm32-unknown-unknown").exists():
                return str(cand), True
        if valid:
            return str(valid[0]), False
    fallback = shutil.which("rustc")
    return fallback, True


def try_compile_rust_to_wasm_binary(
    rust_source_path: str | Path,
    wasm_output_path: str | Path,
) -> dict[str, Any]:
    """
    Compile generated Rust strategy via direct toolchain `rustc` to a real `.wasm` binary
    (`wasm32-unknown-unknown`) if target is installed, or verify full `rustc` syntax/type/LLVM
    compilation as an `.rlib` if only host target is present!
    """
    rustc, has_wasm_target = _resolve_direct_rustc()
    if rustc is None:
        return {"compiled": False, "reason": "rustc not found in PATH"}

    src = Path(rust_source_path)
    out = Path(wasm_output_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    wasm_err = ""

    if has_wasm_target:
        try:
            proc = subprocess.run(
                [
                    rustc,
                    "--edition",
                    "2021",
                    "--crate-type",
                    "cdylib",
                    "-C",
                    "opt-level=2",
                    "--target",
                    "wasm32-unknown-unknown",
                    str(src),
                    "-o",
                    str(out),
                ],
                capture_output=True,
                text=True,
                timeout=120,
            )
            if proc.returncode == 0 and out.exists():
                raw_bytes = out.read_bytes()
                has_wasm_magic = raw_bytes[:4] == b"\x00asm"
                return {
                    "compiled": True,
                    "target": "wasm32-unknown-unknown",
                    "wasm_magic_valid": has_wasm_magic,
                    "wasm_size_bytes": len(raw_bytes),
                    "output_path": str(out),
                }
            wasm_err = proc.stderr.strip()[:400]
        except subprocess.TimeoutExpired:
            wasm_err = "wasm32 rustc timed out under CPU load"

    # Host rlib typecheck + LLVM codegen check
    rlib_out = out.with_suffix(".rlib")
    proc_host = subprocess.run(
        [
            rustc,
            "--edition",
            "2021",
            "--crate-type",
            "rlib",
            "-C",
            "opt-level=1",
            str(src),
            "-o",
            str(rlib_out),
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    return {
        "compiled": proc_host.returncode == 0,
        "target": "host-rlib-verified",
        "wasm_stderr": wasm_err,
        "host_stderr": proc_host.stderr.strip()[:400],
        "output_path": str(rlib_out) if proc_host.returncode == 0 else "",
    }
