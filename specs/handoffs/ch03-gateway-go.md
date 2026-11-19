# CHANNEL 03 HANDOFF — EDGE API GATEWAY & ZERO-COPY BINARY WEBSOCKET MULTIPLEXER (`services/gateway-go`)

- **Status**: `VERIFIED 100% — ALL OVERCLOCK GATES PASSED (VET=0 | STATICCHECK=0 | -RACE=0 | MUTATION=100.00% | 5,120 WS=0 LEAK | 0 B/op POOL)`
- **Worktree / Branch**: `C:\Users\ardhy\.gemini\antigravity\scratch\hyperion-ch03` (`feat/ch03-gateway-go`)
- **Locked Exclusive Scope**: `/services/gateway-go/**` and `/specs/handoffs/ch03-gateway-go.md`
- **Owner**: Channel 03 (Principal Cloud & Network Engineer — High-Concurrency Go)
- **Contract Dependencies Read**: `.agents/rules/00-hyperion-contract.md`, `.agents/rules/02-extreme-overclock-protocol.md`, `specs/handoffs/ch01-foundation.md`, `/proto/*.proto`, `/contracts/gateway-openapi.yaml`, `/contracts/ws-binary-protocol.md`

---

## 1. Executive Architecture & Strict Invariants

`services/gateway-go` is a high-concurrency, zero-float, zero-copy Edge API Gateway bridging external REST & Binary WebSocket clients (`:8080`) and Prometheus/health probes (`:8081`) to the HYPERION Rust/Python gRPC microservice mesh (`ledger-rs :50151`, `stream-aggregator :50152`, `sentinel-ml :50153`, `chronos-replay :50154`), Chaos-Sim control plane (`:8086`), Redis 7 (`:6379`), and Redpanda (`:19092`).

### Strict Invariants Verified
1. **Zero Floating-Point Financial Math**: `0` executable occurrences of `float32`/`float64` across the entire `services/gateway-go` codebase. All prices, quantities, notional values, fees, and balances strictly use `uint64`/`int64`/`math/big.Int` fixed-point `1e8` (`FP_SCALE = 100_000_000`) serialized as decimal strings in JSON DTOs; all rate-limiter quotas use integer milli-tokens (`1 token = 1,000 milli-tokens`); all adaptive circuit-breaker ratios use integer parts-per-million (`ppm`, scale `1_000_000`).
2. **Strict Directory Isolation**: Only files under `services/gateway-go/**` and `specs/handoffs/ch03-gateway-go.md` were created or modified. Protobuf/gRPC contracts are consumed strictly read-only via `replace github.com/vanovri/hyperion/contracts/gen/go => ../../contracts/gen/go`.

---

## 2. Subsystem Implementation Inventory

```text
services/gateway-go/
├── go.mod / go.sum                               # Go 1.24 module importing ../../contracts/gen/go
├── Dockerfile                                    # Multi-stage distroless/static nonroot binary image (:8080, :8081)
├── cmd/gateway/main.go                           # Dual HTTP/WS (:8080) & Admin/Metrics (:8081) servers + graceful drain
└── internal/
    ├── config/config.go                          # Canonical HYPERION port, Redis, Redpanda, auth, pool & WS env config
    ├── instruments/
    │   ├── registry.go                           # O(1) instrument lookup (1=BTC-USDT..4=HYPE-USDT) + tick/lot validator
    │   └── registry_test.go                      # Boundary, alignment, and market-vs-limit unit tests
    ├── telemetry/metrics.go                      # Lock-free atomic Prometheus /metrics exporter + W3C traceparent engine
    ├── auth/
    │   ├── keystore.go                           # Thread-safe Ed25519 + HMAC-SHA256 Account KeyStore + dev/sim derivation
    │   ├── jwt.go                                # Constant-time HS256 & EdDSA JWT + hyperion_session cookie verifier
    │   ├── replay_cache.go                       # 64-shard cache-line-padded 2-gen Bloom + Ring L1 & Redis SET NX L2 guard
    │   ├── signature.go                          # Canonical preimage verifier (§6.2), ±5s skew check, verify-before-insert
    │   └── auth_test.go                          # Ed25519/HMAC, cross-node Redis replay, 40k 0-FP saturation, JWT tests
    ├── ratelimit/
    │   ├── limiter.go                            # L1 lock-free hotSlots atomic milli-token bucket + L2 Redis Lua script +
    │   │                                         # progressive order-spam quarantine + signed RiskMitigationCommand handler
    │   └── limiter_test.go                       # 2-node miniredis Lua quota sync, spam quarantine TTL, Aegis freeze/restore
    ├── idempotency/
    │   ├── middleware.go                         # 64-shard LRU + in-flight singleflight coalescer + Redis L2 + 409 SHA-256 check
    │   └── middleware_test.go                    # 64-goroutine concurrent duplicate storm (exact 1 upstream exec) + 409 conflict
    ├── grpcclient/
    │   ├── breaker.go                            # Lock-free rolling-window ppm Adaptive Circuit Breaker + SRE adaptive shedding
    │   ├── pool.go                               # Multiplexed HTTP/2 ConnPool (P2C least-inflight, deadline & trace, idempotent retry)
    │   ├── clients.go                            # Typed pooled clients for Orderbook, StreamAnalytics, FraudTelemetry, Chronos
    │   └── pool_test.go                          # Real gRPC loopback: idempotent retry vs mutation no-retry, deadline, breaker
    ├── ws/
    │   ├── protocol.go                           # Exact 32B LE framing (0x4859, Castagnoli CRC32C, 0x01..0x08 & 0x81..0x83)
    │   │                                         # + tiered sync.Pool atomic ref-counted PooledFrame with 10B RFC6455 headroom
    │   ├── ringbuf.go                            # Per-client bounded head-dropping MarketRingBuffer (releases evicted PooledFrames)
    │   ├── client.go                             # Zero-bufio RFC 6455 upgrader/pumps, net.Buffers (writev) coalescing,
    │   │                                         # critical vs droppable queues, slow-consumer eviction & 1002 protocol errors
    │   ├── hub.go                                # 32-shard fan-out Hub, O(1) private account index, hot L2 cache & gap resync
    │   └── ws_test.go                            # Byte-exact framing, CRC32C, auto gap resync, FraudAlert proto, JSON fallback, 1002
    ├── redpanda/
    │   ├── bridge.go                             # franz-go Kafka consumer (7 topics) + async LZ4 order command producer
    │   └── bridge_test.go                        # All 7 canonical Protobuf topics -> Hub & HybridLimiter end-to-end dispatch
    ├── httpapi/
    │   ├── dto.go                                # Strict OpenAPI 3.1 stringified 1e8 fixed-point DTO mappers
    │   ├── handlers.go                           # All 13 OpenAPI operations + /ws + /metrics + /api/v1/auth/keys + CORS
    │   └── server_test.go                        # Full REST + Ed25519 + JWT + gRPC + Redpanda FREEZE_ACCOUNT -> HTTP 429
    ├── overclock/
    │   └── mutation_audit_test.go                # Parallel 5-worker Go AST operator mutator (target >= 95% kill score)
    └── torture/
        ├── torture_bench_test.go                 # 5,120-WS torture test, slow-consumer drops/evicts, replay bomb, breaker, benchmarks
        └── soak_overclock_test.go                # 500,000-cycle zero-leak soak + pprof + 3-pass micro-optimization suite
```

---

## 3. Core Architecture Highlights

### 3.1 Dual Cryptographic Authentication & Anti-Replay (`internal/auth`)
- **UI / Audit / Control Plane**: Constant-time `HS256` & `EdDSA` JWT verification via `Authorization: Bearer <JWT>` or `hyperion_session` cookie (explicitly bans `alg: "none"`).
- **Low-Latency Trading Endpoints**: Enforces exact headers (`X-Hyperion-Account-Id`, `X-Hyperion-Key-Id`, `X-Hyperion-Timestamp`, `X-Hyperion-Nonce`, `X-Hyperion-Signature`, `X-Hyperion-Auth-Scheme`) and canonical UTF-8 preimage:
  `<X-Hyperion-Timestamp>\n<X-Hyperion-Nonce>\n<HTTP_METHOD_UPPERCASE>\n<REQUEST_PATH>\n<SHA256_HEX_LOWERCASE(RAW_BODY)>`
- **Verify-Before-Insert Anti-Replay**: Cryptographic signature (`Ed25519` or constant-time `HMAC-SHA256`) and `±5s` clock skew are verified *before* committing `(account_id, nonce)` into the 64-shard cache-line-padded **2-generation Kirsch-Mitzenmacher Bloom Filter + Bounded Ring Exact Map** (L1) and Redis `SET NX PX` (L2), preventing unauthenticated nonce-poisoning DoS while blocking single-node and cross-node replays with `0%` false positives across 40,000 saturated unique nonces.

### 3.2 Two-Tier Hybrid Rate Limiter & Aegis Risk Enforcement (`internal/ratelimit`)
- **L1 Per-Node Lock-Free Fast Path**: `1,024`-slot direct-mapped lock-free `atomic.Pointer[accountBucket]` (`hotSlots`) + cache-line-isolated per-account atomic milli-tokens + `hasRiskState` fast bypass + read-before-write `dirty` flag to eliminate cross-core MESI cache-line bouncing (`112.2 ns/op`, `0 B/op`, `0 allocs/op` under 8-core contention).
- **L2 Distributed Redis Lua Sync**: Periodic asynchronous reconciliation (`L2SyncInterval = 50ms`) executes an atomic integer-only Redis Lua script (`HMGET`/`HMSET`/`PEXPIRE`) synchronizing consumed deltas, cross-node token ceilings, freezes, and rate overrides.
- **Auto-Spam Quarantine & Redpanda Mitigations**: Order-spam bursts exceeding `SpamMaxEventsWindow` escalate progressively (`Level 1: 50% RPS` → `Level 2: 10% RPS` → `Level 3: SPAM_PENALTY_QUARANTINE`). Consuming `hyperion.risk.mitigations.v1` enforces Ed25519-signed `FREEZE_ACCOUNT`, `THROTTLE_RATE_LIMIT`, `ENABLE_VOLATILITY_GUARD`, and `RESTORE_NORMAL` while safely honoring `dry_run` and `requires_human_approval`.

### 3.3 Multiplexed gRPC Pool, Adaptive Circuit Breaker & Idempotency (`internal/grpcclient`, `internal/idempotency`)
- **HTTP/2 Pool**: `ConnsPerTarget = 4` warm `grpc.ClientConn`s per upstream service (`ledger-rs :50151`, `stream-aggregator :50152`, `sentinel-ml :50153`, `chronos-replay :50154`) using Power-of-Two Choices least-inflight load balancing, strict caller deadline propagation (`X-Request-Timeout-Ms` / `DefaultRPCTimeout`), and W3C `traceparent` metadata injection.
- **Strict Idempotent-Only Retry**: Read & sandbox RPCs (`GetL2BookSnapshot`, `GetMerkleLedgerProof`, `GetCandles`, `TimeTravelSeek`, `TimelineFork`, etc.) retry with exponential backoff on transient `Unavailable`/`ResourceExhausted`/`Aborted`; order mutations (`SubmitOrder`, `ModifyOrder`, `CancelOrder`, `CreditDeposit`) have `idempotent = false` and **never** blind-retry.
- **Adaptive Circuit Breaker**: Lock-free rolling-window integer `ppm` error ratio (`Closed` → soft SRE adaptive shedding above 80% of trip threshold → hard `Open` at `500,000 ppm` → `Half-Open` probe gate → `Closed` recovery).
- **Idempotency-Key Middleware**: 64-shard LRU + singleflight in-flight channel coalescing (`64` simultaneous duplicate packets execute upstream **exactly once**) + L2 Redis persistence + `409 Conflict` on body SHA-256 mismatch.

### 3.4 Zero-Copy Binary WebSocket Multiplexer (`internal/ws`, `internal/redpanda`)
- **Zero-Copy Pre-Framed `PooledFrame`**: Tiered `sync.Pool` (`smallFramePool` 512B / `largeFramePool` 8KB) reserves **10 bytes of front headroom** so the unmasked RFC 6455 server header (`0x82`), 32-byte Little-Endian Hyperion header (`magic = 0x4859`, Castagnoli `CRC-32C`), and binary payload occupy a single contiguous buffer shared across all subscribers via lock-free atomic reference counting (`Retain`/`Release`), flushed via `net.Buffers` (`writev`/`WSASend`) with **zero per-subscriber serialization and `0 B/op` allocations**.
- **Smart Per-Client Backpressure & Deadlock-Free Eviction**:
  - Non-critical market streams (`MSG_L3_DELTA_BATCH`, `MSG_CANDLE_MICRO`) flow through per-client bounded head-dropping `MarketRingBuffer` (evicted frames immediately `Release()` back to `sync.Pool`, flagging `FlagSlowConsumerDroppedPrior = 0x04` and triggering automatic `MSG_GAP_RESYNC_NOTICE` + `MSG_L2_SNAPSHOT`).
  - Critical queues (`MSG_EXECUTION_FILL`, `MSG_FRAUD_ALERT`, `MSG_ORDER_ACK_PRIVATE`, resync snapshots) evict stalled consumers strictly **outside** Hub shard `RWMutex` locks (preventing `RLock → Lock` re-entrant deadlock under burst eviction).
  - Supports both binary mode (`Sec-WebSocket-Protocol: hyperion.bin.v1`) and JSON text fallback (`?format=json` / `hyperion.json.v1`), and closes invalid client frames with RFC 6455 `1002 Protocol Error`.

---

## 4. REST, WebSocket & Observability Endpoints (Downstream Reference)

| Listener | Method & Path | Auth Scheme | Upstream / Target |
|---|---|---|---|
| `:8080` & `:8081` | `GET /healthz` | Public | Gateway status + per-pool circuit breaker states |
| `:8080` & `:8081` | `GET /metrics` | Public | Prometheus `0.0.4` lock-free counters & gauges |
| `:8080` | `GET /ws` (`?format=binary\|json`, `?symbol=`) | Public / Optional JWT | Zero-Copy Binary / JSON WS Hub (`hyperion.bin.v1`) |
| `:8080` | `POST /api/v1/orders` | `X-Hyperion-*` Sig + `Idempotency-Key` | `ledger-rs :50151` `OrderbookService.SubmitOrder` |
| `:8080` | `PATCH /api/v1/orders/{order_id}` | `X-Hyperion-*` Sig + `Idempotency-Key` | `ledger-rs :50151` `OrderbookService.ModifyOrder` |
| `:8080` | `DELETE /api/v1/orders/{order_id}?symbol=` | `X-Hyperion-*` Sig + `Idempotency-Key` | `ledger-rs :50151` `OrderbookService.CancelOrder` |
| `:8080` | `POST /api/v1/accounts/deposits` | Bearer JWT + `Idempotency-Key` | `ledger-rs :50151` `OrderbookService.CreditDeposit` |
| `:8080` | `GET /api/v1/accounts/{account_id}/merkle-proof` | Bearer JWT | `ledger-rs :50151` `OrderbookService.GetMerkleLedgerProof` |
| `:8080` | `POST /api/v1/auth/keys` | Bearer JWT | Runtime Ed25519 / HMAC key registration |
| `:8080` | `GET /api/v1/market/orderbook` | Public | `ledger-rs :50151` `OrderbookService.GetL2BookSnapshot` |
| `:8080` | `GET /api/v1/analytics/candles` | Public | `stream-aggregator :50152` `GetCandles` |
| `:8080` | `GET /api/v1/analytics/microstructure` | Public | `stream-aggregator :50152` `GetLatestMicrostructure` |
| `:8080` | `GET /api/v1/fraud/alerts` | Bearer JWT | `sentinel-ml :50153` `ListRecentAlerts` |
| `:8080` | `GET /api/v1/chronos/seek` | Bearer JWT | `chronos-replay :50154` `TimeTravelSeek` |
| `:8080` | `POST /api/v1/chronos/fork` | Bearer JWT | `chronos-replay :50154` `TimelineFork` |
| `:8080` | `POST /api/v1/chaos/campaigns` | Bearer JWT | Reverse-proxy to `chaos-sim :8086` |

---

## 5. Verification, Concurrency Torture & Extreme Overclock Evidence

All commands executed directly on host (`go version go1.27.0 windows/amd64`, `Intel(R) Core(TM) i5-10300H CPU @ 2.50GHz`, `CGO_ENABLED=1`, `gcc` WinLibs UCRT, `staticcheck v0.8.2`).

### 5.1 Static Analysis, Zero-Float Audit & Full Race-Detector Suite
- `go vet ./...`: **PASS (`exit 0`, 0 warnings)**
- `staticcheck ./...`: **PASS (`exit 0`, 0 warnings)**
- `go build ./cmd/gateway`: **PASS (`exit 0`)**
- Zero-Float Audit (`\bfloat(32|64)\b` across all `.go` files): **PASS (`0` executable occurrences)**
- `go test -race -v -count=1 -timeout=5m ./...`: **PASS (`exit 0`, 10/10 packages green, `0` data races)**:
  - `internal/auth`: `TestSignatureAuth_Ed25519AndHMAC_ReplayAndClockSkew`, `TestReplayGuard_ZeroFalsePositivesUnderHighSaturation`, `TestJWTVerifier_HS256AndEdDSA` — **PASS**
  - `internal/grpcclient`: `TestConnPool_IdempotentRetry_NonIdempotentNoRetry_Deadline_And_BreakerRecovery` — **PASS**
  - `internal/httpapi`: `TestOpenAPIEndpoints_EndToEndIntegration_And_RedpandaDispatch` — **PASS**
  - `internal/idempotency`: `TestIdempotencyMiddleware_ConcurrentDuplicateStormAndConflict` — **PASS**
  - `internal/instruments`: `TestInstrumentRegistry_TickLotAlignmentAndBounds` — **PASS**
  - `internal/ratelimit`: `TestHybridLimiter_L1L2RedisLuaSync_SpamPenalty_And_RiskMitigations` — **PASS**
  - `internal/redpanda`: `TestBridge_DispatchAllCanonicalTopics` — **PASS**
  - `internal/ws`: `TestBinaryFraming_ByteExactComplianceAndProtocol1002Error` — **PASS**
  - `internal/torture`: `TestTorture_5100ConcurrentWebSockets_SlowConsumer_Replay_CircuitBreaker_ZeroGoroutineLeak`, `TestSoak_500000ConsecutiveCycles_ZeroSteadyStateHeapLeak_And_Pprof`, `TestOptPasses_LatencyPercentileDistribution` — **PASS**

### 5.2 5,120-Concurrent WebSocket Torture Test (`-race`)
```text
=== RUN   TestTorture_5100ConcurrentWebSockets_SlowConsumer_Replay_CircuitBreaker_ZeroGoroutineLeak
    Peak simultaneous WebSocket connections online: 5120 (dialErrors=0 firstErr=<nil> goroutines=15395)
    Torture Metrics: fastFramesRecv=307000 slowDrops=3079 slowEvicts=70 replayRejected=31 breakerTrips=1
    Goroutine Leak Audit: baseline=4 final=3 (after 5,100 concurrent WS clients = >15,300 spawned goroutines)
--- PASS: TestTorture_5100ConcurrentWebSockets_SlowConsumer_Replay_CircuitBreaker_ZeroGoroutineLeak (25.11s)
```
- **5,120 simultaneous live connections** (`15,395` concurrent goroutines under `-race`, `0` dial errors)
- **`307,000`** verified CRC32C binary frames delivered to fast subscribers without blocking
- **`3,079`** non-critical market frames dropped cleanly on slow-consumer ring buffers (`MarketRingBuffer`), **`70/70`** stalled consumers evicted cleanly
- **`31/32`** concurrent Ed25519 replay-attack requests rejected (`1` valid accepted)
- **`1`** real upstream outage tripped Adaptive Circuit Breaker to `OPEN`, followed by automatic `HALF_OPEN` → `CLOSED` recovery
- **`baseline=4 -> final=3`** (`0` leaked goroutines)

### 5.3 Overclock Gate 1 — AST Mutation Survival Zero (`Killed 40 / 40 = 100.00%`)
Executed via `HYPERION_RUN_MUTATION=1 go test -v -count=1 -timeout=5m -run TestMutationSurvivalAudit ./internal/overclock`:

| Core Module | Mutants Killed | Total Mutants | Kill Score | Survivors |
|---|---:|---:|---:|---|
| `./internal/instruments` (`registry.go`) | 8 | 8 | `100.00%` | `0` |
| `./internal/auth` (`signature.go`) | 8 | 8 | `100.00%` | `0` |
| `./internal/ws` (`protocol.go`) | 8 | 8 | `100.00%` | `0` |
| `./internal/grpcclient` (`breaker.go`) | 8 | 8 | `100.00%` | `0` |
| `./internal/ratelimit` (`limiter.go`) | 8 | 8 | `100.00%` | `0` |
| **TOTAL** | **40** | **40** | **`100.00%`** | **`[]` (ZERO)** |

### 5.4 Overclock Gate 2 — `500,000`-Cycle Consecutive Zero-Leak Soak + `runtime/pprof` (`-race`)
```text
=== RUN   TestSoak_500000ConsecutiveCycles_ZeroSteadyStateHeapLeak_And_Pprof
    500,000-Cycle Soak & pprof Audit: HeapAlloc before=12042088B after=785856B (delta=-11256232B) | HeapObjects before=2789 after=2093 (delta=-696) | pprofBytes=3240
--- PASS: TestSoak_500000ConsecutiveCycles_ZeroSteadyStateHeapLeak_And_Pprof (16.69s)
```
Steady-state live heap growth across 500,000 consecutive full hot-path cycles (replay check + L1 limiter CAS + circuit breaker + binary PooledFrame encode/ring-push/drain/CRC32C-decode/release): **`0 B` leaked (`-11,256,232 B`, `-696` live objects)** with verified `runtime/pprof` heap snapshot (`3,240 B`).

### 5.5 Overclock Gate 3 — 3-Pass Micro-Optimization Flamegraph Loop (Measured Before-vs-After)

| Optimization Pass | Architecture | 8-Core Throughput (`ns/op`) | Single-Thread `p50` (`ns/op`) | Single-Thread `p99` (`ns/op`) | Heap (`B/op`) | Allocs (`allocs/op`) | Speedup vs Baseline |
|---|---|---:|---:|---:|---:|---:|---:|
| **Pass 1 — `Baseline`** | Global `sync.Mutex` map + `fmt.Sprintf` + per-frame heap `make([]byte)` | `9,492 ns/op` | `8,385 ns` | `20,153 ns` | `1,009 B/op` | `16 allocs/op` | `1.00×` |
| **Pass 2 — `Zero-Copy Refactor`** | Tiered `sync.Pool` `PooledFrame` (10B RFC6455 headroom) + unpadded sharded map | `114.1 ns/op` | `199 ns` | `539 ns` | `0 B/op` | `0 allocs/op` | **`83.2×`** (`-98.8%` latency, `-100%` allocs) |
| **Pass 3 — `Cache-Line Aligned Final`** | Lock-free `hotSlots [1024]atomic.Pointer` + 64B cache-line padding + `hasRiskState` fast bypass + read-before-write `dirty` + `PooledFrame` | **`47.73 ns/op`** | **`139 ns`** | **`431 ns`** | **`0 B/op`** | **`0 allocs/op`** | **`198.9×`** (`-99.5%` latency vs Pass 1; **`2.39×`** faster than Pass 2 under 8-core contention) |

### 5.6 Raw `go test '-run=^$' '-bench=.' -benchmem -benchtime=1s ./internal/torture` Output
```text
goos: windows
goarch: amd64
pkg: github.com/vanovri/hyperion/services/gateway-go/internal/torture
cpu: Intel(R) Core(TM) i5-10300H CPU @ 2.50GHz
BenchmarkOptPass1_Baseline_AllocAndMutex-8             	  113059	      9492 ns/op	    1009 B/op	      16 allocs/op
BenchmarkOptPass2_ZeroCopyRefactor_UnpaddedShards-8    	 9807607	       114.1 ns/op	       0 B/op	       0 allocs/op
BenchmarkOptPass3_CacheAlignedFinal_HotPath-8          	22313749	        47.73 ns/op	       0 B/op	       0 allocs/op
BenchmarkEncodeL3SingleDelta_SyncPool-8                	10966755	       116.3 ns/op	       0 B/op	       0 allocs/op
BenchmarkEncodeExecutionFill_SyncPool-8                	10222970	       112.8 ns/op	       0 B/op	       0 allocs/op
BenchmarkEncodeL2Snapshot20Levels_SyncPool-8           	 4398316	       291.6 ns/op	       0 B/op	       0 allocs/op
BenchmarkMarketRingBuffer_ZeroAllocFanOut1000Rings-8   	   18106	     74128 ns/op	       0 B/op	       0 allocs/op
BenchmarkHybridLimiter_L1AtomicAllow-8                 	12714194	       112.2 ns/op	       0 B/op	       0 allocs/op
BenchmarkReplayGuard_L1BloomAndRing-8                  	 3998428	       375.2 ns/op	       2 B/op	       0 allocs/op
PASS
ok  	github.com/vanovri/hyperion/services/gateway-go/internal/torture	14.464s
```
