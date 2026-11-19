#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

//! # Hyperion Quant Sandbox — Deterministic Host-Guest Shared Memory ABI (`v1`)
//!
//! Strictly `#![no_std]`, zero-dependency, fixed-width C-ABI definitions shared across
//! `x86_64`/`aarch64` Host (`quant-wasm-host`) and `wasm32-unknown-unknown` Guests.
//!
//! ## Architectural Guarantees
//! 1. **Floating-Point Ban (`00-hyperion-contract.md` §5.1)**: Zero `f32`/`f64` fields.
//!    All prices, quantities, notionals, fees, spreads, OFI, and inventory positions use
//!    integer fixed-point scaled by `FP_SCALE = 100_000_000` (`1e8`). Ratios and Z-scores
//!    use signed parts-per-million (`PPM_SCALE = 1_000_000`) or basis points (`BPS_SCALE = 10_000`).
//! 2. **Pointer-Width Independence**: No `usize`, `isize`, `bool`, or Rust `enum` crosses the ABI;
//!    every struct size and field offset is verified at compile time to be bit-identical on
//!    32-bit WebAssembly and 64-bit native hosts.
//! 3. **Cache-Line Alignment (`64B`)**: Control header, `4 KiB` ingress ring (`64 x 64B`),
//!    `4 KiB` egress command ring (`64 x 64B`), and 5-level L2 microstructure extension slots
//!    are aligned to 64-byte CPU cache lines (`01-hyperion-omega.md` §4.5).

use core::mem::size_of;

/// Canonical fixed-point scale for all prices, quantities, notionals, fees, and inventory (`10^8`).
pub const FP_SCALE: i64 = 100_000_000;
pub const FP_SCALE_U64: u64 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const FP_SCALE_U128: u128 = 100_000_000;

/// Parts-per-million scale (`10^6`) for ratios, Z-scores, and order-book imbalances.
pub const PPM_SCALE: i64 = 1_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;

/// Basis-points scale (`10^4`, `1 bps = 0.01%`).
pub const BPS_SCALE: u64 = 10_000;

/// Magic header `b"HYPWASM1"` in little-endian (`0x314D_5341_5750_5948`).
pub const ABI_MAGIC: u64 = 0x314D_5341_5750_5948;
pub const ABI_VERSION: u32 = 1;

/// Magic header `b"WASMSNP1"` in little-endian (`0x3150_4E53_4D53_4157`) for live hot-swap snapshots.
pub const SNAPSHOT_MAGIC: u64 = 0x3150_4E53_4D53_4157;
pub const SNAPSHOT_VERSION: u32 = 1;

/// Ring-buffer capacities (`64` slots x `64` bytes = exact `4,096` bytes = `4 KiB` each).
pub const RING_CAPACITY: usize = 64;
pub const RING_MASK: usize = RING_CAPACITY - 1;

/// Number of order-book depth levels passed in zero-copy microstructure ticks.
pub const L2_BOOK_LEVELS: usize = 5;

/// Maximum serialized strategy state payload bytes during atomic hot-swap handoff.
pub const SNAPSHOT_MAX_PAYLOAD: usize = 4096;

/// Canonical resource ceilings mandated by `01-hyperion-omega.md` §4.
pub const WASM_PAGE_BYTES: usize = 65_536;
pub const INITIAL_LINEAR_MEMORY_PAGES: u32 = 16; // 1 MiB
pub const MAX_LINEAR_MEMORY_PAGES: u32 = 256; // 16 MiB
pub const MAX_LINEAR_MEMORY_BYTES: usize = 16 * 1024 * 1024; // 16,777,216 B
pub const MAX_WASM_STACK_BYTES: usize = 512 * 1024; // 524,288 B
pub const MAX_TABLE_ELEMENTS: usize = 4_096;
pub const DEFAULT_FUEL_PER_TICK: u64 = 50_000;
pub const MAX_INSTITUTIONAL_FUEL_PER_TICK: u64 = 200_000;
pub const INIT_AND_SNAPSHOT_FUEL: u64 = 500_000;
pub const MAX_ORDERS_PER_TICK: u32 = 16;

// =============================================================================
// Canonical Wire Discriminants (Aligned with `common.proto` & `orderbook.proto`)
// =============================================================================

pub const SIDE_UNSPECIFIED: u8 = 0;
pub const SIDE_BUY: u8 = 1;
pub const SIDE_SELL: u8 = 2;

pub const ORDER_TYPE_UNSPECIFIED: u8 = 0;
pub const ORDER_TYPE_LIMIT: u8 = 1;
pub const ORDER_TYPE_MARKET: u8 = 2;
pub const ORDER_TYPE_IOC: u8 = 3;
pub const ORDER_TYPE_FOK: u8 = 4;
pub const ORDER_TYPE_POST_ONLY: u8 = 5;

pub const STP_NONE: u8 = 1;
pub const STP_CANCEL_RESTING: u8 = 2;
pub const STP_CANCEL_AGGRESSING: u8 = 3;
pub const STP_CANCEL_BOTH: u8 = 4;

pub const ORDER_STATUS_ACCEPTED: u8 = 1;
pub const ORDER_STATUS_PARTIALLY_FILLED: u8 = 2;
pub const ORDER_STATUS_FILLED: u8 = 3;
pub const ORDER_STATUS_CANCELED: u8 = 4;
pub const ORDER_STATUS_REJECTED: u8 = 6;

pub const EVENT_KIND_NONE: u8 = 0;
pub const EVENT_KIND_INIT: u8 = 1;
pub const EVENT_KIND_BOOK_L2: u8 = 2;
pub const EVENT_KIND_TRADE: u8 = 3;
pub const EVENT_KIND_ORDER_FILL: u8 = 4;

pub const CMD_KIND_NONE: u8 = 0;
pub const CMD_KIND_SUBMIT_LIMIT: u8 = 1;
pub const CMD_KIND_CANCEL_ORDER: u8 = 2;

// Strategy identity discriminants for safe state-snapshot schema verification on hot-swap.
pub const STRATEGY_KIND_GENERIC: u32 = 0;
pub const STRATEGY_KIND_STAT_ARB_ZSCORE: u32 = 1;
pub const STRATEGY_KIND_OBI_SNIPER: u32 = 2;
pub const STRATEGY_KIND_DELTA_HEDGER: u32 = 3;

// Deterministic Pre-Trade Risk Gate rejection codes (returned as negative `-code` from host imports).
pub const RISK_OK: i32 = 0;
pub const RISK_ERR_INVALID_PRICE_OR_QTY: i32 = 1;
pub const RISK_ERR_TICK_SIZE_ALIGNMENT: i32 = 2;
pub const RISK_ERR_LOT_SIZE_ALIGNMENT: i32 = 3;
pub const RISK_ERR_FAT_FINGER_COLLAR: i32 = 4;
pub const RISK_ERR_MAX_ORDER_QTY: i32 = 5;
pub const RISK_ERR_MAX_ORDER_NOTIONAL: i32 = 6;
pub const RISK_ERR_MAX_OPEN_ORDERS: i32 = 7;
pub const RISK_ERR_MAX_POSITION_DELTA: i32 = 8;
pub const RISK_ERR_MARKET_ORDER_FORBIDDEN: i32 = 9;
pub const RISK_ERR_INVALID_SIDE_OR_STP: i32 = 10;
pub const RISK_ERR_ACCOUNT_OR_REGIME_HALTED: i32 = 11;
pub const RISK_ERR_INSTRUMENT_MISMATCH: i32 = 12;
pub const RISK_ERR_UNKNOWN_OR_FOREIGN_ORDER: i32 = 13;
pub const RISK_ERR_DUPLICATE_CLIENT_ORDER_ID: i32 = 14;
pub const RISK_ERR_POST_ONLY_WOULD_CROSS: i32 = 15;

// Host/Guest callback return status codes.
pub const CALLBACK_OK: i32 = 0;
pub const CALLBACK_ERR_BAD_SLOT: i32 = -1;
pub const CALLBACK_ERR_BAD_CONFIG: i32 = -2;
pub const CALLBACK_ERR_SNAPSHOT_CORRUPT: i32 = -3;

// =============================================================================
// Cache-Line Aligned Shared Memory Structures (`#[repr(C, align(64))]`)
// =============================================================================

/// 64-byte control header at offset 0 of `SharedMemoryRegion`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct AbiControlHeader {
    pub magic: u64,
    pub abi_version: u32,
    pub strategy_kind: u32,
    pub account_id: u64,
    pub instrument_id: u32,
    pub strategy_version: u32,
    /// Host -> Guest monotonic ingress write sequence.
    pub ingress_write_seq: u64,
    /// Guest monotonic ingress read sequence.
    pub ingress_read_seq: u64,
    /// Guest -> Host monotonic egress write sequence.
    pub egress_write_seq: u64,
    /// Host monotonic egress read sequence.
    pub egress_read_seq: u64,
}

impl AbiControlHeader {
    pub const fn new(strategy_kind: u32) -> Self {
        Self {
            magic: ABI_MAGIC,
            abi_version: ABI_VERSION,
            strategy_kind,
            account_id: 0,
            instrument_id: 0,
            strategy_version: 1,
            ingress_write_seq: 0,
            ingress_read_seq: 0,
            egress_write_seq: 0,
            egress_read_seq: 0,
        }
    }
}

/// Exact 64-byte Ingress Ring Slot (`64 slots * 64B = 4,096B = 4 KiB` ingress ring).
///
/// Carries `on_init` summary, `on_book_l2_tick` top-of-book & microstructure scalars
/// (with 5-level depth stored in companion `L2DepthExt[slot_idx]`), `on_trade_tick`,
/// or `on_order_fill` without a single heap allocation.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct IngressSlot64 {
    /// Monotonic matching-engine sequence ID (`engine_seq`).
    pub engine_seq: u64,
    /// Deterministic nanosecond timestamp (`ts_ns`) injected by Host.
    pub ts_ns: u64,
    /// Primary fixed-point price (`best_bid_fp` for L2, `price_fp` for Trade, `fill_price_fp` for Fill, `tick_size_fp` for Init).
    pub primary_price_fp: u64,
    /// Secondary fixed-point price/value (`best_ask_fp` for L2, `vwap_fp` for Trade, `remaining_qty_fp` for Fill, `lot_size_fp` for Init).
    pub secondary_price_fp: u64,
    /// Primary quantity/size (`mid_price_fp` for L2, `qty_fp` for Trade, `fill_qty_fp` for Fill, `order_qty_fp` for Init).
    pub qty_or_mid_fp: u64,
    /// Signed fixed-point signal (`ofi_fp` for L2/Trade, `net_inventory_fp` for Fill, `max_position_fp` for Init).
    pub signed_metric_a_fp: i64,
    /// Secondary signed fixed-point/ppm metric (`book_imbalance_top5_ppm` for L2, `external_delta_fp` for Fill, `aux_id` for Trade/Init).
    pub signed_metric_b: i64,
    /// Instrument identifier (`1 = BTC-USDT`, `2 = ETH-USDT`, etc.).
    pub instrument_id: u32,
    /// Event discriminant (`EVENT_KIND_*`).
    pub event_kind: u8,
    /// Side or aggressor flag (`SIDE_BUY` / `SIDE_SELL`).
    pub side: u8,
    /// Order lifecycle status or L2 bid level count.
    pub aux_u8_a: u8,
    /// L2 ask level count or flags.
    pub aux_u8_b: u8,
}

impl IngressSlot64 {
    pub const fn zeroed() -> Self {
        Self {
            engine_seq: 0,
            ts_ns: 0,
            primary_price_fp: 0,
            secondary_price_fp: 0,
            qty_or_mid_fp: 0,
            signed_metric_a_fp: 0,
            signed_metric_b: 0,
            instrument_id: 0,
            event_kind: EVENT_KIND_NONE,
            side: SIDE_UNSPECIFIED,
            aux_u8_a: 0,
            aux_u8_b: 0,
        }
    }
}

/// Companion 192-byte (3 cache-line) 5-Level Order Book Depth & Strategy Init Parameter block.
/// Indexed by `slot_idx & RING_MASK` for `on_book_l2_tick` and `on_init`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct L2DepthAndConfigExt {
    pub bids_price_fp: [u64; L2_BOOK_LEVELS], // 40B
    pub bids_qty_fp: [u64; L2_BOOK_LEVELS],   // 80B
    pub asks_price_fp: [u64; L2_BOOK_LEVELS], // 120B
    pub asks_qty_fp: [u64; L2_BOOK_LEVELS],   // 160B
    /// Fair-value reference leg price (`1e8`) for stat-arb spread, or `zscore_entry_ppm` on init.
    pub ext_a: i64, // 168B
    /// `spread_fp` on L2 tick, or `zscore_exit_ppm` on init.
    pub ext_b: i64, // 176B
    /// `order_id` on Fill, or `obi_threshold_ppm` / `ofi_weight_ppm` packed on init.
    pub ext_c: i64, // 184B
    /// `client_order_id` on Fill, or `hedge_band_fp` / `cooldown_ns` on init.
    pub ext_d: i64, // 192B
}

impl L2DepthAndConfigExt {
    pub const fn zeroed() -> Self {
        Self {
            bids_price_fp: [0; L2_BOOK_LEVELS],
            bids_qty_fp: [0; L2_BOOK_LEVELS],
            asks_price_fp: [0; L2_BOOK_LEVELS],
            asks_qty_fp: [0; L2_BOOK_LEVELS],
            ext_a: 0,
            ext_b: 0,
            ext_c: 0,
            ext_d: 0,
        }
    }
}

/// Dedicated rich `on_init` configuration block (`128 bytes` = 2 cache lines) placed in `SharedMemoryRegion`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct StrategyInitBlock {
    pub account_id: u64,
    pub instrument_id: u32,
    pub strategy_version: u32,
    pub tick_size_fp: u64,
    pub lot_size_fp: u64,
    pub max_position_fp: i64,
    pub max_order_qty_fp: u64,
    pub order_qty_fp: u64,
    pub zscore_entry_ppm: i64,
    pub zscore_exit_ppm: i64,
    pub ofi_weight_ppm: i64,
    pub obi_threshold_ppm: i64,
    pub hedge_band_fp: i64,
    pub target_delta_fp: i64,
    pub cooldown_ns: u64,
    pub max_spread_fp: u64,
    pub zscore_window: u32,
    pub stp_mode: u8,
    pub _pad: [u8; 3],
}

impl StrategyInitBlock {
    pub const fn default_btc() -> Self {
        Self {
            account_id: 10_001,
            instrument_id: 1,
            strategy_version: 1,
            tick_size_fp: 1_000_000,          // 0.01 USDT
            lot_size_fp: 1_000,               // 0.00001 BTC
            max_position_fp: 500_000_000,     // 5.00 BTC
            max_order_qty_fp: 100_000_000,    // 1.00 BTC
            order_qty_fp: 10_000_000,         // 0.10 BTC
            zscore_entry_ppm: 1_800_000,      // 1.80 sigma
            zscore_exit_ppm: 350_000,         // 0.35 sigma
            ofi_weight_ppm: 300_000,          // 0.30 OFI blend
            obi_threshold_ppm: 380_000,       // 0.38 weighted 5-level OBI
            hedge_band_fp: 5_000_000,         // 0.05 BTC deadband
            target_delta_fp: 0,               // delta-neutral
            cooldown_ns: 1_000_000,           // 1 ms anti-whipsaw cooldown
            max_spread_fp: 500_000_000,       // 5.00 USDT max snipe spread
            zscore_window: 24,
            stp_mode: STP_CANCEL_BOTH,
            _pad: [0; 3],
        }
    }
}

/// Exact 64-byte Egress Order Command Slot (`64 slots * 64B = 4,096B = 4 KiB` egress ring),
/// also used directly as the pointer argument to host-import `submit_limit_order(cmd_ptr: u32) -> i64`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct EgressOrderSlot64 {
    pub client_order_id: u64,
    pub target_order_id: u64,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub client_ts_ns: u64,
    pub strategy_tag: u64,
    pub instrument_id: u32,
    pub max_slippage_bps: u32,
    pub cmd_kind: u8,
    pub side: u8,
    pub order_type: u8,
    pub stp_mode: u8,
    pub reject_code: i32,
}

impl EgressOrderSlot64 {
    pub const fn zeroed() -> Self {
        Self {
            client_order_id: 0,
            target_order_id: 0,
            price_fp: 0,
            qty_fp: 0,
            client_ts_ns: 0,
            strategy_tag: 0,
            instrument_id: 0,
            max_slippage_bps: 0,
            cmd_kind: CMD_KIND_NONE,
            side: SIDE_UNSPECIFIED,
            order_type: ORDER_TYPE_UNSPECIFIED,
            stp_mode: STP_NONE,
            reject_code: RISK_OK,
        }
    }
}

/// 64-byte header + 4,096-byte payload for live atomic state-snapshot handoff (`v_old -> v_new`).
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct StateSnapshotRegion {
    pub magic: u64,
    pub version: u32,
    pub strategy_kind: u32,
    pub payload_len: u32,
    pub crc32c: u32,
    pub exported_engine_seq: u64,
    pub net_inventory_fp: i64,
    pub active_order_id: u64,
    pub next_client_order_id: u64,
    pub _reserved: u64,
    pub payload: [u8; SNAPSHOT_MAX_PAYLOAD],
}

impl StateSnapshotRegion {
    pub const fn zeroed() -> Self {
        Self {
            magic: SNAPSHOT_MAGIC,
            version: SNAPSHOT_VERSION,
            strategy_kind: STRATEGY_KIND_GENERIC,
            payload_len: 0,
            crc32c: 0,
            exported_engine_seq: 0,
            net_inventory_fp: 0,
            active_order_id: 0,
            next_client_order_id: 1,
            _reserved: 0,
            payload: [0u8; SNAPSHOT_MAX_PAYLOAD],
        }
    }
}

/// Complete Shared Linear Memory Region statically placed inside every Guest `.wasm` instance.
///
/// Layout summary:
/// - `control`: `64 B`
/// - `init_block`: `128 B`
/// - `ingress_ring`: `64 * 64 B = 4,096 B` (`4 KiB`)
/// - `egress_ring`: `64 * 64 B = 4,096 B` (`4 KiB`)
/// - `l2_ext_ring`: `64 * 192 B = 12,288 B` (`12 KiB`)
/// - `snapshot`: `4,160 B`
///
/// Total: `24,832 B` (`~24.25 KiB`, <2.4% of `1 MiB` initial linear memory!).
#[repr(C, align(64))]
pub struct SharedMemoryRegion {
    pub control: AbiControlHeader,
    pub init_block: StrategyInitBlock,
    pub ingress_ring: [IngressSlot64; RING_CAPACITY],
    pub egress_ring: [EgressOrderSlot64; RING_CAPACITY],
    pub l2_ext_ring: [L2DepthAndConfigExt; RING_CAPACITY],
    pub snapshot: StateSnapshotRegion,
}

impl SharedMemoryRegion {
    pub const fn new(strategy_kind: u32) -> Self {
        Self {
            control: AbiControlHeader::new(strategy_kind),
            init_block: StrategyInitBlock::default_btc(),
            ingress_ring: [IngressSlot64::zeroed(); RING_CAPACITY],
            egress_ring: [EgressOrderSlot64::zeroed(); RING_CAPACITY],
            l2_ext_ring: [L2DepthAndConfigExt::zeroed(); RING_CAPACITY],
            snapshot: StateSnapshotRegion::zeroed(),
        }
    }
}

// =============================================================================
// Compile-Time ABI Layout Assertions (Guaranteed Identical on wasm32 & x86_64)
// =============================================================================
const _: () = assert!(size_of::<AbiControlHeader>() == 64);
const _: () = assert!(size_of::<IngressSlot64>() == 64);
const _: () = assert!(size_of::<EgressOrderSlot64>() == 64);
const _: () = assert!(size_of::<L2DepthAndConfigExt>() == 192);
const _: () = assert!(size_of::<StrategyInitBlock>() == 128);
const _: () = assert!(size_of::<StateSnapshotRegion>() == 64 + SNAPSHOT_MAX_PAYLOAD);
const _: () = assert!(size_of::<[IngressSlot64; RING_CAPACITY]>() == 4096);
const _: () = assert!(size_of::<[EgressOrderSlot64; RING_CAPACITY]>() == 4096);
const _: () = assert!(size_of::<SharedMemoryRegion>() == 24_832);

/// Deterministic Castagnoli CRC-32C (polynomial `0x82F63B78`) in pure `#![no_std]` for snapshot integrity.
#[inline]
pub const fn crc32c_compute(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let mut i = 0;
    while i < bytes.len() {
        crc ^= bytes[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
            bit += 1;
        }
        i += 1;
    }
    !crc
}
