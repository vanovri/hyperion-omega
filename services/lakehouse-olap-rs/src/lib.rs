#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! HYPERION Channel 17 — Columnar Lakehouse OLAP & Forensic Query Engine (`lakehouse-olap-rs`).
//!
//! Combines zero-copy Apache Arrow ingestion (from Redpanda & `HYPWAL01` Cold WAL segments),
//! Zstd + Split-Block Bloom Filter + Dictionary Parquet compaction, native vectorized
//! DataFusion financial microstructure UDFs/UDAFs & `ASOF JOIN`, SQL AST Security Guard,
//! and HTTP JSON/Arrow IPC + Apache Arrow Flight gRPC streaming endpoints.

pub mod arrow_writer;
pub mod ast_guard;
pub mod compactor;
pub mod config;
pub mod engine;
pub mod error;
pub mod math_fp;
pub mod schema;
pub mod server_flight;
pub mod server_http;
pub mod synth_bench;
pub mod udfs;
pub mod wal_ingest;
