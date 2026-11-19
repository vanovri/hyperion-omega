from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "src")))

import hyperion_proto as hp  # noqa: E402


class HyperionProtoContractTest(unittest.TestCase):
    def test_i128_and_u128_two_complement_roundtrip_and_boundaries(self) -> None:
        for val in [0, 1, -1, (1 << 64) + 99, -((1 << 99) + 123456789), hp.MAX_I128, hp.MIN_I128]:
            pb = hp.to_i128(val)
            self.assertEqual(hp.from_i128(pb), val)

        with self.assertRaises(ValueError):
            hp.to_i128(hp.MIN_I128 - 1)
        with self.assertRaises(ValueError):
            hp.to_i128(hp.MAX_I128 + 1)

        for val in [0, 1, (1 << 64) + 7, (1 << 120) + 42, hp.MAX_U128]:
            pb_u = hp.to_u128(val)
            self.assertEqual(hp.from_u128(pb_u), val)

        with self.assertRaises(ValueError):
            hp.to_u128(-1)
        with self.assertRaises(ValueError):
            hp.to_u128(hp.MAX_U128 + 1)

    def test_notional_and_fee_fixed_point_math_boundaries(self) -> None:
        self.assertEqual(hp.compute_notional_fp(6_842_550_000_000, 125_000_000), 8_553_187_500_000)
        trunc0 = hp.compute_notional_fp(3, 33_333_333)
        self.assertIsInstance(trunc0, int)
        self.assertEqual(trunc0, 0)
        self.assertEqual(hp.compute_notional_fp(3, 33_333_334), 1)
        self.assertEqual(hp.compute_notional_fp(0, 100_000_000), 0)
        self.assertEqual(hp.compute_notional_fp(100_000_000, 0), 0)

        with self.assertRaises(ValueError):
            hp.compute_notional_fp(-1, 10)
        with self.assertRaises(ValueError):
            hp.compute_notional_fp(10, -1)

        # Fee & maker rebate math + boundaries
        self.assertEqual(hp.checked_fee_fp(8_553_187_500_000, -20), -171_063_750)
        self.assertEqual(hp.checked_fee_fp(8_553_187_500_000, 100), 855_318_750)
        self.assertEqual(hp.checked_fee_fp(0, 100), 0)
        self.assertEqual(hp.checked_fee_fp(999_999, -1), 0)
        self.assertEqual(hp.checked_fee_fp(1_000_000, -1), -1)
        self.assertEqual(hp.checked_fee_fp(999_999, 1), 0)
        self.assertEqual(hp.checked_fee_fp(1_000_000, 1), 1)
        self.assertEqual(hp.checked_fee_fp(1_000_000, hp.MIN_I64), hp.MIN_I64)
        self.assertEqual(hp.checked_fee_fp(1_000_000, hp.MAX_I64), hp.MAX_I64)
        self.assertEqual(hp.checked_fee_fp(hp.MAX_U128, 0), 0)

        with self.assertRaises(ValueError):
            hp.checked_fee_fp(-1, 10)
        with self.assertRaises(ValueError):
            hp.checked_fee_fp(hp.MAX_U128 + 1, 1)
        with self.assertRaises(ValueError):
            hp.checked_fee_fp(100, hp.MIN_I64 - 1)
        with self.assertRaises(ValueError):
            hp.checked_fee_fp(100, hp.MAX_I64 + 1)
        with self.assertRaises(OverflowError):
            hp.checked_fee_fp(1_000_001, hp.MAX_I64)
        with self.assertRaises(OverflowError):
            hp.checked_fee_fp(1_000_001, hp.MIN_I64)

    def test_batch_zero_copy_and_aligned64_simd(self) -> None:
        prices = [
            684_255_000,
            350_120_000,
            184_500_000,
            245_000_000,
            100_000_000,
            0,
            300_000_000,
            500_000_000,
        ]
        qtys = [
            125_000_000,
            500_000_000,
            1_250_000_000,
            10_000_000_000,
            0,
            100_000_000,
            33_333_334,
            75_000_000,
        ]
        expected = [hp.compute_notional_fp(p, q) for p, q in zip(prices, qtys)]

        # Pass 2 zero-copy into pre-allocated list with non-zero sentinels
        out_zc = [999_999] * len(prices)
        self.assertEqual(hp.compute_notional_batch_zero_copy(prices, qtys, out_zc), len(prices))
        self.assertEqual(out_zc, expected)
        self.assertEqual(hp.compute_notional_batch_zero_copy([], [], []), 0)
        with self.assertRaises(ValueError):
            hp.compute_notional_batch_zero_copy([-1], [1], [0])
        with self.assertRaises(ValueError):
            hp.compute_notional_batch_zero_copy([1], [-1], [0])

        # Pass 3 cache-line aligned 64B SIMD NumPy u64
        p_al = hp.allocate_aligned64_u64(len(prices))
        q_al = hp.allocate_aligned64_u64(len(qtys))
        o_al = hp.allocate_aligned64_u64(len(prices))
        self.assertEqual(p_al.ctypes.data & 63, 0)
        self.assertEqual(q_al.ctypes.data & 63, 0)
        self.assertEqual(o_al.ctypes.data & 63, 0)
        p_al[:] = prices
        q_al[:] = qtys
        o_al[:] = 999_999
        self.assertEqual(hp.compute_notional_batch_aligned64_u64(p_al, q_al, o_al), len(prices))
        self.assertEqual([int(v) for v in o_al], expected)
        empty_al = hp.allocate_aligned64_u64(0)
        self.assertEqual(len(empty_al), 0)
        self.assertEqual(hp.compute_notional_batch_aligned64_u64(empty_al, empty_al, empty_al), 0)
        with self.assertRaises(ValueError):
            hp.allocate_aligned64_u64(-1)

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
