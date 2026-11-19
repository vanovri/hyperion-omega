"""
Deterministic Microstructure Tape Generator & Chronos/Stream-Aggregator Replay Adapter.

Generates realistic Level-2 multi-level order book trajectories with:
- Regime-switching stochastic volatility & jump-diffusion innovations
- Self-exciting Hawkes-like aggressive order arrival bursts
- Cont-Kukanov-Stoikov Order Flow Imbalance (OFI) & Stoikov Microprice dynamics
- Non-trivial causal microstructure drift (alpha ground-truth hidden under realistic low SNR)
- Binary `HYPWAL01` header verification & `MicrostructureMetrics` dict/JSON replay ingestion
  aligned with Channel 04 (`stream-aggregator`) and Channel 08 (`chronos-replay`).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import struct
from typing import Any
import numpy as np
from numpy.typing import NDArray

from alpha_evo.fixed_point import CANONICAL_INSTRUMENTS, FP_SCALE

HYPWAL_MAGIC = b"HYPWAL01"
HYPSNP_MAGIC = b"HYPSNP01"


@dataclass(frozen=True, slots=True)
class MarketGenConfig:
    """Configuration for deterministic microstructure tape generation."""

    symbol: str = "BTC-USDT"
    n_steps: int = 12_000
    step_ms: float = 100.0
    n_levels: int = 10
    base_spread_ticks: int = 40           # $0.40 (~0.58 bps realistic institutional BTC-USDT L2 spread)
    base_depth_lots: int = 2_500          # 2,500 * 0.00001 BTC = 0.025 BTC per top level
    hawkes_baseline: float = 0.35
    hawkes_alpha: float = 0.42
    hawkes_beta: float = 0.65
    volatility_ticks_per_step: float = 180.0  # $1.80 per 100ms bar (~0.26 bps/bar, ~0.83 bps/1s -> ~50% ann vol)
    alpha_signal_strength: float = 0.28       # Realistic institutional HFT microstructure SNR (IC ~ 0.10 - 0.22)
    seed: int = 20261119


@dataclass(slots=True)
class MarketDataset:
    """
    Vectorized columnar microstructure tape aligned with `stream_analytics.proto`
    and `orderbook.proto` (all `_fp` columns are `int64` scaled by `1e8`).
    """

    symbol: str
    ts_ns: NDArray[np.int64]
    mid_price_fp: NDArray[np.int64]
    best_bid_fp: NDArray[np.int64]
    best_ask_fp: NDArray[np.int64]
    spread_fp: NDArray[np.int64]
    spread_bps: NDArray[np.float64]
    bid_qty_l1_fp: NDArray[np.int64]
    ask_qty_l1_fp: NDArray[np.int64]
    bid_depth_5_fp: NDArray[np.int64]
    ask_depth_5_fp: NDArray[np.int64]
    bid_depth_10_fp: NDArray[np.int64]
    ask_depth_10_fp: NDArray[np.int64]
    taker_buy_qty_fp: NDArray[np.int64]
    taker_sell_qty_fp: NDArray[np.int64]
    rolling_vwap_fp: NDArray[np.int64]
    ofi_norm: NDArray[np.float64]
    obi_5: NDArray[np.float64]
    obi_10: NDArray[np.float64]
    trade_imbalance: NDArray[np.float64]
    vwap_dev: NDArray[np.float64]
    realized_vol: NDArray[np.float64]
    forward_return_bps: NDArray[np.float64]
    tick_size_fp: int
    lot_size_fp: int

    def to_feature_dict(self) -> dict[str, NDArray[np.float64]]:
        """Return dimensionless causal feature dictionary for GP Symbolic Regression."""
        mid_f = self.mid_price_fp.astype(np.float64) / FP_SCALE
        mid_ret = np.zeros_like(mid_f)
        mid_ret[1:] = (mid_f[1:] - mid_f[:-1]) / np.maximum(mid_f[:-1], 1e-8) * 1e4
        return {
            "mid_ret": mid_ret,
            "spread_bps": self.spread_bps.copy(),
            "ofi": self.ofi_norm.copy(),
            "obi_5": self.obi_5.copy(),
            "obi_10": self.obi_10.copy(),
            "trade_imb": self.trade_imbalance.copy(),
            "vwap_dev": self.vwap_dev.copy(),
            "volatility": self.realized_vol.copy(),
            "bid_px": self.best_bid_fp.astype(np.float64) / FP_SCALE,
            "ask_px": self.best_ask_fp.astype(np.float64) / FP_SCALE,
            "bid_sz": self.bid_qty_l1_fp.astype(np.float64) / FP_SCALE,
            "ask_sz": self.ask_qty_l1_fp.astype(np.float64) / FP_SCALE,
        }


def generate_microstructure_dataset(
    cfg: MarketGenConfig | None = None,
    pred_horizon_bars: int = 10,
) -> MarketDataset:
    """
    Generate a realistic, strictly causal synthetic limit order book microstructure dataset
    exhibiting empirically documented HFT stylized facts:
    1. Clustered volatility & Hawkes order flow bursts
    2. Bid-ask bounce & transient spread widening during toxic flow
    3. Latent microstructure predictability driven by non-linear interaction of:
       - Cont-Kukanov-Stoikov OFI impulse + linear decay
       - Stoikov microprice skew (`(bid_sz - ask_sz) / (bid_sz + ask_sz)`) gated by narrow/normal spreads
       - Trade aggression confirmation (`taker_buy - taker_sell`)
    """
    if cfg is None:
        cfg = MarketGenConfig()

    spec = CANONICAL_INSTRUMENTS.get(cfg.symbol, CANONICAL_INSTRUMENTS["BTC-USDT"])
    rng = np.random.default_rng(cfg.seed)
    n = cfg.n_steps
    tick_fp = spec.tick_size_fp
    lot_fp = spec.lot_size_fp
    base_depth_fp = cfg.base_depth_lots * lot_fp

    # 1. Regime-switching volatility & bivariate Hawkes aggressive arrival intensities
    vol_regime = np.empty(n, dtype=np.float64)
    lam_buy = np.empty(n, dtype=np.float64)
    lam_sell = np.empty(n, dtype=np.float64)

    bid_qty_l1 = np.empty(n, dtype=np.int64)
    ask_qty_l1 = np.empty(n, dtype=np.int64)
    bid_d5 = np.empty(n, dtype=np.int64)
    ask_d5 = np.empty(n, dtype=np.int64)
    bid_d10 = np.empty(n, dtype=np.int64)
    ask_d10 = np.empty(n, dtype=np.int64)
    spread_ticks = np.empty(n, dtype=np.int64)
    taker_buy = np.empty(n, dtype=np.int64)
    taker_sell = np.empty(n, dtype=np.int64)
    mid_ticks = np.empty(n, dtype=np.int64)

    z_noise = rng.standard_normal(n)
    z_imb = rng.standard_normal(n)
    z_depth_b = rng.lognormal(mean=0.0, sigma=0.38, size=n)
    z_depth_a = rng.lognormal(mean=0.0, sigma=0.38, size=n)
    u_jump = rng.random(n)
    jump_sign = rng.choice(np.array([-1, 1], dtype=np.int64), size=n)

    cur_mid_ticks = spec.reference_price_fp // tick_fp
    cur_vol = cfg.volatility_ticks_per_step
    cur_lb = cfg.hawkes_baseline
    cur_ls = cfg.hawkes_baseline
    latent_imb = 0.0
    ema_ofi = 0.0
    prev_bp = cur_mid_ticks - 1
    prev_ap = cur_mid_ticks + 1
    prev_bq = float(base_depth_fp)
    prev_aq = float(base_depth_fp)

    for t in range(n):
        # GARCH-like stochastic volatility state
        cur_vol = 0.96 * cur_vol + 0.04 * cfg.volatility_ticks_per_step * (0.5 + abs(z_noise[t]))
        vol_regime[t] = cur_vol

        # Persistent latent order-book pressure (mean-reverting OU state)
        latent_imb = 0.84 * latent_imb + 0.52 * z_imb[t]
        skew_factor = np.tanh(latent_imb)

        # Top-of-book & multi-level depths respond to latent book pressure
        bq1 = max(lot_fp * 20, int(base_depth_fp * z_depth_b[t] * np.exp(0.55 * skew_factor)))
        aq1 = max(lot_fp * 20, int(base_depth_fp * z_depth_a[t] * np.exp(-0.55 * skew_factor)))
        bq1 = (bq1 // lot_fp) * lot_fp
        aq1 = (aq1 // lot_fp) * lot_fp

        bid_qty_l1[t] = bq1
        ask_qty_l1[t] = aq1
        bid_d5[t] = bq1 * 4 + int(base_depth_fp * 1.5 * z_depth_b[t])
        ask_d5[t] = aq1 * 4 + int(base_depth_fp * 1.5 * z_depth_a[t])
        bid_d10[t] = bid_d5[t] * 2
        ask_d10[t] = ask_d5[t] * 2

        # Spread widens during high volatility / toxic imbalance bursts
        sp = cfg.base_spread_ticks + int(cur_vol > 1.75 * cfg.volatility_ticks_per_step) * 2
        if abs(skew_factor) > 0.82:
            sp += 2
        spread_ticks[t] = sp

        # Hawkes intensities modulated by latent directional pressure
        cur_lb = cfg.hawkes_baseline + (cur_lb - cfg.hawkes_baseline) * np.exp(-cfg.hawkes_beta)
        cur_ls = cfg.hawkes_baseline + (cur_ls - cfg.hawkes_baseline) * np.exp(-cfg.hawkes_beta)
        lam_buy[t] = max(0.05, cur_lb * np.exp(0.45 * skew_factor))
        lam_sell[t] = max(0.05, cur_ls * np.exp(-0.45 * skew_factor))

        n_buy_events = rng.poisson(lam_buy[t])
        n_sell_events = rng.poisson(lam_sell[t])
        if n_buy_events > 0:
            cur_lb += cfg.hawkes_alpha * min(n_buy_events, 4)
        if n_sell_events > 0:
            cur_ls += cfg.hawkes_alpha * min(n_sell_events, 4)

        tb = (n_buy_events * (base_depth_fp // 3) // lot_fp) * lot_fp
        ts = (n_sell_events * (base_depth_fp // 3) // lot_fp) * lot_fp
        taker_buy[t] = tb
        taker_sell[t] = ts

        # Current quotes before next step innovation
        bp = cur_mid_ticks - (sp // 2)
        ap = bp + sp

        # Cont-Kukanov-Stoikov tick OFI normalized by base_depth_fp
        e_bid = (bq1 if bp >= prev_bp else 0.0) - (prev_bq if bp <= prev_bp else 0.0)
        e_ask = (aq1 if ap <= prev_ap else 0.0) - (prev_aq if ap >= prev_ap else 0.0)
        raw_ofi = (e_bid - e_ask) / float(base_depth_fp)
        ema_ofi = 0.72 * ema_ofi + 0.28 * np.clip(raw_ofi, -4.0, 4.0)

        micro_skew = (bq1 - aq1) / float(bq1 + aq1 + 1)
        aggr_imb = (tb - ts) / float(tb + ts + base_depth_fp)

        # Causal microstructure drift + diffusive noise + occasional jump
        # Notice how microprice skew + decaying OFI + aggressive flow jointly drive next-tick mid!
        alpha_drift = cfg.alpha_signal_strength * (
            1.25 * micro_skew
            + 0.95 * np.tanh(ema_ofi)
            + 0.65 * aggr_imb
        ) / (1.0 + 0.25 * (sp - cfg.base_spread_ticks))

        jump = int(jump_sign[t] * 4) if u_jump[t] < 0.004 else 0
        mid_ticks[t] = cur_mid_ticks

        delta_mid = int(np.round(alpha_drift * cur_vol + cur_vol * z_noise[t])) + jump
        cur_mid_ticks = max(1_000, cur_mid_ticks + delta_mid)

        prev_bp = bp
        prev_ap = ap
        prev_bq = float(bq1)
        prev_aq = float(aq1)

    mid_price_fp = mid_ticks * tick_fp
    half_spread_fp = (spread_ticks // 2) * tick_fp
    best_bid_fp = mid_price_fp - half_spread_fp
    best_ask_fp = best_bid_fp + spread_ticks * tick_fp
    spread_fp = best_ask_fp - best_bid_fp
    spread_bps = (spread_fp.astype(np.float64) / np.maximum(mid_price_fp.astype(np.float64), 1.0)) * 1e4

    # Rolling causal 30-step VWAP in fixed-point
    trade_vol = (taker_buy + taker_sell).astype(np.float64)
    mid_float = mid_price_fp.astype(np.float64)
    notional_proxy = mid_float * np.maximum(trade_vol, float(lot_fp))
    w_vwap = 30
    kernel = np.ones(w_vwap, dtype=np.float64)
    num_vwap = np.convolve(notional_proxy, kernel, mode="full")[:n]
    den_vwap = np.convolve(np.maximum(trade_vol, float(lot_fp)), kernel, mode="full")[:n]
    rolling_vwap_fp = np.round(num_vwap / np.maximum(den_vwap, 1.0)).astype(np.int64)

    # Normalized Cont-Kukanov-Stoikov OFI series
    ofi_fp = np.zeros(n, dtype=np.float64)
    bp_diff = np.diff(best_bid_fp, prepend=best_bid_fp[0])
    ap_diff = np.diff(best_ask_fp, prepend=best_ask_fp[0])
    bq_prev = np.roll(bid_qty_l1, 1)
    bq_prev[0] = bid_qty_l1[0]
    aq_prev = np.roll(ask_qty_l1, 1)
    aq_prev[0] = ask_qty_l1[0]

    ofi_bid = np.where(bp_diff >= 0, bid_qty_l1, 0) - np.where(bp_diff <= 0, bq_prev, 0)
    ofi_ask = np.where(ap_diff <= 0, ask_qty_l1, 0) - np.where(ap_diff >= 0, aq_prev, 0)
    ofi_fp = (ofi_bid - ofi_ask).astype(np.float64) / float(base_depth_fp)
    ofi_fp[0] = 0.0

    obi_5 = (bid_d5 - ask_d5).astype(np.float64) / np.maximum((bid_d5 + ask_d5).astype(np.float64), 1.0)
    obi_10 = (bid_d10 - ask_d10).astype(np.float64) / np.maximum((bid_d10 + ask_d10).astype(np.float64), 1.0)
    trade_imb = (taker_buy - taker_sell).astype(np.float64) / np.maximum(
        (taker_buy + taker_sell + base_depth_fp // 2).astype(np.float64), 1.0
    )
    vwap_dev = (mid_price_fp - rolling_vwap_fp).astype(np.float64) / np.maximum(spread_fp.astype(np.float64), float(tick_fp))

    # Strictly causal 20-bar realized return volatility (bps)
    ret_1 = np.zeros(n, dtype=np.float64)
    ret_1[1:] = (mid_float[1:] - mid_float[:-1]) / np.maximum(mid_float[:-1], 1.0) * 1e4
    ret_sq = np.convolve(ret_1**2, np.ones(20, dtype=np.float64) / 20.0, mode="full")[:n]
    realized_vol = np.sqrt(np.maximum(ret_sq, 1e-6))

    # Forward H-bar return target in bps (strictly used ONLY as target y[t], never in X[t]!)
    fwd_ret_bps = np.zeros(n, dtype=np.float64)
    if pred_horizon_bars < n:
        fwd_ret_bps[:-pred_horizon_bars] = (
            (mid_float[pred_horizon_bars:] - mid_float[:-pred_horizon_bars])
            / np.maximum(mid_float[:-pred_horizon_bars], 1.0)
            * 1e4
        )

    step_ns = int(cfg.step_ms * 1_000_000)
    base_ts_ns = 1_763_478_000_000_000_000
    ts_ns = base_ts_ns + np.arange(n, dtype=np.int64) * step_ns

    return MarketDataset(
        symbol=cfg.symbol,
        ts_ns=ts_ns,
        mid_price_fp=mid_price_fp,
        best_bid_fp=best_bid_fp,
        best_ask_fp=best_ask_fp,
        spread_fp=spread_fp,
        spread_bps=spread_bps,
        bid_qty_l1_fp=bid_qty_l1,
        ask_qty_l1_fp=ask_qty_l1,
        bid_depth_5_fp=bid_d5,
        ask_depth_5_fp=ask_d5,
        bid_depth_10_fp=bid_d10,
        ask_depth_10_fp=ask_d10,
        taker_buy_qty_fp=taker_buy,
        taker_sell_qty_fp=taker_sell,
        rolling_vwap_fp=rolling_vwap_fp,
        ofi_norm=ofi_fp,
        obi_5=obi_5,
        obi_10=obi_10,
        trade_imbalance=trade_imb,
        vwap_dev=vwap_dev,
        realized_vol=realized_vol,
        forward_return_bps=fwd_ret_bps,
        tick_size_fp=tick_fp,
        lot_size_fp=lot_fp,
    )


class ChronosMicrostructureAdapter:
    """
    Interoperability adapter for Channel 04 (`MicrostructureMetrics`) and
    Channel 08 (`HYPWAL01` / `ReplayFrameStream`) records.
    """

    @staticmethod
    def verify_wal_segment_header(wal_path: str | Path) -> tuple[int, int, int]:
        """
        Parse & verify 16-byte Little-Endian `HYPWAL01` WAL header per Section 6.1
        of `.agents/rules/00-hyperion-contract.md`.
        Returns `(version, flags, first_frame_offset)`.
        """
        data = Path(wal_path).read_bytes()
        if len(data) < 16:
            raise ValueError("Truncated WAL segment (< 16 bytes)")
        magic, version, flags = struct.unpack("<8sII", data[:16])
        if magic != HYPWAL_MAGIC:
            raise ValueError(f"Invalid WAL magic {magic!r}; expected {HYPWAL_MAGIC!r}")
        return int(version), int(flags), 16

    @staticmethod
    def from_microstructure_records(
        records: list[dict[str, Any]],
        symbol: str = "BTC-USDT",
        pred_horizon_bars: int = 10,
    ) -> MarketDataset:
        """
        Convert a list of Channel 04 `MicrostructureMetrics` / JSON REST snapshots
        (where `_fp` fields may be decimal strings per Section 5.1.3) into `MarketDataset`.
        """
        if not records:
            raise ValueError("records list must be non-empty")
        spec = CANONICAL_INSTRUMENTS.get(symbol, CANONICAL_INSTRUMENTS["BTC-USDT"])
        n = len(records)

        ts_ns = np.array([int(r.get("ts_ns", i * 100_000_000)) for i, r in enumerate(records)], dtype=np.int64)
        mid_fp = np.array([int(r["mid_price_fp"]) for r in records], dtype=np.int64)
        spread_fp = np.array([max(spec.tick_size_fp, int(r.get("spread_fp", spec.tick_size_fp * 2))) for r in records], dtype=np.int64)
        best_bid_fp = mid_fp - spread_fp // 2
        best_ask_fp = best_bid_fp + spread_fp
        spread_bps = (spread_fp.astype(np.float64) / np.maximum(mid_fp.astype(np.float64), 1.0)) * 1e4

        vwap_fp = np.array([int(r.get("rolling_vwap_fp", m)) for r, m in zip(records, mid_fp)], dtype=np.int64)
        obi_5 = np.array([float(r.get("book_imbalance_top5_ppm", 0)) / 1e6 for r in records], dtype=np.float64)
        obi_10 = np.array([float(r.get("book_imbalance_top10_ppm", 0)) / 1e6 for r in records], dtype=np.float64)
        ofi_raw = np.array([float(r.get("ofi_fp", 0)) / float(FP_SCALE) for r in records], dtype=np.float64)

        default_depth = spec.lot_size_fp * 2_500
        bid_l1 = np.maximum(spec.lot_size_fp, np.round(default_depth * (1.0 + 0.5 * obi_5)).astype(np.int64))
        ask_l1 = np.maximum(spec.lot_size_fp, np.round(default_depth * (1.0 - 0.5 * obi_5)).astype(np.int64))

        mid_f = mid_fp.astype(np.float64)
        ret_1 = np.zeros(n, dtype=np.float64)
        ret_1[1:] = (mid_f[1:] - mid_f[:-1]) / np.maximum(mid_f[:-1], 1.0) * 1e4
        realized_vol = np.sqrt(np.maximum(np.convolve(ret_1**2, np.ones(min(20, n)) / min(20, n), mode="full")[:n], 1e-6))

        fwd = np.zeros(n, dtype=np.float64)
        if pred_horizon_bars < n:
            fwd[:-pred_horizon_bars] = (mid_f[pred_horizon_bars:] - mid_f[:-pred_horizon_bars]) / np.maximum(mid_f[:-pred_horizon_bars], 1.0) * 1e4

        return MarketDataset(
            symbol=symbol,
            ts_ns=ts_ns,
            mid_price_fp=mid_fp,
            best_bid_fp=best_bid_fp,
            best_ask_fp=best_ask_fp,
            spread_fp=spread_fp,
            spread_bps=spread_bps,
            bid_qty_l1_fp=bid_l1,
            ask_qty_l1_fp=ask_l1,
            bid_depth_5_fp=bid_l1 * 5,
            ask_depth_5_fp=ask_l1 * 5,
            bid_depth_10_fp=bid_l1 * 10,
            ask_depth_10_fp=ask_l1 * 10,
            taker_buy_qty_fp=np.maximum(0, np.round(default_depth * 0.25 * (1.0 + np.tanh(ofi_raw))).astype(np.int64)),
            taker_sell_qty_fp=np.maximum(0, np.round(default_depth * 0.25 * (1.0 - np.tanh(ofi_raw))).astype(np.int64)),
            rolling_vwap_fp=vwap_fp,
            ofi_norm=ofi_raw,
            obi_5=obi_5,
            obi_10=obi_10,
            trade_imbalance=np.tanh(ofi_raw),
            vwap_dev=(mid_fp - vwap_fp).astype(np.float64) / np.maximum(spread_fp.astype(np.float64), float(spec.tick_size_fp)),
            realized_vol=realized_vol,
            forward_return_bps=fwd,
            tick_size_fp=spec.tick_size_fp,
            lot_size_fp=spec.lot_size_fp,
        )
