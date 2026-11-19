#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! # HYPERION OMEGA Channel 13 — Zero-Knowledge Proof of Reserves & Liabilities
//!
//! Production cryptographic engine combining:
//! - **Cryptographic Merkle-Sum Sparse Tree (MSST)** with Grassi et al. (ePrint 2019/458 Appendix B)
//!   Sparse-MDS Poseidon over BN254 (`Fr`, width 6, rate 5, `R_F = 8`, `R_P = 57`) + Hybrid Blake3
//!   commitments and cache-line-aligned (`#[repr(C, align(64))]`) Rayon parallelism.
//! - **Real BN254 Groth16 R1CS Arithmetic Circuits (`arkworks`)** proving 64-bit non-negativity
//!   (defeating phantom `-1 mod r` leaves & Maxwell-Buterin sibling cancellation), overflow-free
//!   binary path sum integrity, and global solvency (`total_liabilities <= verified_reserves`).
//! - **Tonic gRPC `ZkSolvencyService` (`:50162`)** + HTTP observability (`:8092`) + `HYPSNP01`
//!   Channel 02 ledger snapshot ingestion + `271 KiB` WebAssembly client verifier (`verifier-wasm`).

pub mod circuit;
pub mod error;
pub mod msst;
pub mod poseidon_params;
pub mod prover;
pub mod service;
pub mod snapshot_loader;
pub mod verifier;
