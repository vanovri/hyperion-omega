use std::sync::Arc;
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use crate::config::NS_PER_HOUR;

pub const TABLE_TRADES: &str = "trades";
pub const TABLE_QUOTES: &str = "quotes";
pub const TABLE_FRAUD_ALERTS: &str = "fraud_alerts";

/// Resolves canonical Hyperion `instrument_id` <-> `symbol` mapping (`00-hyperion-contract.md` §4.3).
#[inline]
pub fn symbol_for_instrument_id(instrument_id: u32) -> &'static str {
    match instrument_id {
        1 => "BTC-USDT",
        2 => "ETH-USDT",
        3 => "SOL-USDT",
        4 => "HYPE-USDT",
        _ => "UNKNOWN-USDT",
    }
}

/// Resolves canonical `instrument_id` from `symbol`.
#[inline]
pub fn instrument_id_for_symbol(symbol: &str) -> u32 {
    match symbol {
        "BTC-USDT" => 1,
        "ETH-USDT" => 2,
        "SOL-USDT" => 3,
        "HYPE-USDT" => 4,
        _ => 99,
    }
}

/// Computes deterministic UTC hour bucket (`timestamp_ns / 3_600_000_000_000`).
#[inline]
pub fn hour_bucket_from_ts_ns(timestamp_ns: i64) -> u64 {
    if timestamp_ns <= 0 {
        0
    } else {
        (timestamp_ns as u64) / NS_PER_HOUR
    }
}

/// Physical Arrow schema for `trades` Parquet segments (excluding Hive directory columns `symbol`, `hour_bucket`).
/// Physically sorted by composite key `(instrument_id, timestamp_ns, seq_id)`.
pub fn trades_physical_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("instrument_id", DataType::UInt32, false),
        Field::new("timestamp_ns", DataType::Int64, false),
        Field::new("seq_id", DataType::UInt64, false),
        Field::new("fill_id", DataType::UInt64, false),
        Field::new("maker_account_id", DataType::UInt64, false),
        Field::new("taker_account_id", DataType::UInt64, false),
        Field::new("account_id", DataType::UInt64, false),
        Field::new(
            "aggressor_side",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("side_sign", DataType::Int64, false),
        Field::new("price_fp", DataType::Int64, false),
        Field::new("qty_fp", DataType::Int64, false),
        Field::new("notional_fp", DataType::Int64, false),
        Field::new("maker_fee_fp", DataType::Int64, false),
        Field::new("taker_fee_fp", DataType::Int64, false),
        Field::new(
            "ingest_source",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
    ]))
}

/// Physical Arrow schema for `quotes` (L2 order book snapshots) Parquet segments.
/// Physically sorted by composite key `(instrument_id, timestamp_ns, seq_id)`.
pub fn quotes_physical_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("instrument_id", DataType::UInt32, false),
        Field::new("timestamp_ns", DataType::Int64, false),
        Field::new("seq_id", DataType::UInt64, false),
        Field::new("best_bid_fp", DataType::Int64, false),
        Field::new("best_ask_fp", DataType::Int64, false),
        Field::new("mid_price_fp", DataType::Int64, false),
        Field::new("spread_fp", DataType::Int64, false),
        Field::new("bid_depth_fp", DataType::Int64, false),
        Field::new("ask_depth_fp", DataType::Int64, false),
        Field::new(
            "market_regime",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
    ]))
}

/// Physical Arrow schema for `fraud_alerts` (Sentinel-ML anomaly alerts) Parquet segments.
/// Physically sorted by composite key `(instrument_id, timestamp_ns, seq_id)`.
pub fn fraud_alerts_physical_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("instrument_id", DataType::UInt32, false),
        Field::new("timestamp_ns", DataType::Int64, false),
        Field::new("seq_id", DataType::UInt64, false),
        Field::new("alert_id", DataType::Utf8, false),
        Field::new("account_id", DataType::UInt64, false),
        Field::new(
            "pattern_type",
            DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            false,
        ),
        Field::new("severity_score", DataType::UInt32, false),
        Field::new("confidence_ppm", DataType::UInt32, false),
    ]))
}

/// Canonical domain struct for an ingested trade execution (fixed-point 1e8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeRecord {
    pub symbol: String,
    pub instrument_id: u32,
    pub timestamp_ns: i64,
    pub seq_id: u64,
    pub fill_id: u64,
    pub maker_account_id: u64,
    pub taker_account_id: u64,
    pub is_buy_aggressor: bool,
    pub price_fp: i64,
    pub qty_fp: i64,
    pub notional_fp: i64,
    pub maker_fee_fp: i64,
    pub taker_fee_fp: i64,
    pub from_wal: bool,
}

/// Canonical domain struct for an ingested L2 order book snapshot (fixed-point 1e8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteRecord {
    pub symbol: String,
    pub instrument_id: u32,
    pub timestamp_ns: i64,
    pub seq_id: u64,
    pub best_bid_fp: i64,
    pub best_ask_fp: i64,
    pub mid_price_fp: i64,
    pub spread_fp: i64,
    pub bid_depth_fp: i64,
    pub ask_depth_fp: i64,
    pub market_regime: &'static str,
}

/// Canonical domain struct for an ingested Sentinel-ML fraud alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FraudAlertRecord {
    pub symbol: String,
    pub instrument_id: u32,
    pub timestamp_ns: i64,
    pub seq_id: u64,
    pub alert_id: String,
    pub account_id: u64,
    pub pattern_type: &'static str,
    pub severity_score: u32,
    pub confidence_ppm: u32,
}
