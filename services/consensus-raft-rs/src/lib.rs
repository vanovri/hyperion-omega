#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! HYPERION CH12 — Hardened 5-Node Raft Consensus Engine, Blake3 WAL/Snapshot (`HYPWAL01` / `HYPSNP01`),
//! TigerBeetle-Style Deterministic Simulation Testing (`VOPR`), and Concurrent Linearizability &
//! Strict Serializability Verifier.

/// Generated Protobuf v3 & Tonic gRPC bindings (`hyperion.v1` + `hyperion.omega.v1`).
#[allow(clippy::all, dead_code)]
pub mod proto {
    pub mod v1 {
        tonic::include_proto!("hyperion.v1");
    }
    pub mod omega {
        pub mod v1 {
            tonic::include_proto!("hyperion.omega.v1");
        }
    }
    pub use omega::v1 as omega_v1;
    pub use v1::*;
}

pub mod checker;
pub mod mutation_suite;
pub mod opt_rounds;
pub mod prng;
pub mod quorum;
pub mod raft_core;
pub mod rpc_service;
pub mod sim;
pub mod state_machine;
pub mod traits;
pub mod types;
pub mod wal_codec;
