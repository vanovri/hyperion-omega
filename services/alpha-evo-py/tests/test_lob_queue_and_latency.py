"""Tests for Vectorized LOB Gym: FIFO queue tracking, amend priority, latency ring, fees, impact, & Gymnasium API."""

from __future__ import annotations

from gymnasium.utils.env_checker import check_env
import numpy as np

from alpha_evo.lob_gym.env import HyperionLOBGymEnv, LOBEnvConfig, VectorizedLOBEnv
from alpha_evo.lob_gym.market_generator import (
    ChronosMicrostructureAdapter,
    HYPWAL_MAGIC,
    MarketGenConfig,
    generate_microstructure_dataset,
)


def test_gymnasium_api_compliance() -> None:
    gym_env = HyperionLOBGymEnv(LOBEnvConfig(num_envs=1, episode_steps=32, seed=123))
    check_env(gym_env, skip_render_check=True)
    obs, info = gym_env.reset(seed=123)
    assert obs.shape == (14,)
    assert info["double_entry_balanced"] is True
    obs2, rew, term, trunc, info2 = gym_env.step(gym_env.action_space.sample())
    assert obs2.shape == (14,)
    assert isinstance(rew, float)
    assert term is False
    assert trunc is False
    assert info2["double_entry_balanced"] is True


def test_fifo_queue_position_blocks_fill_until_queue_ahead_depleted() -> None:
    cfg = LOBEnvConfig(
        num_envs=2,
        episode_steps=20,
        enable_competing_mm=False,
        maker_fee_ppm=-100,
        taker_fee_ppm=500,
        seed=7,
    )
    env = VectorizedLOBEnv(cfg)
    env.reset(seed=7)

    # Set deterministic L1 resting depth = 25,000,000 fp (0.25 BTC)
    env.l1_bid_depth_fp[:] = 25_000_000
    env.l1_ask_depth_fp[:] = 25_000_000

    # Place resting bid of 10,000,000 fp (0.10 BTC) at best_bid_fp with 0 latency override
    own_qty = np.array([10_000_000, 10_000_000], dtype=np.int64)
    env.enqueue_raw_quote_command(
        bid_px_fp=env.best_bid_fp.copy(),
        ask_px_fp=env.best_ask_fp.copy(),
        bid_qty_fp=own_qty,
        ask_qty_fp=own_qty,
        latency_steps_override=np.array([0, 0], dtype=np.int64),
    )

    # Joining the tail means queue_ahead == 25,000,000 fp!
    np.testing.assert_array_equal(env.bid_queue_ahead_fp, np.array([25_000_000, 25_000_000]))

    # Step 1: Aggressive sell of 15,000,000 (< 25,000,000 queue ahead) -> MUST yield ZERO fill for us!
    fb1, fa1 = env.execute_order_flow_step(
        aggr_buy_fp=np.array([0, 0], dtype=np.int64),
        aggr_sell_fp=np.array([15_000_000, 25_000_000], dtype=np.int64),
        cancel_rate_ahead=0.0,
    )
    # Boundary check: even exact 25,000_000 only depletes queue_ahead to 0 without filling our order!
    assert fb1[0] == 0
    assert fb1[1] == 0
    assert env.bid_queue_ahead_fp[0] == 10_000_000
    assert env.bid_queue_ahead_fp[1] == 0
    assert fa1[0] == 0

    # Step 2: Another 6,000,000 aggressive sell:
    # Env 0 has 10M ahead -> still 0 fill (4M remaining ahead)
    # Env 1 has 0 ahead  -> gets exact 6,000,000 partial fill!
    fb2, _ = env.execute_order_flow_step(
        aggr_buy_fp=np.array([0, 0], dtype=np.int64),
        aggr_sell_fp=np.array([6_000_000, 6_000_000], dtype=np.int64),
        cancel_rate_ahead=0.0,
    )
    assert fb2[0] == 0
    assert env.bid_queue_ahead_fp[0] == 4_000_000
    assert fb2[1] == 6_000_000
    assert env.bid_qty_fp[1] == 4_000_000
    assert env.inventory_fp[1] == 6_000_000
    assert np.all(env.double_entry_residual_fp == 0)


def test_order_amend_size_reduction_preserves_queue_priority_while_increase_loses_priority() -> None:
    cfg = LOBEnvConfig(num_envs=2, enable_competing_mm=False, seed=11)
    env = VectorizedLOBEnv(cfg)
    env.reset(seed=11)
    env.l1_bid_depth_fp[:] = 30_000_000

    env.enqueue_raw_quote_command(
        bid_px_fp=env.best_bid_fp.copy(),
        ask_px_fp=env.best_ask_fp.copy(),
        bid_qty_fp=np.array([10_000_000, 10_000_000], dtype=np.int64),
        ask_qty_fp=np.array([10_000_000, 10_000_000], dtype=np.int64),
        latency_steps_override=np.array([0, 0], dtype=np.int64),
    )
    # Advance queue ahead down to 5,000,000
    env.execute_order_flow_step(
        aggr_buy_fp=np.array([0, 0], dtype=np.int64),
        aggr_sell_fp=np.array([25_000_000, 25_000_000], dtype=np.int64),
        cancel_rate_ahead=0.0,
    )
    np.testing.assert_array_equal(env.bid_queue_ahead_fp, np.array([5_000_000, 5_000_000]))

    # Env 0 reduces qty (10M -> 6M at same price) -> MUST keep queue_ahead == 5M!
    # Env 1 increases qty (10M -> 12M at same price) -> MUST lose priority -> queue_ahead resets to 30M!
    env.enqueue_raw_quote_command(
        bid_px_fp=env.best_bid_fp.copy(),
        ask_px_fp=env.best_ask_fp.copy(),
        bid_qty_fp=np.array([6_000_000, 12_000_000], dtype=np.int64),
        ask_qty_fp=np.array([10_000_000, 10_000_000], dtype=np.int64),
        latency_steps_override=np.array([0, 0], dtype=np.int64),
    )
    assert env.bid_queue_ahead_fp[0] == 5_000_000
    assert env.bid_queue_ahead_fp[1] == 30_000_000


def test_post_only_rejects_crossing_quotes_and_lognormal_latency_delays_arrival() -> None:
    cfg = LOBEnvConfig(num_envs=4, post_only=True, min_latency_steps=3, max_latency_steps=8, seed=21)
    env = VectorizedLOBEnv(cfg)
    env.reset(seed=21)

    lats = env.sample_latency_steps(500)
    assert np.all(lats >= 3) and np.all(lats <= 8)
    # Log-normal right-skewness creates dispersion across [3, 8]
    assert np.max(lats) > np.min(lats)

    # Enqueue with exact 2-step delay: order MUST NOT be active at step 0 or step 1, only at step 2!
    env.enqueue_raw_quote_command(
        bid_px_fp=env.best_bid_fp.copy(),
        ask_px_fp=env.best_ask_fp.copy(),
        bid_qty_fp=np.full(4, 5_000_000, dtype=np.int64),
        ask_qty_fp=np.full(4, 5_000_000, dtype=np.int64),
        latency_steps_override=np.full(4, 2, dtype=np.int64),
    )
    assert not np.any(env.bid_active)
    env.step_idx += 1
    env._drain_current_ring_slot()
    assert not np.any(env.bid_active)
    env.step_idx += 1
    env._drain_current_ring_slot()
    assert np.all(env.bid_active)

    # Crossing post-only bid (`bid_px == best_ask_fp`) MUST be rejected!
    env.enqueue_raw_quote_command(
        bid_px_fp=env.best_ask_fp.copy(),
        ask_px_fp=env.best_ask_fp.copy(),
        bid_qty_fp=np.full(4, 5_000_000, dtype=np.int64),
        ask_qty_fp=np.full(4, 5_000_000, dtype=np.int64),
        latency_steps_override=np.zeros(4, dtype=np.int64),
    )
    assert np.all(env.post_only_rejects == 1)
    assert not np.any(env.bid_active)


def test_permanent_and_temporary_market_impact_penalizes_aggressive_taker() -> None:
    cfg = LOBEnvConfig(num_envs=2, taker_fee_ppm=500, seed=33)
    env = VectorizedLOBEnv(cfg)
    env.reset(seed=33)

    mid_before = env.mid_ticks.copy()
    ask_before = env.best_ask_fp.copy()

    # Env 0 buys aggressively (+0.05 BTC = 5,000,000 fp); Env 1 sells aggressively (-0.05 BTC)
    exec_px, impact_ticks = env.execute_taker_order_vec(np.array([5_000_000, -5_000_000], dtype=np.int64))

    assert exec_px[0] > ask_before[0]          # Taker buy pays worse than best ask due to temp impact!
    assert env.mid_ticks[0] > mid_before[0]    # Permanent impact shifts fundamental mid upward!
    assert env.mid_ticks[1] < mid_before[1]    # Permanent sell impact shifts fundamental mid downward!
    assert np.all(impact_ticks > 0.0)
    assert np.all(env.cumulative_fees_fp > 0)  # Positive taker fee charged!
    assert np.all(env.double_entry_residual_fp == 0)


def test_chronos_wal_header_and_microstructure_adapter(tmp_path) -> None:
    wal_file = tmp_path / "segment_00000001.wal"
    wal_file.write_bytes(HYPWAL_MAGIC + (1).to_bytes(4, "little") + (0).to_bytes(4, "little") + b"\x00" * 32)
    ver, flags, offset = ChronosMicrostructureAdapter.verify_wal_segment_header(wal_file)
    assert (ver, flags, offset) == (1, 0, 16)

    ds = generate_microstructure_dataset(MarketGenConfig(n_steps=120, seed=5))
    recs = [
        {
            "ts_ns": str(ds.ts_ns[i]),
            "mid_price_fp": str(ds.mid_price_fp[i]),
            "spread_fp": str(ds.spread_fp[i]),
            "rolling_vwap_fp": str(ds.rolling_vwap_fp[i]),
            "book_imbalance_top5_ppm": int(ds.obi_5[i] * 1e6),
            "ofi_fp": int(ds.ofi_norm[i] * 1e8),
        }
        for i in range(60)
    ]
    adapted = ChronosMicrostructureAdapter.from_microstructure_records(recs)
    assert adapted.mid_price_fp.shape == (60,)
    assert np.all(adapted.best_ask_fp > adapted.best_bid_fp)
