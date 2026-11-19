//! # `quant-wasm-host` — Hyperion Channel 15 Co-Located Serverless Quant Sandbox
//!
//! Exports the deterministic Zero-Syscall Wasmtime JIT engine, Shared Ring-Buffer ABI bridge,
//! Pre-Trade Risk Gate, Live Atomic Hot-Swapper, Nanosecond Fuel Profiler, and gRPC server.

// Preserve explicit modulo divisibility checks (`% != 0`) for MSRV 1.86 container builds.
#![allow(clippy::manual_is_multiple_of)]

pub mod engine;
pub mod grpc_service;
pub mod profiler;
pub mod proto;
pub mod risk_gate;
pub mod shm_ring;

pub use hyperion_quant_abi as abi;
