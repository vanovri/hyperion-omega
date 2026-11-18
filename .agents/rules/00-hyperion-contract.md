# HYPERION — GLOBAL ARCHITECTURAL CONTRACT & WORKSPACE GOVERNANCE (`SINGLE SOURCE OF TRUTH`)

- **Version**: `1.0.0` (Channel 01 Locked Contract)
- **Canonical Monorepo Root**: `C:\Users\ardhy\.gemini\antigravity\scratch\hyperion`
- **Protobuf Package**: `hyperion.v1` (`/proto/*.proto`)
- **Pre-Generated Multi-Language SDKs**: `/contracts/gen/{rust,go,python,ts}`

Dokumen ini adalah hukum arsitektur tertinggi (**Single Source of Truth**) untuk seluruh 10 channel agen pada monorepo **HYPERION** (*Distributed Deterministic Exchange, Graph Fraud AI, Time-Travel Engine & Autonomous SRE Mesh*). Setiap agen WAJIB mematuhi seluruh spesifikasi di bawah ini tanpa deviasi sedikit pun.

---

## 1. PETA STRUKTUR DIREKTORI MONOREPO & KEPEMILIKAN CHANNEL

```text
hyperion/
├── .agents/
│   └── rules/
│       └── 00-hyperion-contract.md        # [CH01] Global governance, invariants, ports, topics, wire formats
├── proto/                                 # [CH01] Canonical Protobuf v3 schemas (package hyperion.v1)
│   ├── buf.yaml                           # [CH01] Buf v2 lint & breaking-change config
│   ├── common.proto                       # [CH01] I128, U128, TraceMetadata, enums bersama
│   ├── orderbook.proto                    # [CH01] LOB commands, ExecutionFill, L2/L3, MerkleLedgerProof
│   ├── telemetry_fraud.proto              # [CH01] Graph edges, AnomalyAlert, SHAP, RiskMitigationCommand
│   ├── chronos.proto                      # [CH01] TimeTravelSeek, TimelineFork, StateTreeDiff, ReplayFrame
│   └── stream_analytics.proto             # [CH01] OHLCV Candles, Microstructure, VolumeProfile, Heatmap
├── contracts/                             # [CH01] REST OpenAPI 3.1, Binary WS spec, & pre-compiled SDKs
│   ├── gateway-openapi.yaml               # [CH01] OpenAPI 3.1.0 REST specification
│   ├── ws-binary-protocol.md              # [CH01] Little-Endian Binary WebSocket wire framing spec
│   ├── hyperion_descriptor.binpb          # [CH01] Compiled Protobuf FileDescriptorSet
│   └── gen/                               # [CH01] Pre-generated & type-checked multi-language bindings
│       ├── rust/                          # Crate: hyperion-proto (prost + tonic + i128/u128 helpers)
│       ├── go/                            # Module: github.com/vanovri/hyperion/contracts/gen/go
│       ├── python/                        # Package: hyperion-proto (hyperion_proto.* _pb2 & _pb2_grpc)
│       └── ts/                            # Package: @hyperion/contracts (strict TS types + codecs + WS constants)
├── services/
│   ├── ledger-rs/                         # [CH02 EXCLUSIVE] Rust LMAX LOB & Double-Entry Blake3 Merkle Ledger
│   ├── gateway-go/                        # [CH03 EXCLUSIVE] Go Edge REST + Zero-Copy Binary WS Gateway
│   ├── stream-aggregator/                 # [CH04 EXCLUSIVE] Rust Exactly-Once Microstructure & TimescaleDB Engine
│   ├── sentinel-ml/                       # [CH05 EXCLUSIVE] Python 3.12+ Temporal Graph + ONNX Fraud Engine
│   ├── aegis-agent/                       # [CH06 EXCLUSIVE] TypeScript Custom MCP Server & Autonomous SRE Agent
│   ├── chaos-sim/                         # [CH07 EXCLUSIVE] Go 1,000+ Bot Market Simulator & Chaos Fault Injector
│   └── chronos-replay/                    # [CH08 EXCLUSIVE] Rust Nanosecond Time-Travel & Counterfactual Fork Engine
├── apps/
│   └── terminal-web/                      # [CH09 EXCLUSIVE] Next.js + WebWorker + WebGL/Canvas 60fps Command UI
├── deploy/                                # [CH01 Infra Init / CH10 Full Prod K8s & Helm]
│   ├── sql/                               # [CH01] TimescaleDB hypertables, continuous aggregates, retention
│   ├── otel/                              # [CH01] OpenTelemetry Collector pipeline config
│   ├── prometheus/                        # [CH01] Prometheus scrape targets & alert rules
│   └── redpanda/                          # [CH01] Redpanda topic bootstrap script
├── e2e/                                   # [CH10 EXCLUSIVE] End-to-End Chaos Crucible & k6 Load Harness
├── specs/
│   └── handoffs/                          # [ALL CHANNELS] Per-channel architecture & verification evidence
│       ├── ch01-foundation.md             # [CH01]
│       ├── ch02-ledger-rs.md              # [CH02]
│       ├── ch03-gateway-go.md             # [CH03]
│       ├── ch04-stream-aggregator.md      # [CH04]
│       ├── ch05-sentinel-ml.md            # [CH05]
│       ├── ch06-aegis-agent.md            # [CH06]
│       ├── ch07-chaos-sim.md              # [CH07]
│       ├── ch08-chronos-replay.md         # [CH08]
│       └── ch09-terminal-web.md           # [CH09]
├── scripts/                               # [CH01] Cross-platform codegen, OpenAPI/SQL validators, healthchecks
├── docker-compose.infra.yml               # [CH01] Redpanda, TimescaleDB, Redis 7, OTel, Prometheus, Jaeger
├── justfile                               # [CH01] Primary task runner (proto, lint, infra-up, health, verify)
└── Makefile                               # [CH01] POSIX Make mirror of justfile targets
```

---

## 2. ATURAN ISOLASI MUTLAK ANTAR CHANNEL (STRICT DIRECTORY ISOLATION)

1. **Larangan Lintas Direktori**: Setiap agen pada Channel 02 s.d. Channel 09 **DILARANG KERAS** membuat, mengedit, mengganti nama, atau menghapus file apa pun di luar direktori eksklusif miliknya dan satu file handoff miliknya di `specs/handoffs/chXX-*.md`:
   - **Channel 01**: `.agents/`, `proto/`, `contracts/`, `deploy/` (fondasi awal), `scripts/`, `docker-compose.infra.yml`, `justfile`, `Makefile`, `specs/handoffs/ch01-foundation.md`.
   - **Channel 02**: HANYA `services/ledger-rs/**` dan `specs/handoffs/ch02-ledger-rs.md`.
   - **Channel 03**: HANYA `services/gateway-go/**` dan `specs/handoffs/ch03-gateway-go.md`.
   - **Channel 04**: HANYA `services/stream-aggregator/**` dan `specs/handoffs/ch04-stream-aggregator.md`.
   - **Channel 05**: HANYA `services/sentinel-ml/**` dan `specs/handoffs/ch05-sentinel-ml.md`.
   - **Channel 06**: HANYA `services/aegis-agent/**` dan `specs/handoffs/ch06-aegis-agent.md`.
   - **Channel 07**: HANYA `services/chaos-sim/**` dan `specs/handoffs/ch07-chaos-sim.md`.
   - **Channel 08**: HANYA `services/chronos-replay/**` dan `specs/handoffs/ch08-chronos-replay.md`.
   - **Channel 09**: HANYA `apps/terminal-web/**` dan `specs/handoffs/ch09-terminal-web.md`.
   - **Channel 10**: Integrasi akhir di `deploy/**`, `e2e/**`, `docs/**`, `.github/workflows/**`, `docker-compose.full.yml`, `README.md`, serta perbaikan integrasi bedah presisi lintas servis bila diperlukan saat penyatuan akhir.
2. **Konsumsi Kontrak Read-Only**: Servis di `services/*` dan `apps/terminal-web` wajib mengimpor kontrak dari `/contracts/gen/{rust,go,python,ts}` (mis. `path = "../../contracts/gen/rust"` di `Cargo.toml`, `replace github.com/vanovri/hyperion/contracts/gen/go => ../../contracts/gen/go` di `go.mod`, `PYTHONPATH` / editable dependency `../../contracts/gen/python`, atau `file:../../contracts/gen/ts` di `package.json`) atau mengompilasi `/proto/*.proto` ke dalam direktori internal miliknya sendiri tanpa menyentuh `/proto` atau `/contracts`.
3. **Standalone Crate / Module**: Jangan membuat root `Cargo.toml` workspace atau root `package.json` di Channel 02–09 agar tidak terjadi merge conflict di root monorepo. Setiap servis memiliki manifest independen di dalam foldernya sendiri.

---

## 3. ALOKASI PORT JARINGAN TETAP (CANONICAL PORT MATRIX)

> **Catatan Kompatibilitas Windows Host**: Rentang port `50000–50059` dan `64819–65418` diblokir oleh Windows Hyper-V/WinNAT TCP port exclusion. Oleh sebab itu seluruh layanan gRPC HYPERION dialokasikan secara deterministik pada rentang bebas konflik **`50151–50159`**.

### 3.1 Microservices & Aplikasi

| Komponen / Servis | Channel | Bahasa | gRPC Port | HTTP / WS / SSE / Metrics Port | Endpoint Kesehatan |
|---|---|---|---|---|---|
| `services/ledger-rs` | CH02 | Rust | `50151` | `8082` (Health & `/metrics`) | `GET http://localhost:8082/healthz` |
| `services/gateway-go` | CH03 | Go | — (Client) | `8080` (REST `/api/v1/*` + Binary WS `/ws`) · `8081` (`/healthz`, `/metrics`) | `GET http://localhost:8081/healthz` |
| `services/stream-aggregator` | CH04 | Rust | `50152` | `8083` (REST `/api/v1/analytics/*`, `/healthz`, `/metrics`) | `GET http://localhost:8083/healthz` |
| `services/sentinel-ml` | CH05 | Python 3.12+ | `50153` | `8084` (REST `/api/v1/fraud/*`, `/healthz`, `/metrics`) | `GET http://localhost:8084/healthz` |
| `services/aegis-agent` | CH06 | TypeScript | — (Client) | `8085` (MCP `/mcp`, REST `/api/v1/aegis/*`, SSE `/events/incidents`, `/metrics`) | `GET http://localhost:8085/healthz` |
| `services/chaos-sim` | CH07 | Go | — (Client) | `8086` (Control REST `/api/v1/chaos/*`, SSE `/events/chaos`, `/metrics`) · Toxic Proxies `19001–19005` | `GET http://localhost:8086/healthz` |
| `services/chronos-replay` | CH08 | Rust | `50154` | `8088` (REST `/api/v1/chronos/*`, VCR SSE `/events/replay`, `/healthz`, `/metrics`) | `GET http://localhost:8088/healthz` |
| `apps/terminal-web` | CH09 | Next.js / TS | — | `3000` (Web UI) | `GET http://localhost:3000` |

### 3.2 Infrastruktur & Observability (`docker-compose.infra.yml`)

| Infrastruktur | Container Name | Host Port -> Container Port | Internal Docker Endpoint | Kredensial / Konfigurasi Default Lokal |
|---|---|---|---|---|
| **Redpanda (Kafka API)** | `hyperion-redpanda` | `19092:19092` | `redpanda:9092` | Tanpa SASL pada dev lokal (`PLAINTEXT`) |
| **Redpanda Schema Registry** | `hyperion-redpanda` | `18081:8081` | `redpanda:8081` | HTTP REST Schema Registry |
| **Redpanda Pandaproxy** | `hyperion-redpanda` | `18082:8082` | `redpanda:8082` | HTTP Kafka Bridge |
| **Redpanda Admin & Metrics** | `hyperion-redpanda` | `19644:9644` | `redpanda:9644` | `GET /v1/status/ready`, `/public_metrics` |
| **TimescaleDB (PG 16)** | `hyperion-timescaledb` | `5432:5432` | `timescaledb:5432` | `postgres://hyperion:hyperion_dev_secret@localhost:5432/hyperion?sslmode=disable` |
| **Redis 7** | `hyperion-redis` | `6379:6379` | `redis:6379` | `redis://localhost:6379/0` (AOF disabled, maxmemory 512mb) |
| **OpenTelemetry Collector** | `hyperion-otel-collector` | `4317:4317` (gRPC), `4318:4318` (HTTP), `8888:8888`, `8889:8889`, `13133:13133` | `otel-collector:4317` | OTLP gRPC `localhost:4317` / HTTP `localhost:4318`, health `:13133/` |
| **Prometheus** | `hyperion-prometheus` | `9090:9090` | `prometheus:9090` | `GET http://localhost:9090/-/healthy` |
| **Jaeger All-in-One (OTLP)** | `hyperion-jaeger` | `16686:16686` (UI), `14268:14268` | `jaeger:4317` | UI Tracing: `http://localhost:16686` |

---

## 4. TOPIK EVENT REDPANDA, ATURAN PARTISI, & FORMAT KEY DETERMINISTIK

Seluruh komunikasi event-driven melalui Redpanda menggunakan serialisasi **Protobuf biner (`application/x-protobuf`)** dari skema `/proto/*.proto` (dengan header Kafka wajib: `traceparent`, `x-engine-seq`, `x-schema-type`, `x-source-service`).

### 4.1 Aturan Partisi & Determinisme Urutan
- **Partisi per Simbol (`symbol`)**: Semua topik buku order dan eksekusi menggunakan **key UTF-8 berupa kode simbol instrumen kanonik** (mis. `"BTC-USDT"`, `"ETH-USDT"`, `"SOL-USDT"`, `"HYPE-USDT"`) dan **hasher `murmur2_random` standar Kafka**. Dengan demikian seluruh event untuk satu simbol dijamin mendarat di partisi yang sama dalam urutan `engine_seq` yang ketat (*strict total order per symbol*).
- **Partisi per Akun (`account_id`)**: Topik komando risiko atau audit per akun menggunakan **key string desimal `account:<account_id>`** (mis. `"account:10042"`).

### 4.2 Daftar Kanonik Topik Redpanda

| Nama Topik | Partisi | Replikasi | Retention | Format Key Deterministik | Protobuf Message Type (`hyperion.v1`) | Producer -> Consumer |
|---|---|---|---|---|---|---|
| `hyperion.orders.commands.v1` | `8` | `1` | `24h` | `<SYMBOL>` (mis. `BTC-USDT`) | `MarketEventEnvelope` / `SubmitOrder` / `CancelOrder` / `ModifyOrder` | `gateway-go` -> `ledger-rs` (audit/async ingress) |
| `hyperion.market.l3-deltas.v1` | `8` | `1` | `24h` | `<SYMBOL>` | `L3BookDelta` | `ledger-rs` -> `gateway-go`, `stream-aggregator`, `sentinel-ml` |
| `hyperion.market.l2-snapshots.v1` | `8` | `1` | `24h` (compact+delete) | `<SYMBOL>` | `L2BookSnapshot` | `ledger-rs` -> `gateway-go`, `stream-aggregator` |
| `hyperion.market.executions.v1` | `8` | `1` | `72h` | `<SYMBOL>` | `ExecutionFill` | `ledger-rs` -> `gateway-go`, `stream-aggregator`, `sentinel-ml` |
| `hyperion.ledger.journals.v1` | `8` | `1` | `168h` (7d) | `account:<account_id>` atau `<SYMBOL>` | `LedgerJournalEntry` | `ledger-rs` -> `stream-aggregator`, `chronos-replay` |
| `hyperion.analytics.candles.v1` | `8` | `1` | `48h` | `<SYMBOL>:<INTERVAL>` (mis. `BTC-USDT:1s`) | `OhlcvCandle` | `stream-aggregator` -> `gateway-go` |
| `hyperion.analytics.microstructure.v1` | `8` | `1` | `24h` | `<SYMBOL>` | `MicrostructureMetrics` | `stream-aggregator` -> `gateway-go`, `sentinel-ml` |
| `hyperion.fraud.alerts.v1` | `4` | `1` | `168h` (7d) | `<SYMBOL>` | `AnomalyAlert` | `sentinel-ml` -> `aegis-agent`, `gateway-go`, `stream-aggregator` |
| `hyperion.risk.mitigations.v1` | `4` | `1` | `168h` (7d) | `account:<target_account_id>` atau `symbol:<target_symbol>` | `RiskMitigationCommand` | `aegis-agent` -> `gateway-go`, `ledger-rs` |
| `hyperion.sre.incidents.v1` | `2` | `1` | `168h` (7d) | `incident:<incident_id>` | JSON UTF-8 Structured Incident / Post-Mortem Event | `aegis-agent` -> `gateway-go`, `terminal-web` |

### 4.3 Instrumen Kanonik Default
Setiap servis wajib mengenali pemetaan `instrument_id` <-> `symbol` berikut secara deterministik:
- `1` <-> `"BTC-USDT"` (Tick size: `1_000_000` = `0.01` USDT; Lot size: `1_000` = `0.00001` BTC)
- `2` <-> `"ETH-USDT"` (Tick size: `1_000_000` = `0.01` USDT; Lot size: `10_000` = `0.0001` ETH)
- `3` <-> `"SOL-USDT"` (Tick size: `100_000` = `0.001` USDT; Lot size: `100_000` = `0.001` SOL)
- `4` <-> `"HYPE-USDT"` (Tick size: `10_000` = `0.0001` USDT; Lot size: `1_000_000` = `0.01` HYPE)

---

## 5. INVARIAN SISTEM MUTLAK (MATHEMATICAL & FINANCIAL INVARIANTS)

### 5.1 Larangan Mutlak Floating-Point untuk Nilai Uang & Kuantitas
1. **SKALA FIXED-POINT `1e8` (`FP_SCALE = 100_000_000`)**:
   - Seluruh harga (`price_fp`), kuantitas aset dasar (`qty_fp`), nilai nosional (`notional_fp`), biaya transaksi (`maker_fee_fp`, `taker_fee_fp`), saldo akun (`available_fp`, `locked_fp`, `total_fp`), dan harga rata-rata (`vwap_fp`, `twap_fp`) **WAJIB** menggunakan integer fixed-point 64-bit (`u64`/`i64`/`bigint`) atau 128-bit (`i128`/`u128`/`I128`/`U128`) dengan skala `100_000_000` ($10^8$).
   - **DILARANG KERAS** memakai tipe `f32`, `f64`, `float`, `float32`, `float64`, `double`, atau `REAL`/`DOUBLE PRECISION` SQL untuk menyimpan atau menghitung uang, harga kliring, ukuran order, biaya, maupun saldo buku besar!
2. **Rumus Kanonik Perkalian Harga × Kuantitas & Fee (Bebas Overflow)**:
   - Nilai nosional quote:
     $$\text{notional\_fp} = \left\lfloor \frac{\text{u128}(\text{price\_fp}) \times \text{u128}(\text{qty\_fp})}{100\_000\_000} \right\rfloor$$
   - Biaya transaksi dalam satuan parts-per-million (`fee_ppm`, skala $10^6$, di mana `100 ppm = 1 bps = 0.01%`, bertanda `i64` sehingga nilai negatif merepresentasikan *maker rebate*):
     $$\text{fee\_fp} = \text{ round\_toward\_zero}\left( \frac{\text{i128}(\text{notional\_fp}) \times \text{i128}(\text{fee\_ppm})}{1\_000\_000} \right)$$
3. **Representasi JSON / REST**:
   - Karena `Number` IEEE-754 pada JavaScript hanya presisi hingga $2^{53}-1$, seluruh nilai `_fp` pada payload JSON REST (`gateway-openapi.yaml`) **WAJIB** dikirim sebagai **string integer desimal** (contoh: `"6842550000000"` untuk `68,425.50` USDT) dan diparse menggunakan `BigInt` di TypeScript/Browser.
4. **Representasi Database (TimescaleDB / PostgreSQL)**:
   - Kolom harga/kuantitas per order menggunakan `BIGINT` (`int8`), sedangkan saldo kumulatif, volume agregat, dan nilai nosional 128-bit menggunakan `NUMERIC(38, 0)` (integer eksak presisi 38 digit desimal tanpa pecahan mengambang).

### 5.2 Invarian Kekekalan Dana Buku Besar (Double-Entry Conservation Law)
Pada setiap urutan `engine_seq` dan untuk setiap aset $A$:
$$\sum_{u \in \text{Accounts}} \left( \text{Available}_{u,A} + \text{Locked}_{u,A} \right) + \text{TreasuryFeePool}_{A} \equiv \text{TotalDeposits}_{A} - \text{TotalWithdrawals}_{A}$$
Selisih **1 unit terkecil ($10^{-8}$)** pun merupakan pelanggaran fatal (`CRITICAL INVARIANT BREACH`).

### 5.3 Invarian Buku Order (Uncrossed Book Law)
Setelah siklus pencocokan sinkron sebuah komando selesai pada rezim pasar aktif:
$$\text{BestBid}(\text{symbol}) < \text{BestAsk}(\text{symbol})$$
(kecuali salah satu sisi buku order kosong).

---

## 6. SPESIFIKASI LINTAS SERVIS TAMBAHAN (CROSS-CHANNEL WIRE COMPATIBILITY)

### 6.1 Format Biner Blake3 Write-Ahead Log (WAL) & Snapshot (`CH02 ledger-rs` <-> `CH08 chronos-replay`)
Agar `chronos-replay` (Channel 08) dapat mem-parsing segmen WAL dan Snapshot milik `ledger-rs` (Channel 02) secara deterministik:
- **Direktori Default Runtime WAL/Snapshot**: `${HYPERION_DATA_DIR:-./data/ledger}` (`wal/segment_XXXXXXXX.wal` dan `snapshots/snap_XXXXXXXXXXXXXXXX.snap`).
- **WAL Segment File Header (16 bytes, Little-Endian)**:
  - `magic`: `[u8; 8] = b"HYPWAL01"`
  - `version`: `u32 = 1`
  - `flags`: `u32 = 0`
- **WAL Frame Layout (Little-Endian, berurutan)**:
  - `payload_len`: `u32` (panjang byte `payload` Protobuf `MarketEventEnvelope`)
  - `crc32c`: `u32` (Castagnoli CRC-32C atas `engine_seq || ts_ns || prev_hash || payload`)
  - `engine_seq`: `u64` (monoton naik +1 tanpa celah mulai dari `1`)
  - `ts_ns`: `u64` (timestamp nanodetik deterministik saat komit)
  - `prev_blake3`: `[u8; 32]` (0x00..00 untuk `engine_seq == 1`, selebihnya `frame_blake3` dari `engine_seq - 1`)
  - `frame_blake3`: `[u8; 32]` = `blake3(engine_seq_le || ts_ns_le || prev_blake3 || payload)`
  - `payload`: `[u8; payload_len]` (serialisasi Protobuf `hyperion.v1.MarketEventEnvelope`)
- **Snapshot File Layout (`HYPSNP01`, Little-Endian)**:
  - `magic`: `[u8; 8] = b"HYPSNP01"`
  - `version`: `u32 = 1`
  - `crc32c`: `u32` (atas `engine_seq || ts_ns || state_root_blake3 || payload`)
  - `engine_seq`: `u64`
  - `ts_ns`: `u64`
  - `state_root_blake3`: `[u8; 32]`
  - `payload_len`: `u64`
  - `payload`: `[u8; payload_len]` (serialisasi Protobuf atau struktur kanonik snapshot berisi `L2BookSnapshot` per simbol + daftar saldo akun)

### 6.2 Header Autentikasi Kriptografis & Canonical Signing Preimage (`CH03 gateway-go` <-> `CH07 chaos-sim` / `CH09` / `CH10`)
Untuk endpoint mutasi latensi rendah (`/api/v1/orders`, `/api/v1/orders/{order_id}`):
- Header HTTP wajib:
  - `X-Hyperion-Account-Id`: `<uint64 decimal>`
  - `X-Hyperion-Key-Id`: `<key_id string>`
  - `X-Hyperion-Timestamp`: `<unix_epoch_nanoseconds decimal>` (ditolak jika $|\text{now} - \text{ts}| > 5\text{ detik}$)
  - `X-Hyperion-Nonce`: `<uint64 decimal atau hex unik per akun>`
  - `X-Hyperion-Signature`: `<hex-encoded Ed25519 (64B) atau HMAC-SHA256 (32B) signature>`
  - `X-Hyperion-Auth-Scheme`: `"ed25519"` atau `"hmac-sha256"`
  - `Idempotency-Key`: `<UUIDv4 / string unik klien>`
- **Canonical Preimage UTF-8** yang ditandatangani:
  ```text
  <X-Hyperion-Timestamp>\n<X-Hyperion-Nonce>\n<HTTP_METHOD_UPPERCASE>\n<REQUEST_PATH>\n<SHA256_HEX_LOWERCASE(RAW_BODY)>
  ```
- Endpoint baca UI publik/analitik menerima `Authorization: Bearer <JWT>` (atau akses baca publik untuk market data publik `/api/v1/market/*`).
