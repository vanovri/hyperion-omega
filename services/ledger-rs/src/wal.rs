use memmap2::MmapOptions;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Exact 8-byte segment file magic per `00-hyperion-contract.md` Section 6.1.
pub const WAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const WAL_VERSION: u32 = 1;
pub const WAL_FILE_HEADER_LEN: usize = 16;
pub const WAL_FRAME_HEADER_LEN: usize = 88; // 4 + 4 + 8 + 8 + 32 + 32

/// Exact 8-byte snapshot file magic per `00-hyperion-contract.md` Section 6.1.
pub const SNAPSHOT_MAGIC: &[u8; 8] = b"HYPSNP01";
pub const SNAPSHOT_VERSION: u32 = 1;
pub const SNAPSHOT_HEADER_LEN: usize = 72; // 8 + 4 + 4 + 8 + 8 + 32 + 8

#[derive(Debug, Error)]
pub enum WalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid WAL/Snapshot magic header")]
    InvalidMagic,
    #[error("unsupported format version: {0}")]
    UnsupportedVersion(u32),
    #[error("CRC32C checksum mismatch at engine_seq {engine_seq}")]
    CrcMismatch { engine_seq: u64 },
    #[error("Blake3 hash chain broken at engine_seq {engine_seq}")]
    HashChainBroken { engine_seq: u64 },
    #[error("non-monotonic engine_seq: expected {expected}, got {actual}")]
    SequenceGap { expected: u64, actual: u64 },
    #[error("snapshot payload decode error: {0}")]
    SnapshotCodec(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalFrame {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub prev_blake3: [u8; 32],
    pub frame_blake3: [u8; 32],
    pub crc32c: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalSparseIndexEntry {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub file_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalSyncMode {
    /// Writes into pre-allocated memory group-commit buffer and flushes on batch threshold or explicit flush (`< 300 ns` hot path).
    GroupCommit { flush_every_bytes: usize },
    /// Flushes OS buffer + `sync_data()` on every frame (strict synchronous durability).
    SyncEveryFrame,
    /// Pure in-memory disabled disk I/O mode (used only for isolated CPU micro-benchmarks when requested).
    InMemoryChainOnly,
}

/// Computes `blake3(engine_seq_le || ts_ns_le || prev_blake3 || payload)` per Section 6.1.
/// Uses a zero-heap stack fast path for frames `<= 464 B` to avoid 2 KB `Hasher` stack initialization
/// while producing 100% bit-identical standard BLAKE3 digests.
#[inline(always)]
pub fn compute_wal_frame_blake3(
    engine_seq: u64,
    ts_ns: u64,
    prev_blake3: &[u8; 32],
    payload: &[u8],
) -> [u8; 32] {
    if payload.len() <= 464 {
        let mut buf = [0u8; 512];
        buf[0..8].copy_from_slice(&engine_seq.to_le_bytes());
        buf[8..16].copy_from_slice(&ts_ns.to_le_bytes());
        buf[16..48].copy_from_slice(prev_blake3);
        let end = 48 + payload.len();
        buf[48..end].copy_from_slice(payload);
        return *blake3::hash(&buf[..end]).as_bytes();
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&engine_seq.to_le_bytes());
    hasher.update(&ts_ns.to_le_bytes());
    hasher.update(prev_blake3);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Computes hardware Castagnoli CRC-32C over `engine_seq_le || ts_ns_le || prev_blake3 || payload` per Section 6.1.
#[inline(always)]
pub fn compute_wal_frame_crc32c(
    engine_seq: u64,
    ts_ns: u64,
    prev_blake3: &[u8; 32],
    payload: &[u8],
) -> u32 {
    let mut crc = 0u32;
    crc = crc32c::crc32c_append(crc, &engine_seq.to_le_bytes());
    crc = crc32c::crc32c_append(crc, &ts_ns.to_le_bytes());
    crc = crc32c::crc32c_append(crc, prev_blake3);
    crc32c::crc32c_append(crc, payload)
}

/// High-throughput Blake3 hash-chained Write-Ahead Log writer (`HYPWAL01`).
pub struct WalWriter {
    file: Option<File>,
    segment_path: Option<PathBuf>,
    sync_mode: WalSyncMode,
    write_buf: Vec<u8>,
    last_blake3: [u8; 32],
    last_engine_seq: u64,
}

impl WalWriter {
    /// Creates an in-memory-only hash-chain tracker (computes real CRC32C + Blake3 hashes with 0 disk syscalls).
    pub fn in_memory() -> Self {
        Self {
            file: None,
            segment_path: None,
            sync_mode: WalSyncMode::InMemoryChainOnly,
            write_buf: Vec::new(),
            last_blake3: [0u8; 32],
            last_engine_seq: 0,
        }
    }

    /// Opens or initializes a `HYPWAL01` segment file on disk. If existing frames exist, validates & recovers
    /// any torn/corrupted tail frame cleanly back to the last valid frame boundary!
    pub fn open_segment(
        path: impl AsRef<Path>,
        sync_mode: WalSyncMode,
    ) -> Result<(Self, Vec<WalFrame>, bool), WalError> {
        let seg_path = path.as_ref().to_path_buf();
        if let Some(parent) = seg_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if seg_path.exists() && fs::metadata(&seg_path)?.len() > 0 {
            let recovery = WalReader::recover_segment(&seg_path)?;
            let mut file = OpenOptions::new().read(true).write(true).open(&seg_path)?;
            if recovery.truncated_corrupt_tail {
                file.set_len(recovery.valid_byte_len)?;
                file.sync_all()?;
            }
            file.seek(SeekFrom::End(0))?;
            let (last_seq, last_hash) = recovery
                .frames
                .last()
                .map(|f| (f.engine_seq, f.frame_blake3))
                .unwrap_or((0, [0u8; 32]));
            let buf_cap = match sync_mode {
                WalSyncMode::GroupCommit { flush_every_bytes } => flush_every_bytes.max(65_536) * 2,
                _ => 65_536,
            };
            Ok((
                Self {
                    file: Some(file),
                    segment_path: Some(seg_path),
                    sync_mode,
                    write_buf: Vec::with_capacity(buf_cap),
                    last_blake3: last_hash,
                    last_engine_seq: last_seq,
                },
                recovery.frames,
                recovery.truncated_corrupt_tail,
            ))
        } else {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&seg_path)?;
            let mut hdr = [0u8; WAL_FILE_HEADER_LEN];
            hdr[0..8].copy_from_slice(WAL_MAGIC);
            hdr[8..12].copy_from_slice(&WAL_VERSION.to_le_bytes());
            hdr[12..16].copy_from_slice(&0u32.to_le_bytes());
            file.write_all(&hdr)?;
            file.sync_all()?;

            let buf_cap = match sync_mode {
                WalSyncMode::GroupCommit { flush_every_bytes } => flush_every_bytes.max(65_536) * 2,
                _ => 65_536,
            };
            Ok((
                Self {
                    file: Some(file),
                    segment_path: Some(seg_path),
                    sync_mode,
                    write_buf: Vec::with_capacity(buf_cap),
                    last_blake3: [0u8; 32],
                    last_engine_seq: 0,
                },
                Vec::new(),
                false,
            ))
        }
    }

    #[inline(always)]
    pub const fn last_blake3(&self) -> [u8; 32] {
        self.last_blake3
    }

    #[inline(always)]
    pub const fn last_engine_seq(&self) -> u64 {
        self.last_engine_seq
    }

    pub fn set_chain_anchor(&mut self, engine_seq: u64, last_blake3: [u8; 32]) {
        self.last_engine_seq = engine_seq;
        self.last_blake3 = last_blake3;
    }

    pub fn segment_path(&self) -> Option<&Path> {
        self.segment_path.as_deref()
    }

    /// Appends a Protobuf `MarketEventEnvelope` payload as a cryptographically chained `HYPWAL01` frame.
    #[inline]
    pub fn append_frame(
        &mut self,
        engine_seq: u64,
        ts_ns: u64,
        payload: &[u8],
    ) -> Result<[u8; 32], WalError> {
        if self.last_engine_seq != 0 && engine_seq != self.last_engine_seq + 1 {
            return Err(WalError::SequenceGap {
                expected: self.last_engine_seq + 1,
                actual: engine_seq,
            });
        }
        let prev = self.last_blake3;
        let crc = compute_wal_frame_crc32c(engine_seq, ts_ns, &prev, payload);
        let frame_hash = compute_wal_frame_blake3(engine_seq, ts_ns, &prev, payload);

        if let Some(file) = &mut self.file {
            let payload_len = payload.len() as u32;
            self.write_buf.extend_from_slice(&payload_len.to_le_bytes());
            self.write_buf.extend_from_slice(&crc.to_le_bytes());
            self.write_buf.extend_from_slice(&engine_seq.to_le_bytes());
            self.write_buf.extend_from_slice(&ts_ns.to_le_bytes());
            self.write_buf.extend_from_slice(&prev);
            self.write_buf.extend_from_slice(&frame_hash);
            self.write_buf.extend_from_slice(payload);

            match self.sync_mode {
                WalSyncMode::GroupCommit { flush_every_bytes } => {
                    if self.write_buf.len() >= flush_every_bytes {
                        file.write_all(&self.write_buf)?;
                        file.sync_data()?;
                        self.write_buf.clear();
                    }
                }
                WalSyncMode::SyncEveryFrame => {
                    file.write_all(&self.write_buf)?;
                    file.sync_data()?;
                    self.write_buf.clear();
                }
                WalSyncMode::InMemoryChainOnly => {
                    self.write_buf.clear();
                }
            }
        }

        self.last_blake3 = frame_hash;
        self.last_engine_seq = engine_seq;
        Ok(frame_hash)
    }

    /// Forces an immediate group-commit flush + `fsync` to persistent storage.
    pub fn flush_and_sync(&mut self) -> Result<(), WalError> {
        if let Some(file) = &mut self.file {
            if !self.write_buf.is_empty() {
                file.write_all(&self.write_buf)?;
                self.write_buf.clear();
            }
            file.sync_data()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct WalRecoveryReport {
    pub frames: Vec<WalFrame>,
    pub sparse_index: Vec<WalSparseIndexEntry>,
    pub valid_byte_len: u64,
    pub truncated_corrupt_tail: bool,
}

/// Memory-mapped (`memmap2`) zero-copy WAL reader, cryptographic chain verifier, and tail-corruption healer.
pub struct WalReader;

impl WalReader {
    /// Reads a `HYPWAL01` segment via memory-mapped I/O (`memmap2`), verifying every CRC32C and Blake3 link.
    /// If the final frame at the end of the file is truncated or has a corrupted tail byte/CRC (simulating power loss
    /// mid-write), cleanly stops at `valid_byte_len` and sets `truncated_corrupt_tail = true`.
    /// Mid-stream hash chain breaks before a valid subsequent frame still return `Err(WalError::HashChainBroken)`.
    pub fn recover_segment(path: impl AsRef<Path>) -> Result<WalRecoveryReport, WalError> {
        let file = File::open(path.as_ref())?;
        let file_len = file.metadata()?.len() as usize;
        if file_len < WAL_FILE_HEADER_LEN {
            return Err(WalError::InvalidMagic);
        }
        // SAFETY: Read-only mapping of local WAL segment file for deterministic frame parsing.
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Self::parse_bytes(&mmap, true)
    }

    /// Strict verification mode (rejects even tail corruption with `Err`).
    pub fn verify_segment_strict(path: impl AsRef<Path>) -> Result<WalRecoveryReport, WalError> {
        let file = File::open(path.as_ref())?;
        let file_len = file.metadata()?.len() as usize;
        if file_len < WAL_FILE_HEADER_LEN {
            return Err(WalError::InvalidMagic);
        }
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Self::parse_bytes(&mmap, false)
    }

    pub fn parse_bytes(
        bytes: &[u8],
        tolerate_tail_corruption: bool,
    ) -> Result<WalRecoveryReport, WalError> {
        if bytes.len() < WAL_FILE_HEADER_LEN || &bytes[0..8] != WAL_MAGIC {
            return Err(WalError::InvalidMagic);
        }
        let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        if version != WAL_VERSION {
            return Err(WalError::UnsupportedVersion(version));
        }

        let mut offset = WAL_FILE_HEADER_LEN;
        let mut frames = Vec::new();
        let mut sparse_index = Vec::new();
        let mut expected_prev: Option<[u8; 32]> = None;
        let mut expected_seq: Option<u64> = None;
        let mut truncated_corrupt_tail = false;

        while offset < bytes.len() {
            if offset + WAL_FRAME_HEADER_LEN > bytes.len() {
                if tolerate_tail_corruption {
                    truncated_corrupt_tail = true;
                    break;
                } else {
                    return Err(WalError::CrcMismatch {
                        engine_seq: expected_seq.unwrap_or(1),
                    });
                }
            }

            let hdr = &bytes[offset..offset + WAL_FRAME_HEADER_LEN];
            let payload_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
            let stored_crc = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
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

            let frame_end = match (offset + WAL_FRAME_HEADER_LEN).checked_add(payload_len) {
                Some(end) if end <= bytes.len() => end,
                _ => {
                    if tolerate_tail_corruption {
                        truncated_corrupt_tail = true;
                        break;
                    } else {
                        return Err(WalError::CrcMismatch { engine_seq });
                    }
                }
            };

            let payload = &bytes[offset + WAL_FRAME_HEADER_LEN..frame_end];
            let calc_crc = compute_wal_frame_crc32c(engine_seq, ts_ns, &prev_blake3, payload);
            if calc_crc != stored_crc {
                // Only treat as torn tail if this corrupt frame is at the end (no valid frame follows, or last frame)
                if tolerate_tail_corruption && frame_end == bytes.len() {
                    truncated_corrupt_tail = true;
                    break;
                }
                // What if payload_len itself was corrupted near EOF? If remaining bytes <= 64 KB at tail, truncate tail cleanly.
                if tolerate_tail_corruption && (bytes.len() - offset) <= 65_536 {
                    truncated_corrupt_tail = true;
                    break;
                }
                return Err(WalError::CrcMismatch { engine_seq });
            }

            if let Some(exp_s) = expected_seq {
                if engine_seq != exp_s {
                    return Err(WalError::SequenceGap {
                        expected: exp_s,
                        actual: engine_seq,
                    });
                }
            }
            if let Some(exp_prev) = expected_prev {
                if prev_blake3 != exp_prev {
                    return Err(WalError::HashChainBroken { engine_seq });
                }
            } else if engine_seq == 1 && prev_blake3 != [0u8; 32] {
                return Err(WalError::HashChainBroken { engine_seq });
            }

            let calc_hash = compute_wal_frame_blake3(engine_seq, ts_ns, &prev_blake3, payload);
            if calc_hash != frame_blake3 {
                if tolerate_tail_corruption && frame_end == bytes.len() {
                    truncated_corrupt_tail = true;
                    break;
                }
                return Err(WalError::HashChainBroken { engine_seq });
            }

            if frames.len() % 64 == 0 {
                sparse_index.push(WalSparseIndexEntry {
                    engine_seq,
                    ts_ns,
                    file_offset: offset as u64,
                });
            }

            frames.push(WalFrame {
                engine_seq,
                ts_ns,
                prev_blake3,
                frame_blake3,
                crc32c: stored_crc,
                payload: payload.to_vec(),
            });

            expected_prev = Some(frame_blake3);
            expected_seq = Some(engine_seq + 1);
            offset = frame_end;
        }

        Ok(WalRecoveryReport {
            frames,
            sparse_index,
            valid_byte_len: offset as u64,
            truncated_corrupt_tail,
        })
    }
}

/// Deterministic serializable state payload stored inside `HYPSNP01` snapshots.
/// Uses canonical JSON/UTF-8 deterministic schema inside `payload` so both `ledger-rs` and external
/// polyglot/Rust tools (`chronos-replay`) can inspect or reconstruct complete exchange state losslessly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPayloadV1 {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub journal_seq: u64,
    pub last_wal_blake3: [u8; 32],
    pub last_journal_blake3: [u8; 32],
    pub next_order_id: u64,
    pub next_fill_id: u64,
    pub state_root_blake3: [u8; 32],
    pub accounts: Vec<SnapshotAccountV1>,
    pub asset_totals: Vec<SnapshotAssetTotalV1>,
    pub instruments: Vec<SnapshotInstrumentV1>,
    pub exec_leaf_hashes: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAccountV1 {
    pub account_id: u64,
    pub status: u8,
    pub balances: Vec<SnapshotAssetBalanceV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAssetBalanceV1 {
    pub asset: String,
    pub available_fp: String,
    pub locked_fp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAssetTotalV1 {
    pub asset: String,
    pub total_deposits_fp: String,
    pub total_available_fp: String,
    pub total_locked_fp: String,
    pub treasury_fee_pool_fp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInstrumentV1 {
    pub instrument_id: u32,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub regime: u8,
    pub maker_fee_ppm: i64,
    pub taker_fee_ppm: i64,
    pub prev_l3_seq: u64,
    pub resting_orders: Vec<SnapshotOrderV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotOrderV1 {
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

/// Computes `crc32c(engine_seq_le || ts_ns_le || state_root_blake3 || payload)` per Section 6.1 `HYPSNP01`.
#[inline]
pub fn compute_snapshot_crc32c(
    engine_seq: u64,
    ts_ns: u64,
    state_root_blake3: &[u8; 32],
    payload: &[u8],
) -> u32 {
    let mut crc = 0u32;
    crc = crc32c::crc32c_append(crc, &engine_seq.to_le_bytes());
    crc = crc32c::crc32c_append(crc, &ts_ns.to_le_bytes());
    crc = crc32c::crc32c_append(crc, state_root_blake3);
    crc32c::crc32c_append(crc, payload)
}

pub struct SnapshotStore;

impl SnapshotStore {
    /// Writes an atomic `HYPSNP01` snapshot file (`write temp -> fsync -> atomic rename`).
    pub fn write_snapshot(
        dir: impl AsRef<Path>,
        snap: &SnapshotPayloadV1,
    ) -> Result<PathBuf, WalError> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let final_path = dir.join(format!("snap_{:016}.snap", snap.engine_seq));
        let tmp_path = dir.join(format!(".snap_{:016}.snap.tmp", snap.engine_seq));

        let payload =
            serde_json::to_vec(snap).map_err(|e| WalError::SnapshotCodec(e.to_string()))?;
        let crc = compute_snapshot_crc32c(
            snap.engine_seq,
            snap.ts_ns,
            &snap.state_root_blake3,
            &payload,
        );

        let mut hdr = [0u8; SNAPSHOT_HEADER_LEN];
        hdr[0..8].copy_from_slice(SNAPSHOT_MAGIC);
        hdr[8..12].copy_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
        hdr[12..16].copy_from_slice(&crc.to_le_bytes());
        hdr[16..24].copy_from_slice(&snap.engine_seq.to_le_bytes());
        hdr[24..32].copy_from_slice(&snap.ts_ns.to_le_bytes());
        hdr[32..64].copy_from_slice(&snap.state_root_blake3);
        hdr[64..72].copy_from_slice(&(payload.len() as u64).to_le_bytes());

        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(&hdr)?;
            f.write_all(&payload)?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        Ok(final_path)
    }

    /// Reads and cryptographically verifies a `HYPSNP01` snapshot file via `memmap2`.
    pub fn read_snapshot(path: impl AsRef<Path>) -> Result<SnapshotPayloadV1, WalError> {
        let file = File::open(path.as_ref())?;
        let len = file.metadata()?.len() as usize;
        if len < SNAPSHOT_HEADER_LEN {
            return Err(WalError::InvalidMagic);
        }
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        if &mmap[0..8] != SNAPSHOT_MAGIC {
            return Err(WalError::InvalidMagic);
        }
        let version = u32::from_le_bytes([mmap[8], mmap[9], mmap[10], mmap[11]]);
        if version != SNAPSHOT_VERSION {
            return Err(WalError::UnsupportedVersion(version));
        }
        let stored_crc = u32::from_le_bytes([mmap[12], mmap[13], mmap[14], mmap[15]]);
        let engine_seq = u64::from_le_bytes([
            mmap[16], mmap[17], mmap[18], mmap[19], mmap[20], mmap[21], mmap[22], mmap[23],
        ]);
        let ts_ns = u64::from_le_bytes([
            mmap[24], mmap[25], mmap[26], mmap[27], mmap[28], mmap[29], mmap[30], mmap[31],
        ]);
        let mut state_root = [0u8; 32];
        state_root.copy_from_slice(&mmap[32..64]);
        let payload_len = u64::from_le_bytes([
            mmap[64], mmap[65], mmap[66], mmap[67], mmap[68], mmap[69], mmap[70], mmap[71],
        ]) as usize;

        if SNAPSHOT_HEADER_LEN.saturating_add(payload_len) != len {
            return Err(WalError::CrcMismatch { engine_seq });
        }
        let payload = &mmap[SNAPSHOT_HEADER_LEN..SNAPSHOT_HEADER_LEN + payload_len];
        let calc_crc = compute_snapshot_crc32c(engine_seq, ts_ns, &state_root, payload);
        if calc_crc != stored_crc {
            return Err(WalError::CrcMismatch { engine_seq });
        }

        let decoded: SnapshotPayloadV1 =
            serde_json::from_slice(payload).map_err(|e| WalError::SnapshotCodec(e.to_string()))?;
        if decoded.state_root_blake3 != state_root || decoded.engine_seq != engine_seq {
            return Err(WalError::HashChainBroken { engine_seq });
        }
        Ok(decoded)
    }

    /// Finds the latest valid snapshot `<= max_engine_seq` in `dir`.
    pub fn find_latest_snapshot_before(
        dir: impl AsRef<Path>,
        max_engine_seq: u64,
    ) -> Result<Option<SnapshotPayloadV1>, WalError> {
        let dir = dir.as_ref();
        if !dir.exists() {
            return Ok(None);
        }
        let mut candidates: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.starts_with("snap_") && s.ends_with(".snap"))
                    .unwrap_or(false)
            })
            .collect();
        candidates.sort();
        for path in candidates.into_iter().rev() {
            if let Ok(snap) = Self::read_snapshot(&path) {
                if snap.engine_seq <= max_engine_seq {
                    return Ok(Some(snap));
                }
            }
        }
        Ok(None)
    }
}
