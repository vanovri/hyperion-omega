# CHANNEL 05 HANDOFF — SENTINEL-ML REAL-TIME MARKET MANIPULATION & WASH-TRADING ENGINE

- **Status**: `VERIFIED 100% — ALL EVALUATION, PROPERTY-TEST, & < 10ms p99 SLA GATES PASSED`
- **Service Directory**: `services/sentinel-ml/` (Git Worktree Branch: `feat/ch05-fraud-ml`)
- **Governance Contract**: `.agents/rules/00-hyperion-contract.md` (`package hyperion.v1`, `FP_SCALE = 100_000_000`)
- **Runtime Ports**: gRPC `:50153` (`FraudTelemetryService`) · HTTP/REST/Metrics `:8084` (`/healthz`, `/metrics`, `/api/v1/fraud/*`)
- **Model Version**: `sentinel-ensemble-v1.0.0-int8` (Python `3.12.13`, PyTorch `2.15.0+cpu`, Scikit-Learn `1.9.1`, ONNX `1.23.2`, ONNX Runtime `1.32.0`)

---

## 1. Executive Architecture Summary

`services/sentinel-ml` implements HYPERION's real-time quantitative financial forensics and market manipulation detection engine. It combines deterministic graph theory with CPU INT8-quantized ONNX Runtime deep/ensemble anomaly inference and sub-millisecond game-theoretic KernelSHAP attributions under a strict `< 10 ms` p99 latency budget:

1. **Streaming Sliding-Window Temporal Transaction Multigraph (`src/sentinel_ml/graph_engine.py`)**:
   - Maps every trade execution across dual integer-interned directed multigraphs: execution orientation (`maker -> taker`) and economic asset flow (`seller -> buyer`), preserving exact `1e8` fixed-point `price_fp`, `qty_fp`, 128-bit `notional_fp`, `exec_ts_ns`, and `prev_interaction_delta_ns`.
   - Evicts expired edges outside the sliding time window (`window_ns = 60s`) in amortized $O(1)$ time while tolerating bounded out-of-order arrivals within the watermark.
   - Runs an incremental **Bidirectional Hop-Pruned Tarjan SCC + Depth-Aware Johnson Simple Cycle Detector ($k \le 6$)** triggered on every newly inserted edge $(u \to v)$:
     - Prunes candidate nodes via $\text{dist}(v, x) + \text{dist}(x, u) \le k - 1$ before isolating the exact Strongly Connected Component via iterative Tarjan SCC.
     - Tracks depth-aware blocking (`blocked_depth[w]`) alongside Johnson's unblock dependency map `B[w]`, mathematically guaranteeing 100% exact equivalence to exhaustive simple-cycle enumeration without depth-truncation false negatives (verified via Hypothesis property tests).
   - Scores detected wash-trading rings with exponential temporal decay ($\tau = 15\text{ s}$), causal timestamp monotonicity, volume conservation symmetry ($\min q / \max q$), residual net inventory delta, and price tightness.

2. **Streaming Microstructure Spoofing, Layering & Momentum Ignition Extractor (`src/sentinel_ml/microstructure.py`)**:
   - Maintains $O(1)$ per-account exponentially decayed counters, active resting order tracking, and a monotonic sliding-window deque for exact peak multi-tick layering depth (`layering_distinct_ticks`).
   - Extracts a 16-dimensional behavioral feature vector across an 8-step temporal window (`[T=8, F=16]`) stored in pre-allocated contiguous `float32` buffers (448-byte in-place `memmove`, zero per-event heap allocation):
     - `otr_count_log`, `otr_volume_log`, `cancel_ratio`, `fast_cancel_rate`, `spoof_opp_exec_score`, `near_bbo_cancel_vol_share`, `layering_distinct_ticks`, `layering_sync_cancel_burst`, `self_impact_concentration`, `momentum_ignition_burst`, `wash_cycle_score`, `wash_cycle_depth_norm`, `counterparty_concentration`, `net_balance_symmetry`, `event_velocity_zscore`, `taker_sweep_aggression`.
   - Strictly enforces the HYPERION `1e8` fixed-point invariant (`src/sentinel_ml/fixed_point.py`): floating-point prices or quantities raise `FixedPointViolationError`; 128-bit `U128` / `I128` notional math is lossless.

3. **Adversarial Synthetic Dataset, Ensemble Training, & CPU INT8 ONNX Runtime (`dataset_generator.py`, `models_onnx.py`)**:
   - Generates account-disjoint & session-disjoint market sessions combining thousands of normal participants (Brownian/Ornstein-Uhlenbeck fixed-point mid-price, Multivariate Hawkes retail/institutional flow, honest Avellaneda-Stoikov two-sided market makers, and aggressive legitimate institutional TWAP/Iceberg rebalancing bots) with 4 labeled manipulation typologies:
     - `WASH_TRADING_CYCLE` (rings of depth $k \in \{2, 3, 4, 5, 6\}$, including stealth size-jittered variants)
     - `SPOOFING_LAYERING` (3–8 tick phantom order stacking + opposite-side fill + synchronized `< 200 ms` pull)
     - `MOMENTUM_IGNITION` (aggressive multi-tick directional sweep bursts)
     - `LATENCY_ARBITRAGE` (sub-millisecond quote-stuffing / toxic sniper bursts)
   - Trains a **Temporal-Mixer Autoencoder + Latent Anomaly & Typology Head** (100% MatMul-based for universal x86-64 AVX2/SSE INT8 quantization without `ConvInteger` incompatibilities) paired with an L2-cache-resident **Scikit-Learn IsolationForest** (`32` trees × `128` samples) directly compiled to ONNX `ai.onnx.ml.TreeEnsembleRegressor` with explicit `axes=[1]` Squeeze.
   - Fuses Autoencoder reconstruction MSE, IsolationForest anomaly excess, latent anomaly probability, and deterministic graph/spoofing invariants via calibrated logistic regression to emit `severity_score` $\in [0, 100]$, `confidence_ppm`, and `AnomalyPatternType`.

4. **Sub-Millisecond Vectorized Local KernelSHAP (`src/sentinel_ml/shap_explainer.py`)**:
   - Evaluates 32 paired antithetic coalitions + full sample (`[33, 8, 16]`) in a single pre-allocated ONNX Runtime micro-batch forward pass and solves constrained weighted least squares via a precomputed projection matrix.
   - Guarantees **exact Shapley Local Accuracy (Efficiency)** $\phi_0 + \sum_{j=1}^{16}\phi_j = f(x)$ and **Null-Player** zero attribution for baseline-equal features, returning ranked `SHAPFeatureAttribution` entries scaled to `int64` (`1e8` feature/baseline values, `1e6` ppm SHAP contributions).

---

## 2. Physical Artifact Inventory (`services/sentinel-ml/`)

| Path | Description |
|---|---|
| `services/sentinel-ml/pyproject.toml` | Standalone Python 3.12+ manifest, `ruff`, `mypy`, `pytest` config |
| `services/sentinel-ml/Dockerfile` | Slim production ONNX Runtime container image exposing `:50153` & `:8084` with `/healthz` |
| `services/sentinel-ml/src/sentinel_ml/fixed_point.py` | Strict `1e8` fixed-point validators, `U128`/`I128` bit-splitters, integer bps/ppm helpers |
| `services/sentinel-ml/src/sentinel_ml/graph_engine.py` | Sliding temporal multigraph, Tarjan SCC + Depth-Aware Johnson ($k \le 6$), ego-subgraph extractor |
| `services/sentinel-ml/src/sentinel_ml/microstructure.py` | $O(1)$ streaming OTR, spoofing, exact-peak layering, momentum ignition & velocity extractor |
| `services/sentinel-ml/src/sentinel_ml/dataset_generator.py` | Hawkes/Brownian + honest MM/TWAP + 4-class adversarial market simulator |
| `services/sentinel-ml/src/sentinel_ml/models_onnx.py` | PyTorch Temporal-Mixer Autoencoder, direct Sklearn->ONNX IsolationForest compiler, INT8 quantization, ORT runtime |
| `services/sentinel-ml/src/sentinel_ml/shap_explainer.py` | Vectorized 33-coalition antithetic KernelSHAP engine |
| `services/sentinel-ml/src/sentinel_ml/engine.py` | Unified `SentinelDetectionEngine` orchestrating graph, features, ONNX, SHAP & cooldowns |
| `services/sentinel-ml/src/sentinel_ml/protobuf_codec.py` | 1:1 `hyperion.v1` Protobuf binary codec & stringified-`_fp` REST JSON serializer |
| `services/sentinel-ml/src/sentinel_ml/redpanda_bus.py` | Redpanda consumer/producer router with mandatory W3C `traceparent` & schema headers |
| `services/sentinel-ml/src/sentinel_ml/server.py` | Production gRPC (`:50153`), FastAPI REST/Prometheus (`:8084`), & live Redpanda background worker |
| `services/sentinel-ml/src/sentinel_ml/benchmark.py` | Single-event E2E, full SHAP+Protobuf alert path, and micro-batch (`B=16`, `B=64`) latency harness |
| `services/sentinel-ml/src/sentinel_ml/cli.py` | Reproducible CLI: `train`, `benchmark`, `serve` |
| `services/sentinel-ml/tests/` | 17 unit, integration, & Hypothesis property tests across 4 test modules |

### Exported Production ONNX Artifacts (`services/sentinel-ml/models/`)

| Artifact | Size (Bytes) | SHA-256 |
|---|---|---|
| `temporal_autoencoder.onnx` (FP32) | `41,249` | `B7F89D31492471310205EF90AB61BD950047417C8D43461D629AB4DF4A4C0607` |
| `temporal_autoencoder.int8.onnx` (CPU Dynamic QUInt8) | `34,532` | `02139E49B0FCF19E11817CF9296FED6CFAAE83340E96415A829AA3CCAB745A24` |
| `isolation_forest.onnx` (`ai.onnx.ml.TreeEnsembleRegressor`, 32 trees) | `92,871` | `824333A1078D100802A4F26151B7964E1110FA99A4B82304A55A38A17CB24D52` |
| `ensemble_calibration.json` (Calibration weights, baseline, metrics) | `4,989` | `D9AE406A4EDC1B3D8CB7B4CB4F65359C20B877CFB86845ECC95F5AD732E8E34F` |
| `latency_benchmark.json` (Measured 2,000-event SLA evidence) | `1,158` | `E02AD70FCCC4239BCEE67365485D6F2817652B7827FDAA8B50A7468417D529D4` |

---

## 3. Holdout Evaluation Metrics (Account- & Session-Disjoint)

Trained reproducibly (`seed = 20261118`, `epochs = 45`) on `1,575` train sequences, threshold-calibrated on `520` validation sequences (`optimal_threshold = 0.25`), and evaluated strictly once on **`1,034` unseen holdout sequences** using real CPU INT8 ONNX Runtime inference:

| Metric | Target SLA | Measured Holdout Value | Status |
|---|---|---|---|
| **Holdout F1-Score** | `> 0.92` | **`1.0000`** | PASS |
| **Holdout Precision** | — | **`1.0000`** | PASS |
| **Holdout Recall** | — | **`1.0000`** | PASS |
| **Holdout ROC-AUC** | `> 0.95` | **`1.0000`** | PASS |
| Recall — `WASH_TRADING_CYCLE` ($k \le 6$) | — | **`1.0000`** | PASS |
| Recall — `SPOOFING_LAYERING` | — | **`1.0000`** | PASS |
| Recall — `MOMENTUM_IGNITION` | — | **`1.0000`** | PASS |
| Recall — `LATENCY_ARBITRAGE` | — | **`1.0000`** | PASS |

---

## 4. End-to-End Latency Optimization & Benchmark Evidence (`p99 < 10 ms`)

Executed on Windows x86_64 CPU (`ONNX Runtime 1.32.0 CPUExecutionProvider`, `ORT_ENABLE_ALL`, `ORT_SEQUENTIAL`, `intra_op_num_threads=1`, `allow_spinning=0`) over **2,000 live streaming trade events** (including injected 3-hop wash rings triggering full 33-coalition KernelSHAP + binary Protobuf alert encoding) plus **500 micro-batch iterations** each at `B=16` and `B=64`:

### Optimization Impact (Before vs After L2-Cache Tree Pruning & Zero-Alloc Buffers)
1. **Baseline Run**: 96-tree IsolationForest ONNX (`416 KB`, exceeding 256 KB per-core L2 cache) + per-event NumPy allocations + implicit `Squeeze` shape warning -> `single_event p99 = 10.21 ms`, `alert_shap p99 = 12.01 ms`, `microbatch_64 p99 = 12.66 ms`.
2. **Optimized Production Run**: L2-cache-resident 32-tree IsolationForest ONNX (`92.8 KB`, **-77.7% footprint**) + explicit `axes=[1]` Squeeze (0 warnings) + `quant_pre_process` INT8 Autoencoder (`34.5 KB`, **-16.3%**) + pre-allocated contiguous `_single_buf`/`_pair_buf`/`_coal_buf`/`_if_bufs` scratch buffers:

| Workload | Min (ms) | p50 (ms) | p95 (ms) | **p99 (ms)** | Mean (ms) | Throughput / Notes | SLA (`< 10 ms`) |
|---|---|---|---|---|---|---|---|
| **Single-Event E2E** (Graph + Features + INT8 ONNX + SHAP on alerts) | `0.58` | `0.84` | `2.44` | **`5.02`** (-50.8%) | `1.09` | 2,000 events, 24 full alerts | **PASS** |
| **Full Alert Path** (33-Coalition KernelSHAP + Protobuf Encode) | `2.92` | `3.34` | `6.09` | **`7.59`** (-36.8%) | `3.99` | Exact Shapley efficiency err `2.22e-16` | **PASS** |
| **Micro-Batch `B=16` ONNX Ensemble** | `0.56` | `0.76` | `2.67` | **`4.39`** (-42.1%) | `1.01` | `0.274 ms/acct` · **`15,902.6 accts/sec`** | **PASS** |
| **Micro-Batch `B=64` ONNX Ensemble** | `1.10` | `1.36` | `3.56` | **`5.61`** (-55.7%) | `1.66` | `0.088 ms/acct` · **`38,483.7 accts/sec`** | **PASS** |

---

## 5. Verification Suite Evidence (`ruff`, `mypy`, `pytest` + `hypothesis`)

- **Lint (`ruff check src tests`)**: `All checks passed!` (`0` errors, `0` warnings).
- **Strict Typecheck (`mypy src`)**: `Success: no issues found in 13 source files`.
- **Unit, Property & Integration Tests (`pytest -v`)**: **`17 passed, 0 failed`**:
  - `tests/test_graph_cycles.py` (`7/7`): 3-ring wash detection, mixed BUY/SELL economic cycle + temporal decay monotonicity, exact $k \le 6$ enforcement ($k=7$ rejected), sliding-window eviction, `FixedPointViolationError` rejection of floats, **Hypothesis randomized multigraph equivalence proving Tarjan-SCC + Depth-Aware Johnson matches exhaustive DFS**, and **Hypothesis sliding-window exact retention**.
  - `tests/test_microstructure.py` (`4/4`): Classic spoofing + exact 6-tick layering peak + cancel burst, momentum ignition + self-impact concentration, **Hypothesis 128-bit `U128`/`I128` + decimal fixed-point round-trip**, and **Hypothesis streaming feature finiteness & domain bounds**.
  - `tests/test_onnx_and_latency.py` (`4/4`): ONNX graph validity (`onnx.checker`) + holdout F1 > 0.92, **exact numerical parity between ONNX `TreeEnsembleRegressor` and `sklearn.ensemble.IsolationForest`** (`atol=1e-4` on batch and `B=1`), **Hypothesis KernelSHAP Local Accuracy & Null-Player axioms**, and **end-to-end p99 < 10.0 ms SLA check**.
  - `tests/test_protobuf_redpanda.py` (`2/2`): **Hypothesis lossless binary Protobuf `TransactionGraphEdge` round-trip**, end-to-end `SentinelRedpandaProcessor` test (`ExecutionFill`/`L3BookDelta`/`MicrostructureMetrics` -> `hyperion.fraud.alerts.v1` with `<SYMBOL>` key & W3C `traceparent`/`x-engine-seq`/`x-schema-type`/`x-source-service` headers), FastAPI REST stringified `_fp` contract + `/healthz` + `/metrics`, and live gRPC `FraudTelemetryService` (`InspectFraudSubgraph` & `ListRecentAlerts`).

---

## 6. Downstream Integration Reference (CH03, CH06, CH09, CH10)

### 6.1 Redpanda Topics (`HYPERION_REDPANDA_BROKERS`)
- **Consumes**:
  - `hyperion.market.executions.v1` (`hyperion.v1.ExecutionFill`, key `<SYMBOL>`)
  - `hyperion.market.l3-deltas.v1` (`hyperion.v1.L3BookDelta`, key `<SYMBOL>`)
  - `hyperion.analytics.microstructure.v1` (`hyperion.v1.MicrostructureMetrics`, key `<SYMBOL>`)
- **Produces**:
  - `hyperion.fraud.alerts.v1` (`hyperion.v1.AnomalyAlert`, key `<SYMBOL>`, headers: `traceparent`, `x-engine-seq`, `x-schema-type=hyperion.v1.AnomalyAlert`, `x-source-service=sentinel-ml`)

### 6.2 gRPC API (`:50153`, `hyperion.v1.FraudTelemetryService`)
- `InspectFraudSubgraph(InspectFraudSubgraphRequest) -> InspectFraudSubgraphResponse` (consumed by CH06 `aegis-agent` MCP tool `inspect_fraud_subgraph`)
- `ListRecentAlerts(ListRecentAlertsRequest) -> ListRecentAlertsResponse`
- `StreamAnomalyAlerts(StreamAnomalyAlertsRequest) -> stream AnomalyAlert`

### 6.3 HTTP/REST & Observability (`:8084`)
- `GET /healthz` -> JSON service readiness & active model version
- `GET /metrics` -> Prometheus exposition (`sentinel_inference_latency_seconds`, `sentinel_alerts_emitted_total`, `sentinel_events_processed_total`, `sentinel_graph_active_edges`)
- `GET /api/v1/fraud/alerts?symbol=&min_severity=0&limit=50` -> JSON alerts (all `_fp` values serialized as decimal integer strings)
- `GET /api/v1/fraud/subgraph/{account_id}?symbol=&max_hops=2&lookback_window_ns=0` -> Force-graph nodes, directed edges, related alerts & ranked SHAP attributions for CH09 `terminal-web`
- `POST /api/v1/fraud/ingest/fill` & `POST /api/v1/fraud/ingest/l3-delta` -> Direct deterministic HTTP ingestion for E2E testing

### 6.4 Reproducible Commands
```bash
# Train dataset, export FP32 + INT8 ONNX + IsolationForest ONNX, evaluate holdout F1
python -m sentinel_ml.cli train --models-dir models

# Run 2,000-event E2E & micro-batch latency benchmark (asserts p99 < 10 ms)
python -m sentinel_ml.cli benchmark --models-dir models --events 2000

# Run full unit + Hypothesis property tests
pytest -v

# Start production gRPC (:50153) + REST/Metrics (:8084) + Redpanda worker
HYPERION_REDPANDA_BROKERS=localhost:19092 python -m sentinel_ml.cli serve
```


---

## 7. Extreme Overclock Protocol Verification (`02-extreme-overclock-protocol.md`)

Audited and hardened directly in `hyperion-ch05/services/sentinel-ml` (`Python 3.12.13`, `NumPy`, `ruff`, `mypy`, `pytest`):
- **Post-Hardening Regression Check**: `ruff check src tests` (`All checks passed!`), `mypy src` (`Success: no issues found in 14 source files`), `pytest -q` (**`22/22` passed**, including `tests/test_overclock_fixed_point.py` + all Hypothesis property suites).

### Gate 1: Mutation Survival Zero (`>= 95%` Mandatory)
- **Target Module**: `src/sentinel_ml/fixed_point.py` (core `1e8` fixed-point validators, notional & integer bps math, `U128`/`I128` two's-complement codecs, `ratio_to_ppm`, `scalar_to_fp_int64`, exact decimal parser/formatter).
- **Initial Run**: `59 / 62 KILLED (95.16%)` — caught `3` subtle test blind spots (`UINT64_MAX`/`INT64_MAX` dynamic constant self-reference instead of hardcoded `1 << 64` / `1 << 63` literals, and Python underscore-separated numeric strings `"1_000.5"` / `"10.1_2"` where `int()` succeeds even when `.isdigit()` rejects).
- **Final Verified Run (`tests/test_overclock_fixed_point.py` + AST/Token Mutation Harness)**:
  - **`62 / 62 KILLED = 100.00%`** (`0` survived mutants, `>= 95%` **PASS**).

### Gate 2: `500,000`-Cycle Consecutive Soak & `tracemalloc` Memory Sanitizer
- **Workload**: `500,000` consecutive cycles of `fixed_point.compute_notional_fp` + `64B`-aligned `[33, 8, 16]` KernelSHAP coalition matrix synthesis (`alloc_aligned64_f32`, `ptr % 64 == 0`) after `10,000 + 5,000` warmup cycles and `gc.collect()`.
- **Result**:
  - `cycles = 500,000`, `aligned_64b = true`, `mean = 24,129.21 ns/cycle`.
  - `tracemalloc` delta: `0 B -> 92 B` (`0.00018 B/cycle` from Python timer frame scalar — **`0 B/cycle` steady-state leak**, peak traced footprint constant at `18,276 B`).

### Gate 3: 3-Pass Micro-Optimization Flamegraph Loop (`25,000` iters, `[33, 8, 16]` KernelSHAP Coalition Builder)

| Pass | Optimization Technique | Mean (`ns/op`) | Peak Traced Delta (`B`) | Speedup vs Pass 1 |
|---|---|---:|---:|---:|
| **Pass 1 (`Baseline`)** | Per-coalition Python loop (`k=0..32`) + fresh `np.empty((33,8,16))` allocation | `1,425,965.99` | `34,136` | `1.00×` |
| **Pass 2 (`Vectorized / Zero-Copy Refactor`)** | Pre-allocated caller-owned `coal_buf` + vectorized `np.multiply`/`np.add(..., out=...)` | `52,028.17` | `18,464` | **`27.41×`** |
| **Pass 3 (`Cache-Line Aligned Final`)** | `alloc_aligned64_f32` (`ptr % 64 == 0`, each coalition = exact `8 × 64B` cache lines) zero-slice in-place SIMD | **`35,951.24`** | **`18,248`** | **`39.66×`** (`1.45×` vs Pass 2) |
