from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / "contracts" / "gen" / "python" / "src"))
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from hyperion_proto import common_pb2
from hyperion_omega_proto import (
    consensus_pb2,
    margin_risk_pb2,
    settlement_l2_pb2,
    wasm_quant_pb2,
    zk_solvency_pb2,
)


def test_all_five_omega_proto_domains_roundtrip() -> None:
    pv = consensus_pb2.PreVote(
        cluster_id=1,
        prospective_term=5,
        candidate_id=3,
        last_log_index=99,
        last_log_term=4,
    )
    assert consensus_pb2.PreVote.FromString(pv.SerializeToString()).prospective_term == 5

    inc = zk_solvency_pb2.InclusionSumProof(
        epoch_id=8,
        snapshot_engine_seq=42_000,
        asset="USDT",
        user_balance_fp=125_000_000_000,
        expected_root_sum_fp=common_pb2.U128(hi=0, lo=9_000_000_000_000),
    )
    assert zk_solvency_pb2.InclusionSumProof.FromString(inc.SerializeToString()).user_balance_fp == 125_000_000_000

    adl = margin_risk_pb2.ADLTrigger(
        adl_id="adl-01",
        engine_seq=777,
        bankrupt_account_id=404,
        symbol="BTC-USDT",
        uncovered_deficit_fp=common_pb2.U128(hi=0, lo=50_000_000_000),
        system_solvency_preserved=True,
    )
    assert margin_risk_pb2.ADLTrigger.FromString(adl.SerializeToString()).system_solvency_preserved is True

    fuel = wasm_quant_pb2.FuelMeteringReport(
        strategy_id="stat_arb_zscore",
        account_id=88,
        version=2,
        fuel_allocated=50_000,
        fuel_consumed=11_420,
        fuel_remaining=38_580,
        execution_latency_ns=8_400,
    )
    assert wasm_quant_pb2.FuelMeteringReport.FromString(fuel.SerializeToString()).fuel_consumed == 11_420

    batch = settlement_l2_pb2.StateRootBatchCommit(
        batch_id=10,
        epoch_id=8,
        chain_id=994201,
        start_engine_seq=1,
        end_engine_seq=42_000,
    )
    assert settlement_l2_pb2.StateRootBatchCommit.FromString(batch.SerializeToString()).chain_id == 994201
