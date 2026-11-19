//! Deterministic boundary traits for Time (`Clock`), Network (`NetworkTransport`), and Storage (`DiskStorage`).
//! Every single external side-effect in `consensus-raft-rs` passes through these traits so that
//! TigerBeetle-style VOPR simulation controls time, network topology, packet reordering, fsync stalls,
//! torn writes, and disk bit-rot from a single 64-bit PRNG seed.

use crate::types::{HardState, LogEntry, LogIndex, NetworkEnvelope, NodeId, SnapshotArtifact};
use thiserror::Error;

/// Monotonic nanosecond clock abstraction.
pub trait Clock: Send + Sync {
    fn now_ns(&self, node_id: NodeId) -> u64;
}

/// Deterministic or production network packet switch abstraction.
pub trait NetworkTransport: Send {
    fn send(&mut self, now_ns: u64, envelope: NetworkEnvelope);
}

/// Explicit storage corruption & I/O failure taxonomy (distinguishes recoverable tail torn writes
/// from fatal silent data corruption / bit-rot on committed frames).
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StorageError {
    #[error("unrecoverable superblock bit-rot on node {node_id}: both ping-pong slots failed CRC32C/Blake3 verification")]
    SuperblockBitRot { node_id: NodeId },

    #[error("silent data corruption (bit-rot) detected at log index {index} (byte offset {offset}): expected CRC32C {expected_crc32c:#010x}, got {actual_crc32c:#010x}")]
    WalFrameBitRotCrc {
        index: LogIndex,
        offset: usize,
        expected_crc32c: u32,
        actual_crc32c: u32,
    },

    #[error("cryptographic Blake3 hash-chain break at log index {index}: expected {expected_hex}, got {actual_hex}")]
    WalHashChainMismatch {
        index: LogIndex,
        expected_hex: String,
        actual_hex: String,
    },

    #[error("snapshot HYPSNP01 corruption: {reason}")]
    SnapshotCorrupted { reason: String },

    #[error("disk I/O stall or hardware write failure: {reason}")]
    IoFailure { reason: String },
}

/// Summary of recovered persistent state upon node boot / crash-restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredDiskState {
    pub hard_state: HardState,
    pub snapshot: Option<SnapshotArtifact>,
    pub entries: Vec<LogEntry>,
    /// Number of uncommitted/torn tail bytes safely truncated during crash recovery.
    pub truncated_torn_tail_bytes: usize,
}

/// Atomic persistence batch requested by `RaftCore` before emitting dependent RPCs or advancing commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistBatch {
    pub io_id: u64,
    pub hard_state: Option<HardState>,
    /// If `Some(idx)`, truncate uncommitted suffix `>= idx` before appending `entries`.
    pub truncate_from_index: Option<LogIndex>,
    pub entries: Vec<LogEntry>,
    /// If `Some(snap)`, atomically install snapshot and compact log prefix `<= snap.meta.last_included_index`.
    pub install_snapshot: Option<SnapshotArtifact>,
}

/// Block-accurate persistent storage trait supporting page-cache writes, explicit `fsync`,
/// crash power-loss simulation, torn writes, and bit-rot scrubbing.
pub trait DiskStorage: Send {
    /// Stage a persistence batch into the OS/disk write buffer (not yet durable until `fsync` completes!).
    fn stage_batch(&mut self, batch: &PersistBatch) -> Result<(), StorageError>;

    /// Flush dirty write buffers to durable platter/NVMe.
    fn fsync(&mut self) -> Result<(), StorageError>;

    /// Full integrity scrub of durable superblocks, snapshot, and chained `HYPWAL01` frames.
    fn scrub_and_verify(&self) -> Result<RecoveredDiskState, StorageError>;

    /// Recover durable state after node startup or sudden power-loss crash.
    fn recover_after_restart(&mut self) -> Result<RecoveredDiskState, StorageError>;
}
