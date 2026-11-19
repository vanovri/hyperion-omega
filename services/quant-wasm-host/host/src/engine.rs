//! # Deterministic Zero-Syscall Wasmtime JIT Sandbox Engine (`engine.rs`)
//!
//! Core runtime implementing:
//! 1. **Zero WASI Ambient Authority**: Rejects any `.wasm` module importing `wasi_snapshot_preview1`,
//!    `wasi:*`, `env`, filesystem, sockets, clocks, or RNGs (`WASM_TRAP_CODE_FORBIDDEN_HOST_IMPORT`).
//! 2. **Instruction Determinism**: `cranelift_nan_canonicalization(true)`, `wasm_relaxed_simd(false)`,
//!    no Wasm threads, and `100%` integer `1e8` fixed-point ABI boundary.
//! 3. **Spatial Memory & Stack Isolation**: `16 MiB` (`256` pages) linear memory hard cap enforced
//!    both at module validation and mid-instruction via `ResourceLimiter`, plus `512 KiB` native Wasm
//!    call-stack cap (`WASM_TRAP_CODE_STACK_OVERFLOW`).
//! 4. **Dual Preemption (<100 µs Kill Guarantee)**: Cranelift deterministic `consume_fuel(true)`
//!    paired with a high-precision QPC `EpochWatchdog` ticking `Engine::increment_epoch()` every
//!    `20 µs` whenever any sandbox callback is in flight (`WASM_TRAP_CODE_FUEL_EXHAUSTED` /
//!    `WASM_TRAP_CODE_EPOCH_DEADLINE_EXCEEDED`).
//! 5. **Live Atomic Hot-Swapping**: Snapshot export (`CRC-32C` verified) -> new instance init & import ->
//!    atomic open-order & risk-gate handoff without dropping a single resting order, with automatic
//!    rollback to `v_old` if `v_new` fails validation or state import.

use crate::profiler::{ProfilerSummary, StrategyProfiler};
use crate::proto::omega::{
    FuelMeteringReport, SandboxExecutionTrap, StrategyLifecycleState, WasmCallbackHook,
    WasmTrapCode,
};
use crate::proto::v1::TraceMetadata;
use crate::risk_gate::{
    InstrumentSpec, OpenOrderRecord, PreTradeRiskGate, RiskGateOutcome, RiskPolicy,
    ValidatedOrderAction,
};
use crate::shm_ring::*;
use ed25519_dalek::{Signature, VerifyingKey};
use hyperion_quant_abi::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use wasmtime::*;

/// Default epoch tick cadence (`20 µs` = `20,000 ns`).
/// With default deadline = `3` ticks (`60 µs`), infinite loops trap well below `< 100 µs`!
pub const EPOCH_TICK_NS: u64 = 20_000;

#[derive(Debug, Error, Clone)]
pub enum HostTrapSignal {
    #[error("linear memory growth {desired}B exceeds sandbox ceiling {limit}B")]
    MemoryLimitExceeded { desired: usize, limit: usize },
    #[error("table growth {desired} elements exceeds ceiling {limit}")]
    TableLimitExceeded { desired: usize, limit: usize },
    #[error("guest pointer bounds violation: {0}")]
    OutOfBoundsPointer(#[from] ShmBoundsError),
    #[error("order spam circuit breaker tripped: {attempts} mutations in single tick (max {max_allowed})")]
    OrderSpamLimitExceeded { attempts: u32, max_allowed: u32 },
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("forbidden host import `{module}::{name}` (zero-WASI policy)")]
    ForbiddenImport { module: String, name: String },
    #[error("declared initial linear memory {min_pages} pages exceeds ceiling {max_pages} pages")]
    InitialMemoryTooLarge { min_pages: u64, max_pages: u64 },
    #[error("module exports {count} memories (expected exactly 1 `memory` export)")]
    InvalidMemoryExportCount { count: usize },
    #[error("bytecode blake3 mismatch: expected {expected}, actual {actual}")]
    BytecodeHashMismatch { expected: String, actual: String },
    #[error("ed25519 author signature verification failed")]
    InvalidAuthorSignature,
    #[error("missing required C-ABI export `{0}`")]
    MissingExport(&'static str),
    #[error("strategy `{0}` not found")]
    StrategyNotFound(String),
    #[error("strategy `{0}` is not active (current state: {1:?})")]
    StrategyNotActive(String, StrategyLifecycleState),
    #[error("guest callback returned error status {0}")]
    GuestCallbackError(i32),
    #[error("wasm execution trapped: {code:?} ({detail})")]
    ExecutionTrapped {
        code: WasmTrapCode,
        detail: String,
        trap_record: Box<SandboxExecutionTrap>,
    },
    #[error("wasmtime compilation/instantiation failure: {0}")]
    EngineFailure(String),
}

/// High-precision QPC (`Instant`) Epoch Watchdog thread.
/// Parks at `0%` CPU when no sandbox callback is executing; spins on QPC at `20 µs` cadence
/// while `active_calls > 0` to guarantee `< 100 µs` hard preemption even on Windows schedulers.
pub struct EpochWatchdog {
    stop: Arc<AtomicBool>,
    active_calls: Arc<AtomicUsize>,
    parked: Arc<AtomicBool>,
    wake_pair: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl EpochWatchdog {
    pub fn spawn(engine: Engine, tick_ns: u64) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let active_calls = Arc::new(AtomicUsize::new(0));
        let parked = Arc::new(AtomicBool::new(false));
        let wake_pair = Arc::new((Mutex::new(false), Condvar::new()));

        let stop_bg = Arc::clone(&stop);
        let active_bg = Arc::clone(&active_calls);
        let parked_bg = Arc::clone(&parked);
        let wake_bg = Arc::clone(&wake_pair);
        let interval = Duration::from_nanos(tick_ns.max(5_000));

        let handle = thread::Builder::new()
            .name("hyperion-wasm-epoch-watchdog".to_string())
            .spawn(move || {
                let mut idle_since = Instant::now();
                while !stop_bg.load(Ordering::Acquire) {
                    if active_bg.load(Ordering::Acquire) == 0 {
                        if idle_since.elapsed() < Duration::from_millis(5) {
                            std::hint::spin_loop();
                            continue;
                        }
                        parked_bg.store(true, Ordering::Release);
                        let (lock, cvar) = &*wake_bg;
                        let mut guard = lock.lock().unwrap();
                        if active_bg.load(Ordering::Acquire) == 0
                            && !stop_bg.load(Ordering::Acquire)
                        {
                            let res = cvar.wait_timeout(guard, Duration::from_millis(5)).unwrap();
                            guard = res.0;
                        }
                        *guard = false;
                        parked_bg.store(false, Ordering::Release);
                        idle_since = Instant::now();
                        continue;
                    }

                    let mut next_deadline = Instant::now() + interval;
                    while active_bg.load(Ordering::Acquire) > 0 && !stop_bg.load(Ordering::Acquire)
                    {
                        let now = Instant::now();
                        if now >= next_deadline {
                            engine.increment_epoch();
                            next_deadline = now + interval;
                        } else {
                            std::hint::spin_loop();
                        }
                    }
                    idle_since = Instant::now();
                }
            })
            .expect("failed to spawn hyperion-wasm-epoch-watchdog");

        Self {
            stop,
            active_calls,
            parked,
            wake_pair,
            handle: Some(handle),
        }
    }

    #[inline]
    fn enter_call(&self) -> ActiveCallGuard<'_> {
        self.active_calls.fetch_add(1, Ordering::AcqRel);
        if self.parked.load(Ordering::Acquire) {
            let (lock, cvar) = &*self.wake_pair;
            if let Ok(mut woken) = lock.try_lock() {
                *woken = true;
                cvar.notify_one();
            } else {
                cvar.notify_one();
            }
        }
        ActiveCallGuard {
            active_calls: &self.active_calls,
        }
    }
}

struct ActiveCallGuard<'a> {
    active_calls: &'a AtomicUsize,
}

impl Drop for ActiveCallGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.active_calls.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for EpochWatchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let (lock, cvar) = &*self.wake_pair;
        if let Ok(mut guard) = lock.lock() {
            *guard = true;
            cvar.notify_all();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Per-instance Wasmtime `Store` state holding the `ResourceLimiter` and Pre-Trade Risk Gate.
pub struct BotStoreState {
    pub max_linear_memory_bytes: usize,
    pub max_table_elements: usize,
    pub current_ts_ns: u64,
    pub risk_gate: PreTradeRiskGate,
}

impl ResourceLimiter for BotStoreState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_linear_memory_bytes {
            return Err(wasmtime::Error::new(HostTrapSignal::MemoryLimitExceeded {
                desired,
                limit: self.max_linear_memory_bytes,
            }));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_table_elements {
            return Err(wasmtime::Error::new(HostTrapSignal::TableLimitExceeded {
                desired,
                limit: self.max_table_elements,
            }));
        }
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ValidatedSandboxConfig {
    pub max_linear_memory_bytes: usize,
    pub max_stack_bytes: usize,
    pub max_fuel_per_tick: u64,
    pub max_epoch_walltime_ns: u64,
    pub risk_policy: RiskPolicy,
    pub quarantine_on_trap: bool,
    pub auto_cancel_on_quarantine: bool,
}

impl Default for ValidatedSandboxConfig {
    fn default() -> Self {
        Self {
            max_linear_memory_bytes: MAX_LINEAR_MEMORY_BYTES, // 16 MiB
            max_stack_bytes: MAX_WASM_STACK_BYTES,            // 512 KiB
            max_fuel_per_tick: DEFAULT_FUEL_PER_TICK,         // 50,000
            max_epoch_walltime_ns: 80_000,                    // 80 µs (< 100 µs hard ceiling)
            risk_policy: RiskPolicy::default(),
            quarantine_on_trap: true,
            auto_cancel_on_quarantine: true,
        }
    }
}

impl ValidatedSandboxConfig {
    /// Clamps caller-requested limits strictly within governance ceilings (`01-hyperion-omega.md` §4).
    pub fn sanitize(mut self) -> Self {
        if self.max_linear_memory_bytes == 0 || self.max_linear_memory_bytes > MAX_LINEAR_MEMORY_BYTES
        {
            self.max_linear_memory_bytes = MAX_LINEAR_MEMORY_BYTES;
        }
        if self.max_stack_bytes == 0 || self.max_stack_bytes > MAX_WASM_STACK_BYTES {
            self.max_stack_bytes = MAX_WASM_STACK_BYTES;
        }
        if self.max_fuel_per_tick == 0 {
            self.max_fuel_per_tick = DEFAULT_FUEL_PER_TICK;
        } else if self.max_fuel_per_tick > MAX_INSTITUTIONAL_FUEL_PER_TICK {
            self.max_fuel_per_tick = MAX_INSTITUTIONAL_FUEL_PER_TICK;
        }
        if self.max_epoch_walltime_ns == 0 || self.max_epoch_walltime_ns > 100_000 {
            self.max_epoch_walltime_ns = 80_000;
        }
        if self.risk_policy.max_orders_per_tick == 0
            || self.risk_policy.max_orders_per_tick > MAX_ORDERS_PER_TICK
        {
            self.risk_policy.max_orders_per_tick = MAX_ORDERS_PER_TICK;
        }
        self
    }
}

/// Pre-resolved typed Wasm exports for zero-lookup sub-microsecond hot-path execution.
struct CompiledGuestExports {
    memory: Memory,
    shm_base_ptr: u32,
    strategy_kind: u32,
    fn_on_book_l2: TypedFunc<u32, i32>,
    fn_on_trade: TypedFunc<u32, i32>,
    fn_on_fill: TypedFunc<u32, i32>,
    fn_export_state: Option<TypedFunc<u64, i32>>,
    fn_import_state: Option<TypedFunc<(), i32>>,
}

/// Live isolated instance of a compiled quantitative `.wasm` strategy.
pub struct StrategyInstance {
    pub strategy_id: String,
    pub strategy_name: String,
    pub account_id: u64,
    pub version: u32,
    pub subscribed_symbols: Vec<String>,
    pub bytecode_blake3: [u8; 32],
    pub config: ValidatedSandboxConfig,
    pub init_block: StrategyInitBlock,
    pub lifecycle_state: StrategyLifecycleState,
    pub profiler: StrategyProfiler,
    store: Store<BotStoreState>,
    instance: Instance,
    exports: CompiledGuestExports,
}

/// Outcome of a single deterministic callback tick (`on_book_l2_tick`, `on_trade_tick`, `on_order_fill`).
#[derive(Clone, Debug)]
pub struct TickExecutionResult {
    pub return_code: i32,
    pub fuel_allocated: u64,
    pub fuel_consumed: u64,
    pub execution_latency_ns: u64,
    pub linear_memory_bytes: u32,
    pub orders_emitted: u32,
    pub orders_rejected: u32,
}

/// Pluggable sink receiving validated Pre-Trade Risk Gate orders (gRPC `ledger-rs` / `gateway-go` or test recorder).
pub trait OrderGatewaySink: Send + Sync {
    fn on_validated_order(&self, strategy_id: &str, action: ValidatedOrderAction);
}

#[derive(Default)]
pub struct RecordingOrderSink {
    pub actions: Mutex<Vec<(String, ValidatedOrderAction)>>,
}

impl RecordingOrderSink {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }

    pub fn drain(&self) -> Vec<(String, ValidatedOrderAction)> {
        let mut g = self.actions.lock().unwrap();
        std::mem::take(&mut *g)
    }

    pub fn len(&self) -> usize {
        self.actions.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl OrderGatewaySink for RecordingOrderSink {
    #[inline]
    fn on_validated_order(&self, strategy_id: &str, action: ValidatedOrderAction) {
        let mut g = self.actions.lock().unwrap();
        g.push((strategy_id.to_string(), action));
    }
}

/// Zero-overhead no-op sink used during pure Host<->Guest latency micro-benchmarks.
pub struct NullOrderSink;
impl OrderGatewaySink for NullOrderSink {
    #[inline]
    fn on_validated_order(&self, _strategy_id: &str, _action: ValidatedOrderAction) {}
}

#[derive(Clone, Debug)]
pub enum TelemetryEvent {
    Fuel(FuelMeteringReport),
    Trap(SandboxExecutionTrap),
}

/// Production Co-Located Deterministic Wasmtime Quant Sandbox Host.
pub struct QuantWasmHost {
    engine: Engine,
    linker: Arc<Linker<BotStoreState>>,
    watchdog: EpochWatchdog,
    strategies: HashMap<String, StrategyInstance>,
    order_sink: Arc<dyn OrderGatewaySink>,
    trap_seq: AtomicU64,
    order_id_pool: AtomicU64,
    trusted_verifying_key: Option<VerifyingKey>,
    telemetry_tx: tokio::sync::broadcast::Sender<TelemetryEvent>,
    emit_per_tick_broadcast: bool,
}

#[allow(non_snake_case)]
impl QuantWasmHost {
    /// Pre-commit `576 KiB` of OS thread stack pages (`512 KiB` Wasm stack guard + `64 KiB` host
    /// headroom) in `4 KiB` page strides so Windows `PAGE_GUARD` / Linux demand-paging faults
    /// never inject ~140 µs jitter on the trading hot path or delay `Trap::StackOverflow`.
    #[inline(never)]
    pub fn prefault_current_thread_stack() {
        let mut probe = [0u8; 576 * 1024];
        let mut i = 0usize;
        while i < probe.len() {
            unsafe {
                core::ptr::write_volatile(probe.as_mut_ptr().add(i), (i & 0xFF) as u8);
            }
            i += 4096;
        }
        std::hint::black_box(&probe[0]);
    }

    pub fn new(order_sink: Arc<dyn OrderGatewaySink>) -> anyhow::Result<Self> {
        Self::prefault_current_thread_stack();
        let mut wasm_cfg = Config::new();
        wasm_cfg.consume_fuel(true);
        wasm_cfg.epoch_interruption(true);
        wasm_cfg.cranelift_nan_canonicalization(true);
        wasm_cfg.wasm_relaxed_simd(false);
        wasm_cfg.cranelift_opt_level(OptLevel::Speed);
        wasm_cfg.max_wasm_stack(MAX_WASM_STACK_BYTES);
        // Bound virtual address reservation per linear memory so 100s of co-located bots remain lightweight
        // and out-of-bounds reads immediately fault on bounds checks.
        wasm_cfg.memory_reservation(MAX_LINEAR_MEMORY_BYTES as u64);
        wasm_cfg.memory_guard_size(65_536);

        let engine = Engine::new(&wasm_cfg)?;
        let linker = Arc::new(Self::build_zero_wasi_linker(&engine)?);
        let watchdog = EpochWatchdog::spawn(engine.clone(), EPOCH_TICK_NS);
        let (telemetry_tx, _) = tokio::sync::broadcast::channel(4096);

        Ok(Self {
            engine,
            linker,
            watchdog,
            strategies: HashMap::new(),
            order_sink,
            trap_seq: AtomicU64::new(1),
            order_id_pool: AtomicU64::new(100_000),
            trusted_verifying_key: None,
            telemetry_tx,
            emit_per_tick_broadcast: false,
        })
    }

    pub fn set_trusted_author_key(&mut self, vk: VerifyingKey) {
        self.trusted_verifying_key = Some(vk);
    }

    pub fn set_per_tick_telemetry_broadcast(&mut self, enabled: bool) {
        self.emit_per_tick_broadcast = enabled;
    }

    pub fn subscribe_telemetry(&self) -> tokio::sync::broadcast::Receiver<TelemetryEvent> {
        self.telemetry_tx.subscribe()
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    fn build_zero_wasi_linker(engine: &Engine) -> anyhow::Result<Linker<BotStoreState>> {
        let mut linker = Linker::new(engine);
        // Strictly NO WASI linked! Only the two Pre-Trade Risk-Gated order entrypoints exist:
        linker.func_wrap(
            "hyperion",
            "submit_limit_order",
            |mut caller: Caller<'_, BotStoreState>, slot_ptr: u32| -> wasmtime::Result<i64> {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return Err(wasmtime::Error::new(HostTrapSignal::OutOfBoundsPointer(
                        ShmBoundsError::OutOfBounds {
                            ptr: slot_ptr,
                            len: 64,
                            mem_len: 0,
                        },
                    )));
                };
                let slot = {
                    let bytes = mem.data(&caller);
                    read_egress_order_slot(bytes, slot_ptr)
                        .map_err(|e| wasmtime::Error::new(HostTrapSignal::OutOfBoundsPointer(e)))?
                };
                let outcome = caller.data_mut().risk_gate.validate_submit_limit(&slot);
                match outcome {
                    RiskGateOutcome::AcceptedSubmit { order_id } => Ok(order_id as i64),
                    RiskGateOutcome::AcceptedCancel => Ok(0),
                    RiskGateOutcome::SoftRejected { risk_code } => Ok(-(risk_code as i64)),
                    RiskGateOutcome::HardTrapOrderSpam {
                        attempts,
                        max_allowed,
                    } => Err(wasmtime::Error::new(
                        HostTrapSignal::OrderSpamLimitExceeded {
                            attempts,
                            max_allowed,
                        },
                    )),
                }
            },
        )?;

        linker.func_wrap(
            "hyperion",
            "cancel_order",
            |mut caller: Caller<'_, BotStoreState>,
             order_id: u64,
             client_order_id: u64|
             -> wasmtime::Result<i32> {
                let ts_ns = caller.data().current_ts_ns;
                let outcome =
                    caller
                        .data_mut()
                        .risk_gate
                        .validate_cancel(order_id, client_order_id, ts_ns);
                match outcome {
                    RiskGateOutcome::AcceptedCancel => Ok(0),
                    RiskGateOutcome::AcceptedSubmit { .. } => Ok(0),
                    RiskGateOutcome::SoftRejected { risk_code } => Ok(-risk_code),
                    RiskGateOutcome::HardTrapOrderSpam {
                        attempts,
                        max_allowed,
                    } => Err(wasmtime::Error::new(
                        HostTrapSignal::OrderSpamLimitExceeded {
                            attempts,
                            max_allowed,
                        },
                    )),
                }
            },
        )?;

        Ok(linker)
    }

    /// Static Zero-Syscall & Memory Ceiling Audit executed BEFORE instantiation.
    pub fn validate_module_security(
        module: &Module,
        max_memory_bytes: usize,
    ) -> Result<(), SandboxError> {
        for imp in module.imports() {
            let mod_name = imp.module();
            let field = imp.name();
            let allowed = mod_name == "hyperion"
                && (field == "submit_limit_order" || field == "cancel_order");
            if !allowed {
                return Err(SandboxError::ForbiddenImport {
                    module: mod_name.to_string(),
                    name: field.to_string(),
                });
            }
        }

        let max_pages = (max_memory_bytes / WASM_PAGE_BYTES) as u64;
        let mut memory_exports = 0usize;
        for exp in module.exports() {
            if let ExternType::Memory(mem_ty) = exp.ty() {
                memory_exports += 1;
                if mem_ty.minimum() > max_pages {
                    return Err(SandboxError::InitialMemoryTooLarge {
                        min_pages: mem_ty.minimum(),
                        max_pages,
                    });
                }
            }
        }
        if memory_exports != 1 {
            return Err(SandboxError::InvalidMemoryExportCount {
                count: memory_exports,
            });
        }

        Ok(())
    }

    fn verify_integrity_and_signature(
        &self,
        strategy_id: &str,
        version: u32,
        wasm_bytecode: &[u8],
        expected_blake3: Option<&[u8]>,
        author_sig: Option<&[u8]>,
    ) -> Result<[u8; 32], SandboxError> {
        let actual_hash = *blake3::hash(wasm_bytecode).as_bytes();
        if let Some(exp) = expected_blake3 {
            if !exp.is_empty() && exp != actual_hash.as_slice() {
                return Err(SandboxError::BytecodeHashMismatch {
                    expected: hex_encode(exp),
                    actual: hex_encode(&actual_hash),
                });
            }
        }
        if let Some(vk) = &self.trusted_verifying_key {
            let sig_bytes = author_sig.ok_or(SandboxError::InvalidAuthorSignature)?;
            let sig_arr: [u8; 64] = sig_bytes
                .try_into()
                .map_err(|_| SandboxError::InvalidAuthorSignature)?;
            let sig = Signature::from_bytes(&sig_arr);
            let mut preimage = Vec::with_capacity(strategy_id.len() + 4 + 32);
            preimage.extend_from_slice(strategy_id.as_bytes());
            preimage.extend_from_slice(&version.to_be_bytes());
            preimage.extend_from_slice(&actual_hash);
            vk.verify_strict(&preimage, &sig)
                .map_err(|_| SandboxError::InvalidAuthorSignature)?;
        }
        Ok(actual_hash)
    }

    #[inline]
    fn epoch_ticks_for_ns(walltime_ns: u64) -> u64 {
        // E.g. 80,000 ns / 20,000 ns = 4 ticks -> guarantees trap in [60 µs, 80 µs] (< 100 µs!).
        (walltime_ns / EPOCH_TICK_NS).max(2)
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_isolated(
        &self,
        strategy_id: &str,
        strategy_name: &str,
        account_id: u64,
        version: u32,
        subscribed_symbols: Vec<String>,
        wasm_bytecode: &[u8],
        bytecode_blake3: [u8; 32],
        cfg: ValidatedSandboxConfig,
        mut init_block: StrategyInitBlock,
        inherited_risk_gate: Option<PreTradeRiskGate>,
    ) -> Result<StrategyInstance, SandboxError> {
        let module = Module::new(&self.engine, wasm_bytecode)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;
        Self::validate_module_security(&module, cfg.max_linear_memory_bytes)?;

        let starting_oid = self.order_id_pool.fetch_add(100_000, Ordering::Relaxed);
        let mut risk_gate = inherited_risk_gate
            .unwrap_or_else(|| PreTradeRiskGate::new(cfg.risk_policy, starting_oid));
        risk_gate.policy = cfg.risk_policy;
        risk_gate.policy.account_id = account_id;

        let mut store = Store::new(
            &self.engine,
            BotStoreState {
                max_linear_memory_bytes: cfg.max_linear_memory_bytes,
                max_table_elements: MAX_TABLE_ELEMENTS,
                current_ts_ns: now_epoch_ns(),
                risk_gate,
            },
        );
        store.limiter(|state| state);
        store
            .set_fuel(INIT_AND_SNAPSHOT_FUEL)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;
        // Strict immediate hard trap on epoch expiration: zero callback retry loopholes,
        // guaranteeing even 0-fuel `(loop (br 0))` infinite loops die in < 100 µs on ticks.
        store.epoch_deadline_trap();

        // Control-plane instantiate / init / snapshot hooks use 500k fuel & 5 ms cold-page epoch budget;
        // hot-path market-data ticks (`on_book_l2_tick`, `on_trade_tick`, `on_order_fill`) enforce <= 100 µs.
        let control_plane_epoch = Self::epoch_ticks_for_ns(5_000_000);
        let instance = {
            let _guard = self.watchdog.enter_call();
            store.set_epoch_deadline(control_plane_epoch);
            self.linker.instantiate(&mut store, &module).map_err(|e| {
                let (code, detail) = classify_wasmtime_error(&e);
                let trap = build_trap_proto(
                    self.trap_seq.fetch_add(1, Ordering::Relaxed),
                    strategy_id,
                    account_id,
                    version,
                    "",
                    0,
                    WasmCallbackHook::OnInit,
                    code,
                    0,
                    0,
                    0,
                    false,
                    StrategyLifecycleState::Terminated,
                    detail.clone(),
                );
                let _ = self.telemetry_tx.send(TelemetryEvent::Trap(trap.clone()));
                SandboxError::ExecutionTrapped {
                    code,
                    detail,
                    trap_record: Box::new(trap),
                }
            })?
        };

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or(SandboxError::MissingExport("memory"))?;

        let fn_shm_ptr = instance
            .get_typed_func::<(), u32>(&mut store, "hyperion_abi_shm_ptr")
            .map_err(|_| SandboxError::MissingExport("hyperion_abi_shm_ptr"))?;
        let fn_on_init = instance
            .get_typed_func::<u32, i32>(&mut store, "on_init")
            .map_err(|_| SandboxError::MissingExport("on_init"))?;
        let fn_on_book_l2 = instance
            .get_typed_func::<u32, i32>(&mut store, "on_book_l2_tick")
            .map_err(|_| SandboxError::MissingExport("on_book_l2_tick"))?;
        let fn_on_trade = instance
            .get_typed_func::<u32, i32>(&mut store, "on_trade_tick")
            .map_err(|_| SandboxError::MissingExport("on_trade_tick"))?;
        let fn_on_fill = instance
            .get_typed_func::<u32, i32>(&mut store, "on_order_fill")
            .map_err(|_| SandboxError::MissingExport("on_order_fill"))?;
        let fn_export_state = instance
            .get_typed_func::<u64, i32>(&mut store, "hyperion_export_state")
            .ok();
        let fn_import_state = instance
            .get_typed_func::<(), i32>(&mut store, "hyperion_import_state")
            .ok();

        store.set_fuel(INIT_AND_SNAPSHOT_FUEL).unwrap();
        let shm_base_ptr = {
            let _guard = self.watchdog.enter_call();
            store.set_epoch_deadline(control_plane_epoch);
            fn_shm_ptr
                .call(&mut store, ())
                .map_err(|e| SandboxError::EngineFailure(e.to_string()))?
        };

        let strategy_kind = verify_abi_control_header(memory.data(&store), shm_base_ptr)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        init_block.account_id = account_id;
        init_block.strategy_version = version;
        write_init_block(memory.data_mut(&mut store), shm_base_ptr, &init_block)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        // Execute `on_init(0)` under strict fuel + control-plane epoch metering:
        store.data_mut().risk_gate.begin_tick(0, 0, 0);
        store.set_fuel(INIT_AND_SNAPSHOT_FUEL).unwrap();
        let t0 = Instant::now();
        let init_res = {
            let _guard = self.watchdog.enter_call();
            store.set_epoch_deadline(control_plane_epoch);
            fn_on_init.call(&mut store, 0)
        };
        let elapsed_ns = t0.elapsed().as_nanos() as u64;
        let fuel_rem = store.get_fuel().unwrap_or(0);
        let fuel_used = INIT_AND_SNAPSHOT_FUEL.saturating_sub(fuel_rem);

        match init_res {
            Ok(rc) if rc >= 0 => {
                let mut profiler = StrategyProfiler::new();
                profiler.record_tick(elapsed_ns, fuel_used, 0, 0);
                store.data_mut().risk_gate.rollback_tick();
                Ok(StrategyInstance {
                    strategy_id: strategy_id.to_string(),
                    strategy_name: strategy_name.to_string(),
                    account_id,
                    version,
                    subscribed_symbols,
                    bytecode_blake3,
                    config: cfg,
                    init_block,
                    lifecycle_state: StrategyLifecycleState::Active,
                    profiler,
                    store,
                    instance,
                    exports: CompiledGuestExports {
                        memory,
                        shm_base_ptr,
                        strategy_kind,
                        fn_on_book_l2,
                        fn_on_trade,
                        fn_on_fill,
                        fn_export_state,
                        fn_import_state,
                    },
                })
            }
            Ok(err_rc) => Err(SandboxError::GuestCallbackError(err_rc)),
            Err(err) => {
                let (code, detail) = classify_wasmtime_error(&err);
                let mem_bytes = memory.data_size(&store) as u32;
                let trap = build_trap_proto(
                    self.trap_seq.fetch_add(1, Ordering::Relaxed),
                    strategy_id,
                    account_id,
                    version,
                    "",
                    0,
                    WasmCallbackHook::OnInit,
                    code,
                    fuel_used,
                    elapsed_ns,
                    mem_bytes,
                    false,
                    StrategyLifecycleState::Terminated,
                    detail.clone(),
                );
                let _ = self.telemetry_tx.send(TelemetryEvent::Trap(trap.clone()));
                Err(SandboxError::ExecutionTrapped {
                    code,
                    detail,
                    trap_record: Box::new(trap),
                })
            }
        }
    }

    /// Deploys a fresh `.wasm` strategy OR performs a Live Atomic Hot-Swap (`atomic_hot_swap = true`)
    /// preserving all open resting orders, net inventory, and internal rolling state without downtime!
    #[allow(clippy::too_many_arguments)]
    pub fn deploy_or_hot_swap(
        &mut self,
        strategy_id: &str,
        strategy_name: &str,
        account_id: u64,
        version: u32,
        subscribed_symbols: Vec<String>,
        wasm_bytecode: &[u8],
        expected_blake3: Option<&[u8]>,
        author_sig: Option<&[u8]>,
        raw_cfg: ValidatedSandboxConfig,
        init_block: StrategyInitBlock,
        atomic_hot_swap: bool,
        current_engine_seq: u64,
    ) -> Result<(u64, u32), SandboxError> {
        let t_compile = Instant::now();
        let cfg = raw_cfg.sanitize();
        let actual_blake3 = self.verify_integrity_and_signature(
            strategy_id,
            version,
            wasm_bytecode,
            expected_blake3,
            author_sig,
        )?;

        if atomic_hot_swap {
            if let Some(old_bot) = self.strategies.get_mut(strategy_id) {
                // 1. Export live state snapshot from `old_bot` without mutating or stopping it yet.
                let export_fn = old_bot
                    .exports
                    .fn_export_state
                    .clone()
                    .ok_or(SandboxError::MissingExport("hyperion_export_state"))?;
                old_bot.store.set_fuel(INIT_AND_SNAPSHOT_FUEL).unwrap();
                let exp_rc = {
                    let _guard = self.watchdog.enter_call();
                    old_bot
                        .store
                        .set_epoch_deadline(Self::epoch_ticks_for_ns(5_000_000));
                    export_fn
                        .call(&mut old_bot.store, current_engine_seq)
                        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?
                };
                if exp_rc < 0 {
                    return Err(SandboxError::GuestCallbackError(exp_rc));
                }

                let snap_bytes = extract_snapshot_bytes(
                    old_bot.exports.memory.data(&old_bot.store),
                    old_bot.exports.shm_base_ptr,
                )
                .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

                let inherited_risk = old_bot.store.data().risk_gate.clone();
                let inherited_profiler = old_bot.profiler.clone();

                // 2. Compile, instantiate, and `on_init` the new `.wasm` candidate in complete isolation!
                //    If anything fails here, `old_bot` remains 100% untouched and active!
                let mut new_bot = self.instantiate_isolated(
                    strategy_id,
                    strategy_name,
                    account_id,
                    version,
                    subscribed_symbols,
                    wasm_bytecode,
                    actual_blake3,
                    cfg,
                    init_block,
                    Some(inherited_risk),
                )?;

                // 3. Inject snapshot bytes and invoke `hyperion_import_state()` on `new_bot`.
                let import_fn = new_bot
                    .exports
                    .fn_import_state
                    .clone()
                    .ok_or(SandboxError::MissingExport("hyperion_import_state"))?;
                inject_snapshot_bytes(
                    new_bot.exports.memory.data_mut(&mut new_bot.store),
                    new_bot.exports.shm_base_ptr,
                    &snap_bytes,
                )
                .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

                new_bot.store.set_fuel(INIT_AND_SNAPSHOT_FUEL).unwrap();
                let imp_rc = {
                    let _guard = self.watchdog.enter_call();
                    new_bot
                        .store
                        .set_epoch_deadline(Self::epoch_ticks_for_ns(5_000_000));
                    import_fn
                        .call(&mut new_bot.store, ())
                        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?
                };
                if imp_rc != CALLBACK_OK {
                    return Err(SandboxError::GuestCallbackError(imp_rc));
                }

                let handed_off_orders = new_bot.store.data().risk_gate.open_order_count() as u32;
                new_bot.profiler = inherited_profiler;
                new_bot.profiler.record_hot_swap();

                // 4. Atomic pointer swap into active registry; `old_bot` is cleanly dropped!
                self.strategies.insert(strategy_id.to_string(), new_bot);
                let compile_us = (t_compile.elapsed().as_nanos() / 1_000).max(1) as u64;
                return Ok((compile_us, handed_off_orders));
            }
        }

        let new_bot = self.instantiate_isolated(
            strategy_id,
            strategy_name,
            account_id,
            version,
            subscribed_symbols,
            wasm_bytecode,
            actual_blake3,
            cfg,
            init_block,
            None,
        )?;
        self.strategies.insert(strategy_id.to_string(), new_bot);
        let compile_us = (t_compile.elapsed().as_nanos() / 1_000).max(1) as u64;
        Ok((compile_us, 0))
    }

    pub fn pause_or_terminate(
        &mut self,
        strategy_id: &str,
        terminate_permanently: bool,
        cancel_open_orders: bool,
        ts_ns: u64,
    ) -> Result<(StrategyLifecycleState, u32), SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;

        let mut canceled_count = 0u32;
        if cancel_open_orders {
            let cancels = bot
                .store
                .data_mut()
                .risk_gate
                .drain_cancel_all_open_orders(ts_ns);
            canceled_count = cancels.len() as u32;
            for c in cancels {
                self.order_sink.on_validated_order(strategy_id, c);
            }
        }

        if terminate_permanently {
            bot.lifecycle_state = StrategyLifecycleState::Terminated;
            let state = bot.lifecycle_state;
            self.strategies.remove(strategy_id);
            Ok((state, canceled_count))
        } else {
            bot.lifecycle_state = StrategyLifecycleState::Paused;
            Ok((bot.lifecycle_state, canceled_count))
        }
    }

    /// Hot-Path Pass 3 (Production Default): Cache-line aligned shared ring-buffer `on_book_l2_tick`
    /// with zero heap allocations on the steady-state execution path!
    pub fn on_book_l2_tick(
        &mut self,
        strategy_id: &str,
        tick: &HostBookL2Tick,
    ) -> Result<TickExecutionResult, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;
        if bot.lifecycle_state != StrategyLifecycleState::Active {
            return Err(SandboxError::StrategyNotActive(
                strategy_id.to_string(),
                bot.lifecycle_state,
            ));
        }

        let slot_idx = push_book_l2_ring_slot(
            bot.exports.memory.data_mut(&mut bot.store),
            bot.exports.shm_base_ptr,
            tick,
        )
        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        let fuel_budget = bot.config.max_fuel_per_tick;
        let epoch_ticks = Self::epoch_ticks_for_ns(bot.config.max_epoch_walltime_ns);
        {
            let st = bot.store.data_mut();
            st.current_ts_ns = tick.ts_ns;
            st.risk_gate
                .begin_tick(tick.mid_price_fp, tick.best_bid_fp, tick.best_ask_fp);
        }
        let _ = bot.store.set_fuel(fuel_budget);

        let t0 = Instant::now();
        let call_res = {
            let _guard = self.watchdog.enter_call();
            bot.store.set_epoch_deadline(epoch_ticks);
            bot.exports.fn_on_book_l2.call(&mut bot.store, slot_idx)
        };
        let latency_ns = t0.elapsed().as_nanos() as u64;
        let fuel_rem = bot.store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_budget.saturating_sub(fuel_rem);
        let mem_bytes = bot.exports.memory.data_size(&bot.store) as u32;

        Self::finalize_callback_execution(
            bot,
            &self.order_sink,
            &self.trap_seq,
            &self.telemetry_tx,
            self.emit_per_tick_broadcast,
            call_res,
            WasmCallbackHook::OnBookL2Tick,
            InstrumentSpec::symbol_str(tick.instrument_id),
            tick.engine_seq,
            tick.ts_ns,
            fuel_budget,
            fuel_consumed,
            fuel_rem,
            latency_ns,
            mem_bytes,
        )
    }

    pub fn on_trade_tick(
        &mut self,
        strategy_id: &str,
        trade: &HostTradeTick,
    ) -> Result<TickExecutionResult, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;
        if bot.lifecycle_state != StrategyLifecycleState::Active {
            return Err(SandboxError::StrategyNotActive(
                strategy_id.to_string(),
                bot.lifecycle_state,
            ));
        }

        let slot_idx = push_trade_ring_slot(
            bot.exports.memory.data_mut(&mut bot.store),
            bot.exports.shm_base_ptr,
            trade,
        )
        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        let fuel_budget = bot.config.max_fuel_per_tick;
        let epoch_ticks = Self::epoch_ticks_for_ns(bot.config.max_epoch_walltime_ns);
        {
            let st = bot.store.data_mut();
            st.current_ts_ns = trade.ts_ns;
            st.risk_gate.begin_tick(trade.price_fp, 0, 0);
        }
        let _ = bot.store.set_fuel(fuel_budget);

        let t0 = Instant::now();
        let call_res = {
            let _guard = self.watchdog.enter_call();
            bot.store.set_epoch_deadline(epoch_ticks);
            bot.exports.fn_on_trade.call(&mut bot.store, slot_idx)
        };
        let latency_ns = t0.elapsed().as_nanos() as u64;
        let fuel_rem = bot.store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_budget.saturating_sub(fuel_rem);
        let mem_bytes = bot.exports.memory.data_size(&bot.store) as u32;

        Self::finalize_callback_execution(
            bot,
            &self.order_sink,
            &self.trap_seq,
            &self.telemetry_tx,
            self.emit_per_tick_broadcast,
            call_res,
            WasmCallbackHook::OnTradeTick,
            InstrumentSpec::symbol_str(trade.instrument_id),
            trade.engine_seq,
            trade.ts_ns,
            fuel_budget,
            fuel_consumed,
            fuel_rem,
            latency_ns,
            mem_bytes,
        )
    }

    pub fn on_order_fill(
        &mut self,
        strategy_id: &str,
        fill: &HostOrderFill,
    ) -> Result<TickExecutionResult, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;
        if bot.lifecycle_state != StrategyLifecycleState::Active {
            return Err(SandboxError::StrategyNotActive(
                strategy_id.to_string(),
                bot.lifecycle_state,
            ));
        }

        let slot_idx = push_fill_ring_slot(
            bot.exports.memory.data_mut(&mut bot.store),
            bot.exports.shm_base_ptr,
            fill,
        )
        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        let fuel_budget = bot.config.max_fuel_per_tick;
        let epoch_ticks = Self::epoch_ticks_for_ns(bot.config.max_epoch_walltime_ns);
        {
            let st = bot.store.data_mut();
            st.current_ts_ns = fill.ts_ns;
            st.risk_gate.apply_fill_update(
                fill.order_id,
                fill.remaining_qty_fp,
                fill.net_inventory_fp,
            );
            st.risk_gate.begin_tick(fill.fill_price_fp, 0, 0);
        }
        let _ = bot.store.set_fuel(fuel_budget);

        let t0 = Instant::now();
        let call_res = {
            let _guard = self.watchdog.enter_call();
            bot.store.set_epoch_deadline(epoch_ticks);
            bot.exports.fn_on_fill.call(&mut bot.store, slot_idx)
        };
        let latency_ns = t0.elapsed().as_nanos() as u64;
        let fuel_rem = bot.store.get_fuel().unwrap_or(0);
        let fuel_consumed = fuel_budget.saturating_sub(fuel_rem);
        let mem_bytes = bot.exports.memory.data_size(&bot.store) as u32;

        Self::finalize_callback_execution(
            bot,
            &self.order_sink,
            &self.trap_seq,
            &self.telemetry_tx,
            self.emit_per_tick_broadcast,
            call_res,
            WasmCallbackHook::OnOrderFill,
            InstrumentSpec::symbol_str(fill.instrument_id),
            fill.engine_seq,
            fill.ts_ns,
            fuel_budget,
            fuel_consumed,
            fuel_rem,
            latency_ns,
            mem_bytes,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn finalize_callback_execution(
        bot: &mut StrategyInstance,
        order_sink: &Arc<dyn OrderGatewaySink>,
        trap_seq: &AtomicU64,
        telemetry_tx: &tokio::sync::broadcast::Sender<TelemetryEvent>,
        emit_broadcast: bool,
        call_res: Result<i32>,
        hook: WasmCallbackHook,
        symbol: &str,
        engine_seq: u64,
        ts_ns: u64,
        fuel_budget: u64,
        fuel_consumed: u64,
        fuel_rem: u64,
        latency_ns: u64,
        mem_bytes: u32,
    ) -> Result<TickExecutionResult, SandboxError> {
        match call_res {
            Ok(rc) if rc >= 0 => {
                let rejected = bot.store.data().risk_gate.tick_rejected_count();
                let sid = &bot.strategy_id;
                let orders_emitted = bot
                    .store
                    .data_mut()
                    .risk_gate
                    .commit_tick_with(|action| order_sink.on_validated_order(sid, action));

                bot.profiler
                    .record_tick(latency_ns, fuel_consumed, orders_emitted, rejected);

                if emit_broadcast && telemetry_tx.receiver_count() > 0 {
                    let report = FuelMeteringReport {
                        strategy_id: bot.strategy_id.clone(),
                        account_id: bot.account_id,
                        version: bot.version,
                        symbol: symbol.to_string(),
                        engine_seq,
                        callback_hook: hook as i32,
                        fuel_allocated: fuel_budget,
                        fuel_consumed,
                        fuel_remaining: fuel_rem,
                        execution_latency_ns: latency_ns,
                        linear_memory_pages: mem_bytes / (WASM_PAGE_BYTES as u32),
                        linear_memory_bytes: mem_bytes,
                        orders_emitted_in_tick: orders_emitted,
                        orders_rejected_by_risk_gate: rejected,
                        reported_ts_ns: ts_ns,
                        trace: None,
                    };
                    let _ = telemetry_tx.send(TelemetryEvent::Fuel(report));
                }

                Ok(TickExecutionResult {
                    return_code: rc,
                    fuel_allocated: fuel_budget,
                    fuel_consumed,
                    execution_latency_ns: latency_ns,
                    linear_memory_bytes: mem_bytes,
                    orders_emitted,
                    orders_rejected: rejected,
                })
            }
            Ok(err_rc) => {
                bot.store.data_mut().risk_gate.rollback_tick();
                Err(SandboxError::GuestCallbackError(err_rc))
            }
            Err(err) => {
                let rejected = bot.store.data().risk_gate.tick_rejected_count();
                // Roll back ALL staged mutations from the trapped tick (transactional atomicity!):
                bot.store.data_mut().risk_gate.rollback_tick();
                bot.profiler.record_trap(latency_ns, fuel_consumed, rejected);

                let (code, detail) = classify_wasmtime_error(&err);
                let mut auto_canceled = false;
                if bot.config.quarantine_on_trap {
                    bot.lifecycle_state = StrategyLifecycleState::QuarantinedOnTrap;
                    if bot.config.auto_cancel_on_quarantine {
                        let cancels = bot
                            .store
                            .data_mut()
                            .risk_gate
                            .drain_cancel_all_open_orders(ts_ns);
                        if !cancels.is_empty() {
                            auto_canceled = true;
                            for c in cancels {
                                order_sink.on_validated_order(&bot.strategy_id, c);
                            }
                        }
                    }
                }

                let trap = build_trap_proto(
                    trap_seq.fetch_add(1, Ordering::Relaxed),
                    &bot.strategy_id,
                    bot.account_id,
                    bot.version,
                    symbol,
                    engine_seq,
                    hook,
                    code,
                    fuel_consumed,
                    latency_ns,
                    mem_bytes,
                    auto_canceled,
                    bot.lifecycle_state,
                    detail.clone(),
                );
                let _ = telemetry_tx.send(TelemetryEvent::Trap(trap.clone()));

                Err(SandboxError::ExecutionTrapped {
                    code,
                    detail,
                    trap_record: Box::new(trap),
                })
            }
        }
    }

    // =========================================================================
    // Micro-Optimization Comparison Harnesses (`02-extreme-overclock-protocol.md` §3)
    // =========================================================================

    /// **Pass 1 (`Baseline`)**: Dynamic string export lookup (`instance.get_func` + `get_memory` per call),
    /// boxed `Val` argument/return vectors, and intermediate heap `Vec<u8>` buffer serialization.
    pub fn bench_pass1_baseline_tick(
        &mut self,
        strategy_id: &str,
        tick: &HostBookL2Tick,
    ) -> Result<i32, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;

        // Heap-allocated intermediate staging buffers + dynamic export lookups (naive baseline):
        let sym_name = String::from("on_book_l2_tick");
        let mem_name = String::from("memory");
        let mem = bot
            .instance
            .get_memory(&mut bot.store, &mem_name)
            .ok_or(SandboxError::MissingExport("memory"))?;
        let dyn_fn = bot
            .instance
            .get_func(&mut bot.store, &sym_name)
            .ok_or(SandboxError::MissingExport("on_book_l2_tick"))?;

        let mut serialized_slot = vec![0u8; 64];
        serialized_slot[0..8].copy_from_slice(&tick.engine_seq.to_le_bytes());
        serialized_slot[8..16].copy_from_slice(&tick.ts_ns.to_le_bytes());
        serialized_slot[16..24].copy_from_slice(&tick.best_bid_fp.to_le_bytes());
        serialized_slot[24..32].copy_from_slice(&tick.best_ask_fp.to_le_bytes());
        serialized_slot[32..40].copy_from_slice(&tick.mid_price_fp.to_le_bytes());
        serialized_slot[40..48].copy_from_slice(&tick.ofi_fp.to_le_bytes());
        serialized_slot[48..56].copy_from_slice(&tick.book_imbalance_top5_ppm.to_le_bytes());
        serialized_slot[56..60].copy_from_slice(&tick.instrument_id.to_le_bytes());
        serialized_slot[60] = EVENT_KIND_BOOK_L2;
        serialized_slot[62] = tick.bid_levels;
        serialized_slot[63] = tick.ask_levels;

        let mut serialized_ext = vec![0u8; 192];
        for i in 0..L2_BOOK_LEVELS {
            let p = i * 8;
            serialized_ext[p..p + 8].copy_from_slice(&tick.bids_price_fp[i].to_le_bytes());
            serialized_ext[40 + p..48 + p].copy_from_slice(&tick.bids_qty_fp[i].to_le_bytes());
            serialized_ext[80 + p..88 + p].copy_from_slice(&tick.asks_price_fp[i].to_le_bytes());
            serialized_ext[120 + p..128 + p].copy_from_slice(&tick.asks_qty_fp[i].to_le_bytes());
        }
        serialized_ext[160..168].copy_from_slice(&(tick.fair_ref_price_fp as i64).to_le_bytes());
        serialized_ext[168..176].copy_from_slice(&(tick.spread_fp as i64).to_le_bytes());

        let base = bot.exports.shm_base_ptr as usize;
        let data = mem.data_mut(&mut bot.store);
        let write_seq = u64::from_le_bytes(data[base + 32..base + 40].try_into().unwrap());
        let slot_idx = (write_seq as usize) & RING_MASK;
        data[base + OFFSET_INGRESS_RING + slot_idx * 64
            ..base + OFFSET_INGRESS_RING + (slot_idx + 1) * 64]
            .copy_from_slice(&serialized_slot);
        data[base + OFFSET_L2_EXT_RING + slot_idx * 192
            ..base + OFFSET_L2_EXT_RING + (slot_idx + 1) * 192]
            .copy_from_slice(&serialized_ext);
        data[base + 32..base + 40].copy_from_slice(&write_seq.wrapping_add(1).to_le_bytes());

        bot.store
            .data_mut()
            .risk_gate
            .begin_tick(tick.mid_price_fp, tick.best_bid_fp, tick.best_ask_fp);
        let _ = bot.store.set_fuel(bot.config.max_fuel_per_tick);
        bot.store.set_epoch_deadline(10);

        let params = vec![Val::I32(slot_idx as i32)];
        let mut results = vec![Val::I32(0)];
        dyn_fn
            .call(&mut bot.store, &params, &mut results)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        let sid = bot.strategy_id.clone();
        let sink = Arc::clone(&self.order_sink);
        bot.store
            .data_mut()
            .risk_gate
            .commit_tick_with(|a| sink.on_validated_order(&sid, a));
        Ok(results[0].unwrap_i32())
    }

    /// **Pass 2 (`Zero-Copy Refactor`)**: Eliminates heap `Vec` serialization and dynamic export lookups
    /// by using pre-resolved `TypedFunc<u32, i32>` and writing directly into linear memory slice,
    /// without active watchdog lock/unpark overhead when epoch deadline is pre-armed.
    pub fn bench_pass2_zerocopy_tick(
        &mut self,
        strategy_id: &str,
        tick: &HostBookL2Tick,
    ) -> Result<i32, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;

        let slot_idx = push_book_l2_ring_slot(
            bot.exports.memory.data_mut(&mut bot.store),
            bot.exports.shm_base_ptr,
            tick,
        )
        .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        bot.store
            .data_mut()
            .risk_gate
            .begin_tick(tick.mid_price_fp, tick.best_bid_fp, tick.best_ask_fp);
        let _ = bot.store.set_fuel(bot.config.max_fuel_per_tick);
        bot.store.set_epoch_deadline(10);

        let rc = bot
            .exports
            .fn_on_book_l2
            .call(&mut bot.store, slot_idx)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;
        let sid = &bot.strategy_id;
        let sink = &self.order_sink;
        bot.store
            .data_mut()
            .risk_gate
            .commit_tick_with(|a| sink.on_validated_order(sid, a));
        Ok(rc)
    }

    /// **Pass 3 (`Cache-Line Aligned Direct Slot`)**: Single pre-verified `64B`-aligned struct write
    /// directly to cached linear memory offset + Cranelift native trampoline + inline stack risk gate (`0 B/op`).
    #[inline]
    pub fn bench_pass3_cache_aligned_tick(
        &mut self,
        strategy_id: &str,
        tick: &HostBookL2Tick,
    ) -> Result<i32, SandboxError> {
        let bot = self
            .strategies
            .get_mut(strategy_id)
            .ok_or_else(|| SandboxError::StrategyNotFound(strategy_id.to_string()))?;

        let base = bot.exports.shm_base_ptr as usize;
        let mem_bytes = bot.exports.memory.data_mut(&mut bot.store);
        if base + SHM_TOTAL_BYTES > mem_bytes.len() {
            return Err(SandboxError::EngineFailure("shm out of bounds".to_string()));
        }

        let write_seq = u64::from_le_bytes(mem_bytes[base + 32..base + 40].try_into().unwrap());
        let slot_idx = (write_seq as usize) & RING_MASK;
        let slot_off = base + OFFSET_INGRESS_RING + (slot_idx << 6);
        let ext_off = base + OFFSET_L2_EXT_RING + (slot_idx * 192);

        // SAFETY: `mem_bytes` is a contiguous byte slice of length >= `base + SHM_TOTAL_BYTES`,
        // and `IngressSlot64` / `L2DepthAndConfigExt` are plain integer POD types written in native LE on x86_64.
        #[cfg(target_endian = "little")]
        unsafe {
            let slot_ptr = mem_bytes.as_mut_ptr().add(slot_off) as *mut IngressSlot64;
            slot_ptr.write_unaligned(IngressSlot64 {
                engine_seq: tick.engine_seq,
                ts_ns: tick.ts_ns,
                primary_price_fp: tick.best_bid_fp,
                secondary_price_fp: tick.best_ask_fp,
                qty_or_mid_fp: tick.mid_price_fp,
                signed_metric_a_fp: tick.ofi_fp,
                signed_metric_b: tick.book_imbalance_top5_ppm,
                instrument_id: tick.instrument_id,
                event_kind: EVENT_KIND_BOOK_L2,
                side: SIDE_UNSPECIFIED,
                aux_u8_a: tick.bid_levels,
                aux_u8_b: tick.ask_levels,
            });

            let ext_ptr = mem_bytes.as_mut_ptr().add(ext_off) as *mut L2DepthAndConfigExt;
            ext_ptr.write_unaligned(L2DepthAndConfigExt {
                bids_price_fp: tick.bids_price_fp,
                bids_qty_fp: tick.bids_qty_fp,
                asks_price_fp: tick.asks_price_fp,
                asks_qty_fp: tick.asks_qty_fp,
                ext_a: tick.fair_ref_price_fp as i64,
                ext_b: tick.spread_fp as i64,
                ext_c: 0,
                ext_d: 0,
            });
            let seq_ptr = mem_bytes.as_mut_ptr().add(base + 32) as *mut u64;
            seq_ptr.write_unaligned(write_seq.wrapping_add(1));
        }

        bot.store
            .data_mut()
            .risk_gate
            .begin_tick(tick.mid_price_fp, tick.best_bid_fp, tick.best_ask_fp);
        let _ = bot.store.set_fuel(bot.config.max_fuel_per_tick);
        bot.store.set_epoch_deadline(10);

        let rc = bot
            .exports
            .fn_on_book_l2
            .call(&mut bot.store, slot_idx as u32)
            .map_err(|e| SandboxError::EngineFailure(e.to_string()))?;

        let sid = &bot.strategy_id;
        let sink = &self.order_sink;
        bot.store
            .data_mut()
            .risk_gate
            .commit_tick_with(|a| sink.on_validated_order(sid, a));
        Ok(rc)
    }

    // =========================================================================
    // Inspection & Telemetry Accessors
    // =========================================================================

    pub fn strategy_summary(&self, strategy_id: &str) -> Option<ProfilerSummary> {
        self.strategies.get(strategy_id).map(|b| b.profiler.summary())
    }

    pub fn strategy_state(&self, strategy_id: &str) -> Option<StrategyLifecycleState> {
        self.strategies.get(strategy_id).map(|b| b.lifecycle_state)
    }

    pub fn strategy_version(&self, strategy_id: &str) -> Option<u32> {
        self.strategies.get(strategy_id).map(|b| b.version)
    }

    pub fn strategy_kind(&self, strategy_id: &str) -> Option<u32> {
        self.strategies
            .get(strategy_id)
            .map(|b| b.exports.strategy_kind)
    }

    pub fn strategy_memory_bytes(&self, strategy_id: &str) -> Option<usize> {
        self.strategies
            .get(strategy_id)
            .map(|b| b.exports.memory.data_size(&b.store))
    }

    pub fn strategy_open_orders(&self, strategy_id: &str) -> Option<Vec<OpenOrderRecord>> {
        self.strategies
            .get(strategy_id)
            .map(|b| b.store.data().risk_gate.open_orders_slice().to_vec())
    }

    pub fn active_strategy_count(&self) -> usize {
        self.strategies
            .values()
            .filter(|b| b.lifecycle_state == StrategyLifecycleState::Active)
            .count()
    }

    pub fn render_prometheus_metrics(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("# HELP hyperion_wasm_active_strategies Active co-located Wasm quant sandboxes\n");
        out.push_str("# TYPE hyperion_wasm_active_strategies gauge\n");
        out.push_str(&format!(
            "hyperion_wasm_active_strategies {}\n",
            self.active_strategy_count()
        ));

        out.push_str("# HELP hyperion_wasm_tick_p99_ns 99th percentile Host->Wasm->Host tick latency in nanoseconds\n");
        out.push_str("# TYPE hyperion_wasm_tick_p99_ns gauge\n");
        out.push_str("# HELP hyperion_wasm_fuel_consumed_total Total deterministic CPU fuel consumed\n");
        out.push_str("# TYPE hyperion_wasm_fuel_consumed_total counter\n");
        out.push_str("# HELP hyperion_wasm_traps_total Total caught and isolated Wasm traps\n");
        out.push_str("# TYPE hyperion_wasm_traps_total counter\n");
        out.push_str("# HELP hyperion_wasm_hot_swaps_total Total atomic live strategy hot-swaps\n");
        out.push_str("# TYPE hyperion_wasm_hot_swaps_total counter\n");

        for (sid, bot) in &self.strategies {
            let s = bot.profiler.summary();
            out.push_str(&format!(
                "hyperion_wasm_tick_p99_ns{{strategy_id=\"{sid}\",version=\"{}\"}} {}\n",
                bot.version, s.p99_latency_ns
            ));
            out.push_str(&format!(
                "hyperion_wasm_fuel_consumed_total{{strategy_id=\"{sid}\",version=\"{}\"}} {}\n",
                bot.version, s.total_fuel_consumed
            ));
            out.push_str(&format!(
                "hyperion_wasm_traps_total{{strategy_id=\"{sid}\",version=\"{}\"}} {}\n",
                bot.version, s.total_traps
            ));
            out.push_str(&format!(
                "hyperion_wasm_hot_swaps_total{{strategy_id=\"{sid}\"}} {}\n",
                s.hot_swap_count
            ));
        }
        out
    }

    pub fn list_strategies_json(&self) -> serde_json::Value {
        let items: Vec<serde_json::Value> = self
            .strategies
            .values()
            .map(|b| {
                serde_json::json!({
                    "strategy_id": b.strategy_id,
                    "strategy_name": b.strategy_name,
                    "account_id": b.account_id,
                    "version": b.version,
                    "state": format!("{:?}", b.lifecycle_state),
                    "memory_bytes": b.exports.memory.data_size(&b.store),
                    "open_orders": b.store.data().risk_gate.open_order_count(),
                    "profiler": b.profiler.summary(),
                })
            })
            .collect();
        serde_json::json!({ "strategies": items })
    }
}

/// Maps any Wasmtime native trap or custom `HostTrapSignal` to canonical `WasmTrapCode` (`wasm_quant.proto`).
pub fn classify_wasmtime_error(err: &wasmtime::Error) -> (WasmTrapCode, String) {
    if let Some(sig) = err.downcast_ref::<HostTrapSignal>() {
        return match sig {
            HostTrapSignal::MemoryLimitExceeded { .. }
            | HostTrapSignal::TableLimitExceeded { .. } => {
                (WasmTrapCode::MemoryLimitExceeded, sig.to_string())
            }
            HostTrapSignal::OutOfBoundsPointer(_) => {
                (WasmTrapCode::OutOfBoundsMemory, sig.to_string())
            }
            HostTrapSignal::OrderSpamLimitExceeded { .. } => {
                (WasmTrapCode::OrderSpamLimitExceeded, sig.to_string())
            }
        };
    }

    if let Some(trap) = err.downcast_ref::<Trap>() {
        let code = match *trap {
            Trap::OutOfFuel => WasmTrapCode::FuelExhausted,
            Trap::Interrupt => WasmTrapCode::EpochDeadlineExceeded,
            Trap::StackOverflow => WasmTrapCode::StackOverflow,
            Trap::MemoryOutOfBounds | Trap::TableOutOfBounds => WasmTrapCode::OutOfBoundsMemory,
            Trap::IntegerDivisionByZero => WasmTrapCode::IntegerDivideByZero,
            Trap::IntegerOverflow => WasmTrapCode::IntegerOverflow,
            Trap::BadConversionToInteger => WasmTrapCode::InvalidConversionToInt,
            Trap::UnreachableCodeReached => WasmTrapCode::UnreachableInstruction,
            Trap::IndirectCallToNull | Trap::BadSignature => {
                WasmTrapCode::IndirectCallTypeMismatch
            }
            _ => WasmTrapCode::Unspecified,
        };
        return (code, format!("wasm_trap={trap:?}: {err}"));
    }

    let msg = err.to_string();
    if msg.contains("memory") && (msg.contains("maximum") || msg.contains("limit")) {
        return (WasmTrapCode::MemoryLimitExceeded, msg);
    }
    (WasmTrapCode::Unspecified, msg)
}

#[allow(clippy::too_many_arguments)]
fn build_trap_proto(
    seq: u64,
    strategy_id: &str,
    account_id: u64,
    version: u32,
    symbol: &str,
    engine_seq: u64,
    hook: WasmCallbackHook,
    trap_code: WasmTrapCode,
    fuel_consumed: u64,
    elapsed_ns: u64,
    mem_bytes: u32,
    auto_canceled: bool,
    resulting_state: StrategyLifecycleState,
    detail: String,
) -> SandboxExecutionTrap {
    let ts_ns = now_epoch_ns();
    SandboxExecutionTrap {
        trap_event_id: format!("wasm-trap-{seq:08}"),
        strategy_id: strategy_id.to_string(),
        account_id,
        version,
        symbol: symbol.to_string(),
        engine_seq,
        callback_hook: hook as i32,
        trap_code: trap_code as i32,
        fuel_consumed_at_trap: fuel_consumed,
        elapsed_ns_at_trap: elapsed_ns,
        memory_bytes_at_trap: mem_bytes,
        wasm_program_counter_offset: 0,
        open_orders_auto_canceled: auto_canceled,
        resulting_state: resulting_state as i32,
        diagnostic_message: detail,
        trapped_ts_ns: ts_ns,
        trace: Some(TraceMetadata {
            trace_id: format!("{seq:032x}"),
            span_id: format!("{seq:016x}"),
            correlation_id: format!("ch15-trap-{seq}"),
            source_service: "quant-wasm-host".to_string(),
            ingest_ts_ns: ts_ns,
        }),
    }
}

#[inline]
pub fn now_epoch_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}
