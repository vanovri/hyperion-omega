"""Streaming Microstructure Spoofing, Layering, Momentum Ignition & Velocity Spike Extractor.

Maintains O(1) incremental rolling behavioral state per account and maintains a temporal
buffer of 16-dimensional feature vectors [T=8, F=16] in pre-allocated contiguous float32
NumPy arrays (zero per-event heap allocation) for sub-millisecond ONNX inference.

All raw order prices, quantities, and notional values strictly obey the scale-1e8
fixed-point integer invariant.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass
import math
from typing import Literal

import numpy as np
import numpy.typing as npt

from sentinel_ml.fixed_point import (
    PPM_SCALE,
    compute_notional_fp,
    distance_bps_int,
    ratio_to_ppm,
    require_uint64,
)
from sentinel_ml.graph_engine import SlidingTemporalMultigraph

SideLiteral = Literal["BUY", "SELL"]

TEMPORAL_SEQ_LEN: int = 8
NUM_FEATURES: int = 16


def alloc_aligned64_f32(shape: tuple[int, ...]) -> npt.NDArray[np.float32]:
    """Allocate a 64-byte cache-line-aligned contiguous float32 ndarray (`ptr % 64 == 0`)."""
    n_elems = 1
    for dim in shape:
        n_elems *= dim
    n_bytes = n_elems * 4
    raw = np.empty(n_bytes + 64, dtype=np.uint8)
    offset = (-int(raw.ctypes.data)) & 63
    return raw[offset : offset + n_bytes].view(np.float32).reshape(shape)

FEATURE_NAMES: tuple[str, ...] = (
    "otr_count_log",
    "otr_volume_log",
    "cancel_ratio",
    "fast_cancel_rate",
    "spoof_opp_exec_score",
    "near_bbo_cancel_vol_share",
    "layering_distinct_ticks",
    "layering_sync_cancel_burst",
    "self_impact_concentration",
    "momentum_ignition_burst",
    "wash_cycle_score",
    "wash_cycle_depth_norm",
    "counterparty_concentration",
    "net_balance_symmetry",
    "event_velocity_zscore",
    "taker_sweep_aggression",
)


@dataclass(slots=True)
class ActiveRestingOrder:
    """In-flight resting order tracked for spoofing and multi-tick layering forensics."""

    order_id: int
    account_id: int
    symbol: str
    side: SideLiteral
    price_fp: int
    initial_qty_fp: int
    remaining_qty_fp: int
    submit_ts_ns: int
    submit_bbo_dist_bps: int


@dataclass(frozen=True, slots=True)
class SpoofingLayeringEvidence:
    """Structured evidence aligned with hyperion.v1.SpoofingLayering."""

    suspect_account_id: int
    spoofed_side: SideLiteral
    layered_level_count: int
    canceled_spoof_qty_fp: int
    opposite_side_exec_qty_fp: int
    cancel_to_fill_latency_ns: int
    order_to_trade_ratio_ppm: int
    best_quote_distance_bps: int


@dataclass(frozen=True, slots=True)
class VelocitySpikeEvidence:
    """Structured evidence aligned with hyperion.v1.VelocitySpike."""

    suspect_account_id: int
    events_per_second: int
    baseline_events_per_second: int
    burst_multiplier_ppm: int
    cancel_rate_ppm: int
    notional_burst_fp: int
    window_duration_ns: int


class _AccountRollingState:
    """O(1) sliding-window and exponentially decayed microstructure counters per account."""

    __slots__ = (
        "account_id",
        "last_ts_ns",
        "submit_count",
        "modify_count",
        "cancel_count",
        "fast_cancel_count",
        "fill_count",
        "taker_fill_count",
        "submitted_qty_fp",
        "canceled_qty_fp",
        "near_bbo_canceled_qty_fp",
        "executed_qty_fp",
        "taker_executed_qty_fp",
        "buy_exec_qty_fp",
        "sell_exec_qty_fp",
        "notional_burst_fp",
        "spoof_score_ema",
        "sync_cancel_burst_ema",
        "momentum_burst_ema",
        "acct_price_impact_fp",
        "event_rate_fast_ema",
        "event_rate_slow_ema",
        "recent_event_ts",
        "recent_cancel_ts",
        "recent_layer_peaks",
        "last_opp_fill_ts_ns",
        "last_opp_fill_qty_fp",
        "last_spoof_side",
        "last_spoof_canceled_qty_fp",
        "last_spoof_opp_qty_fp",
        "last_cancel_to_fill_ns",
        "last_spoof_dist_bps",
        "open_orders_by_side_price",
        "cur_vec",
        "seq_ring",
        "seq_count",
        "last_snapshot_ts_ns",
    )

    def __init__(self, account_id: int) -> None:
        self.account_id = account_id
        self.last_ts_ns: int = 0

        self.submit_count: float = 0.0
        self.modify_count: float = 0.0
        self.cancel_count: float = 0.0
        self.fast_cancel_count: float = 0.0
        self.fill_count: float = 0.0
        self.taker_fill_count: float = 0.0

        self.submitted_qty_fp: int = 0
        self.canceled_qty_fp: int = 0
        self.near_bbo_canceled_qty_fp: int = 0
        self.executed_qty_fp: int = 0
        self.taker_executed_qty_fp: int = 0
        self.buy_exec_qty_fp: int = 0
        self.sell_exec_qty_fp: int = 0
        self.notional_burst_fp: int = 0

        self.spoof_score_ema: float = 0.0
        self.sync_cancel_burst_ema: float = 0.0
        self.momentum_burst_ema: float = 0.0
        self.acct_price_impact_fp: float = 0.0

        self.event_rate_fast_ema: float = 1.0
        self.event_rate_slow_ema: float = 2.0

        self.recent_event_ts: deque[tuple[int, int]] = deque()
        self.recent_cancel_ts: deque[tuple[int, SideLiteral, int]] = deque()
        # Monotonic deque tracking exact sliding-window peak layered tick depth over 5s
        self.recent_layer_peaks: deque[tuple[int, int]] = deque()

        self.last_opp_fill_ts_ns: dict[SideLiteral, int] = {"BUY": 0, "SELL": 0}
        self.last_opp_fill_qty_fp: dict[SideLiteral, int] = {"BUY": 0, "SELL": 0}

        self.last_spoof_side: SideLiteral = "SELL"
        self.last_spoof_canceled_qty_fp: int = 0
        self.last_spoof_opp_qty_fp: int = 0
        self.last_cancel_to_fill_ns: int = 0
        self.last_spoof_dist_bps: int = 0

        self.open_orders_by_side_price: dict[SideLiteral, dict[int, int]] = {"BUY": {}, "SELL": {}}

        self.cur_vec: npt.NDArray[np.float32] = alloc_aligned64_f32((NUM_FEATURES,))
        self.cur_vec.fill(0.0)
        self.seq_ring: npt.NDArray[np.float32] = alloc_aligned64_f32(
            (TEMPORAL_SEQ_LEN, NUM_FEATURES)
        )
        self.seq_ring.fill(0.0)
        self.seq_count: int = 0
        self.last_snapshot_ts_ns: int = 0


class StreamingMicrostructureExtractor:
    """Extracts 16-D microstructure + graph behavioral tensors per account in O(1) time."""

    def __init__(
        self,
        graph: SlidingTemporalMultigraph,
        *,
        window_ns: int = 30_000_000_000,
        fast_cancel_threshold_ns: int = 500_000_000,
        spoof_opp_window_ns: int = 2_500_000_000,
        sync_cancel_window_ns: int = 400_000_000,
        layer_peak_window_ns: int = 5_000_000_000,
        near_bbo_bps_limit: int = 25,
        snapshot_min_interval_ns: int = 50_000_000,
    ) -> None:
        self.graph = graph
        self.window_ns = int(window_ns)
        self.fast_cancel_threshold_ns = int(fast_cancel_threshold_ns)
        self.spoof_opp_window_ns = int(spoof_opp_window_ns)
        self.sync_cancel_window_ns = int(sync_cancel_window_ns)
        self.layer_peak_window_ns = int(layer_peak_window_ns)
        self.near_bbo_bps_limit = int(near_bbo_bps_limit)
        self.snapshot_min_interval_ns = int(snapshot_min_interval_ns)

        self._accounts: dict[int, _AccountRollingState] = {}
        self._active_orders: dict[int, ActiveRestingOrder] = {}

        self._bbo_by_symbol: dict[str, tuple[int, int]] = {}
        self._last_trade_price_fp: dict[str, int] = {}
        self._market_price_impact_ema: float = 1e8

    def update_bbo(self, symbol: str, best_bid_fp: int, best_ask_fp: int) -> None:
        """Update top-of-book reference quotes (scale 1e8 integers)."""
        require_uint64(best_bid_fp, "best_bid_fp", allow_zero=True)
        require_uint64(best_ask_fp, "best_ask_fp", allow_zero=True)
        self._bbo_by_symbol[symbol] = (best_bid_fp, best_ask_fp)

    def _get_or_create_account(self, account_id: int) -> _AccountRollingState:
        st = self._accounts.get(account_id)
        if st is None:
            st = _AccountRollingState(account_id)
            self._accounts[account_id] = st
        return st

    def _record_layer_depth(self, st: _AccountRollingState, ts_ns: int, depth: int) -> None:
        cutoff = ts_ns - self.layer_peak_window_ns
        dq = st.recent_layer_peaks
        while dq and dq[0][0] < cutoff:
            dq.popleft()
        while dq and dq[-1][1] <= depth:
            dq.pop()
        dq.append((ts_ns, depth))

    def _current_layer_peak(self, st: _AccountRollingState, ts_ns: int) -> int:
        cutoff = ts_ns - self.layer_peak_window_ns
        dq = st.recent_layer_peaks
        while dq and dq[0][0] < cutoff:
            dq.popleft()
        peak_hist = dq[0][1] if dq else 0
        active_buy = len(st.open_orders_by_side_price["BUY"])
        active_sell = len(st.open_orders_by_side_price["SELL"])
        return max(active_buy, active_sell, peak_hist)

    def _apply_decay(self, st: _AccountRollingState, ts_ns: int) -> None:
        if st.last_ts_ns == 0 or ts_ns <= st.last_ts_ns:
            st.last_ts_ns = max(st.last_ts_ns, ts_ns)
            return
        dt_ns = ts_ns - st.last_ts_ns
        st.last_ts_ns = ts_ns

        decay = math.exp(-dt_ns / self.window_ns)
        st.submit_count *= decay
        st.modify_count *= decay
        st.cancel_count *= decay
        st.fast_cancel_count *= decay
        st.fill_count *= decay
        st.taker_fill_count *= decay

        st.submitted_qty_fp = int(st.submitted_qty_fp * decay)
        st.canceled_qty_fp = int(st.canceled_qty_fp * decay)
        st.near_bbo_canceled_qty_fp = int(st.near_bbo_canceled_qty_fp * decay)
        st.executed_qty_fp = int(st.executed_qty_fp * decay)
        st.taker_executed_qty_fp = int(st.taker_executed_qty_fp * decay)
        st.buy_exec_qty_fp = int(st.buy_exec_qty_fp * decay)
        st.sell_exec_qty_fp = int(st.sell_exec_qty_fp * decay)

        fast_decay = math.exp(-dt_ns / 5_000_000_000)
        st.spoof_score_ema *= fast_decay
        st.sync_cancel_burst_ema *= fast_decay
        st.momentum_burst_ema *= fast_decay
        st.acct_price_impact_fp *= decay

        one_sec_cutoff = ts_ns - 1_000_000_000
        while st.recent_event_ts and st.recent_event_ts[0][0] < one_sec_cutoff:
            _, old_notional = st.recent_event_ts.popleft()
            st.notional_burst_fp = max(0, st.notional_burst_fp - old_notional)

        inst_eps = float(len(st.recent_event_ts))
        alpha_slow = 1.0 - decay
        alpha_fast = 1.0 - math.exp(-dt_ns / 1_000_000_000)
        st.event_rate_fast_ema = (1.0 - alpha_fast) * st.event_rate_fast_ema + alpha_fast * inst_eps
        slow_target = min(inst_eps, 25.0)
        st.event_rate_slow_ema = max(
            0.5, (1.0 - alpha_slow) * st.event_rate_slow_ema + alpha_slow * slow_target
        )

    def _record_event_arrival(
        self, st: _AccountRollingState, ts_ns: int, notional_fp: int
    ) -> None:
        self._apply_decay(st, ts_ns)
        st.recent_event_ts.append((ts_ns, notional_fp))
        st.notional_burst_fp += notional_fp
        one_sec_cutoff = ts_ns - 1_000_000_000
        while st.recent_event_ts and st.recent_event_ts[0][0] < one_sec_cutoff:
            _, old_notional = st.recent_event_ts.popleft()
            st.notional_burst_fp = max(0, st.notional_burst_fp - old_notional)
        st.event_rate_fast_ema = max(st.event_rate_fast_ema, float(len(st.recent_event_ts)))

    def _quote_distance_bps(self, symbol: str, side: SideLiteral, price_fp: int) -> int:
        if price_fp == 0:
            return 0
        bbo = self._bbo_by_symbol.get(symbol)
        if bbo is None:
            last_px = self._last_trade_price_fp.get(symbol, price_fp)
            return distance_bps_int(price_fp, last_px) if last_px > 0 else 0
        best_bid_fp, best_ask_fp = bbo
        ref_fp = best_bid_fp if side == "BUY" else best_ask_fp
        if ref_fp <= 0:
            ref_fp = best_ask_fp if best_ask_fp > 0 else price_fp
        return distance_bps_int(price_fp, ref_fp) if ref_fp > 0 else 0

    def on_order_submit(
        self,
        *,
        order_id: int,
        account_id: int,
        symbol: str,
        side: SideLiteral,
        price_fp: int,
        qty_fp: int,
        ts_ns: int,
        is_resting_limit: bool = True,
    ) -> npt.NDArray[np.float32]:
        """Process a SubmitOrder / L3 ADD event and return contiguous [8, 16] sequence."""
        require_uint64(order_id, "order_id", allow_zero=True)
        require_uint64(account_id, "account_id", allow_zero=False)
        require_uint64(price_fp, "price_fp", allow_zero=True)
        require_uint64(qty_fp, "qty_fp", allow_zero=False)
        require_uint64(ts_ns, "ts_ns", allow_zero=True)

        ref_px = price_fp if price_fp > 0 else self._last_trade_price_fp.get(symbol, 100_000_000)
        notional_fp = compute_notional_fp(max(1, ref_px), qty_fp)
        st = self._get_or_create_account(account_id)
        self._record_event_arrival(st, ts_ns, notional_fp)

        st.submit_count += 1.0
        st.submitted_qty_fp += qty_fp

        dist_bps = self._quote_distance_bps(symbol, side, price_fp)
        if is_resting_limit and price_fp > 0 and order_id > 0:
            self._active_orders[order_id] = ActiveRestingOrder(
                order_id=order_id,
                account_id=account_id,
                symbol=symbol,
                side=side,
                price_fp=price_fp,
                initial_qty_fp=qty_fp,
                remaining_qty_fp=qty_fp,
                submit_ts_ns=ts_ns,
                submit_bbo_dist_bps=dist_bps,
            )
            side_levels = st.open_orders_by_side_price[side]
            side_levels[price_fp] = side_levels.get(price_fp, 0) + 1
            self._record_layer_depth(st, ts_ns, len(side_levels))

        return self._snapshot_account_sequence(st, ts_ns)

    def on_order_modify(
        self,
        *,
        order_id: int,
        account_id: int,
        symbol: str,
        new_price_fp: int,
        new_qty_fp: int,
        ts_ns: int,
    ) -> npt.NDArray[np.float32]:
        """Process a ModifyOrder / L3 MODIFY event."""
        require_uint64(account_id, "account_id", allow_zero=False)
        require_uint64(new_price_fp, "new_price_fp", allow_zero=False)
        require_uint64(new_qty_fp, "new_qty_fp", allow_zero=False)
        require_uint64(ts_ns, "ts_ns", allow_zero=True)

        notional_fp = compute_notional_fp(new_price_fp, new_qty_fp)
        st = self._get_or_create_account(account_id)
        self._record_event_arrival(st, ts_ns, notional_fp)
        st.modify_count += 1.0

        existing = self._active_orders.get(order_id)
        if existing is not None:
            old_levels = st.open_orders_by_side_price[existing.side]
            old_cnt = old_levels.get(existing.price_fp, 0) - 1
            if old_cnt <= 0:
                old_levels.pop(existing.price_fp, None)
            else:
                old_levels[existing.price_fp] = old_cnt

            existing.price_fp = new_price_fp
            existing.remaining_qty_fp = new_qty_fp
            existing.submit_bbo_dist_bps = self._quote_distance_bps(
                symbol, existing.side, new_price_fp
            )
            old_levels[new_price_fp] = old_levels.get(new_price_fp, 0) + 1
            self._record_layer_depth(st, ts_ns, len(old_levels))

        return self._snapshot_account_sequence(st, ts_ns)

    def on_order_cancel(
        self,
        *,
        order_id: int,
        account_id: int,
        symbol: str,
        ts_ns: int,
        canceled_qty_override_fp: int = 0,
        side_hint: SideLiteral = "SELL",
        price_hint_fp: int = 0,
    ) -> npt.NDArray[np.float32]:
        """Process a CancelOrder / L3 CANCEL event and score spoofing & layering bursts."""
        require_uint64(account_id, "account_id", allow_zero=False)
        require_uint64(ts_ns, "ts_ns", allow_zero=True)
        require_uint64(canceled_qty_override_fp, "canceled_qty_override_fp", allow_zero=True)
        require_uint64(price_hint_fp, "price_hint_fp", allow_zero=True)

        existing = self._active_orders.pop(order_id, None)
        if existing is not None:
            canceled_qty_fp = existing.remaining_qty_fp
            side = existing.side
            price_fp = existing.price_fp
            lifetime_ns = max(0, ts_ns - existing.submit_ts_ns)
            cur_dist = self._quote_distance_bps(symbol, side, price_fp)
            dist_bps = min(existing.submit_bbo_dist_bps, cur_dist)
        else:
            canceled_qty_fp = canceled_qty_override_fp
            side = side_hint
            price_fp = price_hint_fp
            lifetime_ns = 100_000_000
            dist_bps = self._quote_distance_bps(symbol, side, price_fp) if price_fp > 0 else 10

        ref_px = price_fp if price_fp > 0 else self._last_trade_price_fp.get(symbol, 100_000_000)
        notional_fp = (
            compute_notional_fp(max(1, ref_px), max(1, canceled_qty_fp))
            if canceled_qty_fp > 0
            else 0
        )
        st = self._get_or_create_account(account_id)
        self._record_event_arrival(st, ts_ns, notional_fp)

        if existing is not None:
            side_levels = st.open_orders_by_side_price[side]
            cnt = side_levels.get(price_fp, 0) - 1
            if cnt <= 0:
                side_levels.pop(price_fp, None)
            else:
                side_levels[price_fp] = cnt

        st.cancel_count += 1.0
        st.canceled_qty_fp += canceled_qty_fp
        if lifetime_ns <= self.fast_cancel_threshold_ns:
            st.fast_cancel_count += 1.0

        if dist_bps <= self.near_bbo_bps_limit:
            st.near_bbo_canceled_qty_fp += canceled_qty_fp

        # Classic Spoofing: large near-BBO cancel shortly after opposite-side execution
        opp_side: SideLiteral = "BUY" if side == "SELL" else "SELL"
        opp_ts = st.last_opp_fill_ts_ns[opp_side]
        opp_qty = st.last_opp_fill_qty_fp[opp_side]
        if opp_ts > 0 and opp_qty > 0 and ts_ns >= opp_ts:
            cancel_after_fill_ns = ts_ns - opp_ts
            if (
                cancel_after_fill_ns <= self.spoof_opp_window_ns
                and dist_bps <= self.near_bbo_bps_limit * 2
            ):
                size_ratio = min(5.0, canceled_qty_fp / max(1, opp_qty))
                time_weight = math.exp(-cancel_after_fill_ns / (self.spoof_opp_window_ns * 0.5))
                prox_weight = math.exp(-dist_bps / max(1.0, float(self.near_bbo_bps_limit)))
                spoof_impulse = size_ratio * time_weight * prox_weight
                st.spoof_score_ema = min(10.0, st.spoof_score_ema + spoof_impulse)
                st.last_spoof_side = side
                st.last_spoof_canceled_qty_fp += canceled_qty_fp
                st.last_spoof_opp_qty_fp = opp_qty
                st.last_cancel_to_fill_ns = cancel_after_fill_ns
                st.last_spoof_dist_bps = dist_bps

        # Synchronized Multi-Tick Layering Cancel Burst
        st.recent_cancel_ts.append((ts_ns, side, price_fp))
        sync_cutoff = ts_ns - self.sync_cancel_window_ns
        while st.recent_cancel_ts and st.recent_cancel_ts[0][0] < sync_cutoff:
            st.recent_cancel_ts.popleft()
        distinct_canceled_px = {px for _, s, px in st.recent_cancel_ts if s == side and px > 0}
        if len(distinct_canceled_px) >= 3:
            burst_inc = 0.5 * len(distinct_canceled_px)
            st.sync_cancel_burst_ema = min(10.0, st.sync_cancel_burst_ema + burst_inc)
            st.last_spoof_side = side

        return self._snapshot_account_sequence(st, ts_ns)

    def on_trade_fill(
        self,
        *,
        fill_id: int,
        symbol: str,
        maker_account_id: int,
        taker_account_id: int,
        aggressor_side: SideLiteral,
        price_fp: int,
        qty_fp: int,
        exec_ts_ns: int,
        maker_order_id: int = 0,
        engine_seq: int = 0,
    ) -> tuple[npt.NDArray[np.float32], npt.NDArray[np.float32]]:
        """Update microstructure metrics for both maker and taker on trade execution."""
        require_uint64(maker_account_id, "maker_account_id", allow_zero=False)
        require_uint64(taker_account_id, "taker_account_id", allow_zero=False)
        require_uint64(price_fp, "price_fp", allow_zero=False)
        require_uint64(qty_fp, "qty_fp", allow_zero=False)
        require_uint64(exec_ts_ns, "exec_ts_ns", allow_zero=True)

        prev_px = self._last_trade_price_fp.get(symbol, price_fp)
        abs_dp_fp = abs(price_fp - prev_px)
        self._last_trade_price_fp[symbol] = price_fp
        self._market_price_impact_ema = max(
            1e6, 0.98 * self._market_price_impact_ema + float(abs_dp_fp)
        )

        notional_fp = compute_notional_fp(price_fp, qty_fp)
        maker_side: SideLiteral = "SELL" if aggressor_side == "BUY" else "BUY"
        taker_side: SideLiteral = aggressor_side

        if maker_order_id > 0:
            resting = self._active_orders.get(maker_order_id)
            if resting is not None:
                if qty_fp >= resting.remaining_qty_fp:
                    self._active_orders.pop(maker_order_id, None)
                    maker_st_tmp = self._get_or_create_account(maker_account_id)
                    lvls = maker_st_tmp.open_orders_by_side_price[resting.side]
                    cnt = lvls.get(resting.price_fp, 0) - 1
                    if cnt <= 0:
                        lvls.pop(resting.price_fp, None)
                    else:
                        lvls[resting.price_fp] = cnt
                else:
                    resting.remaining_qty_fp -= qty_fp

        m_st = self._get_or_create_account(maker_account_id)
        self._apply_decay(m_st, exec_ts_ns)
        m_st.fill_count += 1.0
        m_st.executed_qty_fp += qty_fp
        if maker_side == "BUY":
            m_st.buy_exec_qty_fp += qty_fp
        else:
            m_st.sell_exec_qty_fp += qty_fp
        m_st.last_opp_fill_ts_ns[maker_side] = exec_ts_ns
        m_st.last_opp_fill_qty_fp[maker_side] = qty_fp

        t_st = self._get_or_create_account(taker_account_id)
        self._record_event_arrival(t_st, exec_ts_ns, notional_fp)
        t_st.fill_count += 1.0
        t_st.taker_fill_count += 1.0
        t_st.executed_qty_fp += qty_fp
        t_st.taker_executed_qty_fp += qty_fp
        if taker_side == "BUY":
            t_st.buy_exec_qty_fp += qty_fp
        else:
            t_st.sell_exec_qty_fp += qty_fp
        t_st.last_opp_fill_ts_ns[taker_side] = exec_ts_ns
        t_st.last_opp_fill_qty_fp[taker_side] = qty_fp
        t_st.acct_price_impact_fp += float(abs_dp_fp)

        if abs_dp_fp > 0 and prev_px > 0:
            rel_move_bps = (abs_dp_fp * 10_000.0) / prev_px
            vel_factor = min(5.0, len(t_st.recent_event_ts) / 4.0)
            mom_inc = 0.25 * rel_move_bps * vel_factor
            t_st.momentum_burst_ema = min(15.0, t_st.momentum_burst_ema + mom_inc)

        m_seq = self._snapshot_account_sequence(m_st, exec_ts_ns)
        t_seq = (
            m_seq
            if maker_account_id == taker_account_id
            else self._snapshot_account_sequence(t_st, exec_ts_ns)
        )
        return m_seq, t_seq

    def compute_current_features(
        self, account_id: int, now_ns: int | None = None
    ) -> npt.NDArray[np.float32]:
        """Compute current 16-dimensional float32 behavioral feature vector in-place."""
        st = self._accounts.get(account_id)
        if st is None:
            return np.zeros(NUM_FEATURES, dtype=np.float32)

        vec = st.cur_vec
        ts_ref = st.last_ts_ns if now_ns is None else max(st.last_ts_ns, now_ns)
        total_orders = st.submit_count + st.modify_count
        denom_fills = max(1.0, st.fill_count)

        vec[0] = math.log1p(max(0.0, total_orders / denom_fills))

        exec_denom = float(st.executed_qty_fp) if st.executed_qty_fp > 0 else 1e8
        vec[1] = math.log1p(max(0.0, st.submitted_qty_fp / max(1.0, exec_denom)))

        vec[2] = min(1.0, max(0.0, st.cancel_count / max(1.0, st.submit_count)))

        fast_rate = (
            st.fast_cancel_count / max(1.0, st.cancel_count) if st.cancel_count >= 0.5 else 0.0
        )
        vec[3] = min(1.0, max(0.0, fast_rate))

        vec[4] = min(10.0, max(0.0, st.spoof_score_ema))

        near_share = (
            st.near_bbo_canceled_qty_fp / max(1.0, float(st.canceled_qty_fp))
            if st.canceled_qty_fp > 0
            else 0.0
        )
        vec[5] = min(1.0, max(0.0, near_share))

        vec[6] = min(12.0, float(self._current_layer_peak(st, ts_ref)))
        vec[7] = min(10.0, max(0.0, st.sync_cancel_burst_ema))

        self_impact = st.acct_price_impact_fp / max(1e6, self._market_price_impact_ema)
        vec[8] = min(1.0, max(0.0, self_impact))
        vec[9] = min(15.0, max(0.0, st.momentum_burst_ema))

        wash_score, wash_depth_norm, cp_conc = self.graph.get_account_graph_metrics(
            account_id, ts_ref
        )
        vec[10] = wash_score
        vec[11] = wash_depth_norm
        vec[12] = cp_conc

        gross_vol = st.buy_exec_qty_fp + st.sell_exec_qty_fp
        if gross_vol > 0 and st.fill_count >= 2.0:
            net_imbalance = abs(st.buy_exec_qty_fp - st.sell_exec_qty_fp) / gross_vol
            vec[13] = float((1.0 - net_imbalance) * min(1.0, st.fill_count / 4.0))
        else:
            vec[13] = 0.0

        vel_ratio = st.event_rate_fast_ema / max(1.0, st.event_rate_slow_ema)
        vec[14] = min(10.0, math.log1p(max(0.0, vel_ratio - 1.0)))

        taker_share = (
            st.taker_executed_qty_fp / max(1.0, float(st.executed_qty_fp))
            if st.executed_qty_fp > 0
            else 0.0
        )
        vec[15] = min(1.0, max(0.0, taker_share))

        return vec

    def _snapshot_account_sequence(
        self, st: _AccountRollingState, ts_ns: int
    ) -> npt.NDArray[np.float32]:
        """Update chronological [T=8, F=16] buffer in-place via 448-byte memmove (zero alloc)."""
        vec = self.compute_current_features(st.account_id, ts_ns)
        if st.seq_count == 0:
            st.seq_ring[:] = vec
            st.seq_count = TEMPORAL_SEQ_LEN
            st.last_snapshot_ts_ns = ts_ns
        elif ts_ns - st.last_snapshot_ts_ns >= self.snapshot_min_interval_ns:
            st.seq_ring[:-1] = st.seq_ring[1:]
            st.seq_ring[-1] = vec
            st.last_snapshot_ts_ns = ts_ns
        else:
            st.seq_ring[-1] = vec

        return st.seq_ring

    def get_account_sequence(
        self, account_id: int, now_ns: int | None = None
    ) -> npt.NDArray[np.float32]:
        st = self._accounts.get(account_id)
        if st is None:
            return np.zeros((TEMPORAL_SEQ_LEN, NUM_FEATURES), dtype=np.float32)
        if now_ns is not None and now_ns > st.last_ts_ns:
            self._apply_decay(st, now_ns)
            st.seq_ring[-1] = self.compute_current_features(account_id, now_ns)
        return st.seq_ring

    def build_spoofing_evidence(self, account_id: int) -> SpoofingLayeringEvidence:
        st = self._accounts.get(account_id)
        if st is None:
            return SpoofingLayeringEvidence(
                suspect_account_id=account_id,
                spoofed_side="SELL",
                layered_level_count=0,
                canceled_spoof_qty_fp=0,
                opposite_side_exec_qty_fp=0,
                cancel_to_fill_latency_ns=0,
                order_to_trade_ratio_ppm=0,
                best_quote_distance_bps=0,
            )
        otr_ratio = (st.submit_count + st.modify_count) / max(1.0, st.fill_count)
        layered_lvls = self._current_layer_peak(st, st.last_ts_ns)
        canceled_spoof = (
            st.last_spoof_canceled_qty_fp
            if st.last_spoof_canceled_qty_fp > 0
            else st.near_bbo_canceled_qty_fp
        )
        opp_qty = st.last_spoof_opp_qty_fp if st.last_spoof_opp_qty_fp > 0 else st.executed_qty_fp
        return SpoofingLayeringEvidence(
            suspect_account_id=account_id,
            spoofed_side=st.last_spoof_side,
            layered_level_count=layered_lvls,
            canceled_spoof_qty_fp=max(0, int(canceled_spoof)),
            opposite_side_exec_qty_fp=max(0, int(opp_qty)),
            cancel_to_fill_latency_ns=max(0, int(st.last_cancel_to_fill_ns)),
            order_to_trade_ratio_ppm=max(0, ratio_to_ppm(otr_ratio)),
            best_quote_distance_bps=max(0, int(st.last_spoof_dist_bps)),
        )

    def build_velocity_evidence(self, account_id: int) -> VelocitySpikeEvidence:
        st = self._accounts.get(account_id)
        if st is None:
            return VelocitySpikeEvidence(
                suspect_account_id=account_id,
                events_per_second=0,
                baseline_events_per_second=1,
                burst_multiplier_ppm=PPM_SCALE,
                cancel_rate_ppm=0,
                notional_burst_fp=0,
                window_duration_ns=1_000_000_000,
            )
        eps = max(int(round(st.event_rate_fast_ema)), len(st.recent_event_ts))
        base_eps = max(1, int(round(st.event_rate_slow_ema)))
        burst_ppm = ratio_to_ppm(float(eps), float(base_eps))
        cancel_ppm = ratio_to_ppm(st.cancel_count, max(1.0, st.submit_count), clamp_unit=True)
        return VelocitySpikeEvidence(
            suspect_account_id=account_id,
            events_per_second=eps,
            baseline_events_per_second=base_eps,
            burst_multiplier_ppm=max(0, burst_ppm),
            cancel_rate_ppm=cancel_ppm,
            notional_burst_fp=max(0, int(st.notional_burst_fp)),
            window_duration_ns=1_000_000_000,
        )

    def get_account_summary_metrics(self, account_id: int) -> tuple[int, int]:
        """Return (order_to_trade_ratio_ppm, cancel_rate_ppm) for AccountNodeSummary."""
        st = self._accounts.get(account_id)
        if st is None:
            return 0, 0
        otr_ppm = ratio_to_ppm(st.submit_count + st.modify_count, max(1.0, st.fill_count))
        cancel_ppm = ratio_to_ppm(st.cancel_count, max(1.0, st.submit_count), clamp_unit=True)
        return max(0, otr_ppm), max(0, cancel_ppm)
