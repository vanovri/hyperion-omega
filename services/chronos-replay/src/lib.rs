#![deny(clippy::float_arithmetic)]

//! **HYPERION Channel 08 — `chronos-replay`**
//!
//! Deterministic Nanosecond Time-Travel Debugger, Blake3 Cryptographic WAL/Snapshot Engine,
//! Counterfactual "What-If" Timeline Forking Sandbox, and Variable-Speed VCR Streaming Server.

pub use hyperion_proto::v1 as pb;

pub mod engine;
pub mod error;
pub mod fixed;
pub mod merkle;
pub mod playback;
pub mod server;
pub mod state;
pub mod wal;

pub use engine::{
    build_default_demo_scenario, is_policy_override_noop, reconstruct_pass1_baseline,
    reconstruct_pass2_zerocopy, reconstruct_pass3_cache_aligned, ChronosEngine, DetailedSeekOutput,
    ForkedTimelineArtifact, HistoricalTimelineRecorder, ReplayCommand,
};
pub use error::ChronosError;
pub use playback::PlaybackSessionManager;
pub use server::{build_http_router, ChronosGrpcService, ChronosServiceState};
pub use state::{ExchangeState, HotOrderSlot, PriceLevelHead, ReconstructedLiquidityMetrics};
pub use wal::{
    encode_snapshot_bytes, parse_and_verify_snapshot, CanonicalSnapshotPayload, ChronosWalStore,
    IndexedWalEvent, SparseIndexEntry, SparseWalIndex, VerifiedSnapshot, WalSegmentWriter,
    DEFAULT_SPARSE_INDEX_STRIDE, SNAPSHOT_MAGIC, WAL_MAGIC,
};
