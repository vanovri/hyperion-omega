//! Block-accurate `SimulatedFaultyDisk` & `ProductionFileDisk` implementations of `DiskStorage`.
//!
//! `SimulatedFaultyDisk` separates volatile OS Page-Cache writes from durable NVMe/Platter sectors:
//! - Models `fsync` latency & Nemesis `fsync` stalls.
//! - Models sudden power-loss crashes dropping dirty writes OR leaving mid-block **Torn Writes**
//!   on the WAL tail or alternating Ping-Pong 4KiB Superblock slot (`HYPRSB01`).
//! - Models silent data corruption (**Bit-Rot**) flipping physical bits in durable WAL/Snapshot/Superblocks.

use crate::prng::DeterministicPrng;
use crate::traits::{DiskStorage, PersistBatch, RecoveredDiskState, StorageError};
use crate::types::{HardState, LogIndex, NodeId, SnapshotArtifact};
use crate::wal_codec::{
    append_wal_frame_bytes, decode_and_verify_hypsnp01_snapshot, decode_and_verify_wal_segment,
    decode_superblock_record, encode_superblock_record, encode_wal_segment_header,
    SuperblockRecord, SUPERBLOCK_RECORD_BYTES, SUPERBLOCK_SECTOR_BYTES, WAL_SEGMENT_HEADER_LEN,
};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct SimulatedFaultyDisk {
    pub node_id: NodeId,
    /// Two 4KiB sectors on durable platter (`Slot A` and `Slot B`).
    platter_sb_a: [u8; SUPERBLOCK_SECTOR_BYTES],
    platter_sb_b: [u8; SUPERBLOCK_SECTOR_BYTES],
    platter_wal: Vec<u8>,
    platter_snapshot: Option<Vec<u8>>,
    next_sb_generation: u64,

    /// Unflushed page-cache batches waiting for `fsync()`.
    dirty_queue: VecDeque<PersistBatch>,

    /// Configurable fault knobs controlled by Nemesis!
    pub base_fsync_latency_ns: u64,
    pub extra_fsync_stall_ns: u64,
    pub fail_next_fsync: bool,
}

impl SimulatedFaultyDisk {
    pub fn new(node_id: NodeId) -> Self {
        let mut disk = Self {
            node_id,
            platter_sb_a: [0u8; SUPERBLOCK_SECTOR_BYTES],
            platter_sb_b: [0u8; SUPERBLOCK_SECTOR_BYTES],
            platter_wal: encode_wal_segment_header(0).to_vec(),
            platter_snapshot: None,
            next_sb_generation: 1,
            dirty_queue: VecDeque::new(),
            base_fsync_latency_ns: 250_000, // 250 µs NVMe fsync
            extra_fsync_stall_ns: 0,
            fail_next_fsync: false,
        };
        // Initialize Genesis Superblock in Slot A (`generation = 1`).
        let genesis = SuperblockRecord {
            generation: 1,
            hard_state: HardState::default(),
            sealed_max_index: 0,
            sealed_tip_blake3: [0u8; 32],
        };
        let enc = encode_superblock_record(&genesis);
        disk.platter_sb_a[0..SUPERBLOCK_RECORD_BYTES].copy_from_slice(&enc);
        disk.next_sb_generation = 2;
        disk
    }

    pub fn effective_fsync_delay_ns(&self, prng: &mut DeterministicPrng) -> u64 {
        let jitter = prng.range_u64(0, 200_000);
        self.base_fsync_latency_ns
            .saturating_add(jitter)
            .saturating_add(self.extra_fsync_stall_ns)
    }

    pub fn read_active_superblock(&self) -> Result<SuperblockRecord, StorageError> {
        let a = decode_superblock_record(&self.platter_sb_a);
        let b = decode_superblock_record(&self.platter_sb_b);
        match (a, b) {
            (Some(ra), Some(rb)) => {
                if ra.generation >= rb.generation {
                    Ok(ra)
                } else {
                    Ok(rb)
                }
            }
            (Some(ra), None) => Ok(ra),
            (None, Some(rb)) => Ok(rb),
            (None, None) => Err(StorageError::SuperblockBitRot {
                node_id: self.node_id,
            }),
        }
    }

    pub fn sealed_max_index(&self) -> LogIndex {
        self.read_active_superblock()
            .map(|sb| sb.sealed_max_index)
            .unwrap_or(0)
    }

    /// Simulate sudden power loss!
    /// - All unflushed `dirty_queue` batches lose durability;
    /// - If `inject_torn_write == true`, half-writes a torn prefix of the first dirty WAL frame
    ///   AND/OR tears the inactive ping-pong superblock slot mid-sector!
    pub fn simulate_power_loss_crash(
        &mut self,
        prng: &mut DeterministicPrng,
        inject_torn_write: bool,
    ) {
        if inject_torn_write {
            if let Some(front) = self.dirty_queue.front() {
                if let Some(first_entry) = front.entries.first() {
                    let mut full_frame = Vec::new();
                    append_wal_frame_bytes(&mut full_frame, first_entry);
                    if full_frame.len() > 8 {
                        let tear_len = prng.range_u64(5, (full_frame.len() - 1) as u64) as usize;
                        self.platter_wal.extend_from_slice(&full_frame[..tear_len]);
                    }
                } else {
                    // Even if dirty_queue had only HardState, tear the *next* ping-pong superblock slot mid-write!
                    let target_slot_is_a = (self.next_sb_generation % 2) == 1;
                    let slot = if target_slot_is_a {
                        &mut self.platter_sb_a
                    } else {
                        &mut self.platter_sb_b
                    };
                    for b in &mut slot[0..48] {
                        *b ^= 0xA5;
                    }
                }
            } else {
                // Inject an unsealed torn frame stub at the tail of `platter_wal`
                let stub = [0x24, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0x01];
                self.platter_wal.extend_from_slice(&stub);
            }
        }
        self.dirty_queue.clear();
        self.extra_fsync_stall_ns = 0;
    }

    /// Inject silent data corruption (bit-rot) directly onto an already-durable `fsync`ed WAL frame!
    pub fn inject_wal_bit_rot(&mut self, prng: &mut DeterministicPrng) -> bool {
        if self.platter_wal.len() <= WAL_SEGMENT_HEADER_LEN + 16 {
            return false;
        }
        let pos = prng.range_u64(
            (WAL_SEGMENT_HEADER_LEN + 8) as u64,
            (self.platter_wal.len() - 1) as u64,
        ) as usize;
        let bit = 1u8 << (prng.range_u64(0, 7) as u8);
        self.platter_wal[pos] ^= bit;
        true
    }

    /// Inject silent bit-rot into BOTH ping-pong superblocks simultaneously.
    pub fn inject_dual_superblock_bit_rot(&mut self) {
        self.platter_sb_a[24] ^= 0x01;
        self.platter_sb_b[24] ^= 0x01;
    }

    fn apply_batch_to_platter(&mut self, batch: PersistBatch) -> Result<(), StorageError> {
        let mut active_sb = self.read_active_superblock()?;

        // 1. Snapshot installation & log prefix compaction if requested.
        if let Some(ref snap) = batch.install_snapshot {
            let (verified_meta, _) = decode_and_verify_hypsnp01_snapshot(&snap.raw_hypsnp_bytes)?;
            // Retain any existing durable WAL entries strictly > `verified_meta.last_included_index`.
            let init_prev = if active_sb.hard_state.compacted_index > 0 {
                if let Some(ref old_snap_raw) = self.platter_snapshot {
                    decode_and_verify_hypsnp01_snapshot(old_snap_raw)?.0.last_wal_blake3
                } else {
                    [0u8; 32]
                }
            } else {
                [0u8; 32]
            };
            let existing = decode_and_verify_wal_segment(
                &self.platter_wal,
                init_prev,
                active_sb.hard_state.compacted_index + 1,
                active_sb.sealed_max_index,
                false,
            )?;

            let mut rebuilt_wal = encode_wal_segment_header(0).to_vec();
            let mut tip_idx = verified_meta.last_included_index;
            let mut tip_blake3 = verified_meta.last_wal_blake3;
            for e in existing.entries {
                if e.index > verified_meta.last_included_index {
                    if e.index == tip_idx + 1 && e.prev_wal_blake3 == tip_blake3 {
                        append_wal_frame_bytes(&mut rebuilt_wal, &e);
                        tip_idx = e.index;
                        tip_blake3 = e.wal_blake3;
                    } else {
                        break;
                    }
                }
            }

            self.platter_snapshot = Some(snap.raw_hypsnp_bytes.clone());
            self.platter_wal = rebuilt_wal;
            active_sb.hard_state.compacted_index = verified_meta.last_included_index;
            active_sb.hard_state.compacted_term = verified_meta.last_included_term;
            active_sb.sealed_max_index = tip_idx;
            active_sb.sealed_tip_blake3 = tip_blake3;
        }

        // 2. Suffix truncation if conflicting uncommitted entries are overwritten.
        if let Some(trunc_from) = batch.truncate_from_index {
            let init_prev = self.current_wal_base_blake3(&active_sb)?;
            let first_idx = active_sb.hard_state.compacted_index + 1;
            let existing = decode_and_verify_wal_segment(
                &self.platter_wal,
                init_prev,
                first_idx,
                active_sb.sealed_max_index,
                false,
            )?;
            let mut rebuilt = encode_wal_segment_header(0).to_vec();
            let mut tip_idx = active_sb.hard_state.compacted_index;
            let mut tip_hash = init_prev;
            for e in existing.entries {
                if e.index < trunc_from {
                    append_wal_frame_bytes(&mut rebuilt, &e);
                    tip_idx = e.index;
                    tip_hash = e.wal_blake3;
                }
            }
            self.platter_wal = rebuilt;
            active_sb.sealed_max_index = tip_idx;
            active_sb.sealed_tip_blake3 = tip_hash;
        }

        // 3. Append new WAL frames sequentially.
        for entry in &batch.entries {
            if entry.index == active_sb.sealed_max_index + 1 {
                append_wal_frame_bytes(&mut self.platter_wal, entry);
                active_sb.sealed_max_index = entry.index;
                active_sb.sealed_tip_blake3 = entry.wal_blake3;
            }
        }

        // 4. Update `HardState` if present (preserving compacted bounds).
        if let Some(hs) = batch.hard_state {
            active_sb.hard_state.current_term = hs.current_term;
            active_sb.hard_state.voted_for = hs.voted_for;
            if hs.compacted_index >= active_sb.hard_state.compacted_index {
                active_sb.hard_state.compacted_index = hs.compacted_index;
                active_sb.hard_state.compacted_term = hs.compacted_term;
            }
        }

        // 5. Commit new Ping-Pong Superblock to alternating 4KiB sector!
        active_sb.generation = self.next_sb_generation;
        self.next_sb_generation = self.next_sb_generation.wrapping_add(1);
        let raw_sb = encode_superblock_record(&active_sb);
        if (active_sb.generation % 2) == 1 {
            self.platter_sb_a = [0u8; SUPERBLOCK_SECTOR_BYTES];
            self.platter_sb_a[0..SUPERBLOCK_RECORD_BYTES].copy_from_slice(&raw_sb);
        } else {
            self.platter_sb_b = [0u8; SUPERBLOCK_SECTOR_BYTES];
            self.platter_sb_b[0..SUPERBLOCK_RECORD_BYTES].copy_from_slice(&raw_sb);
        }

        Ok(())
    }

    fn current_wal_base_blake3(&self, sb: &SuperblockRecord) -> Result<[u8; 32], StorageError> {
        if sb.hard_state.compacted_index == 0 {
            return Ok([0u8; 32]);
        }
        let Some(ref snap_bytes) = self.platter_snapshot else {
            return Err(StorageError::SnapshotCorrupted {
                reason: "compacted_index > 0 but snapshot platter is missing".to_string(),
            });
        };
        let (meta, _) = decode_and_verify_hypsnp01_snapshot(snap_bytes)?;
        Ok(meta.last_wal_blake3)
    }
}

impl DiskStorage for SimulatedFaultyDisk {
    fn stage_batch(&mut self, batch: &PersistBatch) -> Result<(), StorageError> {
        self.dirty_queue.push_back(batch.clone());
        Ok(())
    }

    fn fsync(&mut self) -> Result<(), StorageError> {
        if self.fail_next_fsync {
            self.fail_next_fsync = false;
            return Err(StorageError::IoFailure {
                reason: "simulated NVMe fsync EIO fault".to_string(),
            });
        }
        while let Some(batch) = self.dirty_queue.pop_front() {
            self.apply_batch_to_platter(batch)?;
        }
        Ok(())
    }

    fn scrub_and_verify(&self) -> Result<RecoveredDiskState, StorageError> {
        let sb = self.read_active_superblock()?;
        let snapshot = if let Some(ref raw_snap) = self.platter_snapshot {
            let (meta, _) = decode_and_verify_hypsnp01_snapshot(raw_snap)?;
            Some(SnapshotArtifact {
                meta,
                raw_hypsnp_bytes: raw_snap.clone(),
            })
        } else {
            None
        };

        let init_prev = snapshot
            .as_ref()
            .map(|s| s.meta.last_wal_blake3)
            .unwrap_or([0u8; 32]);
        let first_idx = sb.hard_state.compacted_index + 1;

        let decoded = decode_and_verify_wal_segment(
            &self.platter_wal,
            init_prev,
            first_idx,
            sb.sealed_max_index,
            false,
        )?;

        Ok(RecoveredDiskState {
            hard_state: sb.hard_state,
            snapshot,
            entries: decoded.entries,
            truncated_torn_tail_bytes: 0,
        })
    }

    fn recover_after_restart(&mut self) -> Result<RecoveredDiskState, StorageError> {
        self.dirty_queue.clear();
        let sb = self.read_active_superblock()?;
        let snapshot = if let Some(ref raw_snap) = self.platter_snapshot {
            let (meta, _) = decode_and_verify_hypsnp01_snapshot(raw_snap)?;
            Some(SnapshotArtifact {
                meta,
                raw_hypsnp_bytes: raw_snap.clone(),
            })
        } else {
            None
        };

        let init_prev = snapshot
            .as_ref()
            .map(|s| s.meta.last_wal_blake3)
            .unwrap_or([0u8; 32]);
        let first_idx = sb.hard_state.compacted_index + 1;

        let decoded = decode_and_verify_wal_segment(
            &self.platter_wal,
            init_prev,
            first_idx,
            sb.sealed_max_index,
            true,
        )?;

        if decoded.torn_tail_bytes > 0 {
            if decoded.valid_bytes_end == 0 {
                self.platter_wal = encode_wal_segment_header(0).to_vec();
            } else {
                self.platter_wal.truncate(decoded.valid_bytes_end);
            }
        }

        Ok(RecoveredDiskState {
            hard_state: sb.hard_state,
            snapshot,
            entries: decoded.entries,
            truncated_torn_tail_bytes: decoded.torn_tail_bytes,
        })
    }
}

/// Real filesystem-backed persistent storage using `HYPWAL01`, `HYPSNP01`, and dual superblocks
/// for the production gRPC `consensus-raft-rs` server.
pub struct ProductionFileDisk {
    dir: PathBuf,
    mirror: SimulatedFaultyDisk,
}

impl ProductionFileDisk {
    pub fn open(node_id: NodeId, dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| StorageError::IoFailure {
            reason: format!("mkdir {}: {e}", dir.display()),
        })?;

        let mut mirror = SimulatedFaultyDisk::new(node_id);
        let sb_a_path = dir.join("superblock_a.bin");
        let sb_b_path = dir.join("superblock_b.bin");
        let wal_path = dir.join("segment_00000001.wal");
        let snap_path = dir.join("latest.snap");

        if sb_a_path.exists() || sb_b_path.exists() {
            if let Ok(bytes) = fs::read(&sb_a_path) {
                if bytes.len() == SUPERBLOCK_SECTOR_BYTES {
                    mirror.platter_sb_a.copy_from_slice(&bytes);
                }
            }
            if let Ok(bytes) = fs::read(&sb_b_path) {
                if bytes.len() == SUPERBLOCK_SECTOR_BYTES {
                    mirror.platter_sb_b.copy_from_slice(&bytes);
                }
            }
            if let Ok(wal_bytes) = fs::read(&wal_path) {
                mirror.platter_wal = wal_bytes;
            }
            if let Ok(snap_bytes) = fs::read(&snap_path) {
                if !snap_bytes.is_empty() {
                    mirror.platter_snapshot = Some(snap_bytes);
                }
            }
            if let Ok(sb) = mirror.read_active_superblock() {
                mirror.next_sb_generation = sb.generation + 1;
            }
        } else {
            let inst = Self { dir, mirror };
            inst.flush_mirror_to_os_files()?;
            return Ok(inst);
        }

        Ok(Self { dir, mirror })
    }

    fn flush_mirror_to_os_files(&self) -> Result<(), StorageError> {
        write_and_sync_file(&self.dir.join("superblock_a.bin"), &self.mirror.platter_sb_a)?;
        write_and_sync_file(&self.dir.join("superblock_b.bin"), &self.mirror.platter_sb_b)?;
        write_and_sync_file(&self.dir.join("segment_00000001.wal"), &self.mirror.platter_wal)?;
        if let Some(ref snap) = self.mirror.platter_snapshot {
            write_and_sync_file(&self.dir.join("latest.snap"), snap)?;
        }
        Ok(())
    }
}

fn write_and_sync_file(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    let tmp = path.with_extension("tmp_sync");
    let mut f = fs::File::create(&tmp).map_err(|e| StorageError::IoFailure {
        reason: format!("create {}: {e}", tmp.display()),
    })?;
    f.write_all(data).map_err(|e| StorageError::IoFailure {
        reason: format!("write {}: {e}", tmp.display()),
    })?;
    f.sync_all().map_err(|e| StorageError::IoFailure {
        reason: format!("fsync {}: {e}", tmp.display()),
    })?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| StorageError::IoFailure {
        reason: format!("rename {} -> {}: {e}", tmp.display(), path.display()),
    })?;
    Ok(())
}

impl DiskStorage for ProductionFileDisk {
    fn stage_batch(&mut self, batch: &PersistBatch) -> Result<(), StorageError> {
        self.mirror.stage_batch(batch)
    }

    fn fsync(&mut self) -> Result<(), StorageError> {
        self.mirror.fsync()?;
        self.flush_mirror_to_os_files()
    }

    fn scrub_and_verify(&self) -> Result<RecoveredDiskState, StorageError> {
        self.mirror.scrub_and_verify()
    }

    fn recover_after_restart(&mut self) -> Result<RecoveredDiskState, StorageError> {
        let rec = self.mirror.recover_after_restart()?;
        self.flush_mirror_to_os_files()?;
        Ok(rec)
    }
}
