# CHANNEL 01 HANDOFF — HYPERION FOUNDATION, CONTRACTS, CODEGEN & INFRASTRUCTURE

- **Status**: `VERIFIED 100% — SINGLE SOURCE OF TRUTH LOCKED`
- **Canonical Workspace Root**: `C:\Users\ardhy\.gemini\antigravity\scratch\hyperion`
- **Governance Rule File**: `.agents/rules/00-hyperion-contract.md`
- **Owner**: Channel 01 (Principal Distributed Systems Architect)

---

## 1. Executive Architecture Summary

Channel 01 establishes the immutable schema, wire-framing, mathematical invariant, and local production infrastructure foundation for **HYPERION** across all 10 parallel engineering channels:

1. **Zero-Float Financial Invariant (`FP_SCALE = 100_000_000`, $10^8$)**:
   - Every price, quantity, notional amount, fee, VWAP/TWAP, OFI, and ledger balance across Protobuf (`uint64`, `int64`, `U128`, `I128`), OpenAPI 3.1 (`FixedPointUint` / `FixedPointInt` exact decimal strings), Binary WebSocket (`u64` / `i64` little-endian), and TimescaleDB (`BIGINT` / `NUMERIC(38, 0)`) uses integer fixed-point arithmetic.
   - `float` / `double` / `f32` / `f64` / `REAL` / `DOUBLE PRECISION` occurrences across all `.proto`, OpenAPI monetary fields, and SQL migrations = **`0`** (enforced programmatically in `scripts/validate_contracts.py` and `#![deny(clippy::float_arithmetic)]` in Rust).
2. **Windows Host TCP Exclusion-Safe Port Matrix**:
   - Host port inspection (`netsh interface ipv4 show excludedportrange protocol=tcp`) identified reserved Windows Hyper-V/WinNAT port exclusions on `50000–50059` and `64819–65418`.
   - All gRPC endpoints are deterministically allocated in **`50151–50154`** (`ledger-rs=50151`, `stream-aggregator=50152`, `sentinel-ml=50153`, `chronos-replay=50154`), preventing `WSAEACCES` socket bind failures on Windows.
3. **Pre-Generated & Compiled 4-Language Contract SDKs (`/contracts/gen/{rust,go,python,ts}`)**:
   - Downstream services (`CH02`–`CH09`) can import ready-to-use, pre-compiled packages without ever mutating files outside their isolated service directories:
     - **Rust (`contracts/gen/rust`, crate `hyperion-proto`)**: `prost` + `tonic` client/server stubs (`185,106` B generated `src/generated/hyperion.v1.rs`) + lossless `i128`/`u128` <-> `I128`/`U128` conversions and checked notional/fee helpers.
     - **Go (`contracts/gen/go`, module `github.com/vanovri/hyperion/contracts/gen/go`)**: 10 generated `.pb.go` & `_grpc.pb.go` files in package `hyperionv1` + `fixedpoint` `math/big.Int` 128-bit helpers.
     - **Python (`contracts/gen/python`, package `hyperion-proto`)**: 15 generated `_pb2.py`, `_pb2.pyi`, and `_pb2_grpc.py` modules (with relative intra-package imports patched automatically) + two's-complement 128-bit helpers.
     - **TypeScript (`contracts/gen/ts`, package `@hyperion/contracts`)**: `ts-proto` generated codecs with `forceLong=bigint` (all 64-bit Protobuf integers strictly typed as native `bigint`, eliminating IEEE-754 $2^{53}-1$ precision loss) + 32-byte little-endian Binary WebSocket header encoder/decoder.

---

## 2. Physical Artifact Inventory

| Path | Description |
|---|---|
| `.agents/rules/00-hyperion-contract.md` | Global workspace governance, directory ownership, port matrix, Redpanda topics & partition keys, fixed-point formulas, Blake3 WAL (`HYPWAL01`) & Snapshot (`HYPSNP01`) binary layout, and Ed25519/HMAC signing preimage |
| `proto/buf.yaml` | Buf v2 lint (`STANDARD`) & breaking-change configuration |
| `proto/common.proto` | `I128`, `U128`, `TraceMetadata`, `OrderSide`, `OrderType`, `SelfTradePreventionMode`, `AccountStatus`, `MarketRegime` |
| `proto/orderbook.proto` | `SubmitOrder`, `CancelOrder`, `ModifyOrder`, `OrderAck`, `ExecutionFill`, `L2BookSnapshot`, `L3BookDelta`, `LedgerJournalEntry`, `MerkleLedgerProof`, `OrderbookService` (`:50151`) |
| `proto/telemetry_fraud.proto` | `TransactionGraphEdge`, `AnomalyAlert` (`WashTradingCycle`, `SpoofingLayering`, `VelocitySpike`), `SHAPFeatureAttribution`, `RiskMitigationCommand`, `FraudTelemetryService` (`:50153`) |
| `proto/chronos.proto` | `TimeTravelSeekRequest`, `TimelineForkRequest` (`SimulationPolicyOverride`), `StateTreeDiffResponse`, `ReplayFrameStream`, `ChronosReplayService` (`:50154`) |
| `proto/stream_analytics.proto` | `OhlcvCandle`, `MicrostructureMetrics`, `LiquidityHeatmapBucket`, `VolumeProfileBin`, `StreamAnalyticsService` (`:50152`) |
| `contracts/gateway-openapi.yaml` | Full OpenAPI `3.1.0` REST specification (`13` operations, `165` resolved `$ref`s, stringified fixed-point schemas, Ed25519/HMAC & Bearer JWT security) |
| `contracts/ws-binary-protocol.md` | Sub-millisecond Little-Endian Binary WebSocket framing specification (`magic=0x4859`, 32-byte header, L2/L3/Fill/Candle/Fraud/Resync payloads) |
| `contracts/hyperion_descriptor.binpb` | Compiled Protobuf `FileDescriptorSet` |
| `contracts/gen/{rust,go,python,ts}` | Generated, type-checked, and unit-tested bindings across all 4 target languages |
| `docker-compose.infra.yml` | Local production stack: Redpanda (`v24.2.10`) + `redpanda-init`, TimescaleDB (`pg16`), Redis (`7.4-alpine`), OpenTelemetry Collector (`0.114.0`), Prometheus (`v3.0.1`), Jaeger (`1.62.0`) |
| `deploy/sql/001_init_timescaledb.sql` | Hypertables (`market_executions`, `l3_book_events`, `ohlcv_candles`, `microstructure_metrics`, `liquidity_heatmap_buckets`, `fraud_anomaly_alerts`, `ledger_journal_audit`), `stream_consumer_checkpoints`, columnar compression, retention policies, and `1m`/`5m`/`1h` continuous aggregates |
| `deploy/redpanda/init-topics.sh` | Automated provisioning of all 10 deterministic Redpanda topics |
| `deploy/otel/otel-collector-config.yaml` | OTLP gRPC (`4317`) & HTTP (`4318`) -> Jaeger traces + Prometheus metrics pipeline |
| `deploy/prometheus/prometheus.yml` | Scrape configuration for OTel, Redpanda, and all 7 backend microservices |
| `scripts/codegen.py` | One-command deterministic 4-language codegen orchestrator |
| `scripts/validate_contracts.py` | Automated validator for Protobuf, OpenAPI 3.1, Binary WS, SQL hypertables/caggs, zero-float rule, and Docker Compose |
| `scripts/healthcheck.py` | Automated TCP + HTTP readiness probe for infra and microservices |
| `justfile` & `Makefile` | Root task runners (`lint`, `codegen`, `test-contracts`, `infra-config`, `infra-up`, `infra-down`, `health`, `verify`) |

---

## 3. Downstream Integration Quick-Reference for Channels 02–10

- **Rust Services (`CH02 ledger-rs`, `CH04 stream-aggregator`, `CH08 chronos-replay`)**:
  ```toml
  [dependencies]
  hyperion-proto = { path = "../../contracts/gen/rust" }
  ```
- **Go Services (`CH03 gateway-go`, `CH07 chaos-sim`)**:
  ```go
  require github.com/vanovri/hyperion/contracts/gen/go v0.0.0
  replace github.com/vanovri/hyperion/contracts/gen/go => ../../contracts/gen/go
  ```
- **Python Service (`CH05 sentinel-ml`)**:
  ```bash
  pip install -e ../../contracts/gen/python
  # or add ../../contracts/gen/python/src to PYTHONPATH
  ```
- **TypeScript Services (`CH06 aegis-agent`, `CH09 terminal-web`)**:
  ```json
  {
    "dependencies": {
      "@hyperion/contracts": "file:../../contracts/gen/ts"
    }
  }
  ```

---

## 4. Verification & Token-Burn Loop Evidence

All verification gates were executed directly on the host terminal (`Windows x86_64`, `buf 1.75.0`, `libprotoc 36.2`, `just 1.59.0`, `rustc 1.99.1`, `go 1.27.0`, `Python 3.14.7`, `Node v24.20.0`, `Docker Compose v5.5.1`):

### 4.1 Schema, OpenAPI 3.1, TimescaleDB SQL & Docker Compose Validation (`just lint`)
```text
$ just lint
buf lint proto
python scripts/validate_contracts.py
[validate] Protobuf v3 lint, message inventory, and zero-float invariant: OK
[validate] OpenAPI 3.1.0 (13 operations, 165 $refs resolved) + WS Binary Spec: OK
[validate] TimescaleDB SQL migration (42 statements, hypertables + caggs + 0 floats): OK
[validate] docker-compose.infra.yml, OpenTelemetry Collector, and Prometheus configs: OK
[validate] ALL HYPERION CHANNEL 01 CONTRACT & INFRA CHECKS PASSED 100%.
```

### 4.2 4-Language Automated Codegen (`just codegen`)
```text
$ just codegen
[codegen] (.) $ buf build proto -o contracts/hyperion_descriptor.binpb
[codegen] (proto) $ protoc --go_out=... --go-grpc_out=... common.proto orderbook.proto telemetry_fraud.proto chronos.proto stream_analytics.proto
[codegen] (contracts\gen\go) $ go mod tidy
[codegen] (proto) $ python -m grpc_tools.protoc --python_out=... --pyi_out=... --grpc_python_out=...
[codegen] (proto) $ protoc --plugin=protoc-gen-ts_proto=... --ts_proto_out=...
[codegen] (contracts\gen\ts) $ npm.cmd run build
[codegen] (contracts\gen\rust) $ cargo build
[codegen] SUCCESS: Rust, Go, Python, and TypeScript contracts generated cleanly.
```

### 4.3 Multi-Language Compilation, Static Analysis & Roundtrip Tests (`just test-contracts`)
- **Rust (`cargo clippy --all-targets -- -D warnings` + `cargo test`)**:
  - `0` warnings, `3/3` unit tests passed (`roundtrip_i128_and_u128_extremes`, `fixed_point_notional_and_rebate_math`, `protobuf_wire_roundtrip_across_domains`).
- **Go (`go vet ./...` + `go test -v ./...`)**:
  - `0` warnings, `2/2` tests passed (`TestFixedPoint128Roundtrip`, `TestGeneratedProtobufWireSerialization`).
- **Python (`python -m unittest discover -s contracts/gen/python/tests -v`)**:
  - `3/3` tests passed (`test_i128_and_u128_two_complement_roundtrip`, `test_notional_fixed_point_math`, `test_fraud_orderbook_chronos_wire_roundtrip`).
- **TypeScript (`tsc --noEmit` + `node --test dist/contracts.test.js`)**:
  - `0` strict typecheck errors, `3/3` tests passed (`bigint fixed-point 128-bit roundtrip`, `SubmitOrder/RiskMitigationCommand/TimelineForkRequest binary Protobuf roundtrip`, `Binary WebSocket 32-byte little-endian header codec`).
