use hyperion_omega_proto::core_v1;
use prost::Message;
use crate::config::FP_SCALE_I128;
use crate::error::{LakehouseError, Result};
use crate::math_fp::saturate_i128_to_i64;
use crate::schema::{
    instrument_id_for_symbol, symbol_for_instrument_id, FraudAlertRecord, QuoteRecord, TradeRecord,
};

/// Canonical WAL header constants per `00-hyperion-contract.md` §6.1.
pub const WAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const WAL_VERSION: u32 = 1;
pub const WAL_HEADER_LEN: usize = 16;
/// Frame fixed prefix: payload_len(4) + crc32c(4) + engine_seq(8) + ts_ns(8) + prev_blake3(32) + frame_blake3(32) = 88 bytes.
pub const WAL_FRAME_PREFIX_LEN: usize = 88;

/// Wire-compatible Protobuf definitions matching `/proto/{common,orderbook,telemetry_fraud}.proto` (`hyperion.v1`).
pub mod wire {
    use prost::Message;

    #[derive(Clone, Copy, PartialEq, Eq, Message)]
    pub struct U128Wire {
        #[prost(uint64, tag = "1")]
        pub hi: u64,
        #[prost(uint64, tag = "2")]
        pub lo: u64,
    }

    impl U128Wire {
        #[inline]
        pub fn to_u128(self) -> u128 {
            ((self.hi as u128) << 64) | (self.lo as u128)
        }

        #[inline]
        pub fn from_u128(v: u128) -> Self {
            Self {
                hi: (v >> 64) as u64,
                lo: v as u64,
            }
        }
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct ExecutionFillWire {
        #[prost(uint64, tag = "1")]
        pub fill_id: u64,
        #[prost(uint64, tag = "2")]
        pub engine_seq: u64,
        #[prost(string, tag = "3")]
        pub symbol: String,
        #[prost(uint32, tag = "4")]
        pub instrument_id: u32,
        #[prost(uint64, tag = "5")]
        pub maker_order_id: u64,
        #[prost(uint64, tag = "6")]
        pub taker_order_id: u64,
        #[prost(uint64, tag = "7")]
        pub maker_account_id: u64,
        #[prost(uint64, tag = "8")]
        pub taker_account_id: u64,
        #[prost(int32, tag = "9")]
        pub aggressor_side: i32, // 1 = BUY, 2 = SELL
        #[prost(uint64, tag = "10")]
        pub price_fp: u64,
        #[prost(uint64, tag = "11")]
        pub qty_fp: u64,
        #[prost(message, optional, tag = "12")]
        pub notional_fp: Option<U128Wire>,
        #[prost(int64, tag = "13")]
        pub maker_fee_fp: i64,
        #[prost(int64, tag = "14")]
        pub taker_fee_fp: i64,
        #[prost(uint64, tag = "15")]
        pub exec_ts_ns: u64,
        #[prost(bytes = "vec", tag = "16")]
        pub wal_blake3_hash: Vec<u8>,
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct PriceLevelWire {
        #[prost(uint64, tag = "1")]
        pub price_fp: u64,
        #[prost(uint64, tag = "2")]
        pub total_qty_fp: u64,
        #[prost(uint32, tag = "3")]
        pub order_count: u32,
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct L2BookSnapshotWire {
        #[prost(uint64, tag = "1")]
        pub engine_seq: u64,
        #[prost(string, tag = "2")]
        pub symbol: String,
        #[prost(uint32, tag = "3")]
        pub instrument_id: u32,
        #[prost(message, repeated, tag = "4")]
        pub bids: Vec<PriceLevelWire>,
        #[prost(message, repeated, tag = "5")]
        pub asks: Vec<PriceLevelWire>,
        #[prost(uint64, tag = "6")]
        pub best_bid_fp: u64,
        #[prost(uint64, tag = "7")]
        pub best_ask_fp: u64,
        #[prost(uint64, tag = "8")]
        pub mid_price_fp: u64,
        #[prost(uint64, tag = "9")]
        pub spread_fp: u64,
        #[prost(uint64, tag = "10")]
        pub snapshot_ts_ns: u64,
        #[prost(bytes = "vec", tag = "11")]
        pub state_root_blake3: Vec<u8>,
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct L3BookDeltaWire {
        #[prost(uint64, tag = "1")]
        pub engine_seq: u64,
        #[prost(uint64, tag = "2")]
        pub prev_engine_seq: u64,
        #[prost(string, tag = "3")]
        pub symbol: String,
        #[prost(uint32, tag = "4")]
        pub instrument_id: u32,
        #[prost(int32, tag = "5")]
        pub op: i32,
        #[prost(int32, tag = "6")]
        pub side: i32,
        #[prost(uint64, tag = "7")]
        pub order_id: u64,
        #[prost(uint64, tag = "8")]
        pub account_id: u64,
        #[prost(uint64, tag = "9")]
        pub price_fp: u64,
        #[prost(uint64, tag = "10")]
        pub remaining_qty_fp: u64,
        #[prost(int64, tag = "11")]
        pub delta_qty_fp: i64,
        #[prost(uint64, tag = "12")]
        pub event_ts_ns: u64,
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct MarketEventEnvelopeWire {
        #[prost(uint64, tag = "1")]
        pub engine_seq: u64,
        #[prost(string, tag = "2")]
        pub symbol: String,
        #[prost(uint64, tag = "3")]
        pub ts_ns: u64,
        #[prost(oneof = "EnvelopePayloadWire", tags = "4, 5, 6")]
        pub payload: Option<EnvelopePayloadWire>,
    }

    #[derive(Clone, PartialEq, Eq, prost::Oneof)]
    pub enum EnvelopePayloadWire {
        #[prost(message, tag = "4")]
        L3Delta(L3BookDeltaWire),
        #[prost(message, tag = "5")]
        Fill(ExecutionFillWire),
        #[prost(message, tag = "6")]
        L2Snapshot(L2BookSnapshotWire),
    }

    #[derive(Clone, PartialEq, Eq, Message)]
    pub struct AnomalyAlertWire {
        #[prost(string, tag = "1")]
        pub alert_id: String,
        #[prost(uint64, tag = "2")]
        pub engine_seq: u64,
        #[prost(string, tag = "3")]
        pub symbol: String,
        #[prost(int32, tag = "4")]
        pub pattern_type: i32,
        #[prost(uint32, tag = "5")]
        pub severity_score: u32,
        #[prost(uint32, tag = "6")]
        pub confidence_ppm: u32,
        #[prost(uint64, repeated, tag = "9")]
        pub offender_account_ids: Vec<u64>,
        #[prost(uint64, tag = "15")]
        pub detected_ts_ns: u64,
    }
}

/// Decoded batch extracted from a Cold WAL segment or Redpanda topic batch.
#[derive(Debug, Default)]
pub struct DecodedIngestionBatch {
    pub trades: Vec<TradeRecord>,
    pub quotes: Vec<QuoteRecord>,
    pub alerts: Vec<FraudAlertRecord>,
    pub verified_wal_frames: usize,
    pub last_wal_blake3: [u8; 32],
}

#[inline]
fn compute_wal_crc32c(engine_seq: u64, ts_ns: u64, prev_blake3: &[u8; 32], payload: &[u8]) -> u32 {
    let mut crc = 0_u32;
    crc = crc32c::crc32c_append(crc, &engine_seq.to_le_bytes());
    crc = crc32c::crc32c_append(crc, &ts_ns.to_le_bytes());
    crc = crc32c::crc32c_append(crc, prev_blake3);
    crc32c::crc32c_append(crc, payload)
}

#[inline]
fn compute_wal_frame_blake3(
    engine_seq: u64,
    ts_ns: u64,
    prev_blake3: &[u8; 32],
    payload: &[u8],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&engine_seq.to_le_bytes());
    hasher.update(&ts_ns.to_le_bytes());
    hasher.update(prev_blake3);
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}

/// Encodes `MarketEventEnvelopeWire` entries into an exact `HYPWAL01` binary WAL segment
/// (`00-hyperion-contract.md` §6.1) with Castagnoli CRC-32C and chained Blake3 digests.
pub fn encode_wal_segment(
    envelopes: &[wire::MarketEventEnvelopeWire],
    initial_prev_blake3: [u8; 32],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(WAL_HEADER_LEN + envelopes.len() * 160);
    buf.extend_from_slice(WAL_MAGIC);
    buf.extend_from_slice(&WAL_VERSION.to_le_bytes());
    buf.extend_from_slice(&0_u32.to_le_bytes()); // flags = 0

    let mut prev_hash = initial_prev_blake3;
    for env in envelopes {
        let payload = env.encode_to_vec();
        let payload_len = payload.len() as u32;
        let crc = compute_wal_crc32c(env.engine_seq, env.ts_ns, &prev_hash, &payload);
        let frame_hash = compute_wal_frame_blake3(env.engine_seq, env.ts_ns, &prev_hash, &payload);

        buf.extend_from_slice(&payload_len.to_le_bytes());
        buf.extend_from_slice(&crc.to_le_bytes());
        buf.extend_from_slice(&env.engine_seq.to_le_bytes());
        buf.extend_from_slice(&env.ts_ns.to_le_bytes());
        buf.extend_from_slice(&prev_hash);
        buf.extend_from_slice(&frame_hash);
        buf.extend_from_slice(&payload);

        prev_hash = frame_hash;
    }

    buf
}

/// Zero-copy parses and cryptographically verifies a Cold WAL binary segment (`HYPWAL01`)
/// directly from an in-memory byte slice (or `mmap` slice), converting frames into typed records.
pub fn parse_cold_wal_segment(
    segment_bytes: &[u8],
    expected_initial_prev_hash: Option<[u8; 32]>,
) -> Result<DecodedIngestionBatch> {
    if segment_bytes.len() < WAL_HEADER_LEN {
        return Err(LakehouseError::WalIntegrity(format!(
            "WAL segment truncated: {} bytes < {} header bytes",
            segment_bytes.len(),
            WAL_HEADER_LEN
        )));
    }

    if &segment_bytes[0..8] != WAL_MAGIC {
        return Err(LakehouseError::WalIntegrity(
            "Invalid WAL magic header (expected HYPWAL01)".to_string(),
        ));
    }

    let version = u32::from_le_bytes([
        segment_bytes[8],
        segment_bytes[9],
        segment_bytes[10],
        segment_bytes[11],
    ]);
    if version != WAL_VERSION {
        return Err(LakehouseError::WalIntegrity(format!(
            "Unsupported WAL version {version} (expected {WAL_VERSION})"
        )));
    }

    let mut offset = WAL_HEADER_LEN;
    let mut expected_prev = expected_initial_prev_hash;
    let mut batch = DecodedIngestionBatch::default();

    while offset < segment_bytes.len() {
        if segment_bytes.len() - offset < WAL_FRAME_PREFIX_LEN {
            return Err(LakehouseError::WalIntegrity(format!(
                "Truncated WAL frame header at byte offset {offset}"
            )));
        }

        let hdr = &segment_bytes[offset..offset + WAL_FRAME_PREFIX_LEN];
        let payload_len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        let expected_crc = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        let engine_seq = u64::from_le_bytes([
            hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
        ]);
        let ts_ns = u64::from_le_bytes([
            hdr[16], hdr[17], hdr[18], hdr[19], hdr[20], hdr[21], hdr[22], hdr[23],
        ]);

        let mut prev_blake3 = [0_u8; 32];
        prev_blake3.copy_from_slice(&hdr[24..56]);
        let mut frame_blake3 = [0_u8; 32];
        frame_blake3.copy_from_slice(&hdr[56..88]);

        let frame_end = offset + WAL_FRAME_PREFIX_LEN + payload_len;
        if frame_end > segment_bytes.len() {
            return Err(LakehouseError::WalIntegrity(format!(
                "Truncated WAL payload for seq={engine_seq} (need {frame_end}, len={})",
                segment_bytes.len()
            )));
        }

        let payload_slice = &segment_bytes[offset + WAL_FRAME_PREFIX_LEN..frame_end];

        // 1. Verify Castagnoli CRC-32C
        let actual_crc = compute_wal_crc32c(engine_seq, ts_ns, &prev_blake3, payload_slice);
        if actual_crc != expected_crc {
            return Err(LakehouseError::WalIntegrity(format!(
                "CRC-32C mismatch at engine_seq={engine_seq}: expected {expected_crc:#010x}, got {actual_crc:#010x}"
            )));
        }

        // 2. Verify Blake3 hash chain continuity
        if let Some(exp_prev) = expected_prev {
            if prev_blake3 != exp_prev {
                return Err(LakehouseError::WalIntegrity(format!(
                    "Blake3 hash chain broken at engine_seq={engine_seq}"
                )));
            }
        } else if engine_seq == 1 && prev_blake3 != [0_u8; 32] {
            return Err(LakehouseError::WalIntegrity(
                "Genesis WAL frame (engine_seq=1) must have all-zero prev_blake3".to_string(),
            ));
        }

        let actual_blake3 = compute_wal_frame_blake3(engine_seq, ts_ns, &prev_blake3, payload_slice);
        if actual_blake3 != frame_blake3 {
            return Err(LakehouseError::WalIntegrity(format!(
                "Frame Blake3 digest mismatch at engine_seq={engine_seq}"
            )));
        }

        // 3. Zero-copy Protobuf decode from slice
        let envelope = wire::MarketEventEnvelopeWire::decode(payload_slice)?;
        match envelope.payload {
            Some(wire::EnvelopePayloadWire::Fill(fill)) => {
                batch.trades.push(convert_wire_fill(fill, true));
            }
            Some(wire::EnvelopePayloadWire::L2Snapshot(snap)) => {
                batch.quotes.push(convert_wire_snapshot(snap));
            }
            Some(wire::EnvelopePayloadWire::L3Delta(_)) | None => {}
        }

        expected_prev = Some(frame_blake3);
        batch.last_wal_blake3 = frame_blake3;
        batch.verified_wal_frames += 1;
        offset = frame_end;
    }

    Ok(batch)
}

/// Decodes a batch of Redpanda Protobuf payloads from `hyperion.market.executions.v1`.
pub fn decode_redpanda_execution_batch<B: AsRef<[u8]>>(payloads: &[B]) -> Result<Vec<TradeRecord>> {
    let mut out = Vec::with_capacity(payloads.len());
    for raw in payloads {
        let fill = wire::ExecutionFillWire::decode(raw.as_ref())?;
        out.push(convert_wire_fill(fill, false));
    }
    Ok(out)
}

/// Decodes a batch of Redpanda Protobuf payloads from `hyperion.market.l2-snapshots.v1`.
pub fn decode_redpanda_l2_snapshot_batch<B: AsRef<[u8]>>(
    payloads: &[B],
) -> Result<Vec<QuoteRecord>> {
    let mut out = Vec::with_capacity(payloads.len());
    for raw in payloads {
        let snap = wire::L2BookSnapshotWire::decode(raw.as_ref())?;
        out.push(convert_wire_snapshot(snap));
    }
    Ok(out)
}

/// Decodes a batch of Redpanda Protobuf payloads from `hyperion.fraud.alerts.v1`.
pub fn decode_redpanda_fraud_alert_batch<B: AsRef<[u8]>>(
    payloads: &[B],
) -> Result<Vec<FraudAlertRecord>> {
    let mut out = Vec::with_capacity(payloads.len());
    for raw in payloads {
        let alert = wire::AnomalyAlertWire::decode(raw.as_ref())?;
        let inst_id = instrument_id_for_symbol(&alert.symbol);
        let pattern_type = match alert.pattern_type {
            1 => "WASH_TRADING_CYCLE",
            2 => "SPOOFING_LAYERING",
            3 => "VELOCITY_SPIKE",
            4 => "MOMENTUM_IGNITION",
            5 => "LATENCY_ARBITRAGE",
            _ => "UNSPECIFIED",
        };
        let primary_account = alert.offender_account_ids.first().copied().unwrap_or(0);
        out.push(FraudAlertRecord {
            symbol: if alert.symbol.is_empty() {
                symbol_for_instrument_id(inst_id).to_string()
            } else {
                alert.symbol
            },
            instrument_id: inst_id,
            timestamp_ns: alert.detected_ts_ns as i64,
            seq_id: alert.engine_seq,
            alert_id: alert.alert_id,
            account_id: primary_account,
            pattern_type,
            severity_score: alert.severity_score,
            confidence_ppm: alert.confidence_ppm,
        });
    }
    Ok(out)
}

#[inline]
fn convert_wire_fill(fill: wire::ExecutionFillWire, from_wal: bool) -> TradeRecord {
    let inst_id = if fill.instrument_id != 0 {
        fill.instrument_id
    } else {
        instrument_id_for_symbol(&fill.symbol)
    };
    let symbol = if fill.symbol.is_empty() {
        symbol_for_instrument_id(inst_id).to_string()
    } else {
        fill.symbol
    };
    let price_fp = fill.price_fp as i64;
    let qty_fp = fill.qty_fp as i64;
    let notional_i128 = match fill.notional_fp {
        Some(n) => n.to_u128() as i128,
        None => (i128::from(price_fp) * i128::from(qty_fp)) / FP_SCALE_I128,
    };
    TradeRecord {
        symbol,
        instrument_id: inst_id,
        timestamp_ns: fill.exec_ts_ns as i64,
        seq_id: fill.engine_seq,
        fill_id: fill.fill_id,
        maker_account_id: fill.maker_account_id,
        taker_account_id: fill.taker_account_id,
        is_buy_aggressor: fill.aggressor_side != 2, // 1 = BUY, 2 = SELL
        price_fp,
        qty_fp,
        notional_fp: saturate_i128_to_i64(notional_i128),
        maker_fee_fp: fill.maker_fee_fp,
        taker_fee_fp: fill.taker_fee_fp,
        from_wal,
    }
}

#[inline]
fn convert_wire_snapshot(snap: wire::L2BookSnapshotWire) -> QuoteRecord {
    let inst_id = if snap.instrument_id != 0 {
        snap.instrument_id
    } else {
        instrument_id_for_symbol(&snap.symbol)
    };
    let symbol = if snap.symbol.is_empty() {
        symbol_for_instrument_id(inst_id).to_string()
    } else {
        snap.symbol
    };
    let bid_depth: u128 = snap
        .bids
        .iter()
        .take(5)
        .map(|lvl| u128::from(lvl.total_qty_fp))
        .sum();
    let ask_depth: u128 = snap
        .asks
        .iter()
        .take(5)
        .map(|lvl| u128::from(lvl.total_qty_fp))
        .sum();

    let mid = if snap.mid_price_fp != 0 {
        snap.mid_price_fp as i64
    } else if snap.best_bid_fp != 0 && snap.best_ask_fp != 0 {
        ((snap.best_bid_fp as i128 + snap.best_ask_fp as i128) / 2) as i64
    } else {
        0
    };
    let spread = if snap.spread_fp != 0 {
        snap.spread_fp as i64
    } else if snap.best_ask_fp >= snap.best_bid_fp {
        (snap.best_ask_fp - snap.best_bid_fp) as i64
    } else {
        0
    };

    QuoteRecord {
        symbol,
        instrument_id: inst_id,
        timestamp_ns: snap.snapshot_ts_ns as i64,
        seq_id: snap.engine_seq,
        best_bid_fp: snap.best_bid_fp as i64,
        best_ask_fp: snap.best_ask_fp as i64,
        mid_price_fp: mid,
        spread_fp: spread,
        bid_depth_fp: saturate_i128_to_i64(bid_depth as i128),
        ask_depth_fp: saturate_i128_to_i64(ask_depth as i128),
        market_regime: "NORMAL",
    }
}

/// Converts a canonical `hyperion.v1.ExecutionFill` Protobuf message directly into `TradeRecord`.
pub fn trade_from_proto_fill(fill: &core_v1::ExecutionFill, from_wal: bool) -> TradeRecord {
    let inst_id = if fill.instrument_id != 0 {
        fill.instrument_id
    } else {
        instrument_id_for_symbol(&fill.symbol)
    };
    let symbol = if fill.symbol.is_empty() {
        symbol_for_instrument_id(inst_id).to_string()
    } else {
        fill.symbol.clone()
    };
    let price_fp = fill.price_fp as i64;
    let qty_fp = fill.qty_fp as i64;
    let notional_i128 = match fill.notional_fp {
        Some(n) => u128::from(n) as i128,
        None => (i128::from(price_fp) * i128::from(qty_fp)) / FP_SCALE_I128,
    };
    TradeRecord {
        symbol,
        instrument_id: inst_id,
        timestamp_ns: fill.exec_ts_ns as i64,
        seq_id: fill.engine_seq,
        fill_id: fill.fill_id,
        maker_account_id: fill.maker_account_id,
        taker_account_id: fill.taker_account_id,
        is_buy_aggressor: fill.aggressor_side != 2,
        price_fp,
        qty_fp,
        notional_fp: saturate_i128_to_i64(notional_i128),
        maker_fee_fp: fill.maker_fee_fp,
        taker_fee_fp: fill.taker_fee_fp,
        from_wal,
    }
}

/// Converts a canonical `hyperion.v1.L2BookSnapshot` Protobuf message directly into `QuoteRecord`.
pub fn quote_from_proto_snapshot(snap: &core_v1::L2BookSnapshot) -> QuoteRecord {
    let inst_id = if snap.instrument_id != 0 {
        snap.instrument_id
    } else {
        instrument_id_for_symbol(&snap.symbol)
    };
    let symbol = if snap.symbol.is_empty() {
        symbol_for_instrument_id(inst_id).to_string()
    } else {
        snap.symbol.clone()
    };
    let bid_depth: u128 = snap
        .bids
        .iter()
        .take(5)
        .map(|lvl| u128::from(lvl.total_qty_fp))
        .sum();
    let ask_depth: u128 = snap
        .asks
        .iter()
        .take(5)
        .map(|lvl| u128::from(lvl.total_qty_fp))
        .sum();
    let mid = if snap.mid_price_fp != 0 {
        snap.mid_price_fp as i64
    } else if snap.best_bid_fp != 0 && snap.best_ask_fp != 0 {
        ((snap.best_bid_fp as i128 + snap.best_ask_fp as i128) / 2) as i64
    } else {
        0
    };
    let spread = if snap.spread_fp != 0 {
        snap.spread_fp as i64
    } else if snap.best_ask_fp >= snap.best_bid_fp {
        (snap.best_ask_fp - snap.best_bid_fp) as i64
    } else {
        0
    };
    QuoteRecord {
        symbol,
        instrument_id: inst_id,
        timestamp_ns: snap.snapshot_ts_ns as i64,
        seq_id: snap.engine_seq,
        best_bid_fp: snap.best_bid_fp as i64,
        best_ask_fp: snap.best_ask_fp as i64,
        mid_price_fp: mid,
        spread_fp: spread,
        bid_depth_fp: saturate_i128_to_i64(bid_depth as i128),
        ask_depth_fp: saturate_i128_to_i64(ask_depth as i128),
        market_regime: "NORMAL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hyperion_v1_protobuf_wire_equivalence() {
        let proto_fill = core_v1::ExecutionFill {
            fill_id: 9001,
            engine_seq: 42,
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            maker_order_id: 100,
            taker_order_id: 101,
            maker_account_id: 5001,
            taker_account_id: 6002,
            aggressor_side: core_v1::OrderSide::Buy as i32,
            price_fp: 6_850_000_000_000,
            qty_fp: 150_000_000,
            notional_fp: Some(core_v1::U128::from(10_275_000_000_000_u128)),
            maker_fee_fp: -20_550_000,
            taker_fee_fp: 102_750_000,
            exec_ts_ns: 1_795_015_000_000_000_000,
            wal_blake3_hash: vec![0xAB; 32],
            trace: None,
        };
        let bytes = proto_fill.encode_to_vec();
        let decoded = decode_redpanda_execution_batch(&[bytes]).expect("decode canonical proto");
        let direct = trade_from_proto_fill(&proto_fill, false);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], direct);
    }
}
