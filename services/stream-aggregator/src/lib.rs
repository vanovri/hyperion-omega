#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! HYPERION Channel 04 — High-Throughput Quantitative Market Microstructure &
//! Exactly-Once TimescaleDB Stream Aggregator (`services/stream-aggregator`).

pub mod api;
pub mod broker;
pub mod candle;
pub mod config;
pub mod dedup;
pub mod engine;
pub mod fixed_point;
pub mod orderbook_state;
pub mod reference;
pub mod storage;
pub mod synthetic;
pub mod watermark;
