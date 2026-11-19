"""End-to-End Real-Time Fraud & Manipulation Detection Engine (Sentinel-ML).

Integrates:
  1. SlidingTemporalMultigraph (incremental k <= 6 Tarjan-SCC + Depth-Aware Johnson cycles)
  2. StreamingMicrostructureExtractor (OTR, spoofing, layering, momentum ignition, velocity)
  3. SentinelOnnxRuntime (INT8 CPU quantized Temporal Autoencoder + L2-cached IsolationForest)
  4. FastVectorizedKernelSHAP (exact-efficiency local feature attributions)
with < 10 ms p99 latency on both single events and micro-batches.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass
from pathlib import Path
import time

import numpy as np
import numpy.typing as npt

from sentinel_ml.dataset_generator import (
    LABEL_LATENCY_ARBITRAGE,
    LABEL_MOMENTUM_IGNITION,
    LABEL_SPOOFING_LAYERING,
    LABEL_VELOCITY_SPIKE,
    LABEL_WASH_TRADING,
)
from sentinel_ml.fixed_point import PPM_SCALE, ratio_to_ppm
from sentinel_ml.graph_engine import (
    DetectedWashCycle,
    SideLiteral,
    SlidingTemporalMultigraph,
    SubgraphSnapshot,
    TradeExecutionEdge,
)
from sentinel_ml.microstructure import (
    NUM_FEATURES,
    TEMPORAL_SEQ_LEN,
    SpoofingLayeringEvidence,
    StreamingMicrostructureExtractor,
    VelocitySpikeEvidence,
    alloc_aligned64_f32,
)
from sentinel_ml.models_onnx import SentinelOnnxRuntime
from sentinel_ml.shap_explainer import FastVectorizedKernelSHAP, FeatureAttributionItem


@dataclass(frozen=True, slots=True)
class AccountNodeSummaryRecord:
    """Domain summary aligned with hyperion.v1.AccountNodeSummary."""

    account_id: int
    max_severity_score: int
    order_to_trade_ratio_ppm: int
    cancel_rate_ppm: int
    traded_volume_fp: int
    active_alert_count: int


@dataclass(frozen=True, slots=True)
class FraudAlertRecord:
    """Complete explainable alert aligned 1:1 with hyperion.v1.AnomalyAlert."""

    alert_id: str
    engine_seq: int
    symbol: str
    pattern_type: int
    severity_score: int
    confidence_ppm: int
    autoencoder_recon_error_ppm: int
    isolation_forest_score_ppm: int
    offender_account_ids: tuple[int, ...]
    evidence_subgraph: tuple[TradeExecutionEdge, ...]
    shap_attributions: tuple[FeatureAttributionItem, ...]
    wash_trading_cycle: DetectedWashCycle | None
    spoofing_layering: SpoofingLayeringEvidence | None
    velocity_spike: VelocitySpikeEvidence | None
    detected_ts_ns: int
    inference_latency_us: int
    model_version: str
    trace_id: str = ""
    span_id: str = ""
    correlation_id: str = ""


@dataclass(frozen=True, slots=True)
class SubgraphInspectionReport:
    """Domain response aligned with hyperion.v1.InspectFraudSubgraphResponse."""

    nodes: tuple[AccountNodeSummaryRecord, ...]
    edges: tuple[TradeExecutionEdge, ...]
    related_alerts: tuple[FraudAlertRecord, ...]
    top_attributions: tuple[FeatureAttributionItem, ...]
    snapshot_ts_ns: int


class SentinelDetectionEngine:
    """Production real-time market surveillance & fraud forensics engine."""

    def __init__(
        self,
        artifacts_dir: Path,
        *,
        graph_window_ns: int = 60_000_000_000,
        feature_window_ns: int = 30_000_000_000,
        alert_cooldown_ns: int = 2_000_000_000,
        max_recent_alerts: int = 512,
        use_int8: bool = True,
    ) -> None:
        self.graph = SlidingTemporalMultigraph(
            window_ns=graph_window_ns,
            max_cycle_depth=6,
            decay_tau_ns=15_000_000_000,
            min_wash_score=0.25,
        )
        self.extractor = StreamingMicrostructureExtractor(
            self.graph,
            window_ns=feature_window_ns,
        )
        self.runtime = SentinelOnnxRuntime(artifacts_dir, use_int8=use_int8)
        self.explainer = FastVectorizedKernelSHAP(self.runtime)

        self.alert_cooldown_ns = int(alert_cooldown_ns)
        self._alert_seq: int = 0
        self._recent_alerts: deque[FraudAlertRecord] = deque(maxlen=max_recent_alerts)
        self._alerts_by_account: dict[int, deque[FraudAlertRecord]] = {}
        self._last_alert_ts: dict[tuple[int, int], int] = {}
        self._max_severity_by_acct: dict[int, int] = {}
        self._alert_count_by_acct: dict[int, int] = {}

        # Pre-allocated 64B cache-line-aligned hot-path tensors (zero per-event NumPy allocations)
        self._single_buf: npt.NDArray[np.float32] = alloc_aligned64_f32(
            (1, TEMPORAL_SEQ_LEN, NUM_FEATURES)
        )
        self._pair_buf: npt.NDArray[np.float32] = alloc_aligned64_f32(
            (2, TEMPORAL_SEQ_LEN, NUM_FEATURES)
        )

    def process_execution_fill(
        self,
        *,
        fill_id: int,
        engine_seq: int,
        symbol: str,
        maker_account_id: int,
        taker_account_id: int,
        aggressor_side: SideLiteral,
        price_fp: int,
        qty_fp: int,
        exec_ts_ns: int,
        maker_order_id: int = 0,
        trace_id: str = "",
        span_id: str = "",
        correlation_id: str = "",
    ) -> list[FraudAlertRecord]:
        """End-to-end streaming trade execution handler: Graph + Features + ONNX + SHAP."""
        t0_ns = time.perf_counter_ns()

        _, cycles = self.graph.ingest_trade(
            fill_id=fill_id,
            symbol=symbol,
            maker_account_id=maker_account_id,
            taker_account_id=taker_account_id,
            price_fp=price_fp,
            qty_fp=qty_fp,
            exec_ts_ns=exec_ts_ns,
            aggressor_side=aggressor_side,
            engine_seq=engine_seq,
        )
        m_seq, t_seq = self.extractor.on_trade_fill(
            fill_id=fill_id,
            symbol=symbol,
            maker_account_id=maker_account_id,
            taker_account_id=taker_account_id,
            aggressor_side=aggressor_side,
            price_fp=price_fp,
            qty_fp=qty_fp,
            exec_ts_ns=exec_ts_ns,
            maker_order_id=maker_order_id,
            engine_seq=engine_seq,
        )

        cand_accts: tuple[int, ...]
        if maker_account_id == taker_account_id:
            cand_accts = (maker_account_id,)
            self._single_buf[0] = m_seq
            batch_seqs = self._single_buf
        else:
            cand_accts = (maker_account_id, taker_account_id)
            self._pair_buf[0] = m_seq
            self._pair_buf[1] = t_seq
            batch_seqs = self._pair_buf

        inf_res = self.runtime.predict_batch(batch_seqs)
        alerts: list[FraudAlertRecord] = []

        best_cycle = cycles[0] if cycles else None
        wash_handled_accounts: set[int] = set()
        if best_cycle is not None and best_cycle.wash_score >= 0.35:
            lead_acct = best_cycle.participant_account_ids[0]
            if self._check_cooldown(lead_acct, LABEL_WASH_TRADING, exec_ts_ns):
                if lead_acct == maker_account_id:
                    lead_seq = m_seq
                elif lead_acct == taker_account_id:
                    lead_seq = t_seq
                else:
                    lead_seq = self.extractor.get_account_sequence(lead_acct, exec_ts_ns)
                lead_idx = 0 if lead_acct == maker_account_id else (len(cand_accts) - 1)
                fx, _, shap_items = self.explainer.explain(lead_seq)
                cycle_conf = min(0.99, 0.55 + 0.45 * best_cycle.wash_score)
                eff_conf = max(float(inf_res.confidence[lead_idx]), fx, cycle_conf)
                cycle_sev = min(100, int(round(65.0 + 35.0 * best_cycle.wash_score)))
                sev = max(int(inf_res.severity_score[lead_idx]), cycle_sev)
                sub = self.graph.extract_subgraph(
                    lead_acct, symbol=symbol, max_hops=2, max_edges=32
                )
                elapsed_us = max(1, (time.perf_counter_ns() - t0_ns) // 1_000)

                alert = self._register_alert(
                    engine_seq=engine_seq,
                    symbol=symbol,
                    pattern_type=LABEL_WASH_TRADING,
                    severity_score=sev,
                    confidence=eff_conf,
                    recon_mse=float(inf_res.recon_error_mse[lead_idx]),
                    if_score=float(inf_res.iforest_score[lead_idx]),
                    offender_account_ids=best_cycle.participant_account_ids,
                    evidence_subgraph=sub.edges if sub.edges else best_cycle.cycle_edges,
                    shap_attributions=shap_items,
                    wash_cycle=best_cycle,
                    spoofing=None,
                    velocity=None,
                    detected_ts_ns=exec_ts_ns,
                    inference_latency_us=elapsed_us,
                    trace_id=trace_id,
                    span_id=span_id,
                    correlation_id=correlation_id,
                )
                alerts.append(alert)
                wash_handled_accounts.update(best_cycle.participant_account_ids)

        for idx_a, acct_id in enumerate(cand_accts):
            if acct_id in wash_handled_accounts:
                continue
            if bool(inf_res.is_anomaly[idx_a]):
                pat = int(inf_res.pattern_class[idx_a])
                if not self._check_cooldown(acct_id, pat, exec_ts_ns):
                    continue
                seq_a = batch_seqs[idx_a]
                _, _, shap_items = self.explainer.explain(seq_a)
                sub = self.graph.extract_subgraph(acct_id, symbol=symbol, max_hops=2, max_edges=32)
                wash_ev, spoof_ev, vel_ev = self._build_pattern_detail(acct_id, pat, sub)
                elapsed_us = max(1, (time.perf_counter_ns() - t0_ns) // 1_000)
                alert = self._register_alert(
                    engine_seq=engine_seq,
                    symbol=symbol,
                    pattern_type=pat,
                    severity_score=int(inf_res.severity_score[idx_a]),
                    confidence=float(inf_res.confidence[idx_a]),
                    recon_mse=float(inf_res.recon_error_mse[idx_a]),
                    if_score=float(inf_res.iforest_score[idx_a]),
                    offender_account_ids=(acct_id,),
                    evidence_subgraph=sub.edges,
                    shap_attributions=shap_items,
                    wash_cycle=wash_ev,
                    spoofing=spoof_ev,
                    velocity=vel_ev,
                    detected_ts_ns=exec_ts_ns,
                    inference_latency_us=elapsed_us,
                    trace_id=trace_id,
                    span_id=span_id,
                    correlation_id=correlation_id,
                )
                alerts.append(alert)

        return alerts

    def process_l3_order_event(
        self,
        *,
        op: str,
        engine_seq: int,
        symbol: str,
        order_id: int,
        account_id: int,
        side: SideLiteral,
        price_fp: int,
        qty_fp: int,
        event_ts_ns: int,
        trace_id: str = "",
        span_id: str = "",
        correlation_id: str = "",
    ) -> FraudAlertRecord | None:
        """Process L3 ADD / MODIFY / CANCEL book delta and run ONNX spoofing/layering inference."""
        t0_ns = time.perf_counter_ns()
        op_upper = op.upper()
        if op_upper == "ADD":
            seq = self.extractor.on_order_submit(
                order_id=order_id,
                account_id=account_id,
                symbol=symbol,
                side=side,
                price_fp=price_fp,
                qty_fp=max(1, qty_fp),
                ts_ns=event_ts_ns,
            )
        elif op_upper == "MODIFY":
            seq = self.extractor.on_order_modify(
                order_id=order_id,
                account_id=account_id,
                symbol=symbol,
                new_price_fp=max(1, price_fp),
                new_qty_fp=max(1, qty_fp),
                ts_ns=event_ts_ns,
            )
        elif op_upper == "CANCEL":
            seq = self.extractor.on_order_cancel(
                order_id=order_id,
                account_id=account_id,
                symbol=symbol,
                ts_ns=event_ts_ns,
                canceled_qty_override_fp=qty_fp,
                side_hint=side,
                price_hint_fp=price_fp,
            )
        else:
            return None

        self._single_buf[0] = seq
        inf_res = self.runtime.predict_batch(self._single_buf)
        if not bool(inf_res.is_anomaly[0]):
            return None

        pat = int(inf_res.pattern_class[0])
        if not self._check_cooldown(account_id, pat, event_ts_ns):
            return None

        _, _, shap_items = self.explainer.explain(seq)
        sub = self.graph.extract_subgraph(account_id, symbol=symbol, max_hops=2, max_edges=32)
        wash_ev, spoof_ev, vel_ev = self._build_pattern_detail(account_id, pat, sub)
        elapsed_us = max(1, (time.perf_counter_ns() - t0_ns) // 1_000)

        return self._register_alert(
            engine_seq=engine_seq,
            symbol=symbol,
            pattern_type=pat,
            severity_score=int(inf_res.severity_score[0]),
            confidence=float(inf_res.confidence[0]),
            recon_mse=float(inf_res.recon_error_mse[0]),
            if_score=float(inf_res.iforest_score[0]),
            offender_account_ids=(account_id,),
            evidence_subgraph=sub.edges,
            shap_attributions=shap_items,
            wash_cycle=wash_ev,
            spoofing=spoof_ev,
            velocity=vel_ev,
            detected_ts_ns=event_ts_ns,
            inference_latency_us=elapsed_us,
            trace_id=trace_id,
            span_id=span_id,
            correlation_id=correlation_id,
        )

    def evaluate_microbatch_sequences(
        self, sequences: npt.NDArray[np.float32]
    ) -> tuple[npt.NDArray[np.int32], npt.NDArray[np.float32]]:
        """Direct vectorized micro-batch inference helper for throughput & latency benchmarks."""
        res = self.runtime.predict_batch(sequences)
        return res.severity_score, res.confidence

    def _build_pattern_detail(
        self,
        account_id: int,
        pattern_type: int,
        sub: SubgraphSnapshot,
    ) -> tuple[
        DetectedWashCycle | None,
        SpoofingLayeringEvidence | None,
        VelocitySpikeEvidence | None,
    ]:
        if pattern_type == LABEL_WASH_TRADING and sub.recent_cycles:
            return sub.recent_cycles[-1], None, None
        if pattern_type == LABEL_SPOOFING_LAYERING:
            return None, self.extractor.build_spoofing_evidence(account_id), None
        if pattern_type in (LABEL_VELOCITY_SPIKE, LABEL_LATENCY_ARBITRAGE, LABEL_MOMENTUM_IGNITION):
            return None, None, self.extractor.build_velocity_evidence(account_id)
        return None, self.extractor.build_spoofing_evidence(account_id), None

    def _check_cooldown(self, account_id: int, pattern_type: int, ts_ns: int) -> bool:
        key = (account_id, pattern_type)
        prev = self._last_alert_ts.get(key)
        if prev is not None and ts_ns - prev < self.alert_cooldown_ns:
            return False
        self._last_alert_ts[key] = ts_ns
        return True

    def _register_alert(
        self,
        *,
        engine_seq: int,
        symbol: str,
        pattern_type: int,
        severity_score: int,
        confidence: float,
        recon_mse: float,
        if_score: float,
        offender_account_ids: tuple[int, ...],
        evidence_subgraph: tuple[TradeExecutionEdge, ...],
        shap_attributions: tuple[FeatureAttributionItem, ...],
        wash_cycle: DetectedWashCycle | None,
        spoofing: SpoofingLayeringEvidence | None,
        velocity: VelocitySpikeEvidence | None,
        detected_ts_ns: int,
        inference_latency_us: int,
        trace_id: str,
        span_id: str,
        correlation_id: str,
    ) -> FraudAlertRecord:
        self._alert_seq += 1
        alert = FraudAlertRecord(
            alert_id=f"ALT-{self._alert_seq:08d}",
            engine_seq=engine_seq,
            symbol=symbol,
            pattern_type=pattern_type,
            severity_score=max(0, min(100, severity_score)),
            confidence_ppm=ratio_to_ppm(confidence, clamp_unit=True),
            autoencoder_recon_error_ppm=max(0, int(round(recon_mse * PPM_SCALE))),
            isolation_forest_score_ppm=int(round(if_score * PPM_SCALE)),
            offender_account_ids=offender_account_ids,
            evidence_subgraph=evidence_subgraph,
            shap_attributions=shap_attributions,
            wash_trading_cycle=wash_cycle,
            spoofing_layering=spoofing,
            velocity_spike=velocity,
            detected_ts_ns=detected_ts_ns,
            inference_latency_us=inference_latency_us,
            model_version=self.runtime.model_version,
            trace_id=trace_id,
            span_id=span_id,
            correlation_id=correlation_id,
        )
        self._recent_alerts.append(alert)
        for acct in offender_account_ids:
            dq = self._alerts_by_account.get(acct)
            if dq is None:
                dq = deque(maxlen=16)
                self._alerts_by_account[acct] = dq
            dq.append(alert)
            prev_max = self._max_severity_by_acct.get(acct, 0)
            self._max_severity_by_acct[acct] = max(prev_max, alert.severity_score)
            self._alert_count_by_acct[acct] = self._alert_count_by_acct.get(acct, 0) + 1
        return alert

    def list_recent_alerts(
        self,
        *,
        symbol: str = "",
        min_severity_score: int = 0,
        limit: int = 50,
    ) -> list[FraudAlertRecord]:
        eff_limit = max(1, min(limit if limit > 0 else 50, 500))
        out: list[FraudAlertRecord] = []
        for alert in reversed(self._recent_alerts):
            if symbol and alert.symbol != symbol:
                continue
            if alert.severity_score < min_severity_score:
                continue
            out.append(alert)
            if len(out) >= eff_limit:
                break
        return out

    def inspect_fraud_subgraph(
        self,
        *,
        center_account_id: int,
        symbol: str = "",
        max_hops: int = 2,
        lookback_window_ns: int = 0,
    ) -> SubgraphInspectionReport:
        sub = self.graph.extract_subgraph(
            center_account_id,
            symbol=symbol,
            max_hops=max_hops if max_hops > 0 else 2,
            lookback_window_ns=lookback_window_ns,
        )
        nodes: list[AccountNodeSummaryRecord] = []
        for acct in sub.account_ids:
            otr_ppm, cancel_ppm = self.extractor.get_account_summary_metrics(acct)
            nodes.append(
                AccountNodeSummaryRecord(
                    account_id=acct,
                    max_severity_score=self._max_severity_by_acct.get(acct, 0),
                    order_to_trade_ratio_ppm=otr_ppm,
                    cancel_rate_ppm=cancel_ppm,
                    traded_volume_fp=self.graph.get_account_traded_volume_fp(acct),
                    active_alert_count=self._alert_count_by_acct.get(acct, 0),
                )
            )

        related = tuple(self._alerts_by_account.get(center_account_id, ()))
        if related:
            top_attr = related[-1].shap_attributions
        else:
            seq = self.extractor.get_account_sequence(center_account_id)
            _, _, top_attr = self.explainer.explain(seq)

        return SubgraphInspectionReport(
            nodes=tuple(nodes),
            edges=sub.edges,
            related_alerts=related,
            top_attributions=top_attr,
            snapshot_ts_ns=sub.snapshot_ts_ns,
        )
