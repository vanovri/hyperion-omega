"""Unit & Hypothesis Property Tests for Streaming Microstructure & Fixed-Point Arithmetic."""

from __future__ import annotations

from hypothesis import given, settings, strategies as st
import numpy as np

from sentinel_ml.fixed_point import (
    INT128_MAX,
    INT128_MIN,
    SCALE,
    UINT128_MAX,
    compute_notional_fp,
    format_fp_to_decimal,
    i128_parts_to_int,
    int_to_i128_parts,
    int_to_u128_parts,
    parse_decimal_to_fp,
    u128_parts_to_int,
)
from sentinel_ml.graph_engine import SlidingTemporalMultigraph
from sentinel_ml.microstructure import (
    NUM_FEATURES,
    TEMPORAL_SEQ_LEN,
    StreamingMicrostructureExtractor,
)


def test_classic_spoofing_and_multi_tick_layering_features() -> None:
    graph = SlidingTemporalMultigraph()
    ext = StreamingMicrostructureExtractor(graph)
    symbol = "BTC-USDT"
    mid = 68_500 * SCALE
    tick = 1_000_000
    ext.update_bbo(symbol, mid - tick, mid + tick)

    spoofer = 777
    t0 = 1_700_000_000_000_000_000

    for lvl in range(6):
        ext.on_order_submit(
            order_id=100 + lvl,
            account_id=spoofer,
            symbol=symbol,
            side="SELL",
            price_fp=(mid + tick) + lvl * tick,
            qty_fp=25 * SCALE,
            ts_ns=t0 + lvl * 2_000_000,
        )

    fill_ts = t0 + 40_000_000
    graph.ingest_trade(
        fill_id=1,
        symbol=symbol,
        maker_account_id=50,
        taker_account_id=spoofer,
        price_fp=mid + tick,
        qty_fp=1 * SCALE,
        exec_ts_ns=fill_ts,
        aggressor_side="BUY",
    )
    ext.on_trade_fill(
        fill_id=1,
        symbol=symbol,
        maker_account_id=50,
        taker_account_id=spoofer,
        aggressor_side="BUY",
        price_fp=mid + tick,
        qty_fp=1 * SCALE,
        exec_ts_ns=fill_ts,
    )

    seq = ext.get_account_sequence(spoofer, fill_ts)
    for lvl in range(6):
        seq = ext.on_order_cancel(
            order_id=100 + lvl,
            account_id=spoofer,
            symbol=symbol,
            ts_ns=fill_ts + 60_000_000 + lvl * 1_000_000,
        )

    latest = seq[-1]
    assert seq.shape == (TEMPORAL_SEQ_LEN, NUM_FEATURES)
    assert latest[0] > 1.5  # otr_count_log elevated
    assert latest[4] > 1.0  # spoof_opp_exec_score triggered
    assert latest[6] >= 6.0  # exact sliding-window layering_distinct_ticks == 6
    assert latest[7] > 1.0  # layering_sync_cancel_burst triggered

    ev = ext.build_spoofing_evidence(spoofer)
    assert ev.suspect_account_id == spoofer
    assert ev.spoofed_side == "SELL"
    assert ev.layered_level_count == 6
    assert ev.canceled_spoof_qty_fp == 150 * SCALE
    assert ev.cancel_to_fill_latency_ns >= 60_000_000


def test_momentum_ignition_and_self_impact_concentration() -> None:
    graph = SlidingTemporalMultigraph()
    ext = StreamingMicrostructureExtractor(graph)
    symbol = "ETH-USDT"
    cur_px = 3_500 * SCALE
    tick = 1_000_000
    t0 = 1_700_000_000_000_000_000
    igniter = 888

    seq = np.zeros((TEMPORAL_SEQ_LEN, NUM_FEATURES), dtype=np.float32)
    for sw in range(12):
        cur_px += 5 * tick
        ts = t0 + sw * 20_000_000
        graph.ingest_trade(
            fill_id=sw + 1,
            symbol=symbol,
            maker_account_id=10 + sw,
            taker_account_id=igniter,
            price_fp=cur_px,
            qty_fp=5 * SCALE,
            exec_ts_ns=ts,
            aggressor_side="BUY",
        )
        _, seq = ext.on_trade_fill(
            fill_id=sw + 1,
            symbol=symbol,
            maker_account_id=10 + sw,
            taker_account_id=igniter,
            aggressor_side="BUY",
            price_fp=cur_px,
            qty_fp=5 * SCALE,
            exec_ts_ns=ts,
        )

    latest = seq[-1]
    assert latest[8] > 0.25  # self_impact_concentration
    assert latest[9] > 0.25  # momentum_ignition_burst
    assert latest[15] >= 0.99  # 100% taker_sweep_aggression


@settings(max_examples=100, deadline=None)
@given(
    u128_val=st.integers(min_value=0, max_value=UINT128_MAX),
    i128_val=st.integers(min_value=INT128_MIN, max_value=INT128_MAX),
    price_fp=st.integers(min_value=1, max_value=(1 << 62)),
    qty_fp=st.integers(min_value=1, max_value=(1 << 62)),
)
def test_property_u128_i128_and_decimal_lossless_invariants(
    u128_val: int,
    i128_val: int,
    price_fp: int,
    qty_fp: int,
) -> None:
    hi_u, lo_u = int_to_u128_parts(u128_val)
    assert u128_parts_to_int(hi_u, lo_u) == u128_val

    hi_i, lo_i = int_to_i128_parts(i128_val)
    assert i128_parts_to_int(hi_i, lo_i) == i128_val

    notional = compute_notional_fp(price_fp, qty_fp)
    assert isinstance(notional, int)
    assert notional == (price_fp * qty_fp) // SCALE

    dec = format_fp_to_decimal(price_fp)
    assert parse_decimal_to_fp(dec) == price_fp


@settings(max_examples=50, deadline=None)
@given(
    ops=st.lists(
        st.tuples(
            st.sampled_from(["SUBMIT", "MODIFY", "CANCEL", "FILL"]),
            st.sampled_from(["BUY", "SELL"]),
            st.integers(min_value=1_000_000, max_value=100_000 * SCALE),
            st.integers(min_value=100_000, max_value=50 * SCALE),
            st.integers(min_value=1_000_000, max_value=500_000_000),
        ),
        min_size=5,
        max_size=50,
    )
)
def test_property_streaming_features_finite_and_bounded(
    ops: list[tuple[str, str, int, int, int]],
) -> None:
    graph = SlidingTemporalMultigraph()
    ext = StreamingMicrostructureExtractor(graph)
    symbol = "BTC-USDT"
    acct = 42
    counterparty = 99
    ts = 1_000_000_000_000
    oid = 1

    for idx, (kind, side, px, qty, dt) in enumerate(ops, start=1):
        ts += dt
        side_lit = "BUY" if side == "BUY" else "SELL"
        if kind == "SUBMIT":
            oid += 1
            seq = ext.on_order_submit(
                order_id=oid,
                account_id=acct,
                symbol=symbol,
                side=side_lit,
                price_fp=px,
                qty_fp=qty,
                ts_ns=ts,
            )
        elif kind == "MODIFY":
            seq = ext.on_order_modify(
                order_id=oid,
                account_id=acct,
                symbol=symbol,
                new_price_fp=px,
                new_qty_fp=qty,
                ts_ns=ts,
            )
        elif kind == "CANCEL":
            seq = ext.on_order_cancel(
                order_id=oid,
                account_id=acct,
                symbol=symbol,
                ts_ns=ts,
                canceled_qty_override_fp=qty,
                side_hint=side_lit,
                price_hint_fp=px,
            )
        else:
            graph.ingest_trade(
                fill_id=idx,
                symbol=symbol,
                maker_account_id=counterparty,
                taker_account_id=acct,
                price_fp=px,
                qty_fp=qty,
                exec_ts_ns=ts,
                aggressor_side=side_lit,
            )
            _, seq = ext.on_trade_fill(
                fill_id=idx,
                symbol=symbol,
                maker_account_id=counterparty,
                taker_account_id=acct,
                aggressor_side=side_lit,
                price_fp=px,
                qty_fp=qty,
                exec_ts_ns=ts,
            )

        assert seq.dtype == np.float32
        assert seq.shape == (TEMPORAL_SEQ_LEN, NUM_FEATURES)
        assert bool(np.all(np.isfinite(seq)))
        assert bool(np.all(seq >= 0.0))
        assert float(seq[-1, 2]) <= 1.0 + 1e-6
        assert float(seq[-1, 3]) <= 1.0 + 1e-6
        assert float(seq[-1, 5]) <= 1.0 + 1e-6
        assert float(seq[-1, 15]) <= 1.0 + 1e-6
