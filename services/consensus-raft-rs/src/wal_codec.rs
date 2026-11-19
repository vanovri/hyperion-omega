//! Canonical `HYPWAL01` Write-Ahead Log, `HYPSNP01` Snapshot, and `HYPRSB01` Dual Ping-Pong Superblock
//! binary codecs with Castagnoli CRC-32C + 32-byte Blake3 cryptographic hash chaining.
//!
//! Wire layout matches `.agents/rules/00-hyperion-contract.md` §6.1 & `01-hyperion-omega.md` §6.2 byte-for-byte.

use crate::traits::StorageError;
use crate::types::{
    ClusterConfig, HardState, LogEntry, LogIndex, NodeId, RaftEntryKind, SnapshotArtifact,
    SnapshotMetadata, Term,
};
use serde::{Deserialize, Serialize};

pub const HYPWAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const HYPSNP_MAGIC: &[u8; 8] = b"HYPSNP01";
pub const HYPRSB_MAGIC: &[u8; 8] = b"HYPRSB01";
pub const WIRE_VERSION_V1: u32 = 1;

/// 16-byte `HYPWAL01` segment header (`magic[8] || version:u32 || flags:u32`).
pub const WAL_SEGMENT_HEADER_LEN: usize = 16;

/// Fixed per-frame header + hashes size before variable `payload`:
/// `payload_len(4) + crc32c(4) + engine_seq(8) + ts_ns(8) + prev_blake3(32) + frame_blake3(32) = 88` bytes.
pub const WAL_FRAME_FIXED_LEN: usize = 88;

/// Fixed `HYPSNP01` header before variable `payload`:
/// `magic(8) + version(4) + crc32c(4) + engine_seq(8) + ts_ns(8) + state_root_blake3(32) + payload_len(8) = 72` bytes.
pub const SNAPSHOT_HEADER_LEN: usize = 72;

/// 4 KiB sector size for dual ping-pong superblocks (`Slot A` @ `0..4096`, `Slot B` @ `4096..8192`).
pub const SUPERBLOCK_SECTOR_BYTES: usize = 4096;
pub const SUPERBLOCK_RECORD_BYTES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WalPayloadEnvelope {
    term: Term,
    kind: RaftEntryKind,
}

/// Encode `(term, kind)` deterministically into canonical little-endian binary payload bytes,
/// and construct a cryptographically chained `LogEntry` matching `HYPWAL01`.
pub fn build_chained_log_entry(
    index: LogIndex,
    term: Term,
    ts_ns: u64,
    prev_wal_blake3: [u8; 32],
    kind: RaftEntryKind,
) -> LogEntry {
    let payload_bytes = encode_entry_payload(term, &kind);
    let wal_blake3 = compute_wal_frame_blake3(index, ts_ns, &prev_wal_blake3, &payload_bytes);
    let payload_crc32c = compute_wal_frame_crc32c(index, ts_ns, &prev_wal_blake3, &payload_bytes);
    LogEntry {
        index,
        term,
        ts_ns,
        prev_wal_blake3,
        wal_blake3,
        payload_crc32c,
        kind,
        payload_bytes,
    }
}

/// Deterministic compact binary encoding of `(term, RaftEntryKind)` (independent of JSON whitespace/maps).
pub fn encode_entry_payload(term: Term, kind: &RaftEntryKind) -> Vec<u8> {
    let env = WalPayloadEnvelope {
        term,
        kind: kind.clone(),
    };
    let json = serde_json::to_vec(&env).unwrap_or_default();
    let mut buf = Vec::with_capacity(12 + json.len());
    buf.extend_from_slice(&term.to_le_bytes());
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

pub fn decode_entry_payload(bytes: &[u8]) -> Result<(Term, RaftEntryKind), StorageError> {
    if bytes.len() < 12 {
        return Err(StorageError::IoFailure {
            reason: "entry payload shorter than 12-byte envelope header".to_string(),
        });
    }
    let term = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    let json_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if 12 + json_len != bytes.len() {
        return Err(StorageError::IoFailure {
            reason: "entry payload length header mismatch".to_string(),
        });
    }
    let env: WalPayloadEnvelope =
        serde_json::from_slice(&bytes[12..]).map_err(|e| StorageError::IoFailure {
            reason: format!("corrupt entry JSON payload: {e}"),
        })?;
    if env.term != term {
        return Err(StorageError::IoFailure {
            reason: "entry term mismatch inside payload envelope".to_string(),
        });
    }
    Ok((env.term, env.kind))
}

/// Exact `00-hyperion-contract.md` §6.1 `frame_blake3`:
/// `blake3(engine_seq_le || ts_ns_le || prev_blake3 || payload)`
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

/// Exact `00-hyperion-contract.md` §6.1 Castagnoli `CRC-32C`:
/// `crc32c(engine_seq_le || ts_ns_le || prev_blake3 || payload)`
#[inline]
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

/// Verify an in-memory `LogEntry`'s internal CRC32C and Blake3 hash chain against `expected_prev_blake3`.
pub fn verify_entry_integrity(
    entry: &LogEntry,
    expected_prev_blake3: &[u8; 32],
    byte_offset: usize,
) -> Result<(), StorageError> {
    let actual_crc = compute_wal_frame_crc32c(
        entry.index,
        entry.ts_ns,
        &entry.prev_wal_blake3,
        &entry.payload_bytes,
    );
    if actual_crc != entry.payload_crc32c {
        return Err(StorageError::WalFrameBitRotCrc {
            index: entry.index,
            offset: byte_offset,
            expected_crc32c: entry.payload_crc32c,
            actual_crc32c: actual_crc,
        });
    }
    if &entry.prev_wal_blake3 != expected_prev_blake3 {
        return Err(StorageError::WalHashChainMismatch {
            index: entry.index,
            expected_hex: hex32(expected_prev_blake3),
            actual_hex: hex32(&entry.prev_wal_blake3),
        });
    }
    let expected_frame_hash = compute_wal_frame_blake3(
        entry.index,
        entry.ts_ns,
        &entry.prev_wal_blake3,
        &entry.payload_bytes,
    );
    if entry.wal_blake3 != expected_frame_hash {
        return Err(StorageError::WalHashChainMismatch {
            index: entry.index,
            expected_hex: hex32(&expected_frame_hash),
            actual_hex: hex32(&entry.wal_blake3),
        });
    }
    Ok(())
}

/// Encode the 16-byte `HYPWAL01` segment file header.
pub fn encode_wal_segment_header(flags: u32) -> [u8; WAL_SEGMENT_HEADER_LEN] {
    let mut out = [0u8; WAL_SEGMENT_HEADER_LEN];
    out[0..8].copy_from_slice(HYPWAL_MAGIC);
    out[8..12].copy_from_slice(&WIRE_VERSION_V1.to_le_bytes());
    out[12..16].copy_from_slice(&flags.to_le_bytes());
    out
}

pub fn verify_wal_segment_header(buf: &[u8]) -> Result<u32, StorageError> {
    if buf.len() < WAL_SEGMENT_HEADER_LEN {
        return Err(StorageError::IoFailure {
            reason: "WAL segment shorter than 16-byte HYPWAL01 header".to_string(),
        });
    }
    if &buf[0..8] != HYPWAL_MAGIC {
        return Err(StorageError::IoFailure {
            reason: "invalid HYPWAL01 segment magic".to_string(),
        });
    }
    let ver = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if ver != WIRE_VERSION_V1 {
        return Err(StorageError::IoFailure {
            reason: format!("unsupported HYPWAL01 version {ver}"),
        });
    }
    let flags = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    Ok(flags)
}

/// Append a single `HYPWAL01` frame into `dst`.
pub fn append_wal_frame_bytes(dst: &mut Vec<u8>, entry: &LogEntry) {
    let payload_len = entry.payload_bytes.len() as u32;
    dst.reserve(WAL_FRAME_FIXED_LEN + entry.payload_bytes.len());
    dst.extend_from_slice(&payload_len.to_le_bytes());
    dst.extend_from_slice(&entry.payload_crc32c.to_le_bytes());
    dst.extend_from_slice(&entry.index.to_le_bytes());
    dst.extend_from_slice(&entry.ts_ns.to_le_bytes());
    dst.extend_from_slice(&entry.prev_wal_blake3);
    dst.extend_from_slice(&entry.wal_blake3);
    dst.extend_from_slice(&entry.payload_bytes);
}

/// Outcome of scanning a raw `HYPWAL01` byte slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedWalSegment {
    pub entries: Vec<LogEntry>,
    pub valid_bytes_end: usize,
    pub torn_tail_bytes: usize,
}

/// Parse and cryptographically verify a `HYPWAL01` segment.
///
/// - `allow_unsealed_tail_truncation`:
///   - `false` (`scrub_and_verify` mode): ANY truncated byte or bad CRC/Blake3 anywhere is an immediate error!
///   - `true` (`recover_after_restart` mode): frames `<= sealed_max_index` MUST pass 100% CRC32C & Blake3 checks
///     (any failure raises `WalFrameBitRotCrc` / `WalHashChainMismatch`), whereas any partially written / torn bytes
///     strictly AFTER `sealed_max_index` are recognized as power-loss torn tail writes and safely truncated!
pub fn decode_and_verify_wal_segment(
    raw: &[u8],
    initial_prev_blake3: [u8; 32],
    expected_first_index: LogIndex,
    sealed_max_index: LogIndex,
    allow_unsealed_tail_truncation: bool,
) -> Result<DecodedWalSegment, StorageError> {
    if raw.is_empty() {
        if sealed_max_index >= expected_first_index {
            return Err(StorageError::WalHashChainMismatch {
                index: expected_first_index,
                expected_hex: "non-empty sealed WAL".to_string(),
                actual_hex: "empty WAL".to_string(),
            });
        }
        return Ok(DecodedWalSegment {
            entries: Vec::new(),
            valid_bytes_end: 0,
            torn_tail_bytes: 0,
        });
    }

    if raw.len() < WAL_SEGMENT_HEADER_LEN {
        if allow_unsealed_tail_truncation && sealed_max_index < expected_first_index {
            return Ok(DecodedWalSegment {
                entries: Vec::new(),
                valid_bytes_end: 0,
                torn_tail_bytes: raw.len(),
            });
        }
        return Err(StorageError::IoFailure {
            reason: "corrupted/truncated HYPWAL01 header on sealed segment".to_string(),
        });
    }

    verify_wal_segment_header(raw)?;

    let mut offset = WAL_SEGMENT_HEADER_LEN;
    let mut entries = Vec::new();
    let mut prev_hash = initial_prev_blake3;
    let mut next_expected_idx = expected_first_index;

    while offset < raw.len() {
        let remaining = &raw[offset..];
        // Trailing all-zero padding check if unsealed tail truncation is allowed.
        if allow_unsealed_tail_truncation
            && next_expected_idx > sealed_max_index
            && remaining.iter().all(|&b| b == 0)
        {
            return Ok(DecodedWalSegment {
                entries,
                valid_bytes_end: offset,
                torn_tail_bytes: remaining.len(),
            });
        }

        if remaining.len() < WAL_FRAME_FIXED_LEN {
            if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                return Ok(DecodedWalSegment {
                    entries,
                    valid_bytes_end: offset,
                    torn_tail_bytes: remaining.len(),
                });
            }
            return Err(StorageError::WalFrameBitRotCrc {
                index: next_expected_idx,
                offset,
                expected_crc32c: 0xFFFF_FFFF,
                actual_crc32c: 0,
            });
        }

        let payload_len =
            u32::from_le_bytes([remaining[0], remaining[1], remaining[2], remaining[3]]) as usize;
        let expected_crc =
            u32::from_le_bytes([remaining[4], remaining[5], remaining[6], remaining[7]]);
        let engine_seq = u64::from_le_bytes([
            remaining[8],
            remaining[9],
            remaining[10],
            remaining[11],
            remaining[12],
            remaining[13],
            remaining[14],
            remaining[15],
        ]);
        let ts_ns = u64::from_le_bytes([
            remaining[16],
            remaining[17],
            remaining[18],
            remaining[19],
            remaining[20],
            remaining[21],
            remaining[22],
            remaining[23],
        ]);

        let frame_total_len = WAL_FRAME_FIXED_LEN.saturating_add(payload_len);
        if remaining.len() < frame_total_len {
            if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                return Ok(DecodedWalSegment {
                    entries,
                    valid_bytes_end: offset,
                    torn_tail_bytes: remaining.len(),
                });
            }
            return Err(StorageError::WalFrameBitRotCrc {
                index: next_expected_idx,
                offset,
                expected_crc32c: expected_crc,
                actual_crc32c: 0,
            });
        }

        let mut frame_prev_blake3 = [0u8; 32];
        frame_prev_blake3.copy_from_slice(&remaining[24..56]);
        let mut frame_blake3 = [0u8; 32];
        frame_blake3.copy_from_slice(&remaining[56..88]);
        let payload = &remaining[88..frame_total_len];

        let actual_crc = compute_wal_frame_crc32c(engine_seq, ts_ns, &frame_prev_blake3, payload);
        if actual_crc != expected_crc {
            if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                return Ok(DecodedWalSegment {
                    entries,
                    valid_bytes_end: offset,
                    torn_tail_bytes: remaining.len(),
                });
            }
            return Err(StorageError::WalFrameBitRotCrc {
                index: next_expected_idx,
                offset,
                expected_crc32c: expected_crc,
                actual_crc32c: actual_crc,
            });
        }

        if engine_seq != next_expected_idx || frame_prev_blake3 != prev_hash {
            if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                return Ok(DecodedWalSegment {
                    entries,
                    valid_bytes_end: offset,
                    torn_tail_bytes: remaining.len(),
                });
            }
            return Err(StorageError::WalHashChainMismatch {
                index: next_expected_idx,
                expected_hex: hex32(&prev_hash),
                actual_hex: hex32(&frame_prev_blake3),
            });
        }

        let computed_blake3 =
            compute_wal_frame_blake3(engine_seq, ts_ns, &frame_prev_blake3, payload);
        if computed_blake3 != frame_blake3 {
            if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                return Ok(DecodedWalSegment {
                    entries,
                    valid_bytes_end: offset,
                    torn_tail_bytes: remaining.len(),
                });
            }
            return Err(StorageError::WalHashChainMismatch {
                index: engine_seq,
                expected_hex: hex32(&computed_blake3),
                actual_hex: hex32(&frame_blake3),
            });
        }

        let (term, kind) = match decode_entry_payload(payload) {
            Ok(v) => v,
            Err(e) => {
                if allow_unsealed_tail_truncation && next_expected_idx > sealed_max_index {
                    return Ok(DecodedWalSegment {
                        entries,
                        valid_bytes_end: offset,
                        torn_tail_bytes: remaining.len(),
                    });
                }
                return Err(e);
            }
        };

        // In recovery mode, never resurrect entries beyond `sealed_max_index` if superblock rolled back!
        if allow_unsealed_tail_truncation && engine_seq > sealed_max_index {
            return Ok(DecodedWalSegment {
                entries,
                valid_bytes_end: offset,
                torn_tail_bytes: remaining.len(),
            });
        }

        prev_hash = frame_blake3;
        next_expected_idx += 1;
        offset += frame_total_len;

        entries.push(LogEntry {
            index: engine_seq,
            term,
            ts_ns,
            prev_wal_blake3: frame_prev_blake3,
            wal_blake3: frame_blake3,
            payload_crc32c: expected_crc,
            kind,
            payload_bytes: payload.to_vec(),
        });
    }

    if sealed_max_index >= next_expected_idx {
        return Err(StorageError::WalHashChainMismatch {
            index: next_expected_idx,
            expected_hex: format!("sealed_max_index={sealed_max_index}"),
            actual_hex: format!("decoded_up_to={}", next_expected_idx.saturating_sub(1)),
        });
    }

    Ok(DecodedWalSegment {
        entries,
        valid_bytes_end: offset,
        torn_tail_bytes: 0,
    })
}

/// Encode a canonical `HYPSNP01` snapshot (`00-hyperion-contract.md` §6.1 lines 198-206).
/// Uses compact fixed-width binary inner framing (`term:8 || last_wal_blake3:32 || cfg_len:4 || cfg_json || sm_len:4 || sm_bytes`)
/// so snapshot size is deterministic, compact, and zero-overhead.
pub fn encode_hypsnp01_snapshot(
    meta: &SnapshotMetadata,
    state_machine_bytes: Vec<u8>,
) -> SnapshotArtifact {
    let cfg_json = serde_json::to_vec(&meta.config).unwrap_or_default();
    let mut payload = Vec::with_capacity(48 + cfg_json.len() + state_machine_bytes.len());
    payload.extend_from_slice(&meta.last_included_term.to_le_bytes());
    payload.extend_from_slice(&meta.last_wal_blake3);
    payload.extend_from_slice(&(cfg_json.len() as u32).to_le_bytes());
    payload.extend_from_slice(&cfg_json);
    payload.extend_from_slice(&(state_machine_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&state_machine_bytes);

    let crc = compute_snapshot_crc32c(
        meta.last_included_index,
        meta.ts_ns,
        &meta.state_root_blake3,
        &payload,
    );

    let mut raw = Vec::with_capacity(SNAPSHOT_HEADER_LEN + payload.len());
    raw.extend_from_slice(HYPSNP_MAGIC);
    raw.extend_from_slice(&WIRE_VERSION_V1.to_le_bytes());
    raw.extend_from_slice(&crc.to_le_bytes());
    raw.extend_from_slice(&meta.last_included_index.to_le_bytes());
    raw.extend_from_slice(&meta.ts_ns.to_le_bytes());
    raw.extend_from_slice(&meta.state_root_blake3);
    raw.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    raw.extend_from_slice(&payload);

    SnapshotArtifact {
        meta: meta.clone(),
        raw_hypsnp_bytes: raw,
    }
}

pub fn decode_and_verify_hypsnp01_snapshot(
    raw: &[u8],
) -> Result<(SnapshotMetadata, Vec<u8>), StorageError> {
    if raw.len() < SNAPSHOT_HEADER_LEN {
        return Err(StorageError::SnapshotCorrupted {
            reason: "HYPSNP01 shorter than 72-byte header".to_string(),
        });
    }
    if &raw[0..8] != HYPSNP_MAGIC {
        return Err(StorageError::SnapshotCorrupted {
            reason: "invalid HYPSNP01 magic".to_string(),
        });
    }
    let ver = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    if ver != WIRE_VERSION_V1 {
        return Err(StorageError::SnapshotCorrupted {
            reason: format!("unsupported HYPSNP01 version {ver}"),
        });
    }
    let expected_crc = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
    let engine_seq = u64::from_le_bytes([
        raw[16], raw[17], raw[18], raw[19], raw[20], raw[21], raw[22], raw[23],
    ]);
    let ts_ns = u64::from_le_bytes([
        raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30], raw[31],
    ]);
    let mut state_root_blake3 = [0u8; 32];
    state_root_blake3.copy_from_slice(&raw[32..64]);
    let payload_len = u64::from_le_bytes([
        raw[64], raw[65], raw[66], raw[67], raw[68], raw[69], raw[70], raw[71],
    ]) as usize;

    if SNAPSHOT_HEADER_LEN.saturating_add(payload_len) != raw.len() {
        return Err(StorageError::SnapshotCorrupted {
            reason: "HYPSNP01 payload_len mismatch".to_string(),
        });
    }
    let payload = &raw[SNAPSHOT_HEADER_LEN..];
    let actual_crc = compute_snapshot_crc32c(engine_seq, ts_ns, &state_root_blake3, payload);
    if actual_crc != expected_crc {
        return Err(StorageError::SnapshotCorrupted {
            reason: format!(
                "HYPSNP01 CRC32C bit-rot: expected {expected_crc:#010x}, got {actual_crc:#010x}"
            ),
        });
    }

    if payload.len() < 48 {
        return Err(StorageError::SnapshotCorrupted {
            reason: "HYPSNP01 inner binary payload shorter than 48 bytes".to_string(),
        });
    }
    let last_included_term = u64::from_le_bytes([
        payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
        payload[7],
    ]);
    let mut last_wal_blake3 = [0u8; 32];
    last_wal_blake3.copy_from_slice(&payload[8..40]);
    let cfg_len =
        u32::from_le_bytes([payload[40], payload[41], payload[42], payload[43]]) as usize;
    let cfg_end = 44usize.saturating_add(cfg_len);
    if cfg_end.saturating_add(4) > payload.len() {
        return Err(StorageError::SnapshotCorrupted {
            reason: "HYPSNP01 config length out of bounds".to_string(),
        });
    }
    let config: ClusterConfig =
        serde_json::from_slice(&payload[44..cfg_end]).map_err(|e| {
            StorageError::SnapshotCorrupted {
                reason: format!("HYPSNP01 config JSON decode error: {e}"),
            }
        })?;
    let sm_len = u32::from_le_bytes([
        payload[cfg_end],
        payload[cfg_end + 1],
        payload[cfg_end + 2],
        payload[cfg_end + 3],
    ]) as usize;
    let sm_start = cfg_end + 4;
    if sm_start.saturating_add(sm_len) != payload.len() {
        return Err(StorageError::SnapshotCorrupted {
            reason: "HYPSNP01 state machine payload length mismatch".to_string(),
        });
    }
    let state_machine_bytes = payload[sm_start..].to_vec();

    let meta = SnapshotMetadata {
        last_included_index: engine_seq,
        last_included_term,
        ts_ns,
        last_wal_blake3,
        state_root_blake3,
        config,
    };
    Ok((meta, state_machine_bytes))
}

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

/// Dual Ping-Pong 4KiB Superblock record (`HYPRSB01`, 128 bytes inside a 4KiB sector).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuperblockRecord {
    pub generation: u64,
    pub hard_state: HardState,
    pub sealed_max_index: LogIndex,
    pub sealed_tip_blake3: [u8; 32],
}

pub fn encode_superblock_record(rec: &SuperblockRecord) -> [u8; SUPERBLOCK_RECORD_BYTES] {
    let mut buf = [0u8; SUPERBLOCK_RECORD_BYTES];
    buf[0..8].copy_from_slice(HYPRSB_MAGIC);
    buf[8..12].copy_from_slice(&WIRE_VERSION_V1.to_le_bytes());
    buf[16..24].copy_from_slice(&rec.generation.to_le_bytes());
    buf[24..32].copy_from_slice(&rec.hard_state.current_term.to_le_bytes());
    let voted_raw = rec.hard_state.voted_for.unwrap_or(0);
    buf[32..40].copy_from_slice(&voted_raw.to_le_bytes());
    buf[40..48].copy_from_slice(&rec.hard_state.compacted_index.to_le_bytes());
    buf[48..56].copy_from_slice(&rec.hard_state.compacted_term.to_le_bytes());
    buf[56..64].copy_from_slice(&rec.sealed_max_index.to_le_bytes());
    buf[64..96].copy_from_slice(&rec.sealed_tip_blake3);

    let body = &buf[0..96];
    let crc = crc32c::crc32c(body);
    buf[12..16].copy_from_slice(&crc.to_le_bytes());
    let mut hasher = blake3::Hasher::new();
    hasher.update(&buf[0..96]);
    buf[96..128].copy_from_slice(hasher.finalize().as_bytes());
    buf
}

pub fn decode_superblock_record(buf: &[u8]) -> Option<SuperblockRecord> {
    if buf.len() < SUPERBLOCK_RECORD_BYTES {
        return None;
    }
    if &buf[0..8] != HYPRSB_MAGIC {
        return None;
    }
    let ver = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if ver != WIRE_VERSION_V1 {
        return None;
    }
    let expected_crc = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    let mut check_header = [0u8; 96];
    check_header.copy_from_slice(&buf[0..96]);
    check_header[12..16].copy_from_slice(&[0, 0, 0, 0]);
    if crc32c::crc32c(&check_header) != expected_crc {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(&buf[0..96]);
    if hasher.finalize().as_bytes() != &buf[96..128] {
        return None;
    }

    let generation = u64::from_le_bytes([
        buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
    ]);
    let current_term = u64::from_le_bytes([
        buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
    ]);
    let voted_raw = u64::from_le_bytes([
        buf[32], buf[33], buf[34], buf[35], buf[36], buf[37], buf[38], buf[39],
    ]);
    let compacted_index = u64::from_le_bytes([
        buf[40], buf[41], buf[42], buf[43], buf[44], buf[45], buf[46], buf[47],
    ]);
    let compacted_term = u64::from_le_bytes([
        buf[48], buf[49], buf[50], buf[51], buf[52], buf[53], buf[54], buf[55],
    ]);
    let sealed_max_index = u64::from_le_bytes([
        buf[56], buf[57], buf[58], buf[59], buf[60], buf[61], buf[62], buf[63],
    ]);
    let mut sealed_tip_blake3 = [0u8; 32];
    sealed_tip_blake3.copy_from_slice(&buf[64..96]);

    Some(SuperblockRecord {
        generation,
        hard_state: HardState {
            current_term,
            voted_for: if voted_raw == 0 {
                None
            } else {
                Some(voted_raw as NodeId)
            },
            compacted_index,
            compacted_term,
        },
        sealed_max_index,
        sealed_tip_blake3,
    })
}

pub fn hex32(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}
