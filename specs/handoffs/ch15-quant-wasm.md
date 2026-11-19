# CHANNEL 15 HANDOFF — CO-LOCATED DETERMINISTIC WASMTIME QUANT SANDBOX (`services/quant-wasm-host`)

- **Status**: `VERIFIED 100% — ALL SECURITY TORTURE, CRITERION <25µs SLA, & OVERCLOCK GATES PASSED (CLIPPY=0 | TESTS=12/12 | MUTATION=22/22 100.00% | 500k SOAK=0 B LEAK | p99=1.925 µs)`
- **Worktree / Branch**: `C:\Users\ardhy\.gemini\antigravity\scratch\hyperion-ch15` (`feat/ch15-quant-wasm`)
- **Locked Exclusive Scope**: `/services/quant-wasm-host/**` and `/specs/handoffs/ch15-quant-wasm.md`
- **Owner**: Channel 15 (Staff Runtime & Compiler Systems Engineer — Rust + WebAssembly)
- **Contract Dependencies Read**: `.agents/rules/00-hyperion-contract.md`, `.agents/rules/01-hyperion-omega.md`, `.agents/rules/02-extreme-overclock-protocol.md`, `specs/handoffs/ch03-gateway-go.md`, `specs/handoffs/ch11-formal-omega.md`, `/proto/common.proto`, `/proto/orderbook.proto`, `/proto/omega/wasm_quant.proto`

---

## 1. Executive Architecture & Hard Isolation Invariants

`services/quant-wasm-host` is a co-located, serverless, multi-tenant WebAssembly quantitative trading runtime powered by **Wasmtime `50.0.2` (Cranelift JIT)** executing untrusted institutional trading algorithms directly adjacent to HYPERION's L2/trade/fill market-data streams with **zero ambient authority**, **strict cross-architecture determinism**, **pre-trade risk containment**, **live atomic hot-swapping**, and **sub-3 µs p99 Host -> Guest -> Host round-trip overhead**.

### Hardened Sandbox Invariants (`01-hyperion-omega.md` §4)
1. **Zero WASI Ambient Authority**: Zero WASI (`wasi_snapshot_preview1` / `preview2`), sockets, host filesystem, env vars, clocks, or RNGs are linked. Static module audit (`QuantWasmHost::validate_module_security`) rejects any `.wasm` importing anything outside `hyperion::submit_limit_order` and `hyperion::cancel_order` with `WASM_TRAP_CODE_FORBIDDEN_HOST_IMPORT` before instantiation. Timestamps (`ts_ns`) are strictly host-injected through the shared ring buffer.
2. **Bit-Exact Determinism**: `Config::cranelift_nan_canonicalization(true)`, `Config::wasm_relaxed_simd(false)`, Wasm `threads` feature disabled at compile time, and **zero floating-point (`f32`/`f64`) across the ABI boundary** — every price, quantity, notional, spread, OFI, and inventory value uses canonical `1e8` integer fixed-point (`FP_SCALE = 100_000_000`, `00-hyperion-contract.md` §5.1).
3. **Spatial Memory & Stack Isolation**:
   - **Initial / Hard Ceiling Linear Memory**: `1 MiB` (`16` pages) initial, **`16 MiB` (`256` pages = `16,777,216 B`)** absolute ceiling enforced both statically at module validation and mid-instruction via `wasmtime::ResourceLimiter::memory_growing` (returning `HostTrapSignal::MemoryLimitExceeded` without OS allocation).
   - **Native Execution Stack Cap**: **`512 KiB` (`524,288 B`)** via `Config::max_wasm_stack(524_288)`, paired with `QuantWasmHost::prefault_current_thread_stack()` (`576 KiB` 4 KiB-stride pre-commit at host initialization to eliminate Windows `PAGE_GUARD` / Linux demand-paging minor faults on the hot path).
   - **Table & Instance Limits**: Max `4,096` indirect function table elements and max `1` linear memory export per module.
4. **Dual Preemption (`< 100 µs` Infinite-Loop Kill Guarantee)**:
   - **Deterministic Fuel Metering**: `Config::consume_fuel(true)` (`50,000` fuel default per tick, `200,000` institutional ceiling, `500,000` for `on_init` / state snapshot export & import).
   - **High-Precision QPC Epoch Interruption**: `Config::epoch_interruption(true)` + `store.epoch_deadline_trap()` (strict immediate trap with zero retry loopholes, neutralizing even 0-fuel empty `(loop (br 0))` bombs) driven by a dedicated QPC `EpochWatchdog` spinning on `20 µs` ticks while callbacks are active and parking cleanly on `Condvar` after `5 ms` idle (`0%` idle CPU).
5. **Pre-Trade Risk Gate & Transactional Order Isolation**: All `submit_limit_order` / `cancel_order` host imports execute synchronously through per-instance `PreTradeRiskGate` (`<= 16` mutations/tick hard circuit breaker `WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED`, tick/lot divisibility, fat-finger collar, `POST_ONLY` crossing guard, max order qty/notional, worst-case position delta, duplicate `client_order_id`, and account/regime halt checks). Pending mutations are staged transactionally and flushed to Gateway/Ledger (`ledger-rs :50151`) **only** on clean `CALLBACK_OK`; any trap atomically rolls back the entire tick and quarantines the offending strategy without touching co-located siblings.

---

## 2. Physical Artifact Inventory (`services/quant-wasm-host`)

```text
services/quant-wasm-host/
├── Cargo.toml                                    # Isolated CH15 sub-workspace manifest (zero root workspace edits)
├── Cargo.lock                                    # Locked deterministic dependency graph (wasmtime 50.0.2, tonic 0.12)
├── Dockerfile                                    # Multi-stage production container matching docker-compose.omega.yml (:50164, :8094)
├── .gitignore                                    # Ignores local /target artifacts
├── .cargo/config.toml                            # wasm32-unknown-unknown linker flags (initial=1MiB, max=16MiB, stack=256KiB, strip-debug)
├── artifacts/
│   ├── stat_arb_zscore.wasm                      # Compiled wasm32 reference strategy #1 (41,040 B, pages 16..256, 0 WASI)
│   ├── obi_sniper.wasm                           # Compiled wasm32 reference strategy #2 (39,649 B, pages 16..256, 0 WASI)
│   ├── delta_hedger.wasm                         # Compiled wasm32 reference strategy #3 (35,053 B, pages 16..256, 0 WASI)
│   ├── malware_bombs.wasm                        # Compiled wasm32 adversarial exploit payload (29,057 B, 6 attack modes)
│   ├── manifest.json                             # Automated SHA-256 / BLAKE2b-256 + import/export/memory ceiling audit manifest
│   └── mutation_report.json                      # 22/22 (100.00%) Mutation Survival Zero audit report
├── crates/
│   ├── hyperion-quant-abi/src/lib.rs             # #![no_std] zero-dep cache-line-aligned (64B) Shared Memory ABI + CRC-32C
│   ├── hyperion-quant-sdk/src/lib.rs             # #![no_std] Guest SDK: fp 1e8 math, isqrt_u128, RollingWindowFp<N>, QuantStrategy, macro
│   ├── malware-bombs/src/lib.rs                  # #![no_std] 6-mode adversarial malware payload compiled to wasm32
│   └── strategies/
│       ├── stat-arb-zscore/src/lib.rs            # Reference Bot 1: Rolling Z-Score Spread + OFI Mean-Reversion
│       ├── obi-sniper/src/lib.rs                 # Reference Bot 2: 5-Level Liquidity Slope & Depth-Weighted OBI Microstructure Sniper
│       └── delta-hedger/src/lib.rs               # Reference Bot 3: Inventory & Portfolio Delta-Neutral Hedger (POST_ONLY + IOC)
├── host/
│   ├── build.rs                                  # Compiles immutable ../../../proto/{common,orderbook,omega/wasm_quant}.proto
│   ├── src/
│   │   ├── lib.rs                                # Crate entrypoint
│   │   ├── main.rs                               # Production daemon: gRPC WasmQuantHostService (:50164) + HTTP/Prometheus (:8094)
│   │   ├── proto.rs                              # Generated hyperion.v1 + hyperion.omega.v1 prost/tonic bindings + U128/I128 helpers
│   │   ├── shm_ring.rs                           # Spatial-bounds-checked SharedMemoryRegion codecs (0-alloc POD fast path)
│   │   ├── risk_gate.rs                          # Synchronous Pre-Trade Risk Gate + transactional staging + open-order tracker
│   │   ├── profiler.rs                           # Zero-allocation 25 ns-bucket latency histogram & CPU fuel profiler
│   │   ├── engine.rs                             # Wasmtime 50 JIT Host, EpochWatchdog, Live Atomic Hot-Swap, 3-Pass bench paths
│   │   └── grpc_service.rs                       # Tonic WasmQuantHostService + LedgerGrpcBridgeSink -> ledger-rs :50151
│   ├── benches/
│   │   └── tick_latency.rs                       # Criterion statistical benchmark suite (Pass 1/2/3 + 3 prod .wasm strategies)
│   └── tests/
│       ├── strategies_integration.rs             # 6 E2E tests: 3 strategies, atomic hot-swap + rollback, NaN determinism, gRPC
│       ├── malware_torture.rs                    # 6+1 adversarial bomb torture suite + 500,000-cycle zero-leak soak + <25µs SLA
│       └── mutation_and_microopt_gate.rs         # Boundary suite killing 22/22 mutants + 3-pass latency & allocation verifier
└── scripts/
    ├── generate_wasm_manifest.py                 # Pure-Python Wasm binary parser auditing 0-WASI, 16..256 pages, exports, hashes
    └── run_mutation_suite.py                     # Automated 22-mutant AST/operator mutation harness (Gate 1 >= 95% kill score)
```

---

## 3. Shared Linear Memory Ring-Buffer ABI (`hyperion-quant-abi v1`)

Every compiled `.wasm` strategy statically places a single `64`-byte-aligned `SharedMemoryRegion` (`24,832 B` = `24.25 KiB`, `< 2.4%` of the `1 MiB` initial linear memory) inside guest linear memory and returns its base pointer via `hyperion_abi_shm_ptr() -> u32`. All structs are `#[repr(C, align(64))]`, pointer-width independent (bit-identical on `wasm32` and `x86_64`/`aarch64`), and compile-time size-asserted.

### 3.1 Memory Region Byte Map (`SHM_TOTAL_BYTES = 24,832 B`)

| Offset (`B`) | Size (`B`) | Alignment | Rust Type (`hyperion-quant-abi`) | Purpose |
|---:|---:|---:|---|---|
| `0` | `64` | `64 B` | `AbiControlHeader` | Magic `0x314D_5341_5750_5948` (`"HYPWASM1"`), ABI `v1`, `strategy_kind`, `account_id`, monotonic ingress/egress ring sequences |
| `64` | `128` | `64 B` | `StrategyInitBlock` | Rich `on_init` parameters (`tick_size_fp`, `lot_size_fp`, `max_position_fp`, Z-score/OFI/OBI/hedge thresholds, `cooldown_ns`, `stp_mode`) |
| `192` | `4,096` | `64 B` | `[IngressSlot64; 64]` | **`4 KiB` Host -> Guest Ingress Ring** (`64 × 64B`): `engine_seq`, `ts_ns`, prices/qty (`1e8`), `ofi_fp`, `book_imbalance_top5_ppm`, `event_kind` |
| `4,288` | `4,096` | `64 B` | `[EgressOrderSlot64; 64]` | **`4 KiB` Guest -> Host Egress Ring** (`64 × 64B`): `client_order_id`, `price_fp`, `qty_fp`, `side`, `order_type`, `stp_mode`, `strategy_tag` |
| `8,384` | `12,288` | `64 B` | `[L2DepthAndConfigExt; 64]` | Companion **`12 KiB` 5-Level L2 Depth Ring** (`64 × 192B`): `bids_price_fp[5]`, `bids_qty_fp[5]`, `asks_price_fp[5]`, `asks_qty_fp[5]`, `fair_ref_price_fp`, `spread_fp`, `order_id`, `client_order_id` |
| `20,672` | `4,160` | `64 B` | `StateSnapshotRegion` | Live hot-swap handoff buffer: `64 B` header (`magic = "WASMSNP1"`, `payload_len`, Castagnoli `crc32c`, `exported_engine_seq`, `net_inventory_fp`, `active_order_id`, `next_client_order_id`) + `4,096 B` payload |

### 3.2 Mandatory Guest Exports & Allowed Host Imports

| Direction | Symbol Signature | Role & Semantics |
|---|---|---|
| **Guest Export** | `memory` | Single linear memory (`min = 16` pages / `1 MiB`, `max <= 256` pages / `16 MiB`) |
| **Guest Export** | `hyperion_abi_shm_ptr() -> u32` | Returns `64B`-aligned linear memory pointer to static `SharedMemoryRegion` |
| **Guest Export** | `on_init(slot_idx: u32) -> i32` | Initializes strategy parameters from `StrategyInitBlock`; returns `0` (`CALLBACK_OK`) |
| **Guest Export** | `on_book_l2_tick(slot_idx: u32) -> i32` | Hot-path 5-level L2 book & OFI callback (reads `ingress_ring[slot_idx]` + `l2_ext_ring[slot_idx]`) |
| **Guest Export** | `on_trade_tick(slot_idx: u32) -> i32` | Hot-path public trade execution callback |
| **Guest Export** | `on_order_fill(slot_idx: u32) -> i32` | Private fill & updated `net_inventory_fp` callback |
| **Guest Export** | `hyperion_export_state(engine_seq: u64) -> i32` | Serializes live internal state into `StateSnapshotRegion` with Castagnoli `CRC-32C`; returns byte count `>= 0` |
| **Guest Export** | `hyperion_import_state() -> i32` | Validates magic + `CRC-32C` + `strategy_kind` and restores state in `v_new` during live atomic hot-swap |
| **Host Import** | `hyperion::submit_limit_order(egress_slot_ptr: u32) -> i64` | Validates `egress_slot_ptr` alignment/bounds, executes Pre-Trade Risk Gate, returns `> 0` (`order_id`) or `-risk_err_code`, or hard-traps on `> 16` orders/tick or OOB pointer |
| **Host Import** | `hyperion::cancel_order(order_id: u64, client_order_id: u64) -> i32` | Validates order ownership via Pre-Trade Risk Gate, returns `0` (`RISK_OK`) or `-risk_err_code` |

---

## 4. Guest Rust SDK (`hyperion-quant-sdk`) & 3 Compiled `.wasm` Reference Strategies

### 4.1 `#![no_std]` Guest SDK Usage (`crates/hyperion-quant-sdk`)
`hyperion-quant-sdk` compiles with **zero `std`**, **zero heap allocator**, and **zero floats**, supplying:
- `fp`: Exact `1e8` fixed-point arithmetic (`mul_fp`, `div_fp`, `notional_fp`, `ratio_ppm`, `isqrt_u128` Newton-Raphson integer square root, `ewma_fp`, `align_bid_tick`, `align_ask_tick`, `align_lot_qty`).
- `RollingWindowFp<const N: usize>`: Fixed-capacity `O(1)` circular buffer computing exact integer mean, variance (`u128`), standard deviation (`isqrt_u128`), and signed `zscore_ppm` without floating-point drift.
- `StateWriter` / `StateReader`: Zero-allocation little-endian cursor for deterministic hot-swap state serialization.
- `QuantStrategy` trait + `export_quant_strategy!(MyBot, MyBot::const_new())` macro automatically emitting `#[panic_handler] -> wasm32::unreachable()`, static `SharedMemoryRegion`, all 7 ABI callbacks, and `CRC-32C` snapshot framing.

```rust
#![no_std]
use hyperion_quant_sdk::abi::*;
use hyperion_quant_sdk::{
    export_quant_strategy, BookL2View, LimitOrderRequest, OrderFillView,
    QuantContext, QuantStrategy, StateReader, StateWriter, TradeTickView,
};

pub struct MyDeskBot { /* fixed-size stack state only */ }
impl QuantStrategy for MyDeskBot {
    const STRATEGY_KIND: u32 = STRATEGY_KIND_GENERIC;
    fn new() -> Self { Self {} }
    fn on_init(&mut self, cfg: &StrategyInitBlock, _ctx: &mut QuantContext<'_>) -> i32 { CALLBACK_OK }
    fn on_book_l2_tick(&mut self, book: &BookL2View, ctx: &mut QuantContext<'_>) -> i32 { CALLBACK_OK }
    fn on_trade_tick(&mut self, _t: &TradeTickView, _ctx: &mut QuantContext<'_>) -> i32 { CALLBACK_OK }
    fn on_order_fill(&mut self, _f: &OrderFillView, _ctx: &mut QuantContext<'_>) -> i32 { CALLBACK_OK }
    fn export_state(&self, _w: &mut StateWriter<'_>) -> (i64, u64, u64, bool) { (0, 0, 1, true) }
    fn import_state(&mut self, _r: &mut StateReader<'_>, _h: &StateSnapshotRegion) -> bool { true }
}
export_quant_strategy!(MyDeskBot, MyDeskBot::new());
```

### 4.2 Verified Compiled `.wasm` Artifacts (`artifacts/manifest.json`)

| Compiled Artifact | Size (`B`) | Linear Memory | Imports (`0` WASI) | SHA-256 Digest | Strategy Algorithm (`100%` Integer Fixed-Point `1e8`) |
|---|---:|---|---|---|---|
| `artifacts/stat_arb_zscore.wasm` | `41,040` | `16..256` pages (`1..16 MiB`) | `submit_limit_order`, `cancel_order` | `bd6410dc083481d7f1a7c02a9f2a335837aafac62d60ef38b62bd89b71e5a8e1` | Rolling `N`-tick spread `S_t = mid_fp - fair_ref_fp` via `RollingWindowFp<64>`, exact `isqrt_u128` Z-score blended with EWMA Order-Flow Imbalance (`OFI`), mean-reversion entry at `|composite_z| >= zscore_entry_ppm`, auto-unwind when `|composite_z| <= zscore_exit_ppm` |
| `artifacts/obi_sniper.wasm` | `39,649` | `16..256` pages (`1..16 MiB`) | `submit_limit_order` | `76916110cf4318ba7f1a250d106b2f18d0b726d3fd16bb2fd14b97f120876c2d` | 5-level exponential-decay weighted OBI (`weights = [50, 30, 12, 5, 3]`) + bid/ask depth slope asymmetry (`top2` vs `deep3`), spread cap filter (`spread_fp <= max_spread_fp`), anti-whipsaw `cooldown_ns`, micro-IOC snipe at top-of-book |
| `artifacts/delta_hedger.wasm` | `35,053` | `16..256` pages (`1..16 MiB`) | `submit_limit_order`, `cancel_order` | `ac482f704b85b828c1784fa20fe56eb8e1c2f54e1972be06f06d96aaa499d58e` | Dual-regime inventory + external options delta neutralizer: hysteresis deadband (`<= hedge_band_fp` cancels stale quote), passive `POST_ONLY` maker hedge inside `1×..2×` deadband, urgent `IOC` taker hedge above `2×` deadband with stale-order cancel-replace |
| `artifacts/malware_bombs.wasm` | `29,057` | `16..256` pages (`1..16 MiB`) | `submit_limit_order` | `e28ff15ac818934c10e9412196a70d1b056e6a497d48c5eb2b44293139ad02ad` | 6-mode adversarial exploit binary (`101`=infinite spin, `102`=deep non-tail recursion, `103`=4 GiB `memory.grow`, `104`=OOB linear read `0x03FF_FFF8`, `105`=10,000 illegal order flood, `106`=OOB host-import ptr `0xDEAD_BEE0`) |

---

## 5. Live Atomic Hot-Swapping, gRPC (`:50164`) & Telemetry Endpoints (`:8094`)

### 5.1 Atomic Hot-Swap Protocol (`atomic_hot_swap = true`)
1. Host invokes `hyperion_export_state(current_engine_seq)` on live `v_old` (without pausing or canceling open orders) and verifies `magic == "WASMSNP1"` and Castagnoli `CRC-32C`.
2. Host compiles, security-audits (`0` WASI, `<= 256` pages, optional BLAKE3 + Ed25519 signature check), instantiates, and runs `on_init` on candidate `v_new` in complete isolation while cloning `v_old`'s live `PreTradeRiskGate` (including all resting `OpenOrderRecord`s and net inventory).
3. Host injects verified snapshot bytes into `v_new`'s linear memory and executes `hyperion_import_state()`.
4. **Atomic Commit / Rollback**: On `CALLBACK_OK`, `self.strategies.insert(strategy_id, new_bot)` swaps the active instance atomically with zero open-order loss (`handed_off_open_orders` preserved). On *any* compile, signature, WASI, or snapshot error, `v_old` remains 100% live and unmodified (`test_live_atomic_hot_swap_preserves_open_orders_and_rollback_on_failure`).

### 5.2 Network & Observability Surface (`01-hyperion-omega.md` §2 & §3)

| Listener / Topic | Interface / Protocol | Operations / Payload |
|---|---|---|
| **gRPC `:50164`** (`GRPC_PORT` / `QUANT_WASM_GRPC_ADDR`) | `hyperion.omega.v1.WasmQuantHostService` (`wasm_quant.proto`) | `DeployStrategy` (deploy & atomic hot-swap), `PauseOrTerminateStrategy` (auto-cancels open orders), `StreamWasmTelemetry` (live broadcast of `FuelMeteringReport` & `SandboxExecutionTrap`) |
| **HTTP `:8094`** (`HTTP_PORT` / `QUANT_WASM_HTTP_ADDR`) | REST / Prometheus `0.0.4` | `GET /healthz`, `GET /metrics` (`hyperion_wasm_tick_p99_ns`, `fuel_consumed_total`, `traps_total`, `hot_swaps_total`), `GET /api/v1/wasm/strategies` |
| **Upstream gRPC `:50151`** (`HYPERION_LEDGER_GRPC_URL` / `LEDGER_GRPC_ADDR`) | `hyperion.v1.OrderbookService` (`LedgerGrpcBridgeSink`) | Async forwarding of Pre-Trade Risk-Gated `SubmitOrder` & `CancelOrder` commands with W3C `TraceMetadata` |
| **Redpanda Topics** | `application/x-protobuf` (`hyperion.omega.v1`) | `hyperion.omega.wasm.fuel.v1` (`FuelMeteringReport`), `hyperion.omega.wasm.traps.v1` (`SandboxExecutionTrap`) |

---

## 6. Verification, Hostile Malware Torture & Extreme Overclock Evidence

All benchmarks and tests executed directly on host (`rustc 1.99.1`, `x86_64-pc-windows-gnu`, `wasmtime 50.0.2`, `libprotoc 36.2`, `Intel Core i5-10300H`).

### 6.1 Static Analysis & Integration Test Suite (`12 / 12` PASSED, `0` Warnings)
- `scripts/generate_wasm_manifest.py`: **PASS** (`4/4` `.wasm` verified `pages=16..256`, `0` WASI imports, `8/8` ABI exports).
- `cargo clippy --release --offline -p quant-wasm-host -p hyperion-quant-abi -p hyperion-quant-sdk --all-targets -- -D warnings`: **PASS (`0` warnings)**.
- `cargo clippy --release --offline --target wasm32-unknown-unknown -p stat-arb-zscore -p obi-sniper -p delta-hedger -p malware-bombs -- -D warnings`: **PASS (`0` warnings)**.
- `cargo build --release --offline -p quant-wasm-host --bin quant-wasm-host`: **PASS (`exit 0`)**.
- `cargo test --release --offline -p quant-wasm-host -- --nocapture --test-threads=1`: **PASS (`12 / 12` tests, `0` failed)**:
  - `strategies_integration` (`6/6`): `stat_arb_zscore`, `obi_sniper`, `delta_hedger`, `live_atomic_hot_swap_and_rollback`, `nan_canonicalization_cross_host_determinism`, `grpc_wasm_quant_service_deploy_telemetry_terminate`.
  - `mutation_and_microopt_gate` (`4/4`): `fp_math_exact_boundaries`, `risk_gate_every_single_boundary`, `shm_ring_exact_spatial_bounds`, `three_pass_micro_optimization_table`.
  - `malware_torture` (`2/2`): `adversarial_malware_suite_all_five_bombs_and_sibling_isolation`, `500k_consecutive_cycles_zero_memory_leak_soak_and_latency_sla`.

### 6.2 Hostile Malware Torture Matrix (`host/tests/malware_torture.rs`)

| Attack Vector | Injected `.wasm` / WAT Payload | Caught `WasmTrapCode` / Error | Measured Kill Time | Fuel Consumed | Post-Trap Containment Proof |
|---|---|---|---:|---:|---|
| **(a1) Infinite `while(true)` Loop (Epoch)** | `malware_bombs.wasm` mode `101` (`max_epoch = 45 µs`, `fuel = 200k`) | `WASM_TRAP_CODE_EPOCH_DEADLINE_EXCEEDED` | **`59.00 µs`** (`< 100 µs` SLA) | `188` | Quarantined cleanly; `0` panics; sibling bot unaffected |
| **(a2) Infinite `while(true)` Loop (Fuel)** | `malware_bombs.wasm` mode `101` (`fuel = 12,000`, `epoch = 100 µs`) | `WASM_TRAP_CODE_FUEL_EXHAUSTED` | **`34.90 µs`** (`< 100 µs` SLA) | `12,000` | Deterministic exact-fuel preemption; quarantined |
| **(b) Deep Recursion Stack Bomb** | `malware_bombs.wasm` mode `102` (non-tail `deep_recurse`) | `WASM_TRAP_CODE_STACK_OVERFLOW` | `398.50 µs` (incl. full stack unwind) | `22,022` | Caught by Wasmtime `512 KiB` native Cranelift stack guard |
| **(c) Sudden 4 GiB Memory Allocation** | `malware_bombs.wasm` mode `103` (`memory.grow(65536)` + `512`) & static 65,536-page WAT | `WASM_TRAP_CODE_MEMORY_LIMIT_EXCEEDED` / `InitialMemoryTooLarge` | **`25.00 µs`** | `188` | Linear memory stayed locked at **`1,048,576 B` (`1 MiB`)**; `0 B` OS leak |
| **(d1) OOB Linear Memory Pointer Read** | `malware_bombs.wasm` mode `104` (`read_volatile(0x03FF_FFF8)`) | `WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY` | **`20.80 µs`** | `188` | Trapped immediately at hardware/bounds guard |
| **(d2) OOB Host-Import Pointer Injection** | `malware_bombs.wasm` mode `106` (`submit_limit_order(0xDEAD_BEE0)`) | `WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY` | **`11.70 µs`** | `210` | Caught by `read_egress_order_slot` bounds/alignment validator before Risk Gate |
| **(e) 10,000 Illegal Order Spam in 1 Tick** | `malware_bombs.wasm` mode `105` (0-px, off-tick, 900% fat-finger flood) | `WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED` | **`15.90 µs`** | `1,532` | Tripped at attempt #17 (`max 16/tick`); **`0` orders leaked** to Gateway/Ledger |
| **(f) Forbidden WASI Ambient Authority** | Raw WAT importing `wasi_snapshot_preview1::fd_write` | `SandboxError::ForbiddenImport` | Static (`< 1 µs`) | `0` | Rejected before instantiation |

### 6.3 Extreme Overclock Gate 1 — Mutation Survival Zero (`>= 95.0%` Required -> `100.00%` Achieved)
Executed via `C:\Python314\python.exe scripts\run_mutation_suite.py` (`artifacts/mutation_report.json`):

| Mutated Domain | Source File | Killed / Total | Kill Score | Survivors |
|---|---|---:|---:|---|
| Guest Fixed-Point Math & Rolling Z-Score (`M01`–`M09`) | `crates/hyperion-quant-sdk/src/lib.rs` | **`9 / 9`** | **`100.00%`** | `0` |
| Castagnoli CRC-32C Snapshot Integrity (`M10`) | `crates/hyperion-quant-abi/src/lib.rs` | **`1 / 1`** | **`100.00%`** | `0` |
| Pre-Trade Risk Gate Boundaries (`M11`–`M20`) | `host/src/risk_gate.rs` | **`10 / 10`** | **`100.00%`** | `0` |
| Shared Linear Memory Spatial & Alignment Bounds (`M21`–`M22`) | `host/src/shm_ring.rs` | **`2 / 2`** | **`100.00%`** | `0` |
| **TOTAL (`595.48s` full release compile-and-test)** | **4 core modules** | **`22 / 22`** | **`100.00%`** | **`[]` (ZERO)** |

### 6.4 Extreme Overclock Gate 2 — `500,000`-Cycle Zero-Leak Soak & `< 25 µs` SLA Proof
Measured via `test_500k_consecutive_cycles_zero_memory_leak_soak_and_latency_sla` with an active global `CountingAlloc` tracking allocator:
- **Consecutive Hot-Path Cycles**: **`500,000`** consecutive Host -> Wasm Guest (`obi_sniper.wasm`) -> Host ticks (after `10,000`-tick warmup).
- **Steady-State Host Heap Leak**: **`0 B` leaked**, **`0` heap allocations across all `500,000` ticks (`0.00 allocs/op`)**.
- **Wasm Linear Memory Delta**: **`1,048,576 B -> 1,048,576 B` (`0 B` growth)**.
- **Full-Telemetry Production SLA (`stat_arb_zscore.wasm`, `2,000` ticks w/ QPC `EpochWatchdog` + `StrategyProfiler`, `0` OS preemptions)**:
  - **`p50 = 1,425 ns (1.425 µs)`** | **`p99 = 1,925 ns (1.925 µs)`** (`13.0×` faster than `< 25 µs` SLA) | `max_fuel = 5,099`.

### 6.5 Extreme Overclock Gate 3 — 3-Pass Micro-Optimization & Criterion Benchmark Table

Measured via **Criterion `0.5.1`** (`cargo bench --offline -p quant-wasm-host --bench tick_latency`, `60` statistical samples, `913k–3.0M` iterations per benchmark) and `CountingAlloc` exact allocation profiling (`test_three_pass_micro_optimization_latency_and_allocation_table`):

| Optimization Pass / Production Benchmark | Architecture / Execution Path | Criterion Mean (`95% CI`) | Profiler `p50` (`ns/op`) | Profiler `p99` (`ns/op`) | Heap (`B/op`) | `allocs/op` | Speedup vs Pass 1 |
|---|---|---:|---:|---:|---:|---:|---:|
| **Pass 1 — `Baseline`** | Dynamic `instance.get_func` + `Val` boxing + heap `Vec<u8>` ring serialization | `3,081.9 ns` (`[2.883, 3.313] µs`) | `1,900 ns` | `3,900 ns` | `332 B/op` | `7 allocs/op` | `1.00×` (baseline) |
| **Pass 2 — `Zero-Copy Refactor`** | Cached `TypedFunc<u32, i32>` + zero-copy linear memory slice codec | `1,458.0 ns` (`[1.374, 1.552] µs`) | `1,000 ns` | `2,000 ns` | **`0 B/op`** | **`0 allocs/op`** | **`2.11×`** (`-52.7%` mean, `-100%` allocs) |
| **Pass 3 — `Cache-Line Aligned Final`** | `#[repr(C, align(64))]` direct POD write + cached base offset + inline stack risk gate | **`1,377.4 ns`** (`[1.262, 1.510] µs`) | `1,300 ns` | `2,200 ns` | **`0 B/op`** | **`0 allocs/op`** | **`2.24×`** (`-55.3%` mean, `-100%` allocs) |
| **Prod Full Tick — `stat_arb_zscore.wasm`** | Full QPC `EpochWatchdog` + Fuel + Pre-Trade Risk Gate + `StrategyProfiler` (`220,317` calls) | **`1,776.5 ns`** (`[1.640, 1.969] µs`) | **`1,325 ns`** | **`2,125 ns`** (`p999=8.13 µs`) | `0 B/op` hot-path | `0` | **`11.8×` faster than `25 µs` SLA** (`mean_fuel=6,151`) |
| **Prod Full Tick — `obi_sniper.wasm`** | Full QPC `EpochWatchdog` + Fuel + 5-Level L2 Slope + `StrategyProfiler` (`18,536` calls) | `17.14 µs`* *(includes non-RT OS-preempt cold redeploy inside Criterion loop)* | **`1,625 ns`** | **`2,925 ns`** (`p999=12.43 µs`) | `0 B/op` hot-path | `0` | **`8.5×` faster than `25 µs` SLA** (`mean_fuel=4,493`) |
| **Prod Full Tick — `delta_hedger.wasm`** | Full QPC `EpochWatchdog` + Fuel + Delta Hysteresis + `StrategyProfiler` (`2,006` calls) | **`1,332.6 ns`** (`[1.087, 1.617] µs`) | **`325 ns`** | **`525 ns`** (`p999=2.63 µs`) | `0 B/op` hot-path | `0` | **`47.6×` faster than `25 µs` SLA** (`mean_fuel=135`) |

---

## 7. Reproducible Build, Test & Benchmark Commands

```powershell
Set-Location services\quant-wasm-host

# 1. Compile all 3 production reference strategies + adversarial malware suite to wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown -p stat-arb-zscore -p obi-sniper -p delta-hedger -p malware-bombs
python scripts/generate_wasm_manifest.py

# 2. Verify zero clippy warnings (host + ABI + SDK + all wasm32 binaries)
cargo clippy --release -p quant-wasm-host -p hyperion-quant-abi -p hyperion-quant-sdk --all-targets -- -D warnings
cargo clippy --release --target wasm32-unknown-unknown -p stat-arb-zscore -p obi-sniper -p delta-hedger -p malware-bombs -- -D warnings

# 3. Execute all 12 release tests (strategies, hot-swap, gRPC, 6+1 malware torture bombs, 500k zero-leak soak, 3-pass micro-opt)
cargo test --release -p quant-wasm-host -- --nocapture --test-threads=1

# 4. Run Criterion statistical tick-latency benchmarks (< 25 us p99 SLA)
cargo bench -p quant-wasm-host --bench tick_latency

# 5. Execute 22-mutant Mutation Survival Zero harness (target >= 95%, achieved 22/22 = 100.00%)
python scripts/run_mutation_suite.py

# 6. Run production gRPC (:50164) & HTTP/Prometheus (:8094) daemon
cargo run --release -p quant-wasm-host --bin quant-wasm-host
```
