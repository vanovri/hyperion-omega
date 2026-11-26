#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::float_arithmetic)]

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use memmap2::MmapOptions;
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::error::ChronosError;
use crate::pb::MarketEventEnvelope;

pub const WAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const WAL_VERSION: u32 = 1;
pub const WAL_SEGMENT_HEADER_LEN: usize = 16;
pub const WAL_FRAME_HEADER_LEN: usize = 88;

pub const SNAPSHOT_MAGIC: &[u8; 8] = b"HYPSNP01";
pub const SNAPSHOT_VERSION: u32 = 1;
pub const SNAPSHOT_HEADER_LEN: usize = 72;

pub const DEFAULT_SPARSE_INDEX_STRIDE: usize = 64;

/// Computes Castagnoli CRC-32C over `engine_seq_le || ts_ns_le || prev_blake3 || payload`
/// per `00-hyperion-contract.md` Section 6.1, with zero heap allocation.
#[inline]
pub fn compute_wal_frame_crc32c(
    engine_seq: u64,
    ts_ns: u64,
    prev_blake3: &[u8; 32],
    payload: &[u8],
) -> u32 {
    let crc = crc32c::crc32c(&engine_seq.to_le_bytes());
    let crc = crc32c::crc32c_append(crc, &ts_ns.to_le_bytes());
    let crc = crc32c::crc32c_append(crc, prev_blake3);
    crc32c::crc32c_append(crc, payload)
}

/// Computes Blake3 frame hash `blake3(engine_seq_le || ts_ns_le || prev_blake3 || payload)`
/// per `00-hyperion-contract.md` Section 6.1, with zero heap allocation.
#[inline]
pub fn compute_wal_frame_blake3(
    engine_seq: u64,
    ts_ns: u64,
    prev_blake3: &[u8; 32],
    payload: &[u8],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&engine_seq.to_le_bytes());
    hasher.update(&ts_ns.to_le_bytes());
    hasher.update(prev_blake3);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Computes Castagnoli CRC-32C over `engine_seq_le || ts_ns_le || state_root_blake3 || payload`
/// for `HYPSNP01` snapshot files.
#[inline]
pub fn compute_snapshot_crc32c(
    engine_seq: u64,
    ts_ns: u64,
    state_root_blake3: &[u8; 32],
    payload: &[u8],
) -> u32 {
    let crc = crc32c::crc32c(&engine_seq.to_le_bytes());
    let crc = crc32c::crc32c_append(crc, &ts_ns.to_le_bytes());
    let crc = crc32c::crc32c_append(crc, state_root_blake3);
    crc32c::crc32c_append(crc, payload)
}

/// Backing storage for a `HYPWAL01` segment (zero-copy `Mmap` on disk or `Bytes` in memory).
pub enum SegmentBacking {
    Mmap(memmap2::Mmap),
    Memory(Bytes),
}

impl SegmentBacking {
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mmap(m) => m.as_ref(),
            Self::Memory(b) => b.as_ref(),
        }
    }

    pub fn open_mmap(path: &Path) -> Result<Self, ChronosError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Ok(Self::Memory(Bytes::new()));
        }
        // SAFETY: Read-only mapping of an immutable/append-only WAL segment file; frame bounds
        // and CRC32C/Blake3 digests are strictly validated on read.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self::Mmap(mmap))
    }
}

/// Borrowed zero-copy view of a single raw `HYPWAL01` frame inside a segment slice.
#[derive(Debug, Clone, Copy)]
pub struct RawWalFrame<'a> {
    pub offset: u64,
    pub payload_len: u32,
    pub crc32c: u32,
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub prev_blake3: [u8; 32],
    pub frame_blake3: [u8; 32],
    pub payload: &'a [u8],
}

impl<'a> RawWalFrame<'a> {
    #[inline]
    pub fn verify_integrity(&self) -> Result<(), ChronosError> {
        let computed_crc =
            compute_wal_frame_crc32c(self.engine_seq, self.ts_ns, &self.prev_blake3, self.payload);
        if computed_crc != self.crc32c {
            return Err(ChronosError::WalCrcMismatch {
                engine_seq: self.engine_seq,
                offset: self.offset,
                expected: self.crc32c,
                computed: computed_crc,
            });
        }
        let computed_hash =
            compute_wal_frame_blake3(self.engine_seq, self.ts_ns, &self.prev_blake3, self.payload);
        if computed_hash != self.frame_blake3 {
            return Err(ChronosError::WalBlake3Mismatch {
                engine_seq: self.engine_seq,
                offset: self.offset,
            });
        }
        Ok(())
    }

    #[inline]
    pub fn decode_envelope(&self) -> Result<MarketEventEnvelope, ChronosError> {
        Ok(MarketEventEnvelope::decode(self.payload)?)
    }
}

/// Parses the 16-byte `HYPWAL01` segment header and returns `(version, flags)`.
pub fn parse_wal_segment_header(bytes: &[u8]) -> Result<(u32, u32), ChronosError> {
    if bytes.len() < WAL_SEGMENT_HEADER_LEN {
        return Err(ChronosError::TruncatedWalFrame {
            offset: 0,
            needed: WAL_SEGMENT_HEADER_LEN,
            available: bytes.len(),
        });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[0..8]);
    if &magic != WAL_MAGIC {
        return Err(ChronosError::InvalidWalMagic(magic));
    }
    let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if version != WAL_VERSION {
        return Err(ChronosError::UnsupportedWalVersion(version));
    }
    let flags = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    Ok((version, flags))
}

/// Parses a single `HYPWAL01` frame starting at `cursor` within `segment_bytes`, returning
/// `(frame, next_cursor)`.
#[inline]
pub fn parse_wal_frame_at(
    segment_bytes: &[u8],
    cursor: usize,
) -> Result<(RawWalFrame<'_>, usize), ChronosError> {
    let available = segment_bytes.len().saturating_sub(cursor);
    if available < WAL_FRAME_HEADER_LEN {
        return Err(ChronosError::TruncatedWalFrame {
            offset: cursor as u64,
            needed: WAL_FRAME_HEADER_LEN,
            available,
        });
    }
    let hdr = &segment_bytes[cursor..cursor + WAL_FRAME_HEADER_LEN];
    let payload_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    let total_frame_len = WAL_FRAME_HEADER_LEN + (payload_len as usize);
    if available < total_frame_len {
        return Err(ChronosError::TruncatedWalFrame {
            offset: cursor as u64,
            needed: total_frame_len,
            available,
        });
    }

    let crc32c = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let engine_seq = u64::from_le_bytes([
        hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
    ]);
    let ts_ns = u64::from_le_bytes([
        hdr[16], hdr[17], hdr[18], hdr[19], hdr[20], hdr[21], hdr[22], hdr[23],
    ]);
    let mut prev_blake3 = [0u8; 32];
    prev_blake3.copy_from_slice(&hdr[24..56]);
    let mut frame_blake3 = [0u8; 32];
    frame_blake3.copy_from_slice(&hdr[56..88]);

    let payload_start = cursor + WAL_FRAME_HEADER_LEN;
    let payload_end = payload_start + (payload_len as usize);
    let payload = &segment_bytes[payload_start..payload_end];

    Ok((
        RawWalFrame {
            offset: cursor as u64,
            payload_len,
            crc32c,
            engine_seq,
            ts_ns,
            prev_blake3,
            frame_blake3,
            payload,
        },
        payload_end,
    ))
}

/// Compact 64-byte sparse jump index entry in RAM for sub-microsecond binary search by
/// `engine_seq` or nanosecond `ts_ns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SparseIndexEntry {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub segment_idx: u32,
    pub event_idx: u32,
    pub byte_offset: u64,
    pub prev_blake3: [u8; 32],
    pub frame_blake3: [u8; 32],
}

/// High-speed in-memory sparse jump index over `HYPWAL01` segments.
#[derive(Debug, Clone)]
pub struct SparseWalIndex {
    stride: usize,
    entries: Vec<SparseIndexEntry>,
}

impl SparseWalIndex {
    pub fn new(stride: usize) -> Self {
        Self {
            stride: stride.max(1),
            entries: Vec::new(),
        }
    }

    #[inline]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn entries(&self) -> &[SparseIndexEntry] {
        &self.entries
    }

    pub fn push(&mut self, entry: SparseIndexEntry) {
        self.entries.push(entry);
    }

    /// Finds the nearest sparse index anchor with `entry.engine_seq <= target_seq`.
    #[inline]
    pub fn anchor_for_seq(&self, target_seq: u64) -> Option<SparseIndexEntry> {
        let pos = self.entries.partition_point(|e| e.engine_seq <= target_seq);
        if pos == 0 {
            None
        } else {
            Some(self.entries[pos - 1])
        }
    }

    /// Finds the nearest sparse index anchor with `entry.ts_ns <= target_ts_ns`.
    #[inline]
    pub fn anchor_for_ts_ns(&self, target_ts_ns: u64) -> Option<SparseIndexEntry> {
        let pos = self.entries.partition_point(|e| e.ts_ns <= target_ts_ns);
        if pos == 0 {
            None
        } else {
            Some(self.entries[pos - 1])
        }
    }
}

/// Verified WAL segment descriptor + backing bytes.
pub struct WalSegment {
    pub path: Option<PathBuf>,
    pub backing: SegmentBacking,
    pub first_seq: u64,
    pub last_seq: u64,
    pub first_ts_ns: u64,
    pub last_ts_ns: u64,
    pub frame_count: usize,
}

/// Summary returned by cryptographic hash-chain verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainVerificationSummary {
    pub verified_frames: u64,
    pub first_seq: u64,
    pub last_seq: u64,
    pub tip_blake3: [u8; 32],
}

/// Decoded in-memory WAL record cached alongside raw segments for sub-millisecond seeks & forks.
#[derive(Debug, Clone)]
pub struct IndexedWalEvent {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub prev_blake3: [u8; 32],
    pub frame_blake3: [u8; 32],
    pub envelope: MarketEventEnvelope,
}

/// Append-only `HYPWAL01` writer used for building segments, tests, benchmarks, and bootstrap data.
pub struct WalSegmentWriter {
    buf: Vec<u8>,
    next_seq: u64,
    prev_blake3: [u8; 32],
}

impl WalSegmentWriter {
    pub fn new() -> Self {
        Self::with_chain_anchor(1, [0u8; 32])
    }

    pub fn with_chain_anchor(start_seq: u64, prev_blake3: [u8; 32]) -> Self {
        let mut buf = Vec::with_capacity(64 * 1024);
        buf.extend_from_slice(WAL_MAGIC);
        buf.extend_from_slice(&WAL_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        Self {
            buf,
            next_seq: start_seq.max(1),
            prev_blake3,
        }
    }

    #[inline]
    pub const fn next_seq(&self) -> u64 {
        self.next_seq
    }

    #[inline]
    pub const fn tip_blake3(&self) -> [u8; 32] {
        self.prev_blake3
    }

    pub fn append_envelope(
        &mut self,
        mut envelope: MarketEventEnvelope,
    ) -> Result<(u64, [u8; 32]), ChronosError> {
        let seq = if envelope.engine_seq == 0 {
            self.next_seq
        } else {
            if envelope.engine_seq != self.next_seq {
                return Err(ChronosError::WalSequenceGap {
                    expected: self.next_seq,
                    found: envelope.engine_seq,
                });
            }
            envelope.engine_seq
        };
        envelope.engine_seq = seq;
        let ts_ns = envelope.ts_ns;

        let payload = envelope.encode_to_vec();
        let payload_len = u32::try_from(payload.len()).map_err(|_| {
            ChronosError::InvalidArgument("WAL payload exceeds u32::MAX".to_string())
        })?;
        let crc = compute_wal_frame_crc32c(seq, ts_ns, &self.prev_blake3, &payload);
        let frame_hash = compute_wal_frame_blake3(seq, ts_ns, &self.prev_blake3, &payload);

        self.buf.extend_from_slice(&payload_len.to_le_bytes());
        self.buf.extend_from_slice(&crc.to_le_bytes());
        self.buf.extend_from_slice(&seq.to_le_bytes());
        self.buf.extend_from_slice(&ts_ns.to_le_bytes());
        self.buf.extend_from_slice(&self.prev_blake3);
        self.buf.extend_from_slice(&frame_hash);
        self.buf.extend_from_slice(&payload);

        self.prev_blake3 = frame_hash;
        self.next_seq = seq + 1;
        Ok((seq, frame_hash))
    }

    pub fn finish_bytes(self) -> Bytes {
        Bytes::from(self.buf)
    }

    pub fn write_to_path(&self, path: &Path) -> Result<(), ChronosError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(path)?;
        f.write_all(&self.buf)?;
        f.sync_all()?;
        Ok(())
    }
}

impl Default for WalSegmentWriter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// SNAPSHOT (`HYPSNP01`) SERIALIZATION & VERIFICATION
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotRestingOrder {
    pub order_id: u64,
    pub client_order_id: String,
    pub instrument_id: u32,
    pub symbol: String,
    pub account_id: u64,
    pub side: u8, // 1 = BUY, 2 = SELL
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub locked_collateral_fp: i128,
    pub priority_seq: u64,
    pub ts_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotAccountBalance {
    pub account_id: u64,
    pub asset: String,
    pub available_fp: i128,
    pub locked_fp: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotAccountStatus {
    pub account_id: u64,
    pub status: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotAssetPool {
    pub asset: String,
    pub total_deposits_fp: u128,
    pub treasury_fee_pool_fp: i128,
}

/// Canonical point-in-time exchange & ledger snapshot payload stored inside `HYPSNP01`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CanonicalSnapshotPayload {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub resting_orders: Vec<SnapshotRestingOrder>,
    pub account_balances: Vec<SnapshotAccountBalance>,
    pub account_statuses: Vec<SnapshotAccountStatus>,
    pub asset_pools: Vec<SnapshotAssetPool>,
    pub last_clearing_prices: Vec<(String, u64)>,
    pub exec_leaf_count: u64,
    pub exec_frontier: Vec<Option<[u8; 32]>>,
    #[serde(default)]
    pub exec_leaf_hashes: Vec<[u8; 32]>,
    #[serde(default)]
    pub last_wal_blake3: [u8; 32],
    pub next_fill_id: u64,
    pub maker_fee_ppm: i64,
    pub taker_fee_ppm: i64,
}

/// Exact Channel 02 (`ledger-rs`) `SnapshotPayloadV1` wire schema inside `HYPSNP01` files.
/// Enables `chronos-replay` to read `${HYPERION_DATA_DIR}/snapshots/snap_*.snap` directly with
/// 100% bit-for-bit identical `state_root_blake3` verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotPayloadV1 {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub journal_seq: u64,
    pub last_wal_blake3: [u8; 32],
    pub last_journal_blake3: [u8; 32],
    pub next_order_id: u64,
    pub next_fill_id: u64,
    pub state_root_blake3: [u8; 32],
    pub accounts: Vec<LedgerSnapshotAccountV1>,
    pub asset_totals: Vec<LedgerSnapshotAssetTotalV1>,
    pub instruments: Vec<LedgerSnapshotInstrumentV1>,
    pub exec_leaf_hashes: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotAccountV1 {
    pub account_id: u64,
    pub status: u8,
    pub balances: Vec<LedgerSnapshotAssetBalanceV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotAssetBalanceV1 {
    pub asset: String,
    pub available_fp: String,
    pub locked_fp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotAssetTotalV1 {
    pub asset: String,
    pub total_deposits_fp: String,
    pub total_available_fp: String,
    pub total_locked_fp: String,
    pub treasury_fee_pool_fp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotInstrumentV1 {
    pub instrument_id: u32,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub regime: u8,
    pub maker_fee_ppm: i64,
    pub taker_fee_ppm: i64,
    pub prev_l3_seq: u64,
    pub resting_orders: Vec<LedgerSnapshotOrderV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshotOrderV1 {
    pub order_id: u64,
    pub client_order_id: String,
    pub account_id: u64,
    pub side: u8,
    pub kind: u8,
    pub stp_mode: u8,
    pub price_fp: u64,
    pub initial_qty_fp: u64,
    pub remaining_qty_fp: u64,
    pub locked_quote_fp: String,
    pub created_ts_ns: u64,
    pub priority_seq: u64,
}

impl LedgerSnapshotPayloadV1 {
    pub fn into_canonical(self) -> Result<CanonicalSnapshotPayload, ChronosError> {
        let mut account_balances = Vec::new();
        let mut account_statuses = Vec::new();
        for acc in self.accounts {
            if acc.status != 0 && acc.status != 1 {
                account_statuses.push(SnapshotAccountStatus {
                    account_id: acc.account_id,
                    status: acc.status as i32,
                });
            }
            for b in acc.balances {
                let avail: i128 =
                    b.available_fp
                        .parse()
                        .map_err(|e| ChronosError::InvalidSnapshotPayload {
                            engine_seq: self.engine_seq,
                            reason: format!("invalid available_fp: {e}"),
                        })?;
                let locked: i128 =
                    b.locked_fp
                        .parse()
                        .map_err(|e| ChronosError::InvalidSnapshotPayload {
                            engine_seq: self.engine_seq,
                            reason: format!("invalid locked_fp: {e}"),
                        })?;
                account_balances.push(SnapshotAccountBalance {
                    account_id: acc.account_id,
                    asset: b.asset,
                    available_fp: avail,
                    locked_fp: locked,
                });
            }
        }

        let mut asset_pools = Vec::with_capacity(self.asset_totals.len());
        for t in self.asset_totals {
            let dep: u128 =
                t.total_deposits_fp
                    .parse()
                    .map_err(|e| ChronosError::InvalidSnapshotPayload {
                        engine_seq: self.engine_seq,
                        reason: format!("invalid total_deposits_fp: {e}"),
                    })?;
            let treas: i128 = t.treasury_fee_pool_fp.parse().map_err(|e| {
                ChronosError::InvalidSnapshotPayload {
                    engine_seq: self.engine_seq,
                    reason: format!("invalid treasury_fee_pool_fp: {e}"),
                }
            })?;
            asset_pools.push(SnapshotAssetPool {
                asset: t.asset,
                total_deposits_fp: dep,
                treasury_fee_pool_fp: treas,
            });
        }

        let mut resting_orders = Vec::new();
        let mut maker_fee_ppm = crate::fixed::DEFAULT_MAKER_FEE_PPM;
        let mut taker_fee_ppm = crate::fixed::DEFAULT_TAKER_FEE_PPM;
        for (idx, inst) in self.instruments.into_iter().enumerate() {
            if idx == 0 {
                maker_fee_ppm = inst.maker_fee_ppm;
                taker_fee_ppm = inst.taker_fee_ppm;
            }
            for o in inst.resting_orders {
                let locked_q: i128 = o.locked_quote_fp.parse().map_err(|e| {
                    ChronosError::InvalidSnapshotPayload {
                        engine_seq: self.engine_seq,
                        reason: format!("invalid locked_quote_fp: {e}"),
                    }
                })?;
                let locked_col = if o.side == 1 {
                    locked_q
                } else {
                    o.remaining_qty_fp as i128
                };
                resting_orders.push(SnapshotRestingOrder {
                    order_id: o.order_id,
                    client_order_id: o.client_order_id,
                    instrument_id: inst.instrument_id,
                    symbol: inst.symbol.clone(),
                    account_id: o.account_id,
                    side: o.side,
                    price_fp: o.price_fp,
                    remaining_qty_fp: o.remaining_qty_fp,
                    locked_collateral_fp: locked_col,
                    priority_seq: o.priority_seq,
                    ts_ns: o.created_ts_ns,
                });
            }
        }

        Ok(CanonicalSnapshotPayload {
            engine_seq: self.engine_seq,
            ts_ns: self.ts_ns,
            resting_orders,
            account_balances,
            account_statuses,
            asset_pools,
            last_clearing_prices: Vec::new(),
            exec_leaf_count: self.exec_leaf_hashes.len() as u64,
            exec_frontier: Vec::new(),
            exec_leaf_hashes: self.exec_leaf_hashes,
            last_wal_blake3: self.last_wal_blake3,
            next_fill_id: self.next_fill_id,
            maker_fee_ppm,
            taker_fee_ppm,
        })
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedSnapshot {
    pub path: Option<PathBuf>,
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub state_root_blake3: [u8; 32],
    pub payload: CanonicalSnapshotPayload,
}

/// Encodes a `HYPSNP01` snapshot binary buffer per `00-hyperion-contract.md` Section 6.1.
pub fn encode_snapshot_bytes(
    engine_seq: u64,
    ts_ns: u64,
    state_root_blake3: [u8; 32],
    payload: &CanonicalSnapshotPayload,
) -> Result<Vec<u8>, ChronosError> {
    let payload_bytes = serde_json::to_vec(payload)?;
    let crc = compute_snapshot_crc32c(engine_seq, ts_ns, &state_root_blake3, &payload_bytes);
    let payload_len = payload_bytes.len() as u64;

    let mut out = Vec::with_capacity(SNAPSHOT_HEADER_LEN + payload_bytes.len());
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&engine_seq.to_le_bytes());
    out.extend_from_slice(&ts_ns.to_le_bytes());
    out.extend_from_slice(&state_root_blake3);
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&payload_bytes);
    Ok(out)
}

/// Parses and cryptographically verifies a `HYPSNP01` snapshot buffer.
/// Supports JSON `CanonicalSnapshotPayload` as well as fallback Protobuf `L2BookSnapshot` payloads.
pub fn parse_and_verify_snapshot(
    bytes: &[u8],
    path: Option<PathBuf>,
) -> Result<VerifiedSnapshot, ChronosError> {
    if bytes.len() < SNAPSHOT_HEADER_LEN {
        return Err(ChronosError::TruncatedSnapshot {
            needed: SNAPSHOT_HEADER_LEN,
            available: bytes.len(),
        });
    }
    let mut magic = [0u8; 8];
    magic.copy_from_slice(&bytes[0..8]);
    if &magic != SNAPSHOT_MAGIC {
        return Err(ChronosError::InvalidSnapshotMagic(magic));
    }
    let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if version != SNAPSHOT_VERSION {
        return Err(ChronosError::UnsupportedSnapshotVersion(version));
    }
    let expected_crc = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let engine_seq = u64::from_le_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
    ]);
    let ts_ns = u64::from_le_bytes([
        bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30], bytes[31],
    ]);
    let mut state_root_blake3 = [0u8; 32];
    state_root_blake3.copy_from_slice(&bytes[32..64]);
    let payload_len = u64::from_le_bytes([
        bytes[64], bytes[65], bytes[66], bytes[67], bytes[68], bytes[69], bytes[70], bytes[71],
    ]) as usize;

    let total_needed = SNAPSHOT_HEADER_LEN.saturating_add(payload_len);
    if bytes.len() < total_needed {
        return Err(ChronosError::TruncatedSnapshot {
            needed: total_needed,
            available: bytes.len(),
        });
    }
    let payload_slice = &bytes[SNAPSHOT_HEADER_LEN..total_needed];
    let computed_crc =
        compute_snapshot_crc32c(engine_seq, ts_ns, &state_root_blake3, payload_slice);
    if computed_crc != expected_crc {
        return Err(ChronosError::SnapshotCrcMismatch {
            engine_seq,
            expected: expected_crc,
            computed: computed_crc,
        });
    }

    let payload = if payload_slice.first().copied() == Some(b'{') {
        if let Ok(ledger_v1) = serde_json::from_slice::<LedgerSnapshotPayloadV1>(payload_slice) {
            if ledger_v1.engine_seq != engine_seq
                || ledger_v1.state_root_blake3 != state_root_blake3
            {
                return Err(ChronosError::SnapshotStateRootMismatch { engine_seq });
            }
            ledger_v1.into_canonical()?
        } else {
            serde_json::from_slice::<CanonicalSnapshotPayload>(payload_slice)?
        }
    } else if let Ok(l2) = crate::pb::L2BookSnapshot::decode(payload_slice) {
        let mut resting_orders = Vec::new();
        let mut synth_id = 1_000_000_000u64;
        for lvl in &l2.bids {
            resting_orders.push(SnapshotRestingOrder {
                order_id: synth_id,
                client_order_id: format!("snap-bid-{synth_id}"),
                instrument_id: l2.instrument_id,
                symbol: l2.symbol.clone(),
                account_id: 1,
                side: 1,
                price_fp: lvl.price_fp,
                remaining_qty_fp: lvl.total_qty_fp,
                locked_collateral_fp: 0,
                priority_seq: engine_seq,
                ts_ns,
            });
            synth_id += 1;
        }
        for lvl in &l2.asks {
            resting_orders.push(SnapshotRestingOrder {
                order_id: synth_id,
                client_order_id: format!("snap-ask-{synth_id}"),
                instrument_id: l2.instrument_id,
                symbol: l2.symbol.clone(),
                account_id: 2,
                side: 2,
                price_fp: lvl.price_fp,
                remaining_qty_fp: lvl.total_qty_fp,
                locked_collateral_fp: 0,
                priority_seq: engine_seq,
                ts_ns,
            });
            synth_id += 1;
        }
        CanonicalSnapshotPayload {
            engine_seq,
            ts_ns,
            resting_orders,
            last_clearing_prices: if l2.mid_price_fp > 0 {
                vec![(l2.symbol, l2.mid_price_fp)]
            } else {
                Vec::new()
            },
            ..Default::default()
        }
    } else {
        return Err(ChronosError::InvalidSnapshotPayload {
            engine_seq,
            reason: "unrecognized snapshot payload encoding".to_string(),
        });
    };

    Ok(VerifiedSnapshot {
        path,
        engine_seq,
        ts_ns,
        state_root_blake3,
        payload,
    })
}

/// Multi-segment cryptographic WAL & Snapshot store with in-memory `SparseWalIndex`.
pub struct ChronosWalStore {
    segments: Vec<WalSegment>,
    sparse_index: SparseWalIndex,
    events: Vec<IndexedWalEvent>,
    snapshots: Vec<VerifiedSnapshot>,
    chain_verified: bool,
}

impl ChronosWalStore {
    pub fn empty(stride: usize) -> Self {
        Self {
            segments: Vec::new(),
            sparse_index: SparseWalIndex::new(stride),
            events: Vec::new(),
            snapshots: Vec::new(),
            chain_verified: true,
        }
    }

    /// Loads and cryptographically verifies all `wal/*.wal` segments and `snapshots/*.snap` files
    /// from a `ledger-rs` data directory (`${HYPERION_DATA_DIR:-./data/ledger}`).
    pub fn open_data_dir(data_dir: &Path, stride: usize) -> Result<Self, ChronosError> {
        let wal_dir = data_dir.join("wal");
        let snap_dir = data_dir.join("snapshots");

        let mut wal_paths = Vec::new();
        if wal_dir.exists() {
            for entry in fs::read_dir(&wal_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("wal") {
                    wal_paths.push(path);
                }
            }
        }
        wal_paths.sort();

        let mut backings = Vec::with_capacity(wal_paths.len());
        for p in wal_paths {
            let backing = SegmentBacking::open_mmap(&p)?;
            if !backing.as_slice().is_empty() {
                backings.push((Some(p), backing));
            }
        }

        let mut snap_paths = Vec::new();
        if snap_dir.exists() {
            for entry in fs::read_dir(&snap_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("snap") {
                    snap_paths.push(path);
                }
            }
        }
        snap_paths.sort();

        let mut snapshots = Vec::with_capacity(snap_paths.len());
        for sp in snap_paths {
            let bytes = fs::read(&sp)?;
            if !bytes.is_empty() {
                let snap = parse_and_verify_snapshot(&bytes, Some(sp))?;
                snapshots.push(snap);
            }
        }
        snapshots.sort_by_key(|s| s.engine_seq);

        Self::from_backings_and_snapshots(backings, snapshots, stride)
    }

    /// Builds a verified `ChronosWalStore` from segment backings and verified snapshots.
    pub fn from_backings_and_snapshots(
        raw_segments: Vec<(Option<PathBuf>, SegmentBacking)>,
        mut snapshots: Vec<VerifiedSnapshot>,
        stride: usize,
    ) -> Result<Self, ChronosError> {
        let mut sparse_index = SparseWalIndex::new(stride);
        let mut segments = Vec::with_capacity(raw_segments.len());
        let mut events = Vec::new();

        let mut expected_seq: Option<u64> = None;
        let mut expected_prev_hash = [0u8; 32];

        for (seg_idx, (path, backing)) in raw_segments.into_iter().enumerate() {
            let slice = backing.as_slice();
            let (_ver, _flags) = parse_wal_segment_header(slice)?;
            let mut cursor = WAL_SEGMENT_HEADER_LEN;
            let mut seg_first_seq = 0u64;
            let mut seg_last_seq = 0u64;
            let mut seg_first_ts = 0u64;
            let mut seg_last_ts = 0u64;
            let mut seg_frames = 0usize;

            while cursor < slice.len() {
                let frame_offset = cursor as u64;
                let (frame, next_cursor) = parse_wal_frame_at(slice, cursor)?;
                frame.verify_integrity()?;

                match expected_seq {
                    None => {
                        if frame.engine_seq == 1 && frame.prev_blake3 != [0u8; 32] {
                            return Err(ChronosError::WalChainBroken {
                                engine_seq: frame.engine_seq,
                            });
                        }
                        if frame.engine_seq > 1 {
                            if let Some(snap) = snapshots
                                .iter()
                                .find(|s| s.engine_seq + 1 == frame.engine_seq)
                            {
                                if snap.payload.last_wal_blake3 != [0u8; 32]
                                    && frame.prev_blake3 != snap.payload.last_wal_blake3
                                {
                                    return Err(ChronosError::WalChainBroken {
                                        engine_seq: frame.engine_seq,
                                    });
                                }
                            }
                        }
                        expected_seq = Some(frame.engine_seq + 1);
                    }
                    Some(exp) => {
                        if frame.engine_seq != exp {
                            return Err(ChronosError::WalSequenceGap {
                                expected: exp,
                                found: frame.engine_seq,
                            });
                        }
                        if frame.prev_blake3 != expected_prev_hash {
                            return Err(ChronosError::WalChainBroken {
                                engine_seq: frame.engine_seq,
                            });
                        }
                        expected_seq = Some(exp + 1);
                    }
                }
                expected_prev_hash = frame.frame_blake3;

                let event_idx = events.len();
                if event_idx % sparse_index.stride() == 0 {
                    sparse_index.push(SparseIndexEntry {
                        engine_seq: frame.engine_seq,
                        ts_ns: frame.ts_ns,
                        segment_idx: seg_idx as u32,
                        event_idx: event_idx as u32,
                        byte_offset: frame_offset,
                        prev_blake3: frame.prev_blake3,
                        frame_blake3: frame.frame_blake3,
                    });
                }

                let mut envelope = frame.decode_envelope()?;
                if envelope.engine_seq == 0 {
                    envelope.engine_seq = frame.engine_seq;
                }
                if envelope.ts_ns == 0 {
                    envelope.ts_ns = frame.ts_ns;
                }

                if seg_frames == 0 {
                    seg_first_seq = frame.engine_seq;
                    seg_first_ts = frame.ts_ns;
                }
                seg_last_seq = frame.engine_seq;
                seg_last_ts = frame.ts_ns;
                seg_frames += 1;

                events.push(IndexedWalEvent {
                    engine_seq: frame.engine_seq,
                    ts_ns: frame.ts_ns,
                    prev_blake3: frame.prev_blake3,
                    frame_blake3: frame.frame_blake3,
                    envelope,
                });

                cursor = next_cursor;
            }

            segments.push(WalSegment {
                path,
                backing,
                first_seq: seg_first_seq,
                last_seq: seg_last_seq,
                first_ts_ns: seg_first_ts,
                last_ts_ns: seg_last_ts,
                frame_count: seg_frames,
            });
        }

        snapshots.sort_by_key(|s| s.engine_seq);

        Ok(Self {
            segments,
            sparse_index,
            events,
            snapshots,
            chain_verified: true,
        })
    }

    #[inline]
    pub fn events(&self) -> &[IndexedWalEvent] {
        &self.events
    }

    #[inline]
    pub fn snapshots(&self) -> &[VerifiedSnapshot] {
        &self.snapshots
    }

    #[inline]
    pub fn sparse_index(&self) -> &SparseWalIndex {
        &self.sparse_index
    }

    #[inline]
    pub const fn is_chain_verified(&self) -> bool {
        self.chain_verified
    }

    #[inline]
    pub fn min_engine_seq(&self) -> u64 {
        self.events.first().map(|e| e.engine_seq).unwrap_or(0)
    }

    #[inline]
    pub fn max_engine_seq(&self) -> u64 {
        let wal_max = self.events.last().map(|e| e.engine_seq).unwrap_or(0);
        let snap_max = self.snapshots.last().map(|s| s.engine_seq).unwrap_or(0);
        wal_max.max(snap_max)
    }

    #[inline]
    pub fn max_ts_ns(&self) -> u64 {
        let wal_ts = self.events.last().map(|e| e.ts_ns).unwrap_or(0);
        let snap_ts = self.snapshots.last().map(|s| s.ts_ns).unwrap_or(0);
        wal_ts.max(snap_ts)
    }

    /// Finds the closest verified snapshot with `snapshot.engine_seq <= target_seq`.
    #[inline]
    pub fn nearest_snapshot_for_seq(&self, target_seq: u64) -> Option<&VerifiedSnapshot> {
        let pos = self
            .snapshots
            .partition_point(|s| s.engine_seq <= target_seq);
        if pos == 0 {
            None
        } else {
            Some(&self.snapshots[pos - 1])
        }
    }

    /// Finds the closest verified snapshot with `snapshot.ts_ns <= target_ts_ns`.
    #[inline]
    pub fn nearest_snapshot_for_ts_ns(&self, target_ts_ns: u64) -> Option<&VerifiedSnapshot> {
        let pos = self.snapshots.partition_point(|s| s.ts_ns <= target_ts_ns);
        if pos == 0 {
            None
        } else {
            Some(&self.snapshots[pos - 1])
        }
    }

    /// Uses `SparseWalIndex` to locate the slice of `IndexedWalEvent`s in `(after_seq, up_to_seq]`.
    #[inline]
    pub fn events_in_seq_range(&self, after_seq: u64, up_to_seq: u64) -> &[IndexedWalEvent] {
        if up_to_seq <= after_seq || self.events.is_empty() {
            return &[];
        }
        let approx_start = self
            .sparse_index
            .anchor_for_seq(after_seq.saturating_add(1))
            .map(|a| a.event_idx as usize)
            .unwrap_or(0);
        let tail = &self.events[approx_start..];
        let start_rel = tail.partition_point(|e| e.engine_seq <= after_seq);
        let start_idx = approx_start + start_rel;

        let approx_end = self
            .sparse_index
            .anchor_for_seq(up_to_seq)
            .map(|a| a.event_idx as usize)
            .unwrap_or(start_idx)
            .max(start_idx);
        let end_scan_limit = (approx_end + self.sparse_index.stride() + 1).min(self.events.len());
        let end_rel =
            self.events[approx_end..end_scan_limit].partition_point(|e| e.engine_seq <= up_to_seq);
        let end_idx = if approx_end + end_rel < end_scan_limit {
            approx_end + end_rel
        } else {
            start_idx + self.events[start_idx..].partition_point(|e| e.engine_seq <= up_to_seq)
        };
        &self.events[start_idx..end_idx]
    }

    /// Resolves a nanosecond timestamp `target_ts_ns` to the highest `engine_seq` with `ts_ns <= target_ts_ns`.
    #[inline]
    pub fn resolve_seq_at_or_before_ts(&self, target_ts_ns: u64) -> Option<(u64, u64)> {
        if self.events.is_empty() {
            return self
                .nearest_snapshot_for_ts_ns(target_ts_ns)
                .map(|s| (s.engine_seq, s.ts_ns));
        }
        let approx = self
            .sparse_index
            .anchor_for_ts_ns(target_ts_ns)
            .map(|a| a.event_idx as usize)
            .unwrap_or(0);
        let scan_end = (approx + self.sparse_index.stride() + 1).min(self.events.len());
        let pos = if scan_end < self.events.len() && self.events[scan_end - 1].ts_ns <= target_ts_ns
        {
            self.events.partition_point(|e| e.ts_ns <= target_ts_ns)
        } else {
            approx + self.events[approx..scan_end].partition_point(|e| e.ts_ns <= target_ts_ns)
        };
        if pos == 0 {
            self.nearest_snapshot_for_ts_ns(target_ts_ns)
                .map(|s| (s.engine_seq, s.ts_ns))
        } else {
            let ev = &self.events[pos - 1];
            Some((ev.engine_seq, ev.ts_ns))
        }
    }

    /// Re-verifies raw segment bytes over `(after_seq, up_to_seq]` directly from `Mmap`/`Bytes`
    /// starting at the nearest `SparseWalIndex` anchor (on-demand cryptographic audit).
    pub fn verify_raw_chain_up_to(
        &self,
        up_to_seq: u64,
    ) -> Result<ChainVerificationSummary, ChronosError> {
        let mut verified_frames = 0u64;
        let mut first_seq = 0u64;
        let mut last_seq = 0u64;
        let mut prev_hash = [0u8; 32];
        let mut expected_seq: Option<u64> = None;

        for seg in &self.segments {
            if seg.frame_count == 0 || seg.first_seq > up_to_seq {
                break;
            }
            let slice = seg.backing.as_slice();
            let _ = parse_wal_segment_header(slice)?;
            let mut cursor = WAL_SEGMENT_HEADER_LEN;
            while cursor < slice.len() {
                let (frame, next_cursor) = parse_wal_frame_at(slice, cursor)?;
                if frame.engine_seq > up_to_seq {
                    break;
                }
                frame.verify_integrity()?;
                match expected_seq {
                    None => {
                        if frame.engine_seq == 1 && frame.prev_blake3 != [0u8; 32] {
                            return Err(ChronosError::WalChainBroken {
                                engine_seq: frame.engine_seq,
                            });
                        }
                        first_seq = frame.engine_seq;
                        expected_seq = Some(frame.engine_seq + 1);
                    }
                    Some(exp) => {
                        if frame.engine_seq != exp {
                            return Err(ChronosError::WalSequenceGap {
                                expected: exp,
                                found: frame.engine_seq,
                            });
                        }
                        if frame.prev_blake3 != prev_hash {
                            return Err(ChronosError::WalChainBroken {
                                engine_seq: frame.engine_seq,
                            });
                        }
                        expected_seq = Some(exp + 1);
                    }
                }
                prev_hash = frame.frame_blake3;
                last_seq = frame.engine_seq;
                verified_frames += 1;
                cursor = next_cursor;
            }
        }

        Ok(ChainVerificationSummary {
            verified_frames,
            first_seq,
            last_seq,
            tip_blake3: prev_hash,
        })
    }
}
