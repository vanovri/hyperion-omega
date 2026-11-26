use thiserror::Error;

use crate::fixed::MathError;

#[derive(Debug, Error)]
pub enum ChronosError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protobuf decode error: {0}")]
    ProtoDecode(#[from] prost::DecodeError),

    #[error("Protobuf encode error: {0}")]
    ProtoEncode(#[from] prost::EncodeError),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Fixed-point arithmetic error: {0}")]
    Math(#[from] MathError),

    #[error("Invalid WAL magic header: expected HYPWAL01, got {0:?}")]
    InvalidWalMagic([u8; 8]),

    #[error("Unsupported WAL version: {0}")]
    UnsupportedWalVersion(u32),

    #[error("Truncated WAL frame at byte offset {offset} (needed {needed} bytes, available {available})")]
    TruncatedWalFrame {
        offset: u64,
        needed: usize,
        available: usize,
    },

    #[error("WAL CRC32C mismatch at seq {engine_seq} (offset {offset}): expected 0x{expected:08x}, computed 0x{computed:08x}")]
    WalCrcMismatch {
        engine_seq: u64,
        offset: u64,
        expected: u32,
        computed: u32,
    },

    #[error("WAL Blake3 frame digest mismatch at seq {engine_seq} (offset {offset})")]
    WalBlake3Mismatch { engine_seq: u64, offset: u64 },

    #[error(
        "WAL hash chain broken at seq {engine_seq}: prev_blake3 does not match prior frame_blake3"
    )]
    WalChainBroken { engine_seq: u64 },

    #[error("Non-monotonic or gapped WAL engine_seq: expected {expected}, found {found}")]
    WalSequenceGap { expected: u64, found: u64 },

    #[error("Invalid Snapshot magic header: expected HYPSNP01, got {0:?}")]
    InvalidSnapshotMagic([u8; 8]),

    #[error("Unsupported Snapshot version: {0}")]
    UnsupportedSnapshotVersion(u32),

    #[error("Truncated Snapshot file (needed {needed} bytes, available {available})")]
    TruncatedSnapshot { needed: usize, available: usize },

    #[error("Snapshot CRC32C mismatch at seq {engine_seq}: expected 0x{expected:08x}, computed 0x{computed:08x}")]
    SnapshotCrcMismatch {
        engine_seq: u64,
        expected: u32,
        computed: u32,
    },

    #[error("Snapshot state root mismatch at seq {engine_seq}")]
    SnapshotStateRootMismatch { engine_seq: u64 },

    #[error("Invalid Snapshot payload at seq {engine_seq}: {reason}")]
    InvalidSnapshotPayload { engine_seq: u64, reason: String },

    #[error("Invalid request parameter: {0}")]
    InvalidArgument(String),

    #[error("Timeline or session not found: {0}")]
    NotFound(String),

    #[error("Double-entry ledger invariant violation: {0}")]
    InvariantViolation(String),
}

impl From<ChronosError> for tonic::Status {
    fn from(err: ChronosError) -> Self {
        match &err {
            ChronosError::InvalidArgument(_) => tonic::Status::invalid_argument(err.to_string()),
            ChronosError::NotFound(_) => tonic::Status::not_found(err.to_string()),
            ChronosError::WalCrcMismatch { .. }
            | ChronosError::WalBlake3Mismatch { .. }
            | ChronosError::WalChainBroken { .. }
            | ChronosError::WalSequenceGap { .. }
            | ChronosError::SnapshotCrcMismatch { .. }
            | ChronosError::SnapshotStateRootMismatch { .. }
            | ChronosError::InvariantViolation(_) => tonic::Status::data_loss(err.to_string()),
            _ => tonic::Status::internal(err.to_string()),
        }
    }
}
