from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "src")))

import hyperion_proto as hp  # noqa: E402


class HyperionProtoContractTest(unittest.TestCase):
    def test_i128_and_u128_two_complement_roundtrip(self) -> None:
        for val in [0, 1, -1, (1 << 64) + 99, -((1 << 99) + 123456789), hp.MAX_I128, hp.MIN_I128]:
            pb = hp.to_i128(val)
            self.assertEqual(hp.from_i128(pb), val)

        for val in [0, 1, (1 << 64) + 7, (1 << 120) + 42, hp.MAX_U128]:
            pb_u = hp.to_u128(val)
            self.assertEqual(hp.from_u128(pb_u), val)

    def test_notional_fixed_point_math(self) -> None:
        self.assertEqual(hp.compute_notional_fp(6_842_550_000_000, 125_000_000), 8_553_187_500_000)

    def test_fraud_orderbook_chronos_wire_roundtrip(self) -> None:
        edge = hp.telemetry_fraud_pb2.TransactionGraphEdge(
            edge_id=1,
            fill_id=77,
            engine_seq=1050,
            symbol="BTC-USDT",
            maker_account_id=501,
            taker_account_id=502,
            aggressor_side=hp.common_pb2.ORDER_SIDE_BUY,
            price_fp=6_850_000_000_000,
            qty_fp=100_000_000,
            notional_fp=hp.to_u128(6_850_000_000_000),
            exec_ts_ns=1_795_013_000_000_000_000,
            temporal_decay_weight_ppm=950_000,
        )
        shap = hp.telemetry_fraud_pb2.SHAPFeatureAttribution(
            feature_name="cycle_closure_score",
            feature_value_fp=98_000_000,
            baseline_value_fp=2_000_000,
            shap_contribution_ppm=640_000,
            importance_rank=1,
        )
        alert = hp.telemetry_fraud_pb2.AnomalyAlert(
            alert_id="alert-py-01",
            engine_seq=1050,
            symbol="BTC-USDT",
            pattern_type=hp.telemetry_fraud_pb2.ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE,
            severity_score=95,
            confidence_ppm=972_000,
            offender_account_ids=[501, 502, 503],
            evidence_subgraph=[edge],
            shap_attributions=[shap],
            wash_trading_cycle=hp.telemetry_fraud_pb2.WashTradingCycle(
                cycle_id="cycle-abc",
                participant_account_ids=[501, 502, 503, 501],
                cycle_edges=[edge],
                cycle_length=3,
                circular_volume_fp=hp.to_u128(20_550_000_000_000),
                volume_symmetry_ppm=991_000,
            ),
            inference_latency_us=1420,
            model_version="sentinel-ensemble-v1",
        )
        raw = alert.SerializeToString()
        decoded = hp.telemetry_fraud_pb2.AnomalyAlert.FromString(raw)
        self.assertEqual(decoded.severity_score, 95)
        self.assertEqual(decoded.shap_attributions[0].feature_name, "cycle_closure_score")
        self.assertEqual(hp.from_u128(decoded.wash_trading_cycle.circular_volume_fp), 20_550_000_000_000)


if __name__ == "__main__":
    unittest.main()
