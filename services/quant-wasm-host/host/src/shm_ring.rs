//! # Bounds-Checked Shared Linear Memory Ring-Buffer Bridge (`shm_ring.rs`)
//!
//! Enforces strict spatial memory safety on all Host <-> Guest linear memory reads and writes:
//! - Every offset + size calculation uses checked arithmetic (`checked_add`) before indexing
//!   into `memory.data(&store)` / `memory.data_mut(&mut store)` (`01-hyperion-omega.md` §4.5).
//! - Any out-of-bounds or unaligned guest pointer raises `ShmBoundsError`, mapped deterministically
//!   to `WASM_TRAP_CODE_OUT_OF_BOUNDS_MEMORY` without host panic or UB.

use core::mem::{align_of, size_of};
use hyperion_quant_abi::*;
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ShmBoundsError {
    #[error("guest pointer 0x{ptr:08x} + {len} exceeds linear memory size {mem_len}")]
    OutOfBounds {
        ptr: u32,
        len: usize,
        mem_len: usize,
    },
    #[error("guest pointer 0x{ptr:08x} violates required alignment {align}")]
    Unaligned { ptr: u32, align: usize },
    #[error("invalid shared ABI header magic=0x{magic:016x} version={version}")]
    BadAbiHeader { magic: u64, version: u32 },
}

// Byte offsets inside `SharedMemoryRegion` (`#[repr(C, align(64))]`) verified at compile time:
pub const OFFSET_CONTROL: usize = 0;
pub const OFFSET_INIT_BLOCK: usize = 64;
pub const OFFSET_INGRESS_RING: usize = 64 + 128; // 192
pub const OFFSET_EGRESS_RING: usize = OFFSET_INGRESS_RING + 4096; // 4288
pub const OFFSET_L2_EXT_RING: usize = OFFSET_EGRESS_RING + 4096; // 8384
pub const OFFSET_SNAPSHOT: usize = OFFSET_L2_EXT_RING + (64 * 192); // 20672
pub const SHM_TOTAL_BYTES: usize = OFFSET_SNAPSHOT + size_of::<StateSnapshotRegion>(); // 24832

const _: () = assert!(SHM_TOTAL_BYTES == size_of::<SharedMemoryRegion>());

#[inline]
pub fn validate_region_bounds(mem_bytes: &[u8], base_ptr: u32) -> Result<usize, ShmBoundsError> {
    let base = base_ptr as usize;
    if base % align_of::<SharedMemoryRegion>() != 0 {
        return Err(ShmBoundsError::Unaligned {
            ptr: base_ptr,
            align: align_of::<SharedMemoryRegion>(),
        });
    }
    let end = base
        .checked_add(SHM_TOTAL_BYTES)
        .ok_or(ShmBoundsError::OutOfBounds {
            ptr: base_ptr,
            len: SHM_TOTAL_BYTES,
            mem_len: mem_bytes.len(),
        })?;
    if end > mem_bytes.len() {
        return Err(ShmBoundsError::OutOfBounds {
            ptr: base_ptr,
            len: SHM_TOTAL_BYTES,
            mem_len: mem_bytes.len(),
        });
    }
    Ok(base)
}

/// Validates and reads a `64-byte` `EgressOrderSlot64` from arbitrary guest pointer `slot_ptr`
/// passed to host-import `submit_limit_order(slot_ptr)`.
#[inline]
pub fn read_egress_order_slot(
    mem_bytes: &[u8],
    slot_ptr: u32,
) -> Result<EgressOrderSlot64, ShmBoundsError> {
    let start = slot_ptr as usize;
    // Require at least 8-byte alignment for u64 fields inside EgressOrderSlot64.
    if start % 8 != 0 {
        return Err(ShmBoundsError::Unaligned {
            ptr: slot_ptr,
            align: 8,
        });
    }
    let len = size_of::<EgressOrderSlot64>();
    let end = start.checked_add(len).ok_or(ShmBoundsError::OutOfBounds {
        ptr: slot_ptr,
        len,
        mem_len: mem_bytes.len(),
    })?;
    if end > mem_bytes.len() {
        return Err(ShmBoundsError::OutOfBounds {
            ptr: slot_ptr,
            len,
            mem_len: mem_bytes.len(),
        });
    }
    let src = &mem_bytes[start..end];
    let mut slot = EgressOrderSlot64::zeroed();
    // Safe byte-level little-endian decode (zero pointer aliasing or alignment UB on any host):
    slot.client_order_id = u64::from_le_bytes(src[0..8].try_into().unwrap());
    slot.target_order_id = u64::from_le_bytes(src[8..16].try_into().unwrap());
    slot.price_fp = u64::from_le_bytes(src[16..24].try_into().unwrap());
    slot.qty_fp = u64::from_le_bytes(src[24..32].try_into().unwrap());
    slot.client_ts_ns = u64::from_le_bytes(src[32..40].try_into().unwrap());
    slot.strategy_tag = u64::from_le_bytes(src[40..48].try_into().unwrap());
    slot.instrument_id = u32::from_le_bytes(src[48..52].try_into().unwrap());
    slot.max_slippage_bps = u32::from_le_bytes(src[52..56].try_into().unwrap());
    slot.cmd_kind = src[56];
    slot.side = src[57];
    slot.order_type = src[58];
    slot.stp_mode = src[59];
    slot.reject_code = i32::from_le_bytes(src[60..64].try_into().unwrap());
    Ok(slot)
}

#[inline]
pub fn verify_abi_control_header(mem_bytes: &[u8], base_ptr: u32) -> Result<u32, ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let hdr = &mem_bytes[base..base + 64];
    let magic = u64::from_le_bytes(hdr[0..8].try_into().unwrap());
    let version = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
    let strategy_kind = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
    if magic != ABI_MAGIC || version != ABI_VERSION {
        return Err(ShmBoundsError::BadAbiHeader { magic, version });
    }
    Ok(strategy_kind)
}

/// Writes `StrategyInitBlock` into `SharedMemoryRegion::init_block` with full bounds checking.
pub fn write_init_block(
    mem_bytes: &mut [u8],
    base_ptr: u32,
    cfg: &StrategyInitBlock,
) -> Result<(), ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let dst = &mut mem_bytes[base + OFFSET_INIT_BLOCK..base + OFFSET_INIT_BLOCK + 128];
    dst.fill(0);
    dst[0..8].copy_from_slice(&cfg.account_id.to_le_bytes());
    dst[8..12].copy_from_slice(&cfg.instrument_id.to_le_bytes());
    dst[12..16].copy_from_slice(&cfg.strategy_version.to_le_bytes());
    dst[16..24].copy_from_slice(&cfg.tick_size_fp.to_le_bytes());
    dst[24..32].copy_from_slice(&cfg.lot_size_fp.to_le_bytes());
    dst[32..40].copy_from_slice(&cfg.max_position_fp.to_le_bytes());
    dst[40..48].copy_from_slice(&cfg.max_order_qty_fp.to_le_bytes());
    dst[48..56].copy_from_slice(&cfg.order_qty_fp.to_le_bytes());
    dst[56..64].copy_from_slice(&cfg.zscore_entry_ppm.to_le_bytes());
    dst[64..72].copy_from_slice(&cfg.zscore_exit_ppm.to_le_bytes());
    dst[72..80].copy_from_slice(&cfg.ofi_weight_ppm.to_le_bytes());
    dst[80..88].copy_from_slice(&cfg.obi_threshold_ppm.to_le_bytes());
    dst[88..96].copy_from_slice(&cfg.hedge_band_fp.to_le_bytes());
    dst[96..104].copy_from_slice(&cfg.target_delta_fp.to_le_bytes());
    dst[104..112].copy_from_slice(&cfg.cooldown_ns.to_le_bytes());
    dst[112..120].copy_from_slice(&cfg.max_spread_fp.to_le_bytes());
    dst[120..124].copy_from_slice(&cfg.zscore_window.to_le_bytes());
    dst[124] = cfg.stp_mode;
    Ok(())
}

/// Host-side input structure for zero-copy 5-level `on_book_l2_tick` ring injection.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct HostBookL2Tick {
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
pub struct HostTradeTick {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub fill_id: u64,
    pub instrument_id: u32,
    pub aggressor_side: u8,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub vwap_fp: u64,
    pub ofi_delta_fp: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct HostOrderFill {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub order_id: u64,
    pub client_order_id: u64,
    pub instrument_id: u32,
    pub side: u8,
    pub order_status: u8,
    pub fill_price_fp: u64,
    pub fill_qty_fp: u64,
    pub remaining_qty_fp: u64,
    pub net_inventory_fp: i64,
    pub external_delta_fp: i64,
}

/// Hot-path Pass 3 (Cache-Line Aligned Direct Ring Write): single bounds check over `SHM_TOTAL_BYTES`,
/// writes `IngressSlot64` + `L2DepthAndConfigExt` in-place, and advances `ingress_write_seq`.
#[inline]
pub fn push_book_l2_ring_slot(
    mem_bytes: &mut [u8],
    base_ptr: u32,
    tick: &HostBookL2Tick,
) -> Result<u32, ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let write_seq = u64::from_le_bytes(mem_bytes[base + 32..base + 40].try_into().unwrap());
    let slot_idx = (write_seq as usize) & RING_MASK;

    let slot_off = base + OFFSET_INGRESS_RING + (slot_idx * 64);
    let slot = &mut mem_bytes[slot_off..slot_off + 64];
    slot[0..8].copy_from_slice(&tick.engine_seq.to_le_bytes());
    slot[8..16].copy_from_slice(&tick.ts_ns.to_le_bytes());
    slot[16..24].copy_from_slice(&tick.best_bid_fp.to_le_bytes());
    slot[24..32].copy_from_slice(&tick.best_ask_fp.to_le_bytes());
    slot[32..40].copy_from_slice(&tick.mid_price_fp.to_le_bytes());
    slot[40..48].copy_from_slice(&tick.ofi_fp.to_le_bytes());
    slot[48..56].copy_from_slice(&tick.book_imbalance_top5_ppm.to_le_bytes());
    slot[56..60].copy_from_slice(&tick.instrument_id.to_le_bytes());
    slot[60] = EVENT_KIND_BOOK_L2;
    slot[61] = SIDE_UNSPECIFIED;
    slot[62] = tick.bid_levels;
    slot[63] = tick.ask_levels;

    let ext_off = base + OFFSET_L2_EXT_RING + (slot_idx * 192);
    let ext = &mut mem_bytes[ext_off..ext_off + 192];
    for i in 0..L2_BOOK_LEVELS {
        let p = i * 8;
        ext[p..p + 8].copy_from_slice(&tick.bids_price_fp[i].to_le_bytes());
        ext[40 + p..48 + p].copy_from_slice(&tick.bids_qty_fp[i].to_le_bytes());
        ext[80 + p..88 + p].copy_from_slice(&tick.asks_price_fp[i].to_le_bytes());
        ext[120 + p..128 + p].copy_from_slice(&tick.asks_qty_fp[i].to_le_bytes());
    }
    ext[160..168].copy_from_slice(&(tick.fair_ref_price_fp as i64).to_le_bytes());
    ext[168..176].copy_from_slice(&(tick.spread_fp as i64).to_le_bytes());
    ext[176..192].fill(0);

    let next_seq = write_seq.wrapping_add(1);
    mem_bytes[base + 32..base + 40].copy_from_slice(&next_seq.to_le_bytes());
    Ok(slot_idx as u32)
}

#[inline]
pub fn push_trade_ring_slot(
    mem_bytes: &mut [u8],
    base_ptr: u32,
    trade: &HostTradeTick,
) -> Result<u32, ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let write_seq = u64::from_le_bytes(mem_bytes[base + 32..base + 40].try_into().unwrap());
    let slot_idx = (write_seq as usize) & RING_MASK;

    let slot_off = base + OFFSET_INGRESS_RING + (slot_idx * 64);
    let slot = &mut mem_bytes[slot_off..slot_off + 64];
    slot[0..8].copy_from_slice(&trade.engine_seq.to_le_bytes());
    slot[8..16].copy_from_slice(&trade.ts_ns.to_le_bytes());
    slot[16..24].copy_from_slice(&trade.price_fp.to_le_bytes());
    slot[24..32].copy_from_slice(&trade.vwap_fp.to_le_bytes());
    slot[32..40].copy_from_slice(&trade.qty_fp.to_le_bytes());
    slot[40..48].copy_from_slice(&trade.ofi_delta_fp.to_le_bytes());
    slot[48..56].copy_from_slice(&(trade.fill_id as i64).to_le_bytes());
    slot[56..60].copy_from_slice(&trade.instrument_id.to_le_bytes());
    slot[60] = EVENT_KIND_TRADE;
    slot[61] = trade.aggressor_side;
    slot[62] = 0;
    slot[63] = 0;

    let next_seq = write_seq.wrapping_add(1);
    mem_bytes[base + 32..base + 40].copy_from_slice(&next_seq.to_le_bytes());
    Ok(slot_idx as u32)
}

#[inline]
pub fn push_fill_ring_slot(
    mem_bytes: &mut [u8],
    base_ptr: u32,
    fill: &HostOrderFill,
) -> Result<u32, ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let write_seq = u64::from_le_bytes(mem_bytes[base + 32..base + 40].try_into().unwrap());
    let slot_idx = (write_seq as usize) & RING_MASK;

    let slot_off = base + OFFSET_INGRESS_RING + (slot_idx * 64);
    let slot = &mut mem_bytes[slot_off..slot_off + 64];
    slot[0..8].copy_from_slice(&fill.engine_seq.to_le_bytes());
    slot[8..16].copy_from_slice(&fill.ts_ns.to_le_bytes());
    slot[16..24].copy_from_slice(&fill.fill_price_fp.to_le_bytes());
    slot[24..32].copy_from_slice(&fill.remaining_qty_fp.to_le_bytes());
    slot[32..40].copy_from_slice(&fill.fill_qty_fp.to_le_bytes());
    slot[40..48].copy_from_slice(&fill.net_inventory_fp.to_le_bytes());
    slot[48..56].copy_from_slice(&fill.external_delta_fp.to_le_bytes());
    slot[56..60].copy_from_slice(&fill.instrument_id.to_le_bytes());
    slot[60] = EVENT_KIND_ORDER_FILL;
    slot[61] = fill.side;
    slot[62] = fill.order_status;
    slot[63] = 0;

    let ext_off = base + OFFSET_L2_EXT_RING + (slot_idx * 192);
    let ext = &mut mem_bytes[ext_off..ext_off + 192];
    ext[176..184].copy_from_slice(&(fill.order_id as i64).to_le_bytes());
    ext[184..192].copy_from_slice(&(fill.client_order_id as i64).to_le_bytes());

    let next_seq = write_seq.wrapping_add(1);
    mem_bytes[base + 32..base + 40].copy_from_slice(&next_seq.to_le_bytes());
    Ok(slot_idx as u32)
}

/// Copies the `4,160-byte` `StateSnapshotRegion` out of an old instance's linear memory.
pub fn extract_snapshot_bytes(
    mem_bytes: &[u8],
    base_ptr: u32,
) -> Result<Vec<u8>, ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let start = base + OFFSET_SNAPSHOT;
    let end = start + size_of::<StateSnapshotRegion>();
    Ok(mem_bytes[start..end].to_vec())
}

/// Writes a previously extracted `StateSnapshotRegion` into a new instance's linear memory.
pub fn inject_snapshot_bytes(
    mem_bytes: &mut [u8],
    base_ptr: u32,
    snap_bytes: &[u8],
) -> Result<(), ShmBoundsError> {
    let base = validate_region_bounds(mem_bytes, base_ptr)?;
    let expected = size_of::<StateSnapshotRegion>();
    if snap_bytes.len() != expected {
        return Err(ShmBoundsError::OutOfBounds {
            ptr: base_ptr,
            len: snap_bytes.len(),
            mem_len: expected,
        });
    }
    let start = base + OFFSET_SNAPSHOT;
    mem_bytes[start..start + expected].copy_from_slice(snap_bytes);
    Ok(())
}
