"""
Vectorized Limit Order Book Gymnasium Environment & Realistic Execution Simulator.

Models every realistic institutional execution friction required by Channel 19:
1. FIFO Price-Time Queue Position Tracking (`queue_ahead_fp`):
   - Resting maker orders join the back of the L2 queue (`queue_ahead_fp = level_resting_qty_fp`).
   - Amending price or increasing size loses priority (`orderbook.proto` L86); reducing size
     at the same price tick strictly preserves `queue_ahead_fp`.
   - Passive order cancellations deplete `queue_ahead_fp` proportionally to front-queue share.
   - Aggressive market flow depletes `queue_ahead_fp` FIRST before filling `own_qty_fp`.
2. Post-Only Crossing Rejection (`ORDER_TYPE_POST_ONLY` -> `REJECT_REASON_POST_ONLY_WOULD_CROSS`):
   - Delayed maker quotes arriving at or across the opposite best quote are deterministically
     rejected instead of receiving counterfactual instantaneous maker fills.
3. Log-Normal Wire & Matching Latency Ring Buffer:
   - Order commands sample `tau_ms ~ LogNormal(latency_log_mean, latency_log_std)` and wait inside
     a vectorized in-flight ring buffer until `arrival_step = step + latency_steps`.
   - Exposes stale resting quotes to genuine adverse selection ("pick-off risk").
4. Exact Channel 01 / Channel 02 Fixed-Point Maker/Taker Fees (`1e8` scale & `ppm` trunc-div):
   - Double-entry verified via `settle_fill_double_entry_vec`.
5. Almgren-Chriss Permanent & Temporary Market Impact:
   - Aggressive inventory-clearing IOC orders walk L2 book depth, incur temporary impact
     `I_temp(t) = rho * I_temp(t-1) + eta * sign(q) * (|q| / V_ref)^beta`, and permanently
     shift the mid-price by Kyle's lambda `gamma * (q / V_ref)`.
6. Multi-Agent Market-Making Competition:
   - Competing adaptive Avellaneda-Stoikov market maker co-quotes in the same book, competing
     for queue priority and spread capture.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable
import gymnasium as gym
from gymnasium import spaces
import numpy as np
from numpy.typing import NDArray

from alpha_evo.fixed_point import (
    CANONICAL_INSTRUMENTS,
    DEFAULT_MAKER_FEE_PPM,
    DEFAULT_TAKER_FEE_PPM,
    FP_SCALE,
    settle_fill_double_entry_vec,
)

OBS_DIM: int = 14
ACT_DIM: int = 4

# Command opcodes in latency ring buffer
CMD_NONE: int = 0
CMD_QUOTE_UPDATE: int = 1
CMD_CANCEL_ALL: int = 2


@dataclass(slots=True)
class LOBEnvConfig:
    """Configuration for VectorizedLOBEnv & HyperionLOBGymEnv."""

    symbol: str = "BTC-USDT"
    num_envs: int = 64
    episode_steps: int = 256
    step_ms: float = 50.0
    # Latency distribution: tau_ms ~ LogNormal(mu, sigma); median = exp(mu) ~ 3.0 ms
    latency_log_mean: float = 1.10        # exp(1.10) ~= 3.00 ms (~1 to 6 ticks at 50ms or tighter at 2ms)
    latency_log_std: float = 0.65         # Heavy right tail spikes (p99 > 13.6 ms)
    latency_tick_ms: float = 2.0          # Granular latency resolution (steps = ceil(tau_ms / latency_tick_ms))
    min_latency_steps: int = 1
    max_latency_steps: int = 12
    # Channel 02 exact fee schedule (signed ppm, scale 1e6)
    maker_fee_ppm: int = DEFAULT_MAKER_FEE_PPM
    taker_fee_ppm: int = DEFAULT_TAKER_FEE_PPM
    post_only: bool = True
    # Order & inventory sizing (in lots; 1 lot BTC-USDT = 0.00001 BTC = 1,000 fp)
    base_quote_lots: int = 10_000         # 0.10 BTC base quote unit (~$6,850 notional)
    max_inventory_lots: int = 80_000      # 0.80 BTC hard risk cap
    initial_target_exec_lots: int = 0     # Optional Almgren-Chriss parent order inventory to unwind
    base_book_depth_lots: int = 25_000    # 0.25 BTC resting L2 top-of-book depth per tick
    base_spread_ticks: int = 20           # $0.20 (~0.29 bps realistic BTC-USDT touch spread)
    base_sigma_ticks: float = 16.0        # $0.16 per 50ms tick
    # Market impact parameters (Almgren-Chriss + Obizhaeva-Wang transient resilience)
    temp_impact_eta_ticks: float = 4.5
    temp_impact_beta: float = 0.65
    temp_impact_resilience: float = 0.72
    perm_impact_gamma_ticks: float = 8.0
    # Avellaneda-Stoikov + Almgren-Chriss + Tail-Drawdown reward weights (USDT-calibrated)
    inv_risk_aversion_gamma: float = 0.22
    ac_exec_urgency_eta: float = 0.08
    tail_drawdown_threshold_quote: float = 1.5
    tail_drawdown_lambda: float = 0.35
    terminal_inv_penalty_kappa: float = 0.45
    enable_competing_mm: bool = True
    seed: int = 42


class VectorizedLOBEnv:
    """
    High-throughput batched Limit Order Book & Optimal Execution simulator (`B` parallel envs).
    """

    def __init__(
        self,
        cfg: LOBEnvConfig | None = None,
        alpha_signal_fn: Callable[[dict[str, NDArray[np.float64]]], NDArray[np.float64]] | None = None,
    ) -> None:
        self.cfg = cfg or LOBEnvConfig()
        self.spec = CANONICAL_INSTRUMENTS.get(self.cfg.symbol, CANONICAL_INSTRUMENTS["BTC-USDT"])
        self.B = int(self.cfg.num_envs)
        self.tick_fp = int(self.spec.tick_size_fp)
        self.lot_fp = int(self.spec.lot_size_fp)
        self.base_quote_fp = int(self.cfg.base_quote_lots * self.lot_fp)
        self.max_inv_fp = int(self.cfg.max_inventory_lots * self.lot_fp)
        self.base_depth_fp = int(self.cfg.base_book_depth_lots * self.lot_fp)
        self.ring_len = int(self.cfg.max_latency_steps + 4)
        self.alpha_signal_fn = alpha_signal_fn

        self.rng = np.random.default_rng(self.cfg.seed)
        self._allocate_state()

    def _allocate_state(self) -> None:
        B = self.B
        R = self.ring_len

        self.step_idx = np.zeros(B, dtype=np.int64)
        self.mid_ticks = np.zeros(B, dtype=np.int64)
        self.spread_ticks = np.zeros(B, dtype=np.int64)
        self.best_bid_fp = np.zeros(B, dtype=np.int64)
        self.best_ask_fp = np.zeros(B, dtype=np.int64)

        self.l1_bid_depth_fp = np.zeros(B, dtype=np.int64)
        self.l1_ask_depth_fp = np.zeros(B, dtype=np.int64)
        self.obi_5 = np.zeros(B, dtype=np.float64)
        self.ofi_ema = np.zeros(B, dtype=np.float64)
        self.ofi_z = np.zeros(B, dtype=np.float64)
        self.ofi_mean = np.zeros(B, dtype=np.float64)
        self.ofi_var = np.ones(B, dtype=np.float64)
        self.sigma_ticks = np.ones(B, dtype=np.float64)
        self.latent_imb = np.zeros(B, dtype=np.float64)
        self.alpha_signal = np.zeros(B, dtype=np.float64)
        self.kyles_lambda_regime = np.ones(B, dtype=np.float64)
        self.temp_impact_ticks = np.zeros(B, dtype=np.float64)

        # Agent 0 (Primary RL Agent) fixed-point accounting (`1e8` scale)
        self.cash_fp = np.zeros(B, dtype=np.int64)
        self.inventory_fp = np.zeros(B, dtype=np.int64)
        self.treasury_fee_fp = np.zeros(B, dtype=np.int64)
        self.initial_equity_quote = np.zeros(B, dtype=np.float64)
        self.peak_equity_quote = np.zeros(B, dtype=np.float64)
        self.prev_equity_quote = np.zeros(B, dtype=np.float64)
        self.drawdown_quote = np.zeros(B, dtype=np.float64)

        # Active resting orders + FIFO queue ahead tracking!
        self.bid_active = np.zeros(B, dtype=np.bool_)
        self.bid_price_fp = np.zeros(B, dtype=np.int64)
        self.bid_qty_fp = np.zeros(B, dtype=np.int64)
        self.bid_queue_ahead_fp = np.zeros(B, dtype=np.int64)

        self.ask_active = np.zeros(B, dtype=np.bool_)
        self.ask_price_fp = np.zeros(B, dtype=np.int64)
        self.ask_qty_fp = np.zeros(B, dtype=np.int64)
        self.ask_queue_ahead_fp = np.zeros(B, dtype=np.int64)

        # Secondary competing market maker inventory & quotes
        self.comp_inv_fp = np.zeros(B, dtype=np.int64)
        self.comp_bid_fp = np.zeros(B, dtype=np.int64)
        self.comp_ask_fp = np.zeros(B, dtype=np.int64)
        self.comp_qty_fp = np.zeros(B, dtype=np.int64)

        # Vectorized Latency Ring Buffer: allow up to 2 queued commands per arrival slot per env
        self.ring_cmd = np.zeros((R, B, 2), dtype=np.int64)
        self.ring_bid_px_fp = np.zeros((R, B, 2), dtype=np.int64)
        self.ring_ask_px_fp = np.zeros((R, B, 2), dtype=np.int64)
        self.ring_bid_qty_fp = np.zeros((R, B, 2), dtype=np.int64)
        self.ring_ask_qty_fp = np.zeros((R, B, 2), dtype=np.int64)
        self.pending_cmd_count = np.zeros(B, dtype=np.int64)

        # Telemetry counters for institutional audit
        self.maker_fills_count = np.zeros(B, dtype=np.int64)
        self.taker_fills_count = np.zeros(B, dtype=np.int64)
        self.post_only_rejects = np.zeros(B, dtype=np.int64)
        self.cumulative_maker_vol_fp = np.zeros(B, dtype=np.int64)
        self.cumulative_taker_vol_fp = np.zeros(B, dtype=np.int64)
        self.cumulative_fees_fp = np.zeros(B, dtype=np.int64)
        self.double_entry_residual_fp = np.zeros(B, dtype=np.int64)

    def reset(self, seed: int | None = None) -> tuple[NDArray[np.float32], dict[str, Any]]:
        if seed is not None:
            self.rng = np.random.default_rng(seed)

        B = self.B
        ref_ticks = self.spec.reference_price_fp // self.tick_fp

        self.step_idx.fill(0)
        self.mid_ticks[:] = ref_ticks + self.rng.integers(-50, 51, size=B, dtype=np.int64)
        self.spread_ticks.fill(self.cfg.base_spread_ticks)
        self._sync_bbo_from_mid()

        self.l1_bid_depth_fp[:] = self.base_depth_fp
        self.l1_ask_depth_fp[:] = self.base_depth_fp
        self.obi_5.fill(0.0)
        self.ofi_ema.fill(0.0)
        self.ofi_z.fill(0.0)
        self.ofi_mean.fill(0.0)
        self.ofi_var.fill(1.0)
        self.sigma_ticks.fill(self.cfg.base_sigma_ticks)
        self.latent_imb[:] = self.rng.normal(0.0, 0.25, size=B)
        self.alpha_signal.fill(0.0)
        self.kyles_lambda_regime.fill(1.0)
        self.temp_impact_ticks.fill(0.0)

        init_inv = int(self.cfg.initial_target_exec_lots * self.lot_fp)
        self.cash_fp.fill(0)
        self.inventory_fp.fill(init_inv)
        self.treasury_fee_fp.fill(0)

        eq0 = self._compute_equity_quote()
        self.initial_equity_quote[:] = eq0
        self.peak_equity_quote[:] = eq0
        self.prev_equity_quote[:] = eq0
        self.drawdown_quote.fill(0.0)

        self.bid_active.fill(False)
        self.bid_price_fp.fill(0)
        self.bid_qty_fp.fill(0)
        self.bid_queue_ahead_fp.fill(0)

        self.ask_active.fill(False)
        self.ask_price_fp.fill(0)
        self.ask_qty_fp.fill(0)
        self.ask_queue_ahead_fp.fill(0)

        self.comp_inv_fp.fill(0)
        self.comp_bid_fp[:] = self.best_bid_fp - self.tick_fp
        self.comp_ask_fp[:] = self.best_ask_fp + self.tick_fp
        self.comp_qty_fp.fill(self.base_quote_fp)

        self.ring_cmd.fill(CMD_NONE)
        self.ring_bid_px_fp.fill(0)
        self.ring_ask_px_fp.fill(0)
        self.ring_bid_qty_fp.fill(0)
        self.ring_ask_qty_fp.fill(0)
        self.pending_cmd_count.fill(0)

        self.maker_fills_count.fill(0)
        self.taker_fills_count.fill(0)
        self.post_only_rejects.fill(0)
        self.cumulative_maker_vol_fp.fill(0)
        self.cumulative_taker_vol_fp.fill(0)
        self.cumulative_fees_fp.fill(0)
        self.double_entry_residual_fp.fill(0)

        return self._build_obs(), {"double_entry_balanced": True}

    def _sync_bbo_from_mid(self) -> None:
        half_sp = (self.spread_ticks // 2) * self.tick_fp
        self.best_bid_fp[:] = self.mid_ticks * self.tick_fp - half_sp
        self.best_ask_fp[:] = self.best_bid_fp + self.spread_ticks * self.tick_fp

    def _compute_equity_quote(self) -> NDArray[np.float64]:
        mid_quote = self.mid_ticks.astype(np.float64) * (self.tick_fp / float(FP_SCALE))
        inv_base = self.inventory_fp.astype(np.float64) / float(FP_SCALE)
        cash_quote = self.cash_fp.astype(np.float64) / float(FP_SCALE)
        return cash_quote + inv_base * mid_quote

    def sample_latency_steps(self, size: int | None = None) -> NDArray[np.int64]:
        """Sample discretized wire + engine latency steps from calibrated Log-Normal distribution."""
        n = self.B if size is None else size
        tau_ms = self.rng.lognormal(mean=self.cfg.latency_log_mean, sigma=self.cfg.latency_log_std, size=n)
        steps = np.ceil(tau_ms / max(self.cfg.latency_tick_ms, 1e-6)).astype(np.int64)
        return np.clip(steps, self.cfg.min_latency_steps, self.cfg.max_latency_steps)

    def enqueue_raw_quote_command(
        self,
        bid_px_fp: NDArray[np.int64],
        ask_px_fp: NDArray[np.int64],
        bid_qty_fp: NDArray[np.int64],
        ask_qty_fp: NDArray[np.int64],
        latency_steps_override: NDArray[np.int64] | None = None,
    ) -> NDArray[np.int64]:
        """
        Schedule a two-sided limit quote update into the in-flight latency ring buffer.
        Used both by `step()` and directly by unit tests verifying queue & latency mechanics.
        """
        lat = (
            self.sample_latency_steps()
            if latency_steps_override is None
            else np.clip(latency_steps_override, 0, self.cfg.max_latency_steps)
        )
        env_ids = np.arange(self.B, dtype=np.int64)

        # Zero-latency path only when explicitly overridden to 0 in deterministic unit tests
        zero_mask = lat == 0
        if np.any(zero_mask):
            self._apply_arrived_quotes(
                zero_mask,
                bid_px_fp,
                ask_px_fp,
                bid_qty_fp,
                ask_qty_fp,
            )

        delayed_mask = ~zero_mask
        if np.any(delayed_mask):
            slots = (self.step_idx[delayed_mask] + lat[delayed_mask]) % self.ring_len
            d_envs = env_ids[delayed_mask]
            # Place into sub-slot 0 if empty else sub-slot 1 (coalescing latest amend if both full)
            use_sub1 = self.ring_cmd[slots, d_envs, 0] != CMD_NONE
            sub = np.where(use_sub1, 1, 0)
            overwrote = self.ring_cmd[slots, d_envs, sub] != CMD_NONE

            self.ring_cmd[slots, d_envs, sub] = CMD_QUOTE_UPDATE
            self.ring_bid_px_fp[slots, d_envs, sub] = bid_px_fp[delayed_mask]
            self.ring_ask_px_fp[slots, d_envs, sub] = ask_px_fp[delayed_mask]
            self.ring_bid_qty_fp[slots, d_envs, sub] = bid_qty_fp[delayed_mask]
            self.ring_ask_qty_fp[slots, d_envs, sub] = ask_qty_fp[delayed_mask]
            self.pending_cmd_count[delayed_mask] += np.where(overwrote, 0, 1)

        return lat

    def _estimate_level_resting_depth_fp(
        self,
        quote_px_fp: NDArray[np.int64],
        is_bid: bool,
    ) -> NDArray[np.int64]:
        """
        Compute L2 resting depth at `quote_px_fp` including competing MM liquidity!
        Levels deeper away from touch have exponentially decaying/tapering liquidity profile.
        """
        if is_bid:
            ticks_away = np.maximum(0, (self.best_bid_fp - quote_px_fp) // self.tick_fp)
            base_l1 = self.l1_bid_depth_fp
            comp_match = np.where(
                self.cfg.enable_competing_mm & (quote_px_fp == self.comp_bid_fp),
                self.comp_qty_fp,
                0,
            )
        else:
            ticks_away = np.maximum(0, (quote_px_fp - self.best_ask_fp) // self.tick_fp)
            base_l1 = self.l1_ask_depth_fp
            comp_match = np.where(
                self.cfg.enable_competing_mm & (quote_px_fp == self.comp_ask_fp),
                self.comp_qty_fp,
                0,
            )

        profile_mult = np.where(ticks_away == 0, 1.0, np.where(ticks_away <= 3, 1.15, 0.75))
        depth = (base_l1.astype(np.float64) * profile_mult).astype(np.int64) + comp_match
        return np.maximum(0, (depth // self.lot_fp) * self.lot_fp)

    def _apply_arrived_quotes(
        self,
        mask: NDArray[np.bool_],
        new_bid_px_fp: NDArray[np.int64],
        new_ask_px_fp: NDArray[np.int64],
        new_bid_qty_fp: NDArray[np.int64],
        new_ask_qty_fp: NDArray[np.int64],
    ) -> None:
        """
        Apply arrived quote updates respecting:
        1. Post-Only crossing rejection (`REJECT_REASON_POST_ONLY_WOULD_CROSS`)
        2. Price-Time FIFO Queue Priority (`orderbook.proto` ModifyOrder invariant):
           - Same price & `0 < new_qty <= old_qty` -> PRESERVE `queue_ahead_fp`!
           - Price change OR `new_qty > old_qty` OR previously inactive -> JOIN TAIL (`queue_ahead_fp = level_depth`)!
           - `new_qty <= 0` -> CANCEL active order!
        """
        # --- BID SIDE ---
        cancel_bid = mask & (new_bid_qty_fp <= 0)
        self.bid_active = np.where(cancel_bid, False, self.bid_active)
        self.bid_qty_fp = np.where(cancel_bid, 0, self.bid_qty_fp)
        self.bid_queue_ahead_fp = np.where(cancel_bid, 0, self.bid_queue_ahead_fp)

        want_bid = mask & (new_bid_qty_fp > 0)
        if self.cfg.post_only:
            cross_bid = want_bid & (new_bid_px_fp >= self.best_ask_fp)
            self.post_only_rejects += cross_bid.astype(np.int64)
            # On post-only reject, cancel stale resting bid to avoid unintended exposure
            self.bid_active = np.where(cross_bid, False, self.bid_active)
            self.bid_qty_fp = np.where(cross_bid, 0, self.bid_qty_fp)
            self.bid_queue_ahead_fp = np.where(cross_bid, 0, self.bid_queue_ahead_fp)
            valid_bid = want_bid & (~cross_bid)
        else:
            valid_bid = want_bid

        preserve_bid_prio = (
            valid_bid
            & self.bid_active
            & (new_bid_px_fp == self.bid_price_fp)
            & (new_bid_qty_fp <= self.bid_qty_fp)
        )
        reset_bid_prio = valid_bid & (~preserve_bid_prio)
        est_bid_depth = self._estimate_level_resting_depth_fp(new_bid_px_fp, is_bid=True)

        self.bid_active = np.where(valid_bid, True, self.bid_active)
        self.bid_price_fp = np.where(valid_bid, new_bid_px_fp, self.bid_price_fp)
        self.bid_qty_fp = np.where(valid_bid, new_bid_qty_fp, self.bid_qty_fp)
        self.bid_queue_ahead_fp = np.where(
            reset_bid_prio,
            est_bid_depth,
            self.bid_queue_ahead_fp,
        )

        # --- ASK SIDE ---
        cancel_ask = mask & (new_ask_qty_fp <= 0)
        self.ask_active = np.where(cancel_ask, False, self.ask_active)
        self.ask_qty_fp = np.where(cancel_ask, 0, self.ask_qty_fp)
        self.ask_queue_ahead_fp = np.where(cancel_ask, 0, self.ask_queue_ahead_fp)

        want_ask = mask & (new_ask_qty_fp > 0)
        if self.cfg.post_only:
            cross_ask = want_ask & (new_ask_px_fp <= self.best_bid_fp)
            self.post_only_rejects += cross_ask.astype(np.int64)
            self.ask_active = np.where(cross_ask, False, self.ask_active)
            self.ask_qty_fp = np.where(cross_ask, 0, self.ask_qty_fp)
            self.ask_queue_ahead_fp = np.where(cross_ask, 0, self.ask_queue_ahead_fp)
            valid_ask = want_ask & (~cross_ask)
        else:
            valid_ask = want_ask

        preserve_ask_prio = (
            valid_ask
            & self.ask_active
            & (new_ask_px_fp == self.ask_price_fp)
            & (new_ask_qty_fp <= self.ask_qty_fp)
        )
        reset_ask_prio = valid_ask & (~preserve_ask_prio)
        est_ask_depth = self._estimate_level_resting_depth_fp(new_ask_px_fp, is_bid=False)

        self.ask_active = np.where(valid_ask, True, self.ask_active)
        self.ask_price_fp = np.where(valid_ask, new_ask_px_fp, self.ask_price_fp)
        self.ask_qty_fp = np.where(valid_ask, new_ask_qty_fp, self.ask_qty_fp)
        self.ask_queue_ahead_fp = np.where(
            reset_ask_prio,
            est_ask_depth,
            self.ask_queue_ahead_fp,
        )

    def _drain_current_ring_slot(self) -> None:
        """Process all commands arriving at `current_step % ring_len` in deterministic order."""
        slots = self.step_idx % self.ring_len
        env_ids = np.arange(self.B, dtype=np.int64)

        for sub in (0, 1):
            cmds = self.ring_cmd[slots, env_ids, sub]
            has_q = cmds == CMD_QUOTE_UPDATE
            if np.any(has_q):
                bpx = self.ring_bid_px_fp[slots, env_ids, sub].copy()
                apx = self.ring_ask_px_fp[slots, env_ids, sub].copy()
                bqt = self.ring_bid_qty_fp[slots, env_ids, sub].copy()
                aqt = self.ring_ask_qty_fp[slots, env_ids, sub].copy()
                self._apply_arrived_quotes(has_q, bpx, apx, bqt, aqt)
                self.ring_cmd[slots, env_ids, sub] = CMD_NONE
                self.pending_cmd_count = np.maximum(0, self.pending_cmd_count - has_q.astype(np.int64))

    def execute_order_flow_step(
        self,
        aggr_buy_fp: NDArray[np.int64],
        aggr_sell_fp: NDArray[np.int64],
        cancel_rate_ahead: float = 0.08,
    ) -> tuple[NDArray[np.int64], NDArray[np.int64]]:
        """
        Advance queue positions and execute realistic FIFO maker fills against incoming
        aggressive buy and sell volume!
        Returns `(filled_bid_qty_fp, filled_ask_qty_fp)`.
        """
        # 1. Proportional queue advancement from natural resting order cancellations
        if cancel_rate_ahead > 0.0:
            bid_cancel_drop = (self.bid_queue_ahead_fp.astype(np.float64) * cancel_rate_ahead).astype(np.int64)
            ask_cancel_drop = (self.ask_queue_ahead_fp.astype(np.float64) * cancel_rate_ahead).astype(np.int64)
            self.bid_queue_ahead_fp = np.maximum(0, self.bid_queue_ahead_fp - bid_cancel_drop)
            self.ask_queue_ahead_fp = np.maximum(0, self.ask_queue_ahead_fp - ask_cancel_drop)

        # 2. Compute aggressive sell volume reaching our bid price tick!
        # If our bid is `k` ticks below `best_bid_fp`, aggressive sell must sweep `k` levels of depth first.
        bid_ticks_behind_best = np.maximum(0, (self.best_bid_fp - self.bid_price_fp) // self.tick_fp)
        depth_above_bid = bid_ticks_behind_best * self.l1_bid_depth_fp
        sell_reaching_bid = np.where(
            self.bid_active & (self.bid_price_fp <= self.best_ask_fp),
            np.maximum(0, aggr_sell_fp - depth_above_bid),
            0,
        )

        # Deplete `bid_queue_ahead_fp` FIRST!
        overflow_past_bid_queue = np.maximum(0, sell_reaching_bid - self.bid_queue_ahead_fp)
        filled_bid_fp = np.where(
            self.bid_active,
            np.minimum(self.bid_qty_fp, overflow_past_bid_queue),
            0,
        )
        # Enforce hard inventory risk ceiling on buy fills
        max_buy_room = np.maximum(0, self.max_inv_fp - self.inventory_fp)
        filled_bid_fp = np.minimum(filled_bid_fp, max_buy_room)

        self.bid_queue_ahead_fp = np.where(
            self.bid_active,
            np.maximum(0, self.bid_queue_ahead_fp - sell_reaching_bid),
            0,
        )
        self.bid_qty_fp = np.maximum(0, self.bid_qty_fp - filled_bid_fp)
        self.bid_active = self.bid_active & (self.bid_qty_fp > 0)

        # 3. Compute aggressive buy volume reaching our ask price tick!
        ask_ticks_behind_best = np.maximum(0, (self.ask_price_fp - self.best_ask_fp) // self.tick_fp)
        depth_below_ask = ask_ticks_behind_best * self.l1_ask_depth_fp
        buy_reaching_ask = np.where(
            self.ask_active & (self.ask_price_fp >= self.best_bid_fp),
            np.maximum(0, aggr_buy_fp - depth_below_ask),
            0,
        )

        overflow_past_ask_queue = np.maximum(0, buy_reaching_ask - self.ask_queue_ahead_fp)
        filled_ask_fp = np.where(
            self.ask_active,
            np.minimum(self.ask_qty_fp, overflow_past_ask_queue),
            0,
        )
        max_sell_room = np.maximum(0, self.max_inv_fp + self.inventory_fp)
        filled_ask_fp = np.minimum(filled_ask_fp, max_sell_room)

        self.ask_queue_ahead_fp = np.where(
            self.ask_active,
            np.maximum(0, self.ask_queue_ahead_fp - buy_reaching_ask),
            0,
        )
        self.ask_qty_fp = np.maximum(0, self.ask_qty_fp - filled_ask_fp)
        self.ask_active = self.ask_active & (self.ask_qty_fp > 0)

        # 4. Exact Channel 02 Fixed-Point Double-Entry Settlement for Maker Bid & Ask Fills!
        if np.any(filled_bid_fp > 0):
            _, m_fee_b, _, m_qdelta_b, t_qdelta_b, treas_b = settle_fill_double_entry_vec(
                self.bid_price_fp,
                filled_bid_fp,
                np.ones(self.B, dtype=np.bool_),
                maker_fee_ppm=self.cfg.maker_fee_ppm,
                taker_fee_ppm=self.cfg.taker_fee_ppm,
            )
            self.cash_fp += m_qdelta_b
            self.inventory_fp += filled_bid_fp
            self.treasury_fee_fp += treas_b
            self.cumulative_fees_fp += m_fee_b
            self.cumulative_maker_vol_fp += filled_bid_fp
            self.maker_fills_count += (filled_bid_fp > 0).astype(np.int64)
            self.double_entry_residual_fp += m_qdelta_b + t_qdelta_b + treas_b

        if np.any(filled_ask_fp > 0):
            _, m_fee_a, _, m_qdelta_a, t_qdelta_a, treas_a = settle_fill_double_entry_vec(
                self.ask_price_fp,
                filled_ask_fp,
                np.zeros(self.B, dtype=np.bool_),
                maker_fee_ppm=self.cfg.maker_fee_ppm,
                taker_fee_ppm=self.cfg.taker_fee_ppm,
            )
            self.cash_fp += m_qdelta_a
            self.inventory_fp -= filled_ask_fp
            self.treasury_fee_fp += treas_a
            self.cumulative_fees_fp += m_fee_a
            self.cumulative_maker_vol_fp += filled_ask_fp
            self.maker_fills_count += (filled_ask_fp > 0).astype(np.int64)
            self.double_entry_residual_fp += m_qdelta_a + t_qdelta_a + treas_a

        return filled_bid_fp, filled_ask_fp

    def execute_taker_order_vec(
        self,
        signed_taker_qty_fp: NDArray[np.int64],
    ) -> tuple[NDArray[np.int64], NDArray[np.float64]]:
        """
        Execute aggressive IOC/Market slice (`signed_taker_qty_fp > 0` = buy, `< 0` = sell)
        walking L2 depth levels, charging Channel 02 `taker_fee_ppm`, AND applying both
        Almgren-Chriss temporary impact (`temp_impact_ticks`) and permanent impact (`mid_ticks`)!
        Returns `(exec_price_fp, total_impact_ticks)`.
        """
        abs_qty = np.abs(signed_taker_qty_fp)
        active = abs_qty > 0
        if not np.any(active):
            return np.zeros(self.B, dtype=np.int64), np.zeros(self.B, dtype=np.float64)

        sign = np.sign(signed_taker_qty_fp)
        is_buy = signed_taker_qty_fp > 0

        # Decay prior transient impact, then add immediate book-walking + Almgren-Chriss temporary impact
        part_ratio = abs_qty.astype(np.float64) / float(max(self.base_depth_fp, 1))
        walk_ticks = 0.5 * np.floor(part_ratio)
        instant_temp_ticks = (
            self.cfg.temp_impact_eta_ticks
            * self.sigma_ticks
            * np.power(np.maximum(part_ratio, 0.0), self.cfg.temp_impact_beta)
        )
        self.temp_impact_ticks = (
            self.cfg.temp_impact_resilience * self.temp_impact_ticks
            + sign.astype(np.float64) * instant_temp_ticks
        )

        # Effective execution price walks against aggressor by `walk_ticks + |temp_impact|`
        adverse_ticks = np.maximum(
            0,
            np.ceil(walk_ticks + sign.astype(np.float64) * self.temp_impact_ticks).astype(np.int64),
        )
        touch_px_fp = np.where(is_buy, self.best_ask_fp, self.best_bid_fp)
        exec_px_fp = np.where(
            is_buy,
            touch_px_fp + adverse_ticks * self.tick_fp,
            np.maximum(self.tick_fp, touch_px_fp - adverse_ticks * self.tick_fp),
        )

        # Settle as Taker (counterparty is resting L2 maker)
        _, _, t_fee, m_qdelta, t_qdelta, treas = settle_fill_double_entry_vec(
            exec_px_fp,
            abs_qty,
            maker_is_buy=~is_buy,
            maker_fee_ppm=self.cfg.maker_fee_ppm,
            taker_fee_ppm=self.cfg.taker_fee_ppm,
        )
        self.cash_fp += np.where(active, t_qdelta, 0)
        self.inventory_fp += signed_taker_qty_fp
        self.treasury_fee_fp += np.where(active, treas, 0)
        self.cumulative_fees_fp += np.where(active, t_fee, 0)
        self.cumulative_taker_vol_fp += abs_qty
        self.taker_fills_count += active.astype(np.int64)
        self.double_entry_residual_fp += np.where(active, m_qdelta + t_qdelta + treas, 0)

        # Permanent price impact shifts fundamental mid-price permanently (Kyle's lambda!)
        perm_shift_ticks = np.round(
            sign.astype(np.float64)
            * self.cfg.perm_impact_gamma_ticks
            * self.kyles_lambda_regime
            * part_ratio
        ).astype(np.int64)
        self.mid_ticks = np.maximum(1_000, self.mid_ticks + np.where(active, perm_shift_ticks, 0))
        self._sync_bbo_from_mid()

        total_impact_ticks = adverse_ticks.astype(np.float64) + np.abs(perm_shift_ticks).astype(np.float64)
        return exec_px_fp, total_impact_ticks

    def _step_competing_market_maker(self) -> None:
        """Secondary strategic Avellaneda-Stoikov market maker competing for queue & spread."""
        if not self.cfg.enable_competing_mm:
            return
        comp_q_norm = self.comp_inv_fp.astype(np.float64) / float(max(self.max_inv_fp, 1))
        skew_ticks = np.round(-2.0 * comp_q_norm + 0.8 * self.alpha_signal).astype(np.int64)
        half_sp_ticks = np.maximum(1, np.round(1.0 + 0.4 * self.sigma_ticks).astype(np.int64))
        res_ticks = self.mid_ticks + skew_ticks
        self.comp_bid_fp[:] = np.minimum(self.best_ask_fp - self.tick_fp, (res_ticks - half_sp_ticks) * self.tick_fp)
        self.comp_ask_fp[:] = np.maximum(self.best_bid_fp + self.tick_fp, (res_ticks + half_sp_ticks) * self.tick_fp)

    def step(
        self,
        actions: NDArray[np.float32 | np.float64],
    ) -> tuple[NDArray[np.float32], NDArray[np.float32], NDArray[np.bool_], NDArray[np.bool_], dict[str, Any]]:
        """
        Execute one vectorized market tick across all `B` environments.

        Action vector `a in [-1, 1]^4`:
          a[:, 0] -> reservation skew offset in [-3, +3] ticks
          a[:, 1] -> half-spread width control mapped to [1, 6] ticks
          a[:, 2] -> resting quote size fraction mapped to [0.2, 1.0] * base_quote_fp
          a[:, 3] -> Almgren-Chriss IOC inventory-clearing urgency (deadzone |a3| <= 0.65)
        """
        act = np.clip(np.asarray(actions, dtype=np.float64), -1.0, 1.0)
        if act.shape != (self.B, ACT_DIM):
            raise ValueError(f"Expected actions shape {(self.B, ACT_DIM)}, got {act.shape}")

        # 1. Drain delayed commands arriving at the start of this tick
        self._drain_current_ring_slot()

        # 2. Update secondary competing MM quotes
        self._step_competing_market_maker()

        # 3. Translate continuous RL actions into discrete tick/lot quotes & enqueue with Log-Normal latency!
        half_base_sp = max(2, self.cfg.base_spread_ticks // 2)
        skew_ticks = np.round(act[:, 0] * float(half_base_sp * 2)).astype(np.int64)
        half_spread_ticks = np.round(
            float(half_base_sp) * (0.85 + 1.15 * (0.5 * (act[:, 1] + 1.0)))
        ).astype(np.int64)
        size_mult = 0.25 + 0.75 * (0.5 * (act[:, 2] + 1.0))
        quote_qty_fp = ((size_mult * self.base_quote_fp).astype(np.int64) // self.lot_fp) * self.lot_fp

        res_mid_fp = self.mid_ticks * self.tick_fp + skew_ticks * self.tick_fp
        target_bid_px_fp = res_mid_fp - half_spread_ticks * self.tick_fp
        target_ask_px_fp = res_mid_fp + half_spread_ticks * self.tick_fp

        # Reduce quote size automatically on side approaching max inventory limit
        can_buy = self.inventory_fp < self.max_inv_fp
        can_sell = self.inventory_fp > -self.max_inv_fp
        bid_qty_cmd = np.where(can_buy, quote_qty_fp, 0)
        ask_qty_cmd = np.where(can_sell, quote_qty_fp, 0)

        self.enqueue_raw_quote_command(target_bid_px_fp, target_ask_px_fp, bid_qty_cmd, ask_qty_cmd)

        # 4. Optional Almgren-Chriss urgent IOC slice when |act[:, 3]| > 0.65
        urg = act[:, 3]
        urg_active = np.abs(urg) > 0.65
        urg_strength = np.where(urg_active, (np.abs(urg) - 0.65) / 0.35, 0.0)
        # Urgent slice pushes inventory toward 0 (or in direction of `sign(urg)` bounded by max_inv)
        raw_taker_fp = (np.sign(urg) * urg_strength * 0.5 * self.base_quote_fp).astype(np.int64)
        raw_taker_fp = (raw_taker_fp // self.lot_fp) * self.lot_fp
        raw_taker_fp = np.clip(raw_taker_fp, -self.max_inv_fp - self.inventory_fp, self.max_inv_fp - self.inventory_fp)
        _, taker_impact_ticks = self.execute_taker_order_vec(raw_taker_fp)

        # 5. Simulate exogenous + informed multi-agent order flow & book evolution!
        z_imb = self.rng.standard_normal(self.B)
        z_mid = self.rng.standard_normal(self.B)
        self.sigma_ticks = 0.95 * self.sigma_ticks + 0.05 * self.cfg.base_sigma_ticks * (0.55 + np.abs(z_mid))
        self.latent_imb = 0.82 * self.latent_imb + 0.55 * z_imb
        skew_state = np.tanh(self.latent_imb)

        # Update L1 depths & OBI
        d_b = self.rng.lognormal(0.0, 0.30, size=self.B)
        d_a = self.rng.lognormal(0.0, 0.30, size=self.B)
        self.l1_bid_depth_fp = np.maximum(
            self.lot_fp * 20,
            ((self.base_depth_fp * d_b * np.exp(0.50 * skew_state)).astype(np.int64) // self.lot_fp) * self.lot_fp,
        )
        self.l1_ask_depth_fp = np.maximum(
            self.lot_fp * 20,
            ((self.base_depth_fp * d_a * np.exp(-0.50 * skew_state)).astype(np.int64) // self.lot_fp) * self.lot_fp,
        )
        self.obi_5 = (self.l1_bid_depth_fp - self.l1_ask_depth_fp).astype(np.float64) / np.maximum(
            (self.l1_bid_depth_fp + self.l1_ask_depth_fp).astype(np.float64), 1.0
        )

        # Toxic flow intensity rises when |skew_state| is high!
        self.kyles_lambda_regime = 1.0 + 1.2 * (np.abs(skew_state) > 0.72).astype(np.float64)
        lam_buy = 0.95 * np.exp(0.65 * skew_state)
        lam_sell = 0.95 * np.exp(-0.65 * skew_state)
        aggr_buy_events = self.rng.poisson(lam_buy)
        aggr_sell_events = self.rng.poisson(lam_sell)

        lot_chunk = (int(0.65 * self.base_depth_fp) // self.lot_fp) * self.lot_fp
        aggr_buy_fp = aggr_buy_events.astype(np.int64) * lot_chunk
        aggr_sell_fp = aggr_sell_events.astype(np.int64) * lot_chunk

        # Execute FIFO queue depletion & maker fills BEFORE adverse mid-price jump!
        filled_bid_fp, filled_ask_fp = self.execute_order_flow_step(aggr_buy_fp, aggr_sell_fp)

        # Competing MM inventory update (approximate fill capture at comp quotes)
        if self.cfg.enable_competing_mm:
            comp_b_hit = (aggr_sell_fp > self.l1_bid_depth_fp).astype(np.int64) * (self.comp_qty_fp // 2)
            comp_a_hit = (aggr_buy_fp > self.l1_ask_depth_fp).astype(np.int64) * (self.comp_qty_fp // 2)
            self.comp_inv_fp = np.clip(self.comp_inv_fp + comp_b_hit - comp_a_hit, -self.max_inv_fp, self.max_inv_fp)

        # Update OFI z-score & GP symbolic alpha feature signal
        flow_imb = (aggr_buy_fp - aggr_sell_fp).astype(np.float64) / float(max(self.base_depth_fp, 1))
        raw_ofi = 1.1 * self.obi_5 + 0.9 * flow_imb
        self.ofi_ema = 0.70 * self.ofi_ema + 0.30 * raw_ofi
        self.ofi_mean = 0.96 * self.ofi_mean + 0.04 * raw_ofi
        self.ofi_var = 0.96 * self.ofi_var + 0.04 * (raw_ofi - self.ofi_mean) ** 2
        self.ofi_z = np.clip((raw_ofi - self.ofi_mean) / np.sqrt(self.ofi_var + 1e-6), -4.0, 4.0)

        if self.alpha_signal_fn is not None:
            self.alpha_signal = np.clip(
                self.alpha_signal_fn(
                    {
                        "obi_5": self.obi_5,
                        "ofi_z": self.ofi_z,
                        "ofi_ema": self.ofi_ema,
                        "spread_ticks": self.spread_ticks.astype(np.float64),
                        "sigma": self.sigma_ticks,
                    }
                ),
                -1.0,
                1.0,
            )
        else:
            self.alpha_signal = np.tanh(0.85 * self.obi_5 + 0.55 * self.ofi_ema)

        # Post-trade mid-price evolution (includes adverse selection against stale filled quotes!)
        drift_ticks = 0.45 * (0.9 * self.obi_5 + 0.7 * np.tanh(flow_imb)) * self.sigma_ticks
        delta_mid_ticks = np.round(drift_ticks + self.sigma_ticks * z_mid).astype(np.int64)
        self.mid_ticks = np.maximum(1_000, self.mid_ticks + delta_mid_ticks)
        self.spread_ticks = self.cfg.base_spread_ticks + 10 * (self.sigma_ticks > 1.45 * self.cfg.base_sigma_ticks).astype(np.int64)
        self._sync_bbo_from_mid()

        # Decay transient market impact
        self.temp_impact_ticks *= self.cfg.temp_impact_resilience

        # 6. Compute Avellaneda-Stoikov + Almgren-Chriss + Tail-Drawdown Reward (USDT-calibrated)!
        new_equity = self._compute_equity_quote()
        step_pnl_quote = new_equity - self.prev_equity_quote
        self.prev_equity_quote[:] = new_equity
        self.peak_equity_quote[:] = np.maximum(self.peak_equity_quote, new_equity)
        self.drawdown_quote[:] = np.maximum(0.0, self.peak_equity_quote - new_equity)

        q_lots_norm = self.inventory_fp.astype(np.float64) / float(max(self.base_quote_fp, 1))
        taker_lots_norm = np.abs(raw_taker_fp).astype(np.float64) / float(max(self.base_quote_fp, 1))
        sigma_usdt = self.sigma_ticks * (self.tick_fp / float(FP_SCALE))

        # (a) Avellaneda-Stoikov quadratic inventory variance penalty: 0.5 * gamma * sigma_usdt^2 * q^2
        inv_penalty = 0.5 * self.cfg.inv_risk_aversion_gamma * (sigma_usdt**2) * (q_lots_norm**2)
        # (b) Almgren-Chriss execution rate quadratic penalty: eta * (dq_aggr)^2
        ac_exec_penalty = self.cfg.ac_exec_urgency_eta * (taker_lots_norm**2)
        # (c) Asymmetric quadratic tail-drawdown penalty beyond threshold
        excess_dd = np.maximum(0.0, self.drawdown_quote - self.cfg.tail_drawdown_threshold_quote)
        tail_dd_penalty = self.cfg.tail_drawdown_lambda * (excess_dd**2)

        self.step_idx += 1
        truncated = self.step_idx >= self.cfg.episode_steps
        terminated = np.zeros(self.B, dtype=np.bool_)

        # Terminal inventory liquidation penalty -kappa_T * q_T^2 at horizon T
        terminal_penalty = np.where(
            truncated,
            self.cfg.terminal_inv_penalty_kappa * (q_lots_norm**2),
            0.0,
        )

        reward = step_pnl_quote - inv_penalty - ac_exec_penalty - tail_dd_penalty - terminal_penalty

        info: dict[str, Any] = {
            "step_pnl_quote": step_pnl_quote,
            "equity_quote": new_equity,
            "net_pnl_quote": new_equity - self.initial_equity_quote,
            "inventory_norm": self.inventory_fp.astype(np.float64) / float(max(self.max_inv_fp, 1)),
            "inv_penalty": inv_penalty,
            "ac_exec_penalty": ac_exec_penalty,
            "tail_dd_penalty": tail_dd_penalty,
            "drawdown_quote": self.drawdown_quote.copy(),
            "filled_bid_fp": filled_bid_fp,
            "filled_ask_fp": filled_ask_fp,
            "taker_impact_ticks": taker_impact_ticks,
            "double_entry_balanced": bool(np.all(self.double_entry_residual_fp == 0)),
        }

        return self._build_obs(), reward.astype(np.float32), terminated, truncated, info

    def _build_obs(self) -> NDArray[np.float32]:
        """Construct `(B, 14)` normalized microstructure & execution state observation matrix."""
        obs = np.empty((self.B, OBS_DIM), dtype=np.float32)
        max_inv = float(max(self.max_inv_fp, 1))
        rem_frac = np.clip(1.0 - self.step_idx.astype(np.float64) / float(max(self.cfg.episode_steps, 1)), 0.0, 1.0)

        micro_skew = (self.l1_bid_depth_fp - self.l1_ask_depth_fp).astype(np.float64) / np.maximum(
            (self.l1_bid_depth_fp + self.l1_ask_depth_fp).astype(np.float64), 1.0
        )
        bid_q_ratio = np.where(
            self.bid_active,
            self.bid_queue_ahead_fp.astype(np.float64)
            / np.maximum((self.bid_queue_ahead_fp + self.bid_qty_fp).astype(np.float64), 1.0),
            1.0,
        )
        ask_q_ratio = np.where(
            self.ask_active,
            self.ask_queue_ahead_fp.astype(np.float64)
            / np.maximum((self.ask_queue_ahead_fp + self.ask_qty_fp).astype(np.float64), 1.0),
            1.0,
        )

        obs[:, 0] = (self.inventory_fp.astype(np.float64) / max_inv).astype(np.float32)
        obs[:, 1] = rem_frac.astype(np.float32)
        obs[:, 2] = ((self.inventory_fp.astype(np.float64) / max_inv) * (1.0 / (rem_frac + 0.1))).clip(-3.0, 3.0).astype(np.float32)
        obs[:, 3] = np.clip(self.spread_ticks.astype(np.float64) / float(max(self.cfg.base_spread_ticks, 1)), 0.0, 5.0).astype(np.float32)
        obs[:, 4] = micro_skew.astype(np.float32)
        obs[:, 5] = (self.ofi_z / 3.0).astype(np.float32)
        obs[:, 6] = self.obi_5.astype(np.float32)
        obs[:, 7] = np.clip(self.sigma_ticks / max(self.cfg.base_sigma_ticks, 1.0), 0.0, 5.0).astype(np.float32)
        obs[:, 8] = self.alpha_signal.astype(np.float32)
        obs[:, 9] = bid_q_ratio.astype(np.float32)
        obs[:, 10] = ask_q_ratio.astype(np.float32)
        obs[:, 11] = np.tanh(self.drawdown_quote / 10.0).astype(np.float32)
        obs[:, 12] = (self.kyles_lambda_regime - 1.0).astype(np.float32)
        obs[:, 13] = np.clip(self.pending_cmd_count.astype(np.float64) / 4.0, 0.0, 1.0).astype(np.float32)
        return obs


class HyperionLOBGymEnv(gym.Env[NDArray[np.float32], NDArray[np.float32]]):
    """
    Standard Farama `gymnasium.Env` interface wrapping `VectorizedLOBEnv(num_envs=1)`.
    Passes `gymnasium.utils.env_checker.check_env` with zero warnings.
    """

    metadata = {"render_modes": []}

    def __init__(self, cfg: LOBEnvConfig | None = None) -> None:
        super().__init__()
        base_cfg = cfg or LOBEnvConfig(num_envs=1)
        single_cfg = LOBEnvConfig(
            symbol=base_cfg.symbol,
            num_envs=1,
            episode_steps=base_cfg.episode_steps,
            step_ms=base_cfg.step_ms,
            latency_log_mean=base_cfg.latency_log_mean,
            latency_log_std=base_cfg.latency_log_std,
            latency_tick_ms=base_cfg.latency_tick_ms,
            min_latency_steps=base_cfg.min_latency_steps,
            max_latency_steps=base_cfg.max_latency_steps,
            maker_fee_ppm=base_cfg.maker_fee_ppm,
            taker_fee_ppm=base_cfg.taker_fee_ppm,
            post_only=base_cfg.post_only,
            base_quote_lots=base_cfg.base_quote_lots,
            max_inventory_lots=base_cfg.max_inventory_lots,
            initial_target_exec_lots=base_cfg.initial_target_exec_lots,
            base_book_depth_lots=base_cfg.base_book_depth_lots,
            base_spread_ticks=base_cfg.base_spread_ticks,
            base_sigma_ticks=base_cfg.base_sigma_ticks,
            temp_impact_eta_ticks=base_cfg.temp_impact_eta_ticks,
            temp_impact_beta=base_cfg.temp_impact_beta,
            temp_impact_resilience=base_cfg.temp_impact_resilience,
            perm_impact_gamma_ticks=base_cfg.perm_impact_gamma_ticks,
            inv_risk_aversion_gamma=base_cfg.inv_risk_aversion_gamma,
            ac_exec_urgency_eta=base_cfg.ac_exec_urgency_eta,
            tail_drawdown_threshold_quote=base_cfg.tail_drawdown_threshold_quote,
            tail_drawdown_lambda=base_cfg.tail_drawdown_lambda,
            terminal_inv_penalty_kappa=base_cfg.terminal_inv_penalty_kappa,
            enable_competing_mm=base_cfg.enable_competing_mm,
            seed=base_cfg.seed,
        )
        self.vec_env = VectorizedLOBEnv(single_cfg)
        self.observation_space = spaces.Box(
            low=-10.0,
            high=10.0,
            shape=(OBS_DIM,),
            dtype=np.float32,
        )
        self.action_space = spaces.Box(
            low=-1.0,
            high=1.0,
            shape=(ACT_DIM,),
            dtype=np.float32,
        )

    def reset(
        self,
        *,
        seed: int | None = None,
        options: dict[str, Any] | None = None,
    ) -> tuple[NDArray[np.float32], dict[str, Any]]:
        super().reset(seed=seed)
        obs, info = self.vec_env.reset(seed=seed)
        return obs[0].copy(), info

    def step(
        self,
        action: NDArray[np.float32],
    ) -> tuple[NDArray[np.float32], float, bool, bool, dict[str, Any]]:
        act_batch = np.asarray(action, dtype=np.float32).reshape(1, ACT_DIM)
        obs, rew, term, trunc, info_batch = self.vec_env.step(act_batch)
        scalar_info = {
            k: (v[0].item() if isinstance(v, np.ndarray) and v.ndim == 1 else v)
            for k, v in info_batch.items()
        }
        return obs[0].copy(), float(rew[0]), bool(term[0]), bool(trunc[0]), scalar_info
