"""Unit & Hypothesis Property-Based Tests for SlidingTemporalMultigraph & Cycle Detection."""

from __future__ import annotations

from hypothesis import given, settings, strategies as st
import pytest

from sentinel_ml.fixed_point import SCALE, FixedPointViolationError
from sentinel_ml.graph_engine import SlidingTemporalMultigraph


def test_circular_wash_trading_3_ring_detected() -> None:
    graph = SlidingTemporalMultigraph(window_ns=60_000_000_000, max_cycle_depth=6)
    px = 68_000 * SCALE
    qty = 2 * SCALE
    t0 = 1_700_000_000_000_000_000

    _, c1 = graph.ingest_trade(
        fill_id=1,
        symbol="BTC-USDT",
        maker_account_id=101,
        taker_account_id=102,
        price_fp=px,
        qty_fp=qty,
        exec_ts_ns=t0,
        aggressor_side="BUY",
        engine_seq=1,
    )
    assert c1 == []

    _, c2 = graph.ingest_trade(
        fill_id=2,
        symbol="BTC-USDT",
        maker_account_id=102,
        taker_account_id=103,
        price_fp=px,
        qty_fp=qty,
        exec_ts_ns=t0 + 50_000_000,
        aggressor_side="BUY",
        engine_seq=2,
    )
    assert c2 == []

    _, c3 = graph.ingest_trade(
        fill_id=3,
        symbol="BTC-USDT",
        maker_account_id=103,
        taker_account_id=101,
        price_fp=px,
        qty_fp=qty,
        exec_ts_ns=t0 + 100_000_000,
        aggressor_side="BUY",
        engine_seq=3,
    )
    assert len(c3) == 1
    cyc = c3[0]
    assert cyc.cycle_length == 3
    assert set(cyc.participant_account_ids) == {101, 102, 103}
    assert cyc.circular_volume_fp == 3 * qty
    assert cyc.net_position_delta_fp == 0
    assert cyc.volume_symmetry_ppm == 1_000_000
    assert cyc.wash_score > 0.90


def test_mixed_side_economic_wash_ring_and_temporal_decay_monotonicity() -> None:
    px = 68_000 * SCALE
    qty = 2 * SCALE
    t0 = 1_700_000_000_000_000_000

    g_fast = SlidingTemporalMultigraph(window_ns=60_000_000_000, decay_tau_ns=15_000_000_000)
    # 1 -> 2 (SELL aggressor: taker=1 sells to maker=2)
    g_fast.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=2, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="SELL",
    )
    # 2 -> 3 (BUY aggressor: maker=2 sells to taker=3)
    g_fast.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=2, taker_account_id=3,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 20_000_000, aggressor_side="BUY",
    )
    # 3 -> 1 (BUY aggressor: maker=3 sells to taker=1)
    _, fast_cycles = g_fast.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=3, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 40_000_000, aggressor_side="BUY",
    )
    assert len(fast_cycles) == 1
    assert set(fast_cycles[0].participant_account_ids) == {1, 2, 3}

    g_slow = SlidingTemporalMultigraph(window_ns=60_000_000_000, decay_tau_ns=15_000_000_000)
    g_slow.ingest_trade(
        fill_id=1, symbol="BTC-USDT", maker_account_id=2, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="SELL",
    )
    g_slow.ingest_trade(
        fill_id=2, symbol="BTC-USDT", maker_account_id=2, taker_account_id=3,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 5_000_000_000, aggressor_side="BUY",
    )
    _, slow_cycles = g_slow.ingest_trade(
        fill_id=3, symbol="BTC-USDT", maker_account_id=3, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 10_000_000_000, aggressor_side="BUY",
    )
    assert len(slow_cycles) == 1
    assert fast_cycles[0].temporal_decay > slow_cycles[0].temporal_decay
    assert fast_cycles[0].wash_score > slow_cycles[0].wash_score


def test_depth_bound_k6_enforced_and_k7_rejected() -> None:
    graph = SlidingTemporalMultigraph(window_ns=60_000_000_000, max_cycle_depth=6)
    px = 50_000 * SCALE
    qty = 1 * SCALE
    t0 = 1_000_000_000_000

    for i in range(1, 6):
        _, cyc = graph.ingest_trade(
            fill_id=i, symbol="ETH-USDT", maker_account_id=i, taker_account_id=i + 1,
            price_fp=px, qty_fp=qty, exec_ts_ns=t0 + i * 10_000_000, aggressor_side="BUY",
        )
        assert cyc == []
    _, cyc6 = graph.ingest_trade(
        fill_id=6, symbol="ETH-USDT", maker_account_id=6, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 60_000_000, aggressor_side="BUY",
    )
    assert len(cyc6) == 1
    assert cyc6[0].cycle_length == 6

    for i in range(10, 16):
        graph.ingest_trade(
            fill_id=100 + i, symbol="SOL-USDT", maker_account_id=i, taker_account_id=i + 1,
            price_fp=px, qty_fp=qty, exec_ts_ns=t0 + i * 10_000_000, aggressor_side="BUY",
        )
    _, cyc7 = graph.ingest_trade(
        fill_id=200, symbol="SOL-USDT", maker_account_id=16, taker_account_id=10,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 200_000_000, aggressor_side="BUY",
    )
    assert cyc7 == []


def test_sliding_window_evicts_expired_edges_and_prevents_stale_cycles() -> None:
    graph = SlidingTemporalMultigraph(window_ns=10_000_000_000, max_cycle_depth=6)
    px = 100 * SCALE
    qty = 5 * SCALE
    t0 = 100_000_000_000

    graph.ingest_trade(
        fill_id=1, symbol="HYPE-USDT", maker_account_id=1, taker_account_id=2,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0, aggressor_side="BUY",
    )
    graph.ingest_trade(
        fill_id=2, symbol="HYPE-USDT", maker_account_id=2, taker_account_id=3,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 5_000_000_000, aggressor_side="BUY",
    )
    _, cyc = graph.ingest_trade(
        fill_id=3, symbol="HYPE-USDT", maker_account_id=3, taker_account_id=1,
        price_fp=px, qty_fp=qty, exec_ts_ns=t0 + 11_000_000_000, aggressor_side="BUY",
    )
    assert cyc == []
    assert graph.active_edge_count == 2
    assert graph.get_account_traded_volume_fp(1) == qty
    assert graph.get_account_traded_volume_fp(2) == qty
    assert graph.get_account_traded_volume_fp(3) == 2 * qty


def test_float_monetary_input_strictly_rejected() -> None:
    graph = SlidingTemporalMultigraph()
    with pytest.raises(FixedPointViolationError):
        graph.ingest_trade(
            fill_id=1,
            symbol="BTC-USDT",
            maker_account_id=1,
            taker_account_id=2,
            price_fp=68500.25,  # type: ignore[arg-type]
            qty_fp=SCALE,
            exec_ts_ns=1000,
        )


@settings(max_examples=80, deadline=None)
@given(
    edges=st.lists(
        st.tuples(
            st.integers(min_value=1, max_value=10),
            st.integers(min_value=1, max_value=10),
        ),
        min_size=1,
        max_size=35,
    ),
    max_k=st.integers(min_value=2, max_value=6),
)
def test_property_tarjan_johnson_matches_bruteforce_simple_cycles(
    edges: list[tuple[int, int]],
    max_k: int,
) -> None:
    """Property test: Incremental Tarjan-SCC + Depth-Aware Johnson equals exact DFS."""
    graph = SlidingTemporalMultigraph(
        window_ns=1_000_000_000_000,
        max_cycle_depth=max_k,
        min_wash_score=0.0,
        max_cycles_per_trade=10_000,
    )
    px = 1_000 * SCALE
    qty = 1 * SCALE
    ts = 1_000_000_000

    for idx, (u, v) in enumerate(edges, start=1):
        ts += 1_000
        _, detected = graph.ingest_trade(
            fill_id=idx,
            symbol="BTC-USDT",
            maker_account_id=u,
            taker_account_id=v,
            price_fp=px,
            qty_fp=qty,
            exec_ts_ns=ts,
            aggressor_side="BUY",
            engine_seq=idx,
        )
        got_canonical: set[tuple[int, ...]] = set()
        for c in detected:
            accts = c.participant_account_ids
            assert len(accts) == len(set(accts))
            assert 1 <= len(accts) <= max_k
            min_pos = min(range(len(accts)), key=accts.__getitem__)
            got_canonical.add(accts[min_pos:] + accts[:min_pos])

        expected_canonical = graph.enumerate_reference_cycles_on_edge(
            u, v, max_depth=max_k, orientation="asset_flow"
        )
        assert got_canonical == expected_canonical


@settings(max_examples=50, deadline=None)
@given(
    dts_ms=st.lists(st.integers(min_value=1, max_value=8_000), min_size=5, max_size=60),
    accts=st.lists(
        st.tuples(st.integers(min_value=1, max_value=8), st.integers(min_value=1, max_value=8)),
        min_size=60,
        max_size=60,
    ),
)
def test_property_sliding_window_exact_edge_retention(
    dts_ms: list[int],
    accts: list[tuple[int, int]],
) -> None:
    window_ns = 15_000_000_000
    graph = SlidingTemporalMultigraph(window_ns=window_ns, max_cycle_depth=6)
    ts_history: list[int] = []
    cur_ts = 100_000_000_000

    for i, dt_ms in enumerate(dts_ms):
        cur_ts += dt_ms * 1_000_000
        ts_history.append(cur_ts)
        u, v = accts[i]
        graph.ingest_trade(
            fill_id=i + 1,
            symbol="BTC-USDT",
            maker_account_id=u,
            taker_account_id=v,
            price_fp=100 * SCALE,
            qty_fp=SCALE,
            exec_ts_ns=cur_ts,
        )
        expected_active = sum(1 for t in ts_history if t >= cur_ts - window_ns)
        assert graph.active_edge_count == expected_active
        assert graph.get_account_traded_volume_fp(u) >= 0
