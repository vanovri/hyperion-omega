use thiserror::Error;

/// Canonical error hierarchy for HYPERION Channel 13 Zero-Knowledge Solvency Engine.
/// Zero panics, zero `.unwrap()` / `.expect()` on runtime or untrusted inputs.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SolvencyError {
    #[error("empty account set: Merkle-Sum Sparse Tree requires at least 1 leaf or explicit sparse depth")]
    EmptyAccountSet,

    #[error("tree depth {depth} out of supported range [1, {max_depth}]")]
    InvalidTreeDepth { depth: usize, max_depth: usize },

    #[error("account count {count} exceeds capacity 2^{depth} = {capacity}")]
    TreeCapacityExceeded {
        count: usize,
        depth: usize,
        capacity: usize,
    },

    #[error("duplicate account_id {account_id} detected in ledger snapshot")]
    DuplicateAccountId { account_id: u64 },

    #[error("account_id {account_id} not found in Merkle-Sum Sparse Tree")]
    AccountNotFound { account_id: u64 },

    #[error("leaf index {index} out of bounds for tree capacity {capacity}")]
    LeafIndexOutOfBounds { index: usize, capacity: usize },

    #[error("arithmetic overflow while aggregating liabilities at tree level {level}: {left} + {right} exceeds u128::MAX")]
    LiabilitySumOverflow { level: usize, left: u128, right: u128 },

    #[error("finite-field range violation at {context}: value exceeds {max_bits}-bit non-negative bound")]
    RangeConstraintViolation {
        context: &'static str,
        max_bits: usize,
    },

    #[error("INSOLVENCY DETECTED: total liabilities ({liabilities_fp}) exceed verified reserves ({reserves_fp}) by {deficit_fp} atomic units")]
    InsufficientReserves {
        liabilities_fp: u128,
        reserves_fp: u128,
        deficit_fp: u128,
    },

    #[error("R1CS constraint system unsatisfied ({unsatisfied_gate})")]
    UnsatisfiedConstraints { unsatisfied_gate: String },

    #[error("ZK proof synthesis failed: {0}")]
    SynthesisFailure(String),

    #[error("cryptographic serialization error: {0}")]
    SerializationError(String),

    #[error("ZK proof verification failed: {reason}")]
    VerificationFailed { reason: String },

    #[error("Merkle-Sum inclusion path length mismatch: expected {expected}, got {actual}")]
    InvalidPathLength { expected: usize, actual: usize },
}

pub type SolvencyResult<T> = Result<T, SolvencyError>;
