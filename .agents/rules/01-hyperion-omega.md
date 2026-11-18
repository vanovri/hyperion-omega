# HYPERION OMEGA — PHASE II GOVERNANCE, PROTOCOL EXTENSION & FORMAL INVARIANTS (`CH11`)

- **Version**: `2.0.0-omega` (Channel 11 Locked Expansion Contract)
- **Extends**: [`.agents/rules/00-hyperion-contract.md`](file:///C:/Users/ardhy/.gemini/antigravity/scratch/hyperion/.agents/rules/00-hyperion-contract.md) (`100% Backward-Compatible` with Channels 01–10)
- **Canonical Monorepo Root**: `C:\Users\ardhy\.gemini\antigravity\scratch\hyperion`
- **OMEGA Protobuf Package**: `hyperion.omega.v1` (`/proto/omega/*.proto`, importing `hyperion.v1` from `/proto/*.proto`)
- **OMEGA Pre-Compiled Multi-Language SDKs**: `/proto/omega/gen/{rust,go,python,ts}`
- **Formal Verification Proofs**: `/specs/formal-verification/{smt,tlaplus,model-checker-rs}`

Dokumen ini memperluas kontrak monorepo **HYPERION** untuk ekspansi fase kedua (**HYPERION OMEGA**, Channel 11–20) tanpa memodifikasi satu pun port, topik Redpanda, maupun skema Protobuf fase pertama (`hyperion.v1`, Channel 01–10). Seluruh agen Channel 12–20 **WAJIB** mematuhi isolasi direktori, peta port, batas memori/fuel sandbox Wasm, skala fixed-point margin/Greeks, serta teorema formal yang telah dibuktikan dengan Z3 SMT Solver dan Explicit-State Model Checker.

---

## 1. KUNCI ISOLASI DIREKTORI MUTLAK (OMEGA STRICT DIRECTORY LOCKS — CH11..CH20)

Setiap agen pada ekspansi **HYPERION OMEGA** terkunci eksklusif pada direktori miliknya sendiri dan **DILARANG KERAS** menyentuh file milik channel lain (baik Channel 01–10 maupun sesama Channel 11–19):

| Channel | Subsistem / Role | Direktori Eksklusif (Read/Write) | File Handoff Wajib | Manifest Mandiri |
|---|---|---|---|---|
| **CH11** | Formal Verification & OMEGA Protocol Architect | `.agents/rules/01-hyperion-omega.md`, `/proto/omega/**`, `/specs/formal-verification/**` | `specs/handoffs/ch11-formal-omega.md` | Crate/venv terisolasi di `/proto/omega/gen` & `/specs/formal-verification` |
| **CH12** | 5-Node Hardened Raft + DST VOPR Engine | `/services/consensus-raft-rs/**` | `specs/handoffs/ch12-consensus-raft.md` | `services/consensus-raft-rs/Cargo.toml` |
| **CH13** | ZK Proof of Solvency (MSST + R1CS/Plonkish) | `/services/zk-solvency-rs/**` | `specs/handoffs/ch13-zk-solvency.md` | `services/zk-solvency-rs/Cargo.toml` |
| **CH14** | Cross-Margin, SVI Vol Surface, Greeks & ADL | `/services/margin-risk-rs/**` | `specs/handoffs/ch14-margin-risk.md` | `services/margin-risk-rs/Cargo.toml` |
| **CH15** | Co-Located Deterministic Wasmtime Quant Host | `/services/quant-wasm-host/**` | `specs/handoffs/ch15-quant-wasm.md` | `services/quant-wasm-host/Cargo.toml` |
| **CH16** | eBPF/XDP Zero-Trust Packet Filter & Histograms | `/services/ebpf-netguard/**` | `specs/handoffs/ch16-ebpf-netguard.md` | `services/ebpf-netguard/Cargo.toml` |
| **CH17** | Arrow / Parquet / DataFusion Forensics Lakehouse | `/services/lakehouse-olap-rs/**` | `specs/handoffs/ch17-lakehouse-olap.md` | `services/lakehouse-olap-rs/Cargo.toml` |
| **CH18** | Solidity L2 Vault/Escape-Hatch & Go Chain Relayer | `/contracts-evm/**`, `/services/chain-relayer-go/**` | `specs/handoffs/ch18-evm-settlement.md` | `contracts-evm/foundry.toml` & `services/chain-relayer-go/go.mod` |
| **CH19** | Genetic Programming Symbolic Alpha & PPO MARL | `/services/alpha-evo-py/**` | `specs/handoffs/ch19-alpha-evo.md` | `services/alpha-evo-py/pyproject.toml` |
| **CH20** | Sovereign Studio (WebGL2 + Tauri v2), E2E & Paper | `/apps/sovereign-studio/**`, `/e2e-omega/**`, `/docs/whitepaper/**`, orkestrasi root (`/deploy/**`, `Makefile`, `justfile`, `README.md`) | `specs/handoffs/ch20-sovereign-singularity.md` | `apps/sovereign-studio/package.json` & `src-tauri/Cargo.toml` |

### Aturan Konsumsi Kontrak Read-Only untuk Channel 12–20
1. **Dilarang Memodifikasi `/proto` atau `/contracts`**: Seluruh skema `/proto/*.proto` (`hyperion.v1`) dan `/proto/omega/*.proto` (`hyperion.omega.v1`) bersifat **IMMUTABLE READ-ONLY** setelah Channel 11 dikomit.
2. **Jalur Impor SDK Lintas Bahasa**:
   - **Rust (`CH12–CH17`, `CH20`)**: Impor crate `hyperion-omega-proto = { path = "../../proto/omega/gen/rust" }` (yang juga mengekspor ulang tipe `hyperion_proto` dari `../../contracts/gen/rust`), atau kompilasi mandiri via `tonic-build`/`prost-build` ke `OUT_DIR` lokal servis.
   - **Go (`CH18`)**: Impor `github.com/vanovri/hyperion/proto/omega/gen/go/omegav1` dengan direktif `replace github.com/vanovri/hyperion/proto/omega/gen/go => ../../proto/omega/gen/go` dan `replace github.com/vanovri/hyperion/contracts/gen/go => ../../contracts/gen/go` di `services/chain-relayer-go/go.mod`.
   - **Python (`CH19`)**: Tambahkan `../../proto/omega/gen/python/src` dan `../../contracts/gen/python/src` pada `PYTHONPATH` / editable install (`from hyperion_omega_proto import consensus_pb2, zk_solvency_pb2, margin_risk_pb2, wasm_quant_pb2, settlement_l2_pb2`).
   - **TypeScript (`CH20`)**: Gunakan `@hyperion/omega-contracts` (`file:../../proto/omega/gen/ts`) berdampingan dengan `@hyperion/contracts` (`file:../../contracts/gen/ts`).
3. **Larangan Root Workspace Prematur**: Channel 12–19 dilarang membuat root `Cargo.toml`, root `package.json`, atau root `go.work` untuk mencegah konflik merge antarcabang worktree.

---

## 2. PETA PORT JARINGAN OMEGA (ZERO-COLLISION PORT MATRIX)

> **Kompatibilitas Windows Host & Fase 1**: Bebas dari blokir WinNAT (`50000–50059`, `64819–65418`) dan nol tumpang-tindih terhadap port Channel 01–10 (`50151–50154`, `8080–8088`, `3000`, `19001–19005`, `19092`, `5432`, `6379`, `4317–4318`, `9090`, `16686`).

| Subsistem / Servis | Channel | Bahasa / Stack | gRPC Port | Peer / Aux Port | HTTP / REST / Metrics Port | Endpoint Kesehatan |
|---|---|---|---|---|---|---|
| `services/consensus-raft-rs` | CH12 | Rust | `50161` (`RaftConsensusService`) | `50201–50205` (5-Node Peer Raft Transport) | `8091` (`/healthz`, `/metrics`, `/api/v1/raft/status`) | `GET http://localhost:8091/healthz` |
| `services/zk-solvency-rs` | CH13 | Rust (Arkworks/Halo2) | `50162` (`ZkSolvencyService`) | — | `8092` (`/healthz`, `/metrics`, `/api/v1/zk/*`) | `GET http://localhost:8092/healthz` |
| `services/margin-risk-rs` | CH14 | Rust (SIMD SoA) | `50163` (`MarginRiskService`) | — | `8093` (`/healthz`, `/metrics`, `/api/v1/margin/*`) | `GET http://localhost:8093/healthz` |
| `services/quant-wasm-host` | CH15 | Rust (Wasmtime JIT) | `50164` (`WasmQuantHostService`) | — | `8094` (`/healthz`, `/metrics`, `/api/v1/wasm/*`) | `GET http://localhost:8094/healthz` |
| `services/ebpf-netguard` | CH16 | Rust + eBPF/XDP | `50165` (`NetGuardControl` / Telemetry) | Raw/XDP Loopback Harness | `8095` (`/healthz`, `/metrics`, `/api/v1/netguard/*`) | `GET http://localhost:8095/healthz` |
| `services/lakehouse-olap-rs` | CH17 | Rust (Arrow/DataFusion) | `50166` (Arrow Flight / OLAP gRPC) | — | `8096` (`POST /api/v1/olap/query`, `/healthz`, `/metrics`) | `GET http://localhost:8096/healthz` |
| `contracts-evm` (Devnet Node) | CH18 | Solidity `0.8.24+` / Anvil | — | `8545` (EVM JSON-RPC & WS, Chain ID `31337` / `994201`) | — | `POST http://localhost:8545` (`eth_chainId`) |
| `services/chain-relayer-go` | CH18 | Go | `50167` (`L2SettlementService`) | — | `8097` (`/healthz`, `/metrics`, `/api/v1/settlement/*`) | `GET http://localhost:8097/healthz` |
| `services/alpha-evo-py` | CH19 | Python 3.12+ / PyTorch | `50168` (Alpha Eval / Factory) | — | `8098` (`/healthz`, `/metrics`, `/api/v1/alpha/*`) | `GET http://localhost:8098/healthz` |
| `apps/sovereign-studio` | CH20 | Next.js + WebGL2 + Tauri v2 | — | Tauri IPC Bridge | `3001` (Sovereign Workstation Web UI) | `GET http://localhost:3001` |

---

## 3. TOPIK REDPANDA BARU (`hyperion.omega.*.v1`)

Seluruh topik baru memakai serialisasi Protobuf biner (`application/x-protobuf`) dari `package hyperion.omega.v1` dengan header wajib `traceparent`, `x-engine-seq`, `x-schema-type`, dan `x-source-service` serta hasher standar Kafka `murmur2_random`:

| Nama Topik Redpanda | Partisi | Retention | Format Key Deterministik | Protobuf Message (`hyperion.omega.v1`) | Producer -> Consumer |
|---|---|---|---|---|---|
| `hyperion.omega.consensus.events.v1` | `8` | `72h` | `raft:shard:<shard_id>` | `ReadIndexBarrier` / `AppendEntriesResponse` | `consensus-raft-rs` -> `ledger-rs`, `lakehouse-olap-rs`, `sovereign-studio` |
| `hyperion.omega.zk.roots.v1` | `2` | `168h` (compact+delete) | `epoch:<epoch_id>` | `MerkleSumRootAnnouncement` | `zk-solvency-rs` -> `chain-relayer-go`, `sovereign-studio` |
| `hyperion.omega.zk.receipts.v1` | `2` | `168h` | `epoch:<epoch_id>` | `ZKSolvencyReceipt` | `zk-solvency-rs` -> `chain-relayer-go`, `lakehouse-olap-rs`, `sovereign-studio` |
| `hyperion.omega.margin.snapshots.v1` | `8` | `48h` | `account:<account_id>` | `PortfolioMarginSnapshot` | `margin-risk-rs` -> `gateway-go`, `lakehouse-olap-rs`, `sovereign-studio` |
| `hyperion.omega.margin.greeks.v1` | `8` | `24h` | `<UNDERLYING_SYMBOL>` (mis. `BTC-USDT`) | `OptionGreeksSurface` | `margin-risk-rs` -> `quant-wasm-host`, `alpha-evo-py`, `sovereign-studio` |
| `hyperion.omega.margin.liquidations.v1` | `8` | `168h` | `account:<account_id>` | `LiquidationWaterfallEvent` | `margin-risk-rs` -> `ledger-rs`, `aegis-agent`, `lakehouse-olap-rs`, `sovereign-studio` |
| `hyperion.omega.margin.adl.v1` | `4` | `168h` | `<SYMBOL>` | `ADLTrigger` | `margin-risk-rs` -> `ledger-rs`, `aegis-agent`, `sovereign-studio` |
| `hyperion.omega.wasm.fuel.v1` | `4` | `48h` | `strategy:<strategy_id>` | `FuelMeteringReport` | `quant-wasm-host` -> `lakehouse-olap-rs`, `aegis-agent`, `sovereign-studio` |
| `hyperion.omega.wasm.traps.v1` | `4` | `168h` | `strategy:<strategy_id>` | `SandboxExecutionTrap` | `quant-wasm-host` -> `aegis-agent`, `sovereign-studio` |
| `hyperion.omega.netguard.drops.v1` | `4` | `48h` | `iface:<interface_name>` | JSON UTF-8 Structured XDP Drop & RTT Histogram Snapshot | `ebpf-netguard` -> `aegis-agent`, `sovereign-studio` |
| `hyperion.omega.settlement.deposits.v1` | `8` | `168h` | `account:<account_id>` | `DepositFinalized` | `chain-relayer-go` -> `ledger-rs`, `lakehouse-olap-rs` |
| `hyperion.omega.settlement.withdrawals.v1` | `8` | `168h` | `account:<account_id>` | `WithdrawalEIP712Intent` | `gateway-go` / `ledger-rs` -> `chain-relayer-go` |
| `hyperion.omega.settlement.batches.v1` | `2` | `168h` | `epoch:<epoch_id>` | `StateRootBatchCommit` | `chain-relayer-go` -> `zk-solvency-rs`, `lakehouse-olap-rs`, `sovereign-studio` |
| `hyperion.omega.settlement.reorgs.v1` | `2` | `168h` | `chain:<chain_id>` | `ChainReorgAlert` | `chain-relayer-go` -> `ledger-rs`, `aegis-agent`, `sovereign-studio` |
| `hyperion.omega.alpha.formulas.v1` | `2` | `168h` | `gen:<generation_id>` | JSON UTF-8 Symbolic Alpha AST + DSR/PBO Certificate | `alpha-evo-py` -> `quant-wasm-host`, `sovereign-studio` |

---

## 4. BATAS MEMORI, CPU FUEL, & DETERMINISME SANDBOX WASM (`CH15 quant-wasm-host`)

Untuk menjamin ko-lokasi bot kuantitatif pengguna tidak pernah membocorkan memori, melanggar determinisme replay, atau memonopoli core CPU mesin pencocok:

1. **Zero WASI Ambient Authority**:
   - Seluruh impor WASI (`wasi_snapshot_preview1` / `preview2`), soket jaringan, filesystem host, variabel lingkungan, `clock_time_get` non-deterministik, dan `random_get` **WAJIB DIMATIKAN TOTAL**. Waktu nanodetik wajib disuntikkan secara deterministik oleh Host melalui argumen/ring-buffer `engine_ts_ns`.
2. **Batas Keras Memori Linear & Stack per Instans Bot**:
   - **Initial Linear Memory**: `1 MiB` (`16` halaman Wasm @ `64 KiB`).
   - **Hard Ceiling Linear Memory**: **`16 MiB` (`256` halaman Wasm = `16,777,216` byte)** per modul aktif (`StoreLimits` / `ResourceLimiter`). Percobaan `memory.grow` melampaui `256` halaman wajib ditolak (`return -1` atau trap `TRAP_CODE_MEMORY_LIMIT_EXCEEDED`) tanpa alokasi OS tambahan.
   - **Max Execution Call Stack**: **`512 KiB` (`524,288` byte)** (`Config::max_wasm_stack(524_288)`). Rekursi dalam wajib ditangkap sebagai `TRAP_CODE_STACK_OVERFLOW`.
   - **Max Table Elements & Instances**: Maksimum `4,096` elemen tabel fungsi tidak langsung per modul; maksimum `1` memori linear per modul.
3. **Determinisme Instruksi Kompilasi**:
   - `cranelift_nan_canonicalization(true)` **WAJIB AKTIF** agar operasi floating-point internal indikator statistik di dalam guest menghasilkan pola bit NaN identik lintas arsitektur x86_64/ARM64.
   - `wasm_threads(false)`, `wasm_relaxed_simd(false)`. Hanya nilai integer fixed-point `1e8` (`i64`/`u64`) yang boleh melintasi batas ABI Host <-> Guest untuk harga, ukuran order, dan posisi.
4. **Fuel Metering & Wall-Clock Epoch Interruption**:
   - **Anggaran Fuel per Tick (`on_book_l2_tick`, `on_trade_tick`, `on_order_fill`)**: **`50,000` fuel units** default (batas maksimum konfigurasi institusional `200,000` fuel units).
   - **Anggaran Fuel Inisialisasi (`on_init` / state handoff)**: **`500,000` fuel units**.
   - **Epoch Interruption Hard Kill**: Ticker epoch host berdetak setiap **`50 µs`**, dengan batas eksekusi callback **`2` epoch ticks (`<= 100 µs` / `100,000 ns`)**. Infinite loop wajib dihentikan paksa dengan `TRAP_CODE_FUEL_EXHAUSTED` atau `TRAP_CODE_EPOCH_TIMEOUT` dalam `< 100 µs`.
5. **Shared Linear Memory Ring-Buffer & Pre-Trade Risk Gate**:
   - **Ring-Buffer ABI**: `4 KiB` ingress (`64` slot × `64` byte) + `4 KiB` egress (`64` slot × `64` byte) di dalam memori linear guest dengan validasi batas offset `offset + len <= memory.data_size()` sebelum pembacaan slice.
   - **Pre-Trade Order Cap**: Maksimum **`16` perintah order per callback tick**; melebihi kuota langsung memicu `TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED` dan membatalkan seluruh batch tick tersebut.

---

## 5. FORMAT FIXED-POINT MARGIN, VOLATILITAS TERSIRAT, & OPTION GREEKS (`CH14` & `CH11`)

Selaras dengan Pasal 5.1 pada `00-hyperion-contract.md`, **DILARANG KERAS** mengirim atau menyimpan `float`/`double` pada protokol kawat (`margin_risk.proto`) maupun buku besar:

### 5.1 Tabel Skala Kanonik Kuantitatif & Risiko

| Besaran Finansial / Risiko | Akhiran Field | Tipe Protobuf / Rust | Skala Integer | Contoh Nilai Aktual -> Representasi Integer |
|---|---|---|---|---|
| Harga (`mark_price_fp`, `index_price_fp`, `strike_fp`, `bankruptcy_price_fp`) | `_fp` | `uint64` / `u64` | $10^8$ (`100_000_000`) | `68,500.25 USDT` -> `6_850_025_000_000` |
| Ekuitas, Kolateral, Unrealized PnL, Margin, Defisit, Saldo Insurance Fund | `_fp` | `I128` / `U128` (`i128`/`u128`) | $10^8$ (`100_000_000`) | `-125.50 USDT` -> `I128(-12_550_000_000)` |
| Rasio Initial Margin (`imr_ppm`) & Maintenance Margin (`mmr_ppm`) | `_ppm` | `uint32` / `u32` | $10^6$ (`1_000_000`) | `5.00%` (`0.05`) -> `50_000`; `2.50%` -> `25_000` |
| Implied Volatility Tahunan (`iv_fp`, `atm_vol_fp`) | `_fp` | `uint64` / `u64` | $10^8$ (`100_000_000`) | `64.25%` (`0.6425`) -> `64_250_000` |
| Perpetual Funding Rate per Interval (`funding_rate_fp`) | `_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `+0.01%` (`+0.0001`) -> `+10_000` (clamp $\pm 500\_000$) |
| **Delta** ($\Delta = \partial V / \partial S$ per `1.0` kontrak dasar) | `delta_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `+0.52341900` -> `+52_341_900` (opsi tunggal $\in [-10^8, +10^8]$) |
| **Gamma** ($\Gamma = \partial^2 V / \partial S^2$ per `1.0` unit quote) | `gamma_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `+0.00041250` -> `+41_250` |
| **Vega** ($\mathcal{V} = \partial V / \partial \sigma$ per `1.00` / `100%` unit vol) | `vega_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `+1,420.50 USDT` -> `+142_050_000_000` (`14.205` per `1%` vol) |
| **Theta** ($\Theta = \partial V / \partial t$ per hari kalender `1/365.25` thn) | `theta_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `-38.125 USDT/hari` -> `-3_812_500_000` |
| **Rho** ($\rho = \partial V / \partial r$ per `1.00` / `100%` suku bunga) | `rho_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | `+215.00 USDT` -> `+21_500_000_000` |
| **Vanna** ($\partial^2 V / \partial S \partial \sigma$) & **Volga** ($\partial^2 V / \partial \sigma^2$) | `vanna_fp`, `volga_fp` | `int64` / `i64` | $10^8$ (`100_000_000`) | Turunan orde-2 tail-shock terkuantisasi ke skala $10^8$ |
| Skor Kuantil ADL (`adl_ranking_score_fp`) | `_fp` | `uint64` / `u64` | $10^8$ (`100_000_000`) | $\text{ProfitPct} \times \text{EffectiveLeverage}$ dalam skala $10^8$ |

### 5.2 Aturan Isolasi Kernel Transendental & Pembulatan Konservatif Margin
1. Evaluasi fungsi transendental Black-Scholes-Merton / SVI (`ln`, `exp`, `sqrt`, `erfc`) di dalam `margin-risk-rs` diizinkan memakai `f64` murni **HANYA** di dalam fungsi murni tanpa state (*stateless analytical pricing kernel*), dan wajib langsung dikuantisasi ke `i64`/`i128` skala $10^8$ sebelum menyentuh agregasi ekuitas portofolio atau keputusan likuidasi.
2. **Arah Pembulatan Konservatif Risiko (Terbukti di Z3 Teorema 1 & 2)**:
   - Nilai kewajiban margin (`maintenance_margin_fp`, `initial_margin_fp`) wajib dibulatkan **ke atas (`ceil_div`)**:
     $$\text{MM\_fp} = \left\lceil \frac{\text{abs\_notional\_fp} \times \text{mmr\_ppm}}{1\_000\_000} \right\rceil$$
   - Potongan biaya bursa (`taker_fee_fp` positif) dan klaim penarikan mengutamakan konservasi eksak double-entry: perubahan saldo quote maker + perubahan saldo quote taker + perubahan `TreasuryFeePool` $\equiv 0$ untuk seluruh domain 64-bit tanpa kehilangan atau penciptaan `1` unit terkecil ($10^{-8}$).

---

## 6. INVARIAN KRIPTOGRAFI ZK-SOLVENCY, KONSENSUS RAFT, & JEMBATAN L2

### 6.1 Merkle-Sum Sparse Tree & Pencegahan Overflow Medan Prima (`CH13`)
- Setiap leaf akun ke-$i$ berkomitmen atas `leaf_balance_fp` bertipe `uint64` ($0 \le b_i < 2^{64}$).
- Di dalam sirkuit aritmetika medan prima $\mathbb{F}_p$ ($p > 2^{253}$, mis. BN254 / BLS12-381 / Pasta), prover **WAJIB** menegakkan kendala *range-check* $0 \le b_i < 2^{64}$ pada setiap leaf dan memeriksa tidak ada wrap-around pada penjumlahan node internal $S_{\text{parent}} = S_{\text{left}} + S_{\text{right}} < 2^{128} \ll p$, ditambah ketaksamaan solvabilitas global $\text{RootLiabilitySum} \le \text{VerifiedOnChainReserves}$.

### 6.2 Konsensus Raft + Blake3 WAL + ReadIndex (`CH12`)
- `AppendEntries` membawa `prev_log_index`, `prev_log_term`, dan `prev_wal_blake3` (rantai hash 32-byte yang selaras dengan `HYPWAL01` pada `00-hyperion-contract.md` Pasal 6.1).
- Protokol `PreVote` wajib aktif sebelum node menaikkan `current_term` agar partisi jaringan asimetris tidak memicu inflasi term saat node pulih.
- Pembacaan linearizable wajib melalui `ReadIndexBarrier` (konfirmasi kuorum detak jantung mayoritas $\lfloor N/2 \rfloor + 1 = 3$ dari `5` node atau lease leader terukur bebas clock-drift).

### 6.3 Imunitas Double-Spend Jembatan L2 & Reorg Finality (`CH18`)
- Deposit L2 (`DepositFinalized`) hanya boleh dikreditkan ke `ledger-rs` setelah mencapai kedalaman finalitas `confirmations >= min_finality_blocks` (default `12` blok) **DAN** `canonical_block_hash` cocok pada rantai kanonik aktif. Deposit prematur pada tinggi blok yang terkena reorganisasi (`ChainReorgAlert`) wajib dibatalkan dari antrean staging sebelum masuk buku besar.
- Penarikan (`WithdrawalEIP712Intent`) diikat secara bijektif ke tupel unik `(chain_id, verifying_vault_address, account_id, withdrawal_nonce, epoch_id, amount_fp, deadline_ts)` pada smart contract `HyperionVault.sol` melalui bitmap/mapping `usedNullifiers[nullifierHash]` yang bersifat monoton naik (append-only pada rantai kanonik), sehingga replay lintas chain, replay lintas epoch, maupun pengiriman ulang pasca-reorg ditolak `100%` (dibuktikan secara formal pada Z3 Teorema 3).
