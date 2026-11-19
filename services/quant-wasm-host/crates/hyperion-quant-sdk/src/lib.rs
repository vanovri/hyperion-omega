#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

//! # `hyperion-quant-sdk` — Zero-Allocation `#![no_std]` Guest Rust SDK
//!
//! Enables institutional quant researchers to write deterministic, sub-microsecond
//! algorithmic trading bots compiled to `wasm32-unknown-unknown` without `std` or `wasi`.
//!
//! ## Provided Primitives
//! - **128-bit Fixed-Point Quant Math (`fp`)**: Overflow-safe `1e8` price/quantity math,
//!   integer Newton-Raphson `isqrt_u128` for exact standard deviation, tick/lot rounding,
//!   and signed `ppm` (`1e6`) Z-score & order-book imbalance operators.
//! - **Rolling Ring Window (`RollingStatsFp<N>`)**: Allocation-free sliding statistics.
//! - **Binary State Cursor (`StateWriter` / `StateReader`)**: Zero-alloc little-endian
//!   codec for live atomic `.wasm` hot-swapping (`export_state` / `import_state`).
//! - **Shared Ring Context (`QuantContext`)**: Direct zero-copy interface to the egress
//!   ring buffer and host-imported `submit_limit_order` / `cancel_order` Pre-Trade Risk Gate.

pub use hyperion_quant_abi as abi;

use abi::*;

// =============================================================================
// 1. Overflow-Safe Deterministic Fixed-Point Arithmetic (`SCALE = 1e8`)
// =============================================================================

pub mod fp {
    use super::{FP_SCALE_I128, FP_SCALE_U128, PPM_SCALE_I128};

    /// Exact integer square root $\lfloor \sqrt{n} \rfloor$ via monotone Newton-Raphson on `u128`.
    /// Bit-identical across every CPU architecture (`0` floating-point instructions).
    #[inline]
    pub const fn isqrt_u128(n: u128) -> u128 {
        if n < 2 {
            return n;
        }
        let shift = (128 - n.leading_zeros() + 1) >> 1;
        let mut x = 1u128 << shift;
        loop {
            let nx = (x + n / x) >> 1;
            if nx >= x {
                return x;
            }
            x = nx;
        }
    }

    /// Multiplies two signed fixed-point numbers (`scale 1e8`), rounding toward zero.
    #[inline]
    pub const fn mul_fp(a_fp: i64, b_fp: i64) -> i64 {
        let prod = (a_fp as i128) * (b_fp as i128);
        (prod / FP_SCALE_I128) as i64
    }

    /// Computes quote notional `(price_fp * qty_fp) / 1e8` in `u128` without 64-bit overflow.
    #[inline]
    pub const fn notional_fp(price_fp: u64, qty_fp: u64) -> u128 {
        ((price_fp as u128) * (qty_fp as u128)) / FP_SCALE_U128
    }

    /// Computes signed parts-per-million `(num * 1_000_000) / den`, returning `0` if `den == 0`.
    #[inline]
    pub const fn ratio_ppm(num: i128, den: i128) -> i64 {
        if den == 0 {
            return 0;
        }
        let scaled = num.saturating_mul(PPM_SCALE_I128) / den;
        if scaled > i64::MAX as i128 {
            i64::MAX
        } else if scaled < i64::MIN as i128 {
            i64::MIN
        } else {
            scaled as i64
        }
    }

    /// Aligns a limit BUY price downward to an exact multiple of `tick_size_fp`.
    #[inline]
    pub const fn align_bid_tick(price_fp: u64, tick_size_fp: u64) -> u64 {
        if tick_size_fp == 0 {
            return price_fp;
        }
        (price_fp / tick_size_fp) * tick_size_fp
    }

    /// Aligns a limit SELL price upward to an exact multiple of `tick_size_fp`.
    #[inline]
    pub const fn align_ask_tick(price_fp: u64, tick_size_fp: u64) -> u64 {
        if tick_size_fp == 0 {
            return price_fp;
        }
        let rem = price_fp % tick_size_fp;
        if rem == 0 {
            price_fp
        } else {
            price_fp.saturating_add(tick_size_fp - rem)
        }
    }

    /// Aligns base quantity downward to an exact multiple of `lot_size_fp`.
    #[inline]
    pub const fn align_lot_qty(qty_fp: u64, lot_size_fp: u64) -> u64 {
        if lot_size_fp == 0 {
            return qty_fp;
        }
        (qty_fp / lot_size_fp) * lot_size_fp
    }

    /// Exponential moving average update with `alpha_ppm` in `[0, 1_000_000]`.
    #[inline]
    pub const fn ewma_fp(prev_fp: i64, sample_fp: i64, alpha_ppm: i64) -> i64 {
        let a = alpha_ppm as i128;
        let one_minus_a = PPM_SCALE_I128 - a;
        (((prev_fp as i128) * one_minus_a + (sample_fp as i128) * a) / PPM_SCALE_I128) as i64
    }
}

// =============================================================================
// 2. Allocation-Free Rolling Window Statistics (`RollingWindowFp<N>`)
// =============================================================================

#[derive(Clone, Copy)]
pub struct RollingWindowFp<const N: usize> {
    buf: [i64; N],
    head: usize,
    len: usize,
    sum: i128,
}

impl<const N: usize> RollingWindowFp<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0i64; N],
            head: 0,
            len: 0,
            sum: 0,
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.buf = [0i64; N];
        self.head = 0;
        self.len = 0;
        self.sum = 0;
    }

    #[inline]
    pub fn push_with_cap(&mut self, value_fp: i64, active_cap: usize) {
        let cap = if active_cap == 0 || active_cap > N {
            N
        } else {
            active_cap
        };
        if self.len < cap {
            let idx = (self.head + self.len) % cap;
            self.buf[idx] = value_fp;
            self.sum += value_fp as i128;
            self.len += 1;
        } else {
            let evicted = self.buf[self.head];
            self.buf[self.head] = value_fp;
            self.sum += (value_fp as i128) - (evicted as i128);
            self.head = (self.head + 1) % cap;
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Computes `(mean_fp, std_dev_fp)` using centered 128-bit integer deviations
    /// (zero catastrophic cancellation even at $100,000 BTC fixed-point scales).
    #[inline]
    pub fn mean_and_std_fp(&self, active_cap: usize) -> (i64, i64) {
        if self.len == 0 {
            return (0, 0);
        }
        let cap = if active_cap == 0 || active_cap > N {
            N
        } else {
            active_cap
        };
        let n = self.len as i128;
        let mean = (self.sum / n) as i64;
        if self.len < 2 {
            return (mean, 0);
        }
        let mut sq_acc: u128 = 0;
        let mut i = 0;
        while i < self.len {
            let idx = (self.head + i) % cap;
            let diff = (self.buf[idx] as i128) - (mean as i128);
            let abs_diff = diff.unsigned_abs();
            sq_acc = sq_acc.saturating_add(abs_diff.saturating_mul(abs_diff));
            i += 1;
        }
        let variance = sq_acc / (self.len as u128);
        let std_fp = fp::isqrt_u128(variance) as i64;
        (mean, std_fp)
    }

    /// Computes Z-score in signed parts-per-million (`1_000_000 = +1.0 sigma`).
    #[inline]
    pub fn zscore_ppm(&self, sample_fp: i64, active_cap: usize) -> i64 {
        let (mean_fp, std_fp) = self.mean_and_std_fp(active_cap);
        if std_fp <= 0 {
            return 0;
        }
        fp::ratio_ppm((sample_fp as i128) - (mean_fp as i128), std_fp as i128)
    }

    pub fn write_snapshot(&self, w: &mut StateWriter<'_>) -> bool {
        if !w.write_u32(self.head as u32)
            || !w.write_u32(self.len as u32)
            || !w.write_i128(self.sum)
        {
            return false;
        }
        let mut i = 0;
        while i < N {
            if !w.write_i64(self.buf[i]) {
                return false;
            }
            i += 1;
        }
        true
    }

    pub fn read_snapshot(&mut self, r: &mut StateReader<'_>) -> bool {
        let Some(head) = r.read_u32() else { return false };
        let Some(len) = r.read_u32() else { return false };
        let Some(sum) = r.read_i128() else { return false };
        if (head as usize) > N || (len as usize) > N {
            return false;
        }
        let mut i = 0;
        while i < N {
            let Some(v) = r.read_i64() else { return false };
            self.buf[i] = v;
            i += 1;
        }
        self.head = head as usize;
        self.len = len as usize;
        self.sum = sum;
        true
    }
}

impl<const N: usize> Default for RollingWindowFp<N> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// 3. Zero-Allocation Little-Endian State Snapshot Cursor (Live Hot-Swapping)
// =============================================================================

pub struct StateWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> StateWriter<'a> {
    #[inline]
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub const fn written(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn write_bytes(&mut self, src: &[u8]) -> bool {
        if self.pos.saturating_add(src.len()) > self.buf.len() {
            return false;
        }
        self.buf[self.pos..self.pos + src.len()].copy_from_slice(src);
        self.pos += src.len();
        true
    }

    #[inline]
    pub fn write_u8(&mut self, v: u8) -> bool {
        self.write_bytes(&[v])
    }

    #[inline]
    pub fn write_u32(&mut self, v: u32) -> bool {
        self.write_bytes(&v.to_le_bytes())
    }

    #[inline]
    pub fn write_u64(&mut self, v: u64) -> bool {
        self.write_bytes(&v.to_le_bytes())
    }

    #[inline]
    pub fn write_i64(&mut self, v: i64) -> bool {
        self.write_bytes(&v.to_le_bytes())
    }

    #[inline]
    pub fn write_i128(&mut self, v: i128) -> bool {
        self.write_bytes(&v.to_le_bytes())
    }
}

pub struct StateReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    #[inline]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn read_exact<const K: usize>(&mut self) -> Option<[u8; K]> {
        if self.pos.saturating_add(K) > self.buf.len() {
            return None;
        }
        let mut out = [0u8; K];
        out.copy_from_slice(&self.buf[self.pos..self.pos + K]);
        self.pos += K;
        Some(out)
    }

    #[inline]
    pub fn read_u8(&mut self) -> Option<u8> {
        self.read_exact::<1>().map(|b| b[0])
    }

    #[inline]
    pub fn read_u32(&mut self) -> Option<u32> {
        self.read_exact::<4>().map(u32::from_le_bytes)
    }

    #[inline]
    pub fn read_u64(&mut self) -> Option<u64> {
        self.read_exact::<8>().map(u64::from_le_bytes)
    }

    #[inline]
    pub fn read_i64(&mut self) -> Option<i64> {
        self.read_exact::<8>().map(i64::from_le_bytes)
    }

    #[inline]
    pub fn read_i128(&mut self) -> Option<i128> {
        self.read_exact::<16>().map(i128::from_le_bytes)
    }
}

// =============================================================================
// 4. Typed Views Over Shared Ring-Buffer Events & Host-Import Gate
// =============================================================================

#[derive(Clone, Copy, Debug)]
pub struct BookL2View {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub instrument_id: u32,
    pub bid_levels: u8,
    pub ask_levels: u8,
    pub best_bid_fp: u64,
    pub best_ask_fp: u64,
    pub mid_price_fp: u64,
    pub spread_fp: u64,
    pub ofi_fp: i64,
    pub book_imbalance_top5_ppm: i64,
    pub fair_ref_price_fp: u64,
    pub bids_price_fp: [u64; L2_BOOK_LEVELS],
    pub bids_qty_fp: [u64; L2_BOOK_LEVELS],
    pub asks_price_fp: [u64; L2_BOOK_LEVELS],
    pub asks_qty_fp: [u64; L2_BOOK_LEVELS],
}

#[derive(Clone, Copy, Debug)]
pub struct TradeTickView {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub instrument_id: u32,
    pub aggressor_side: u8,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub vwap_fp: u64,
    pub ofi_delta_fp: i64,
    pub fill_id: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct OrderFillView {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub instrument_id: u32,
    pub side: u8,
    pub order_status: u8,
    pub order_id: u64,
    pub client_order_id: u64,
    pub fill_price_fp: u64,
    pub fill_qty_fp: u64,
    pub remaining_qty_fp: u64,
    pub net_inventory_fp: i64,
    pub external_delta_fp: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct LimitOrderRequest {
    pub client_order_id: u64,
    pub instrument_id: u32,
    pub side: u8,
    pub order_type: u8,
    pub stp_mode: u8,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub max_slippage_bps: u32,
    pub client_ts_ns: u64,
    pub strategy_tag: u64,
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "hyperion")]
extern "C" {
    /// Synchronously validates an order slot in the egress ring via Host Pre-Trade Risk Gate.
    /// Returns positive `assigned_order_id > 0` on acceptance, or negative `-risk_err_code` on rejection.
    /// Trips `TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED` if called > 16 times in a single tick!
    pub fn submit_limit_order(egress_slot_ptr: u32) -> i64;

    /// Synchronously validates and enqueues cancellation of an open resting order.
    /// Returns `0` (`RISK_OK`) on acceptance, or negative `-risk_err_code` on rejection.
    pub fn cancel_order(order_id: u64, client_order_id: u64) -> i32;
}

/// Host-side test stub for `hyperion::submit_limit_order`.
///
/// # Safety
/// `_egress_slot_ptr` must be an 8-byte aligned linear-memory pointer to an initialized `EgressOrderSlot64`.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub unsafe fn submit_limit_order(_egress_slot_ptr: u32) -> i64 {
    1
}

/// Host-side test stub for `hyperion::cancel_order`.
///
/// # Safety
/// Must be called only inside an active Hyperion quant callback context.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub unsafe fn cancel_order(_order_id: u64, _client_order_id: u64) -> i32 {
    0
}

/// Execution context passed to every `QuantStrategy` callback, writing order intents
/// directly into the `4 KiB` shared egress ring buffer before ringing the host Risk Gate.
pub struct QuantContext<'a> {
    shm: &'a mut SharedMemoryRegion,
}

impl<'a> QuantContext<'a> {
    #[inline]
    pub fn new(shm: &'a mut SharedMemoryRegion) -> Self {
        Self { shm }
    }

    /// Writes `LimitOrderRequest` into the next `64B` egress ring slot and invokes
    /// host-import `submit_limit_order`, returning `Ok(order_id)` or `Err(risk_code)`.
    #[inline]
    pub fn submit_limit_order(&mut self, req: LimitOrderRequest) -> Result<u64, i32> {
        let seq = self.shm.control.egress_write_seq;
        let idx = (seq as usize) & RING_MASK;
        let slot = &mut self.shm.egress_ring[idx];
        *slot = EgressOrderSlot64 {
            client_order_id: req.client_order_id,
            target_order_id: 0,
            price_fp: req.price_fp,
            qty_fp: req.qty_fp,
            client_ts_ns: req.client_ts_ns,
            strategy_tag: req.strategy_tag,
            instrument_id: req.instrument_id,
            max_slippage_bps: req.max_slippage_bps,
            cmd_kind: CMD_KIND_SUBMIT_LIMIT,
            side: req.side,
            order_type: req.order_type,
            stp_mode: req.stp_mode,
            reject_code: RISK_OK,
        };
        self.shm.control.egress_write_seq = seq.wrapping_add(1);
        let slot_ptr = (slot as *const EgressOrderSlot64) as usize as u32;
        // SAFETY: `slot_ptr` points to a 64-byte aligned `EgressOrderSlot64` inside static guest linear memory.
        let rc = unsafe { submit_limit_order(slot_ptr) };
        if rc > 0 {
            Ok(rc as u64)
        } else {
            let err = (-rc) as i32;
            self.shm.egress_ring[idx].reject_code = err;
            Err(err)
        }
    }

    /// Records a cancel command in the egress ring and invokes host-import `cancel_order`.
    #[inline]
    pub fn cancel_order(&mut self, order_id: u64, client_order_id: u64, ts_ns: u64) -> Result<(), i32> {
        let seq = self.shm.control.egress_write_seq;
        let idx = (seq as usize) & RING_MASK;
        self.shm.egress_ring[idx] = EgressOrderSlot64 {
            client_order_id,
            target_order_id: order_id,
            price_fp: 0,
            qty_fp: 0,
            client_ts_ns: ts_ns,
            strategy_tag: 0,
            instrument_id: self.shm.control.instrument_id,
            max_slippage_bps: 0,
            cmd_kind: CMD_KIND_CANCEL_ORDER,
            side: SIDE_UNSPECIFIED,
            order_type: ORDER_TYPE_UNSPECIFIED,
            stp_mode: STP_NONE,
            reject_code: RISK_OK,
        };
        self.shm.control.egress_write_seq = seq.wrapping_add(1);
        // SAFETY: Scalar u64 arguments cross Wasm FFI directly in registers.
        let rc = unsafe { cancel_order(order_id, client_order_id) };
        if rc == 0 {
            Ok(())
        } else {
            let err = -rc;
            self.shm.egress_ring[idx].reject_code = err;
            Err(err)
        }
    }
}

/// Core trait implemented by institutional quant strategies in `#![no_std]` Rust.
pub trait QuantStrategy {
    const STRATEGY_KIND: u32;

    fn new() -> Self;
    fn on_init(&mut self, cfg: &StrategyInitBlock, ctx: &mut QuantContext<'_>) -> i32;
    fn on_book_l2_tick(&mut self, book: &BookL2View, ctx: &mut QuantContext<'_>) -> i32;
    fn on_trade_tick(&mut self, trade: &TradeTickView, ctx: &mut QuantContext<'_>) -> i32;
    fn on_order_fill(&mut self, fill: &OrderFillView, ctx: &mut QuantContext<'_>) -> i32;
    fn export_state(&self, writer: &mut StateWriter<'_>) -> (i64, u64, u64, bool);
    fn import_state(&mut self, reader: &mut StateReader<'_>, header: &StateSnapshotRegion) -> bool;
}

// =============================================================================
// 5. Runtime Dispatcher Helpers Used by `export_quant_strategy!` Macro
// =============================================================================

#[inline]
pub fn decode_book_l2(shm: &SharedMemoryRegion, slot_idx: u32) -> Option<BookL2View> {
    let idx = (slot_idx as usize) & RING_MASK;
    let slot = &shm.ingress_ring[idx];
    if slot.event_kind != EVENT_KIND_BOOK_L2 {
        return None;
    }
    let ext = &shm.l2_ext_ring[idx];
    Some(BookL2View {
        engine_seq: slot.engine_seq,
        ts_ns: slot.ts_ns,
        instrument_id: slot.instrument_id,
        bid_levels: slot.aux_u8_a,
        ask_levels: slot.aux_u8_b,
        best_bid_fp: slot.primary_price_fp,
        best_ask_fp: slot.secondary_price_fp,
        mid_price_fp: slot.qty_or_mid_fp,
        spread_fp: ext.ext_b as u64,
        ofi_fp: slot.signed_metric_a_fp,
        book_imbalance_top5_ppm: slot.signed_metric_b,
        fair_ref_price_fp: ext.ext_a as u64,
        bids_price_fp: ext.bids_price_fp,
        bids_qty_fp: ext.bids_qty_fp,
        asks_price_fp: ext.asks_price_fp,
        asks_qty_fp: ext.asks_qty_fp,
    })
}

#[inline]
pub fn decode_trade_tick(shm: &SharedMemoryRegion, slot_idx: u32) -> Option<TradeTickView> {
    let idx = (slot_idx as usize) & RING_MASK;
    let slot = &shm.ingress_ring[idx];
    if slot.event_kind != EVENT_KIND_TRADE {
        return None;
    }
    Some(TradeTickView {
        engine_seq: slot.engine_seq,
        ts_ns: slot.ts_ns,
        instrument_id: slot.instrument_id,
        aggressor_side: slot.side,
        price_fp: slot.primary_price_fp,
        vwap_fp: slot.secondary_price_fp,
        qty_fp: slot.qty_or_mid_fp,
        ofi_delta_fp: slot.signed_metric_a_fp,
        fill_id: slot.signed_metric_b as u64,
    })
}

#[inline]
pub fn decode_order_fill(shm: &SharedMemoryRegion, slot_idx: u32) -> Option<OrderFillView> {
    let idx = (slot_idx as usize) & RING_MASK;
    let slot = &shm.ingress_ring[idx];
    if slot.event_kind != EVENT_KIND_ORDER_FILL {
        return None;
    }
    let ext = &shm.l2_ext_ring[idx];
    Some(OrderFillView {
        engine_seq: slot.engine_seq,
        ts_ns: slot.ts_ns,
        instrument_id: slot.instrument_id,
        side: slot.side,
        order_status: slot.aux_u8_a,
        fill_price_fp: slot.primary_price_fp,
        remaining_qty_fp: slot.secondary_price_fp,
        fill_qty_fp: slot.qty_or_mid_fp,
        net_inventory_fp: slot.signed_metric_a_fp,
        external_delta_fp: slot.signed_metric_b,
        order_id: ext.ext_c as u64,
        client_order_id: ext.ext_d as u64,
    })
}

/// Generates all C-ABI exports (`on_init`, `on_book_l2_tick`, `on_trade_tick`, `on_order_fill`,
/// `hyperion_abi_shm_ptr`, `hyperion_export_state`, `hyperion_import_state`) and a `#![no_std]`
/// panic trap handler for single-threaded deterministic `wasm32-unknown-unknown` sandboxes.
#[macro_export]
macro_rules! export_quant_strategy {
    ($strategy_ty:ty, $const_init:expr) => {
        struct __HyperionGuestCell<T>(::core::cell::UnsafeCell<T>);
        // SAFETY: Wasm sandbox instances are strictly single-threaded (`wasm_threads = false`).
        unsafe impl<T> Sync for __HyperionGuestCell<T> {}

        static __HYPERION_SHM: __HyperionGuestCell<$crate::abi::SharedMemoryRegion> =
            __HyperionGuestCell(::core::cell::UnsafeCell::new(
                $crate::abi::SharedMemoryRegion::new(<$strategy_ty as $crate::QuantStrategy>::STRATEGY_KIND),
            ));

        static __HYPERION_BOT: __HyperionGuestCell<$strategy_ty> =
            __HyperionGuestCell(::core::cell::UnsafeCell::new($const_init));

        #[no_mangle]
        pub extern "C" fn hyperion_abi_shm_ptr() -> u32 {
            __HYPERION_SHM.0.get() as usize as u32
        }

        #[no_mangle]
        pub extern "C" fn on_init(_slot_idx: u32) -> i32 {
            // SAFETY: Single-threaded Wasm guest execution without reentrancy.
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &mut *__HYPERION_BOT.0.get() };
            let cfg = shm.init_block;
            shm.control.account_id = cfg.account_id;
            shm.control.instrument_id = cfg.instrument_id;
            shm.control.strategy_version = cfg.strategy_version;
            shm.control.ingress_read_seq = shm.control.ingress_write_seq;
            let mut ctx = $crate::QuantContext::new(shm);
            <$strategy_ty as $crate::QuantStrategy>::on_init(bot, &cfg, &mut ctx)
        }

        #[no_mangle]
        pub extern "C" fn on_book_l2_tick(slot_idx: u32) -> i32 {
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &mut *__HYPERION_BOT.0.get() };
            let Some(book) = $crate::decode_book_l2(shm, slot_idx) else {
                return $crate::abi::CALLBACK_ERR_BAD_SLOT;
            };
            shm.control.ingress_read_seq = shm.control.ingress_read_seq.wrapping_add(1);
            let mut ctx = $crate::QuantContext::new(shm);
            <$strategy_ty as $crate::QuantStrategy>::on_book_l2_tick(bot, &book, &mut ctx)
        }

        #[no_mangle]
        pub extern "C" fn on_trade_tick(slot_idx: u32) -> i32 {
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &mut *__HYPERION_BOT.0.get() };
            let Some(trade) = $crate::decode_trade_tick(shm, slot_idx) else {
                return $crate::abi::CALLBACK_ERR_BAD_SLOT;
            };
            shm.control.ingress_read_seq = shm.control.ingress_read_seq.wrapping_add(1);
            let mut ctx = $crate::QuantContext::new(shm);
            <$strategy_ty as $crate::QuantStrategy>::on_trade_tick(bot, &trade, &mut ctx)
        }

        #[no_mangle]
        pub extern "C" fn on_order_fill(slot_idx: u32) -> i32 {
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &mut *__HYPERION_BOT.0.get() };
            let Some(fill) = $crate::decode_order_fill(shm, slot_idx) else {
                return $crate::abi::CALLBACK_ERR_BAD_SLOT;
            };
            shm.control.ingress_read_seq = shm.control.ingress_read_seq.wrapping_add(1);
            let mut ctx = $crate::QuantContext::new(shm);
            <$strategy_ty as $crate::QuantStrategy>::on_order_fill(bot, &fill, &mut ctx)
        }

        #[no_mangle]
        pub extern "C" fn hyperion_export_state(engine_seq: u64) -> i32 {
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &*__HYPERION_BOT.0.get() };
            let (inv_fp, active_oid, next_cid, ok, written) = {
                let mut writer = $crate::StateWriter::new(&mut shm.snapshot.payload);
                let (inv_fp, active_oid, next_cid, ok) =
                    <$strategy_ty as $crate::QuantStrategy>::export_state(bot, &mut writer);
                (inv_fp, active_oid, next_cid, ok, writer.written())
            };
            if !ok {
                return $crate::abi::CALLBACK_ERR_SNAPSHOT_CORRUPT;
            }
            let crc = $crate::abi::crc32c_compute(&shm.snapshot.payload[..written]);
            shm.snapshot.magic = $crate::abi::SNAPSHOT_MAGIC;
            shm.snapshot.version = $crate::abi::SNAPSHOT_VERSION;
            shm.snapshot.strategy_kind = <$strategy_ty as $crate::QuantStrategy>::STRATEGY_KIND;
            shm.snapshot.payload_len = written as u32;
            shm.snapshot.crc32c = crc;
            shm.snapshot.exported_engine_seq = engine_seq;
            shm.snapshot.net_inventory_fp = inv_fp;
            shm.snapshot.active_order_id = active_oid;
            shm.snapshot.next_client_order_id = next_cid;
            written as i32
        }

        #[no_mangle]
        pub extern "C" fn hyperion_import_state() -> i32 {
            let shm = unsafe { &mut *__HYPERION_SHM.0.get() };
            let bot = unsafe { &mut *__HYPERION_BOT.0.get() };
            if shm.snapshot.magic != $crate::abi::SNAPSHOT_MAGIC
                || shm.snapshot.version != $crate::abi::SNAPSHOT_VERSION
                || shm.snapshot.strategy_kind != <$strategy_ty as $crate::QuantStrategy>::STRATEGY_KIND
            {
                return $crate::abi::CALLBACK_ERR_SNAPSHOT_CORRUPT;
            }
            let len = shm.snapshot.payload_len as usize;
            if len > $crate::abi::SNAPSHOT_MAX_PAYLOAD {
                return $crate::abi::CALLBACK_ERR_SNAPSHOT_CORRUPT;
            }
            let expected_crc = $crate::abi::crc32c_compute(&shm.snapshot.payload[..len]);
            if expected_crc != shm.snapshot.crc32c {
                return $crate::abi::CALLBACK_ERR_SNAPSHOT_CORRUPT;
            }
            let header_copy = shm.snapshot;
            let mut reader = $crate::StateReader::new(&header_copy.payload[..len]);
            if <$strategy_ty as $crate::QuantStrategy>::import_state(bot, &mut reader, &header_copy) {
                $crate::abi::CALLBACK_OK
            } else {
                $crate::abi::CALLBACK_ERR_SNAPSHOT_CORRUPT
            }
        }

        #[cfg(target_arch = "wasm32")]
        #[panic_handler]
        fn __hyperion_wasm_panic(_info: &::core::panic::PanicInfo) -> ! {
            ::core::arch::wasm32::unreachable()
        }
    };
}
