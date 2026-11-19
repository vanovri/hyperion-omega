"""Adversarial & Realistic Synthetic Market Microstructure Dataset Generator.

Combines thousands of normal market participants across independent sessions:
  - Geometric Brownian Motion / Ornstein-Uhlenbeck fundamental mid-price (fixed-point 1e8)
  - Multivariate Hawkes process arrival intensities for retail & institutional flow
  - Honest Avellaneda-Stoikov two-sided market makers (moderate OTR, balanced book)
  - Aggressive legitimate institutional TWAP / Iceberg rebalancing bots (hard negative cases)
with 4 labeled adversarial manipulation typologies (including stealth/noisy variants):
  1. WASH_TRADING_CYCLE (depth k in {2, 3, 4, 5, 6}, circular volume laundering)
  2. SPOOFING_LAYERING (3-8 tick phantom layering + opposite fill + fast cancel burst)
  3. MOMENTUM_IGNITION / PUMP_AND_DUMP (aggressive sweep burst igniting price jump)
  4. LATENCY_ARBITRAGE / VELOCITY_SPIKE (sub-ms quote-stuffing / toxic sniper bursts)

Every generated event stream is processed directly through SlidingTemporalMultigraph and
StreamingMicrostructureExtractor for zero train-serve skew and strict account/session splits.
"""

from __future__ import annotations

from dataclasses import dataclass
import math

import numpy as np
import numpy.typing as npt

from sentinel_ml.fixed_point import SCALE
from sentinel_ml.graph_engine import SlidingTemporalMultigraph
from sentinel_ml.microstructure import SideLiteral, StreamingMicrostructureExtractor

# Class labels aligned with hyperion.v1.AnomalyPatternType
LABEL_NORMAL: int = 0
LABEL_WASH_TRADING: int = 1
LABEL_SPOOFING_LAYERING: int = 2
LABEL_VELOCITY_SPIKE: int = 3
LABEL_MOMENTUM_IGNITION: int = 4
LABEL_LATENCY_ARBITRAGE: int = 5

CLASS_NAMES: dict[int, str] = {
    LABEL_NORMAL: "NORMAL",
    LABEL_WASH_TRADING: "WASH_TRADING_CYCLE",
    LABEL_SPOOFING_LAYERING: "SPOOFING_LAYERING",
    LABEL_VELOCITY_SPIKE: "VELOCITY_SPIKE",
    LABEL_MOMENTUM_IGNITION: "MOMENTUM_IGNITION",
    LABEL_LATENCY_ARBITRAGE: "LATENCY_ARBITRAGE",
}


@dataclass(frozen=True, slots=True)
class SyntheticDatasetSplit:
    """Labeled sequence dataset split for training, calibration, and holdout evaluation."""

    sequences: npt.NDArray[np.float32]  # [N, T=8, F=16]
    binary_labels: npt.NDArray[np.int64]  # [N] 0 = normal, 1 = manipulation
    pattern_labels: npt.NDArray[np.int64]  # [N] 0..5 typology class
    account_ids: npt.NDArray[np.uint64]  # [N]


def _hawkes_next_dt_ns(
    rng: np.random.Generator,
    mu_hz: float,
    excitation_hz: float,
    beta_hz: float,
) -> tuple[int, float]:
    """Ogata thinning step for a self-exciting Hawkes arrival process."""
    lam_upper = max(0.5, mu_hz + excitation_hz)
    u = max(1e-9, float(rng.uniform()))
    dt_sec = -math.log(u) / lam_upper
    decayed_exc = excitation_hz * math.exp(-beta_hz * dt_sec)
    next_exc = min(80.0, decayed_exc + 0.35 * beta_hz)
    return max(100_000, int(dt_sec * 1e9)), next_exc


def _simulate_market_episode(
    rng: np.random.Generator,
    *,
    account_id_offset: int,
    num_normal_traders: int = 360,
    num_market_makers: int = 28,
    num_inst_rebalancers: int = 16,
    num_wash_rings: int = 12,
    num_spoof_bots: int = 28,
    num_pump_bots: int = 24,
    num_lat_arb_bots: int = 24,
    symbol: str = "BTC-USDT",
) -> SyntheticDatasetSplit:
    """Simulate a full multi-agent market session and harvest streaming feature sequences."""
    graph = SlidingTemporalMultigraph(
        window_ns=60_000_000_000,
        max_cycle_depth=6,
        decay_tau_ns=15_000_000_000,
        min_wash_score=0.20,
    )
    extractor = StreamingMicrostructureExtractor(
        graph,
        window_ns=30_000_000_000,
        snapshot_min_interval_ns=20_000_000,
    )

    cursor = account_id_offset
    retail_ids = list(range(cursor, cursor + num_normal_traders))
    cursor += num_normal_traders
    mm_ids = list(range(cursor, cursor + num_market_makers))
    cursor += num_market_makers
    rebal_ids = list(range(cursor, cursor + num_inst_rebalancers))
    cursor += num_inst_rebalancers

    wash_rings: list[list[int]] = []
    for _ in range(num_wash_rings):
        ring_k = int(rng.choice([2, 3, 3, 4, 5, 6]))
        wash_rings.append(list(range(cursor, cursor + ring_k)))
        cursor += ring_k

    spoof_ids = list(range(cursor, cursor + num_spoof_bots))
    cursor += num_spoof_bots
    pump_ids = list(range(cursor, cursor + num_pump_bots))
    cursor += num_pump_bots
    lat_arb_ids = list(range(cursor, cursor + num_lat_arb_bots))
    cursor += num_lat_arb_bots

    tick_fp = 1_000_000  # 0.01 USDT in scale 1e8
    mid_fp = 68_500 * SCALE
    ts_ns = 1_700_000_000_000_000_000
    order_seq = 1
    fill_seq = 1
    eng_seq = 1
    hawkes_exc = 2.0

    sequences_out: list[npt.NDArray[np.float32]] = []
    bin_out: list[int] = []
    pat_out: list[int] = []
    acct_out: list[int] = []

    # 1. Background Hawkes + Brownian normal trading + Honest Avellaneda-Stoikov Market Makers
    for step in range(1_250):
        dt_ns, hawkes_exc = _hawkes_next_dt_ns(
            rng, mu_hz=35.0, excitation_hz=hawkes_exc, beta_hz=12.0
        )
        ts_ns += dt_ns

        shock_ticks = int(round(float(rng.normal(0.0, 1.8))))
        mid_fp = max(10_000 * SCALE, mid_fp + shock_ticks * tick_fp)
        spread_ticks = int(rng.integers(1, 4))
        best_bid_fp = mid_fp - spread_ticks * tick_fp
        best_ask_fp = mid_fp + spread_ticks * tick_fp
        extractor.update_bbo(symbol, best_bid_fp, best_ask_fp)

        mm_acct = mm_ids[step % len(mm_ids)]
        mm_qty_fp = int(rng.integers(5_000_000, 45_000_000))
        bid_oid = order_seq
        ask_oid = order_seq + 1
        order_seq += 2
        extractor.on_order_submit(
            order_id=bid_oid,
            account_id=mm_acct,
            symbol=symbol,
            side="BUY",
            price_fp=best_bid_fp - int(rng.integers(0, 2)) * tick_fp,
            qty_fp=mm_qty_fp,
            ts_ns=ts_ns,
        )
        extractor.on_order_submit(
            order_id=ask_oid,
            account_id=mm_acct,
            symbol=symbol,
            side="SELL",
            price_fp=best_ask_fp + int(rng.integers(0, 2)) * tick_fp,
            qty_fp=mm_qty_fp,
            ts_ns=ts_ns + 50_000,
        )

        if step < len(retail_ids):
            taker_acct = retail_ids[step]
        else:
            taker_acct = int(rng.choice(retail_ids))
        maker_acct = int(rng.choice(mm_ids if rng.random() < 0.60 else retail_ids))
        if maker_acct == taker_acct:
            maker_acct = mm_ids[0]
        side: SideLiteral = "BUY" if rng.random() < 0.5 else "SELL"
        exec_px_fp = best_ask_fp if side == "BUY" else best_bid_fp
        exec_qty_fp = int(rng.integers(2_000_000, 35_000_000))

        extractor.on_order_submit(
            order_id=order_seq,
            account_id=taker_acct,
            symbol=symbol,
            side=side,
            price_fp=exec_px_fp,
            qty_fp=exec_qty_fp,
            ts_ns=ts_ns + 100_000,
            is_resting_limit=False,
        )
        order_seq += 1

        graph.ingest_trade(
            fill_id=fill_seq,
            symbol=symbol,
            maker_account_id=maker_acct,
            taker_account_id=taker_acct,
            price_fp=exec_px_fp,
            qty_fp=exec_qty_fp,
            exec_ts_ns=ts_ns + 120_000,
            aggressor_side=side,
            engine_seq=eng_seq,
        )
        m_oid = 0
        if maker_acct == mm_acct:
            m_oid = ask_oid if side == "BUY" else bid_oid
        extractor.on_trade_fill(
            fill_id=fill_seq,
            symbol=symbol,
            maker_account_id=maker_acct,
            taker_account_id=taker_acct,
            aggressor_side=side,
            price_fp=exec_px_fp,
            qty_fp=exec_qty_fp,
            exec_ts_ns=ts_ns + 120_000,
            maker_order_id=m_oid,
            engine_seq=eng_seq,
        )
        fill_seq += 1
        eng_seq += 1

        if rng.random() < 0.30:
            cancel_delay = int(rng.integers(250_000_000, 3_200_000_000))
            extractor.on_order_cancel(
                order_id=bid_oid if side == "BUY" else ask_oid,
                account_id=mm_acct,
                symbol=symbol,
                ts_ns=ts_ns + cancel_delay,
            )

    # 1b. Hard-negative normal institutional TWAP / Iceberg rebalancing bots
    #     (Fast quote refresh + moderate multi-level quotes, but NO opposite-fill spoof pull)
    for rebal in rebal_ids:
        ts_ns += int(rng.integers(40_000_000, 120_000_000))
        r_side: SideLiteral = "BUY" if rng.random() < 0.5 else "SELL"
        for slice_i in range(int(rng.integers(5, 10))):
            s_ts = ts_ns + slice_i * int(rng.integers(90_000_000, 220_000_000))
            s_px = mid_fp + ((-1 if r_side == "BUY" else 1) * (slice_i % 2 + 1) * tick_fp)
            s_qty = int(rng.integers(15_000_000, 50_000_000))
            oid = order_seq
            order_seq += 1
            extractor.on_order_submit(
                order_id=oid,
                account_id=rebal,
                symbol=symbol,
                side=r_side,
                price_fp=s_px,
                qty_fp=s_qty,
                ts_ns=s_ts,
            )
            if slice_i % 2 == 0:
                cp = int(rng.choice(mm_ids))
                graph.ingest_trade(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=cp,
                    taker_account_id=rebal,
                    price_fp=s_px,
                    qty_fp=s_qty,
                    exec_ts_ns=s_ts + 30_000_000,
                    aggressor_side=r_side,
                    engine_seq=eng_seq,
                )
                extractor.on_trade_fill(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=cp,
                    taker_account_id=rebal,
                    aggressor_side=r_side,
                    price_fp=s_px,
                    qty_fp=s_qty,
                    exec_ts_ns=s_ts + 30_000_000,
                    engine_seq=eng_seq,
                )
                fill_seq += 1
                eng_seq += 1
            else:
                extractor.on_order_cancel(
                    order_id=oid,
                    account_id=rebal,
                    symbol=symbol,
                    ts_ns=s_ts + int(rng.integers(300_000_000, 800_000_000)),
                )

    for norm_acct in retail_ids + mm_ids + rebal_ids:
        seq = extractor.get_account_sequence(norm_acct, ts_ns)
        if float(np.sum(np.abs(seq))) > 0.0:
            sequences_out.append(seq.copy())
            bin_out.append(0)
            pat_out.append(LABEL_NORMAL)
            acct_out.append(norm_acct)

    # 2. Scenario A: Circular Wash-Trading Rings (k in 2..6, including noisy camouflage trades)
    for ring in wash_rings:
        k = len(ring)
        rounds = int(rng.integers(2, 4))
        base_qty_fp = int(rng.integers(40_000_000, 180_000_000))
        is_stealth = rng.random() < 0.25
        for _ in range(rounds):
            ts_ns += int(rng.integers(50_000_000, 250_000_000))
            ring_px_fp = mid_fp + int(rng.integers(-1, 2)) * tick_fp
            for leg_i in range(k):
                seller = ring[leg_i]
                buyer = ring[(leg_i + 1) % k]
                lo_j, hi_j = (0.86, 1.14) if is_stealth else (0.96, 1.04)
                jitter_qty = int(base_qty_fp * float(rng.uniform(lo_j, hi_j)))
                leg_px = ring_px_fp + int(rng.integers(0, 2)) * tick_fp
                leg_ts = ts_ns + leg_i * int(rng.integers(15_000_000, 90_000_000))
                agg_side: SideLiteral = "BUY" if rng.random() < 0.8 else "SELL"
                maker = seller if agg_side == "BUY" else buyer
                taker = buyer if agg_side == "BUY" else seller

                extractor.on_order_submit(
                    order_id=order_seq,
                    account_id=maker,
                    symbol=symbol,
                    side="SELL" if agg_side == "BUY" else "BUY",
                    price_fp=leg_px,
                    qty_fp=jitter_qty,
                    ts_ns=leg_ts - 2_000_000,
                )
                m_oid = order_seq
                order_seq += 1
                extractor.on_order_submit(
                    order_id=order_seq,
                    account_id=taker,
                    symbol=symbol,
                    side=agg_side,
                    price_fp=leg_px,
                    qty_fp=jitter_qty,
                    ts_ns=leg_ts,
                    is_resting_limit=False,
                )
                order_seq += 1

                graph.ingest_trade(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=maker,
                    taker_account_id=taker,
                    price_fp=leg_px,
                    qty_fp=jitter_qty,
                    exec_ts_ns=leg_ts,
                    aggressor_side=agg_side,
                    engine_seq=eng_seq,
                )
                extractor.on_trade_fill(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=maker,
                    taker_account_id=taker,
                    aggressor_side=agg_side,
                    price_fp=leg_px,
                    qty_fp=jitter_qty,
                    exec_ts_ns=leg_ts,
                    maker_order_id=m_oid,
                    engine_seq=eng_seq,
                )
                fill_seq += 1
                eng_seq += 1
            ts_ns += k * 95_000_000

        for member in ring:
            sequences_out.append(extractor.get_account_sequence(member, ts_ns).copy())
            bin_out.append(1)
            pat_out.append(LABEL_WASH_TRADING)
            acct_out.append(member)

    # 3. Scenario B: Classic Spoofing & Multi-Tick Layering Bots
    for spoofer in spoof_ids:
        ts_ns += int(rng.integers(40_000_000, 150_000_000))
        best_bid_fp = mid_fp - tick_fp
        best_ask_fp = mid_fp + tick_fp
        extractor.update_bbo(symbol, best_bid_fp, best_ask_fp)

        spoof_side: SideLiteral = "SELL" if rng.random() < 0.5 else "BUY"
        opp_side: SideLiteral = "BUY" if spoof_side == "SELL" else "SELL"
        num_waves = int(rng.integers(2, 4))
        for _ in range(num_waves):
            num_layers = int(rng.integers(3, 9))
            layer_oids: list[int] = []
            for lvl in range(num_layers):
                if spoof_side == "SELL":
                    px = best_ask_fp + lvl * tick_fp
                else:
                    px = best_bid_fp - lvl * tick_fp
                large_qty = int(rng.integers(120_000_000, 450_000_000))
                oid = order_seq
                order_seq += 1
                layer_oids.append(oid)
                extractor.on_order_submit(
                    order_id=oid,
                    account_id=spoofer,
                    symbol=symbol,
                    side=spoof_side,
                    price_fp=px,
                    qty_fp=large_qty,
                    ts_ns=ts_ns + lvl * 4_000_000,
                )

            exec_ts = ts_ns + num_layers * 4_000_000 + int(rng.integers(20_000_000, 90_000_000))
            opp_px = best_ask_fp if opp_side == "BUY" else best_bid_fp
            opp_qty = int(rng.integers(20_000_000, 60_000_000))
            victim = int(rng.choice(mm_ids))
            graph.ingest_trade(
                fill_id=fill_seq,
                symbol=symbol,
                maker_account_id=victim,
                taker_account_id=spoofer,
                price_fp=opp_px,
                qty_fp=opp_qty,
                exec_ts_ns=exec_ts,
                aggressor_side=opp_side,
                engine_seq=eng_seq,
            )
            extractor.on_trade_fill(
                fill_id=fill_seq,
                symbol=symbol,
                maker_account_id=victim,
                taker_account_id=spoofer,
                aggressor_side=opp_side,
                price_fp=opp_px,
                qty_fp=opp_qty,
                exec_ts_ns=exec_ts,
                engine_seq=eng_seq,
            )
            fill_seq += 1
            eng_seq += 1

            cancel_base_ts = exec_ts + int(rng.integers(25_000_000, 180_000_000))
            for idx_o, oid in enumerate(layer_oids):
                extractor.on_order_cancel(
                    order_id=oid,
                    account_id=spoofer,
                    symbol=symbol,
                    ts_ns=cancel_base_ts + idx_o * 3_000_000,
                )
            ts_ns = cancel_base_ts + 40_000_000

        sequences_out.append(extractor.get_account_sequence(spoofer, ts_ns).copy())
        bin_out.append(1)
        pat_out.append(LABEL_SPOOFING_LAYERING)
        acct_out.append(spoofer)

    # 4. Scenario C: Pump-and-Dump / Momentum Ignition Sweepers
    for pumper in pump_ids:
        ts_ns += int(rng.integers(30_000_000, 100_000_000))
        sweep_dir = 1 if rng.random() < 0.5 else -1
        agg_side_p: SideLiteral = "BUY" if sweep_dir > 0 else "SELL"
        num_sweeps = int(rng.integers(9, 18))
        cur_px = mid_fp
        for sw in range(num_sweeps):
            cur_px = max(10_000 * SCALE, cur_px + sweep_dir * int(rng.integers(3, 9)) * tick_fp)
            sw_qty = int(rng.integers(50_000_000, 160_000_000))
            sw_ts = ts_ns + sw * int(rng.integers(18_000_000, 45_000_000))
            victim = int(rng.choice(retail_ids))
            extractor.on_order_submit(
                order_id=order_seq,
                account_id=pumper,
                symbol=symbol,
                side=agg_side_p,
                price_fp=cur_px,
                qty_fp=sw_qty,
                ts_ns=sw_ts,
                is_resting_limit=False,
            )
            order_seq += 1
            graph.ingest_trade(
                fill_id=fill_seq,
                symbol=symbol,
                maker_account_id=victim,
                taker_account_id=pumper,
                price_fp=cur_px,
                qty_fp=sw_qty,
                exec_ts_ns=sw_ts,
                aggressor_side=agg_side_p,
                engine_seq=eng_seq,
            )
            extractor.on_trade_fill(
                fill_id=fill_seq,
                symbol=symbol,
                maker_account_id=victim,
                taker_account_id=pumper,
                aggressor_side=agg_side_p,
                price_fp=cur_px,
                qty_fp=sw_qty,
                exec_ts_ns=sw_ts,
                engine_seq=eng_seq,
            )
            fill_seq += 1
            eng_seq += 1
        ts_ns += num_sweeps * 45_000_000
        sequences_out.append(extractor.get_account_sequence(pumper, ts_ns).copy())
        bin_out.append(1)
        pat_out.append(LABEL_MOMENTUM_IGNITION)
        acct_out.append(pumper)

    # 5. Scenario D: Latency Arbitrage / Quote-Stuffing Velocity Spike Bots
    for sniper in lat_arb_ids:
        ts_ns += int(rng.integers(30_000_000, 90_000_000))
        burst_msgs = int(rng.integers(40, 80))
        for b_i in range(burst_msgs):
            b_ts = ts_ns + b_i * int(rng.integers(400_000, 2_500_000))
            b_side: SideLiteral = "BUY" if (b_i % 2 == 0) else "SELL"
            b_px = mid_fp + (tick_fp if b_side == "SELL" else -tick_fp)
            b_qty = int(rng.integers(15_000_000, 75_000_000))
            oid = order_seq
            order_seq += 1
            extractor.on_order_submit(
                order_id=oid,
                account_id=sniper,
                symbol=symbol,
                side=b_side,
                price_fp=b_px,
                qty_fp=b_qty,
                ts_ns=b_ts,
            )
            if b_i % 12 == 0:
                victim = int(rng.choice(mm_ids))
                graph.ingest_trade(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=victim,
                    taker_account_id=sniper,
                    price_fp=b_px,
                    qty_fp=b_qty,
                    exec_ts_ns=b_ts + 200_000,
                    aggressor_side=b_side,
                    engine_seq=eng_seq,
                )
                extractor.on_trade_fill(
                    fill_id=fill_seq,
                    symbol=symbol,
                    maker_account_id=victim,
                    taker_account_id=sniper,
                    aggressor_side=b_side,
                    price_fp=b_px,
                    qty_fp=b_qty,
                    exec_ts_ns=b_ts + 200_000,
                    engine_seq=eng_seq,
                )
                fill_seq += 1
                eng_seq += 1
            else:
                extractor.on_order_cancel(
                    order_id=oid,
                    account_id=sniper,
                    symbol=symbol,
                    ts_ns=b_ts + 800_000,
                )
        ts_ns += burst_msgs * 2_500_000
        sequences_out.append(extractor.get_account_sequence(sniper, ts_ns).copy())
        bin_out.append(1)
        pat_out.append(LABEL_LATENCY_ARBITRAGE)
        acct_out.append(sniper)

    return SyntheticDatasetSplit(
        sequences=np.stack(sequences_out, axis=0).astype(np.float32),
        binary_labels=np.asarray(bin_out, dtype=np.int64),
        pattern_labels=np.asarray(pat_out, dtype=np.int64),
        account_ids=np.asarray(acct_out, dtype=np.uint64),
    )


def generate_adversarial_datasets(
    seed: int = 20261118,
) -> tuple[SyntheticDatasetSplit, SyntheticDatasetSplit, SyntheticDatasetSplit]:
    """Generate strictly account-disjoint & session-disjoint (train, val, holdout) splits.

    Simulates >2,400 normal traders/market-makers/TWAP bots across independent Hawkes/Brownian
    market sessions plus hundreds of labeled manipulation syndicates.
    """
    rng = np.random.default_rng(seed)

    def _concat_episodes(n_episodes: int, base_offset: int) -> SyntheticDatasetSplit:
        splits = [
            _simulate_market_episode(rng, account_id_offset=base_offset + ep * 10_000)
            for ep in range(n_episodes)
        ]
        return SyntheticDatasetSplit(
            sequences=np.concatenate([s.sequences for s in splits], axis=0),
            binary_labels=np.concatenate([s.binary_labels for s in splits], axis=0),
            pattern_labels=np.concatenate([s.pattern_labels for s in splits], axis=0),
            account_ids=np.concatenate([s.account_ids for s in splits], axis=0),
        )

    train_split = _concat_episodes(n_episodes=3, base_offset=100_000)
    val_split = _concat_episodes(n_episodes=1, base_offset=500_000)
    holdout_split = _concat_episodes(n_episodes=2, base_offset=900_000)
    return train_split, val_split, holdout_split
