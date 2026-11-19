//! Unified Concurrent Execution History Verifier (WGL Linearizability + Adya Strict Serializability).

pub mod linearizability;
pub mod serializability;

pub use linearizability::{
    ClientOpReturn, ClientOpSpec, ConcurrentHistoryOp, LinearizabilityChecker,
    LinearizabilityVerdict,
};
pub use serializability::{
    LedgerKey, ObservedTransaction, SerializabilityVerdict, StrictSerializabilityVerifier,
};
