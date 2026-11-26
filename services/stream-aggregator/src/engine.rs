use crate::candle::{CandleAccumulator, MultiTimeframeCandleBook, Timeframe};
use crate::config::{instrument_spec_for, AggregatorConfig, CANONICAL_INSTRUMENTS};
use crate::dedup::{PartitionCheckpoint, SeqDedupWindow};
use crate::fixed_point::calc_notional_fp;
use crate::orderbook_state::SymbolMicrostructureState;
use crate::watermark::{
    DeterministicWatermark, SequencedItem, WatermarkDecision, WatermarkReorderBuffer,
};
use hyperion_proto::{
    market_event_envelope::Payload, ExecutionFill, GetVolumeProfileResponse, L2BookSnapshot,
    L3BookDelta, LiquidityHeatmapBucket, MarketEventEnvelope, MicrostructureMetrics, OhlcvCandle,
    OrderSide, U128,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CHECKPOINT_MAGIC: &[u8; 8] = b"HYPAGG01";
pub const CHECKPOINT_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("checkpoint serialization failed: {0}")]
    Serialize(String),
    #[error("checkpoint magic/version mismatch")]
    InvalidCheckpointHeader,
    #[error("checkpoint CRC32C or Blake3 integrity check failed")]
    CorruptedCheckpoint,
}

/// Stack-only, zero-allocation `Copy` representation of `ExecutionFill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactFill {
    pub fill_id: u64,
    pub engine_seq: u64,
    pub instrument_id: u32,
    pub aggressor_side: i32,
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub maker_account_id: u64,
    pub taker_account_id: u64,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub notional_fp: u128,
    pub maker_fee_fp: i64,
    pub taker_fee_fp: i64,
    pub exec_ts_ns: u64,
    pub wal_blake3_hash: [u8; 32],
}

impl CompactFill {
    #[inline]
    pub fn from_proto(f: &ExecutionFill) -> Self {
        let notional = f
            .notional_fp
            .map(u128::from)
            .filter(|&v| v > 0)
            .unwrap_or_else(|| calc_notional_fp(f.price_fp, f.qty_fp));
        let mut hash = [0_u8; 32];
        let copy_len = f.wal_blake3_hash.len().min(32);
        hash[..copy_len].copy_from_slice(&f.wal_blake3_hash[..copy_len]);
        Self {
            fill_id: f.fill_id,
            engine_seq: f.engine_seq,
            instrument_id: f.instrument_id,
            aggressor_side: f.aggressor_side,
            maker_order_id: f.maker_order_id,
            taker_order_id: f.taker_order_id,
            maker_account_id: f.maker_account_id,
            taker_account_id: f.taker_account_id,
            price_fp: f.price_fp,
            qty_fp: f.qty_fp,
            notional_fp: notional,
            maker_fee_fp: f.maker_fee_fp,
            taker_fee_fp: f.taker_fee_fp,
            exec_ts_ns: f.exec_ts_ns,
            wal_blake3_hash: hash,
        }
    }

    pub fn to_proto(&self, symbol: &str) -> ExecutionFill {
        ExecutionFill {
            fill_id: self.fill_id,
            engine_seq: self.engine_seq,
            symbol: symbol.to_string(),
            instrument_id: self.instrument_id,
            maker_order_id: self.maker_order_id,
            taker_order_id: self.taker_order_id,
            maker_account_id: self.maker_account_id,
            taker_account_id: self.taker_account_id,
            aggressor_side: self.aggressor_side,
            price_fp: self.price_fp,
            qty_fp: self.qty_fp,
            notional_fp: Some(U128::from(self.notional_fp)),
            maker_fee_fp: self.maker_fee_fp,
            taker_fee_fp: self.taker_fee_fp,
            exec_ts_ns: self.exec_ts_ns,
            wal_blake3_hash: self.wal_blake3_hash.to_vec(),
            trace: None,
        }
    }
}

/// Stack-only, zero-allocation `Copy` representation of `L3BookDelta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactL3Delta {
    pub engine_seq: u64,
    pub prev_engine_seq: u64,
    pub instrument_id: u32,
    pub op: i32,
    pub side: i32,
    pub order_id: u64,
    pub account_id: u64,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub delta_qty_fp: i64,
    pub event_ts_ns: u64,
}

impl CompactL3Delta {
    #[inline]
    pub fn from_proto(d: &L3BookDelta) -> Self {
        Self {
            engine_seq: d.engine_seq,
            prev_engine_seq: d.prev_engine_seq,
            instrument_id: d.instrument_id,
            op: d.op,
            side: d.side,
            order_id: d.order_id,
            account_id: d.account_id,
            price_fp: d.price_fp,
            remaining_qty_fp: d.remaining_qty_fp,
            delta_qty_fp: d.delta_qty_fp,
            event_ts_ns: d.event_ts_ns,
        }
    }

    pub fn to_proto(&self, symbol: &str) -> L3BookDelta {
        L3BookDelta {
            engine_seq: self.engine_seq,
            prev_engine_seq: self.prev_engine_seq,
            symbol: symbol.to_string(),
            instrument_id: self.instrument_id,
            op: self.op,
            side: self.side,
            order_id: self.order_id,
            account_id: self.account_id,
            price_fp: self.price_fp,
            remaining_qty_fp: self.remaining_qty_fp,
            delta_qty_fp: self.delta_qty_fp,
            event_ts_ns: self.event_ts_ns,
            state_root_blake3: Vec::new(),
            trace: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactPayload {
    Fill(CompactFill),
    L3Delta(CompactL3Delta),
}

/// High-level market event variants accepted by the aggregator.
#[derive(Debug, Clone)]
pub enum IngestEvent {
    Execution(ExecutionFill),
    L3Delta(L3BookDelta),
    L2Snapshot(L2BookSnapshot),
}

impl IngestEvent {
    pub fn from_envelope(env: MarketEventEnvelope) -> Option<Self> {
        match env.payload? {
            Payload::Fill(f) => Some(Self::Execution(f)),
            Payload::L3Delta(d) => Some(Self::L3Delta(d)),
            Payload::L2Snapshot(s) => Some(Self::L2Snapshot(s)),
            Payload::OrderAck(_) | Payload::JournalEntry(_) => None,
        }
    }

    pub fn symbol(&self) -> &str {
        match self {
            Self::Execution(f) => &f.symbol,
            Self::L3Delta(d) => &d.symbol,
            Self::L2Snapshot(s) => &s.symbol,
        }
    }

    pub fn engine_seq(&self) -> u64 {
        match self {
            Self::Execution(f) => f.engine_seq,
            Self::L3Delta(d) => d.engine_seq,
            Self::L2Snapshot(s) => s.engine_seq,
        }
    }

    pub fn ts_ns(&self) -> u64 {
        match self {
            Self::Execution(f) => f.exec_ts_ns,
            Self::L3Delta(d) => d.event_ts_ns,
            Self::L2Snapshot(s) => s.snapshot_ts_ns,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Accepted,
    DuplicateRejected,
    OffsetAlreadyCommitted,
    LateDropped { watermark_ts_ns: u64 },
    InvalidPayload,
}

/// Canonical sub-sequence tie-breaker shared with `ReferenceOracle`:
/// `L2Snapshot` (1) < `L3Delta` (2) < `ExecutionFill` (3).
#[inline(always)]
pub const fn canonical_sub_seq(kind_rank: u64, item_id: u64) -> u64 {
    (kind_rank << 48) | (item_id & 0x0000_FFFF_FFFF_FFFF)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SymbolPipelineSnapshot {
    symbol: String,
    fill_dedup: SeqDedupWindow,
    l3_dedup: SeqDedupWindow,
    l2_dedup: SeqDedupWindow,
    fill_watermark: DeterministicWatermark,
    l3_watermark: DeterministicWatermark,
    reorder_items: Vec<SequencedItem<CompactPayload>>,
    candles: MultiTimeframeCandleBook,
    micro: SymbolMicrostructureState,
}

#[derive(Debug, Clone)]
pub struct SymbolPipeline {
    symbol: String,
    fill_dedup: SeqDedupWindow,
    l3_dedup: SeqDedupWindow,
    l2_dedup: SeqDedupWindow,
    fill_watermark: DeterministicWatermark,
    l3_watermark: DeterministicWatermark,
    reorder_buf: WatermarkReorderBuffer<CompactPayload>,
    candles: MultiTimeframeCandleBook,
    micro: SymbolMicrostructureState,
    dirty_for_flush: bool,
    pending_executions: Vec<CompactFill>,
    pending_l3_deltas: Vec<CompactL3Delta>,
    collect_raw_events_for_db: bool,
}

impl SymbolPipeline {
    pub fn new(
        symbol: impl Into<String>,
        cfg: &AggregatorConfig,
        collect_raw_events_for_db: bool,
    ) -> Self {
        let sym = symbol.into();
        Self {
            symbol: sym.clone(),
            fill_dedup: SeqDedupWindow::new(),
            l3_dedup: SeqDedupWindow::new(),
            l2_dedup: SeqDedupWindow::new(),
            fill_watermark: DeterministicWatermark::new(cfg.watermark_lateness_ns),
            l3_watermark: DeterministicWatermark::new(cfg.watermark_lateness_ns),
            reorder_buf: WatermarkReorderBuffer::with_capacity(cfg.max_reorder_buffer),
            candles: MultiTimeframeCandleBook::new(cfg.hot_candle_ring_capacity),
            micro: SymbolMicrostructureState::new(
                sym,
                cfg.heatmap_time_bucket_ns,
                cfg.hot_heatmap_columns,
            ),
            dirty_for_flush: false,
            pending_executions: if collect_raw_events_for_db {
                Vec::with_capacity(cfg.batch_flush_max_events.min(4096))
            } else {
                Vec::new()
            },
            pending_l3_deltas: if collect_raw_events_for_db {
                Vec::with_capacity(cfg.batch_flush_max_events.min(4096))
            } else {
                Vec::new()
            },
            collect_raw_events_for_db,
        }
    }

    /// Multi-topic aligned watermark (`min` across active continuous streams) so a sparse L3 topic
    /// can never outrun a dense executions topic and drop valid events.
    #[inline(always)]
    pub fn effective_watermark_ts_ns(&self) -> u64 {
        let has_fill = self.fill_watermark.max_event_ts_ns() > 0;
        let has_l3 = self.l3_watermark.max_event_ts_ns() > 0;
        match (has_fill, has_l3) {
            (true, true) => self
                .fill_watermark
                .watermark_ts_ns()
                .min(self.l3_watermark.watermark_ts_ns()),
            (true, false) => self.fill_watermark.watermark_ts_ns(),
            (false, true) => self.l3_watermark.watermark_ts_ns(),
            (false, false) => 0,
        }
    }

    #[inline(always)]
    pub fn max_event_ts_ns(&self) -> u64 {
        self.fill_watermark
            .max_event_ts_ns()
            .max(self.l3_watermark.max_event_ts_ns())
    }

    #[inline]
    fn apply_committed_payload(&mut self, payload: CompactPayload) {
        match payload {
            CompactPayload::Fill(f) => {
                let side = OrderSide::try_from(f.aggressor_side).unwrap_or(OrderSide::Unspecified);
                self.candles.apply_fill(
                    f.exec_ts_ns,
                    f.engine_seq,
                    f.fill_id,
                    f.price_fp,
                    f.qty_fp,
                    Some(f.notional_fp),
                    side,
                    self.collect_raw_events_for_db,
                );
                self.micro.apply_execution_fill(
                    f.exec_ts_ns,
                    f.engine_seq,
                    f.price_fp,
                    f.qty_fp,
                    side,
                );
                if self.collect_raw_events_for_db && self.pending_executions.len() < 16_384 {
                    self.pending_executions.push(f);
                }
            }
            CompactPayload::L3Delta(d) => {
                self.micro.apply_l3_fields(
                    d.engine_seq,
                    d.op,
                    d.side,
                    d.order_id,
                    d.price_fp,
                    d.remaining_qty_fp,
                    d.delta_qty_fp,
                    d.event_ts_ns,
                    self.collect_raw_events_for_db,
                );
                if self.collect_raw_events_for_db && self.pending_l3_deltas.len() < 16_384 {
                    self.pending_l3_deltas.push(d);
                }
            }
        }
        self.dirty_for_flush = true;
    }

    #[inline]
    fn drain_ready_up_to(&mut self, watermark_ts_ns: u64) {
        while let Some(item) = self.reorder_buf.pop_ready(watermark_ts_ns) {
            self.apply_committed_payload(item.payload);
        }
        self.candles
            .advance_watermark(watermark_ts_ns, self.collect_raw_events_for_db);
    }

    #[inline]
    pub fn ingest_compact_fill(&mut self, fill: CompactFill) -> IngestOutcome {
        if fill.price_fp == 0 || fill.qty_fp == 0 || fill.exec_ts_ns == 0 {
            return IngestOutcome::InvalidPayload;
        }
        let dedup_id = if fill.fill_id > 0 {
            fill.fill_id
        } else {
            fill.engine_seq
        };
        if !self.fill_dedup.check_and_mark(dedup_id) {
            return IngestOutcome::DuplicateRejected;
        }

        match self.fill_watermark.observe(fill.exec_ts_ns) {
            WatermarkDecision::Accept { .. } => {}
            WatermarkDecision::LateDrop {
                current_watermark_ts_ns,
            } => {
                return IngestOutcome::LateDropped {
                    watermark_ts_ns: current_watermark_ts_ns,
                };
            }
        }
        let wm = self.effective_watermark_ts_ns();

        if self.fill_watermark.allowed_lateness_ns() == 0 && self.reorder_buf.is_empty() {
            self.apply_committed_payload(CompactPayload::Fill(fill));
            self.candles
                .advance_watermark(wm, self.collect_raw_events_for_db);
            return IngestOutcome::Accepted;
        }

        self.reorder_buf.push(SequencedItem {
            ts_ns: fill.exec_ts_ns,
            engine_seq: fill.engine_seq,
            sub_seq: canonical_sub_seq(3, dedup_id),
            payload: CompactPayload::Fill(fill),
        });
        self.drain_ready_up_to(wm);
        IngestOutcome::Accepted
    }

    #[inline]
    pub fn ingest_compact_l3(&mut self, delta: CompactL3Delta) -> IngestOutcome {
        if delta.event_ts_ns == 0 || delta.engine_seq == 0 {
            return IngestOutcome::InvalidPayload;
        }
        if !self.l3_dedup.check_and_mark(delta.engine_seq) {
            return IngestOutcome::DuplicateRejected;
        }

        match self.l3_watermark.observe(delta.event_ts_ns) {
            WatermarkDecision::Accept { .. } => {}
            WatermarkDecision::LateDrop {
                current_watermark_ts_ns,
            } => {
                return IngestOutcome::LateDropped {
                    watermark_ts_ns: current_watermark_ts_ns,
                };
            }
        }
        let wm = self.effective_watermark_ts_ns();

        if self.l3_watermark.allowed_lateness_ns() == 0 && self.reorder_buf.is_empty() {
            self.apply_committed_payload(CompactPayload::L3Delta(delta));
            return IngestOutcome::Accepted;
        }

        self.reorder_buf.push(SequencedItem {
            ts_ns: delta.event_ts_ns,
            engine_seq: delta.engine_seq,
            sub_seq: canonical_sub_seq(2, delta.order_id),
            payload: CompactPayload::L3Delta(delta),
        });
        self.drain_ready_up_to(wm);
        IngestOutcome::Accepted
    }

    pub fn ingest_l2_snapshot(&mut self, snap: &L2BookSnapshot) -> IngestOutcome {
        if snap.snapshot_ts_ns == 0 || snap.engine_seq == 0 {
            return IngestOutcome::InvalidPayload;
        }
        if !self.l2_dedup.check_and_mark(snap.engine_seq) {
            return IngestOutcome::DuplicateRejected;
        }
        let eff_wm = self.effective_watermark_ts_ns();
        if eff_wm > 0 && snap.snapshot_ts_ns < eff_wm {
            return IngestOutcome::LateDropped {
                watermark_ts_ns: eff_wm,
            };
        }
        // Drain all buffered items <= snapshot_ts_ns before applying snapshot
        self.drain_ready_up_to(snap.snapshot_ts_ns);
        self.micro
            .apply_l2_snapshot(snap, self.collect_raw_events_for_db);
        self.dirty_for_flush = true;
        IngestOutcome::Accepted
    }

    /// Flushes all currently buffered items within the watermark reorder queue in strict canonical order.
    /// Used on deterministic stream-end / seal boundaries.
    pub fn flush_watermark_horizon(&mut self) {
        let remaining = self.reorder_buf.drain_all_ordered();
        for item in remaining {
            self.apply_committed_payload(item.payload);
        }
        let max_ts = self.max_event_ts_ns();
        if max_ts > 0 {
            self.candles
                .advance_watermark(max_ts, self.collect_raw_events_for_db);
            self.dirty_for_flush = true;
        }
    }

    pub fn candles(&self) -> &MultiTimeframeCandleBook {
        &self.candles
    }

    pub fn micro(&self) -> &SymbolMicrostructureState {
        &self.micro
    }

    pub fn watermark(&self) -> &DeterministicWatermark {
        &self.fill_watermark
    }

    pub fn fill_dedup(&self) -> &SeqDedupWindow {
        &self.fill_dedup
    }

    pub fn l3_dedup(&self) -> &SeqDedupWindow {
        &self.l3_dedup
    }

    pub fn memory_capacity_units(&self) -> usize {
        self.candles.allocated_capacity_slots()
            + self.micro.allocated_capacity_proxy()
            + self.reorder_buf.capacity()
    }

    fn to_snapshot(&self) -> SymbolPipelineSnapshot {
        SymbolPipelineSnapshot {
            symbol: self.symbol.clone(),
            fill_dedup: self.fill_dedup.clone(),
            l3_dedup: self.l3_dedup.clone(),
            l2_dedup: self.l2_dedup.clone(),
            fill_watermark: self.fill_watermark,
            l3_watermark: self.l3_watermark,
            reorder_items: self.reorder_buf.snapshot_items(),
            candles: self.candles.clone(),
            micro: self.micro.clone(),
        }
    }

    fn from_snapshot(
        snap: SymbolPipelineSnapshot,
        cfg: &AggregatorConfig,
        collect_raw_events_for_db: bool,
    ) -> Self {
        let mut reorder_buf = WatermarkReorderBuffer::with_capacity(cfg.max_reorder_buffer);
        reorder_buf.restore_items(snap.reorder_items);
        Self {
            symbol: snap.symbol,
            fill_dedup: snap.fill_dedup,
            l3_dedup: snap.l3_dedup,
            l2_dedup: snap.l2_dedup,
            fill_watermark: snap.fill_watermark,
            l3_watermark: snap.l3_watermark,
            reorder_buf,
            candles: snap.candles,
            micro: snap.micro,
            dirty_for_flush: false,
            pending_executions: Vec::new(),
            pending_l3_deltas: Vec::new(),
            collect_raw_events_for_db,
        }
    }
}

/// Atomic database & broker batch generated at flush boundaries.
#[derive(Debug, Clone, Default)]
pub struct FlushBatch {
    pub consumer_group: String,
    pub checkpoints: Vec<PartitionCheckpoint>,
    pub executions: Vec<ExecutionFill>,
    pub l3_deltas: Vec<L3BookDelta>,
    pub candles: Vec<OhlcvCandle>,
    pub microstructure: Vec<MicrostructureMetrics>,
    pub heatmap_buckets: Vec<(String, LiquidityHeatmapBucket)>,
    /// Integrity-protected binary snapshot (`HYPAGG01` + CRC32C + Blake3 + bincode state).
    pub engine_checkpoint_blob: Vec<u8>,
}

impl FlushBatch {
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
            && self.executions.is_empty()
            && self.l3_deltas.is_empty()
            && self.candles.is_empty()
            && self.microstructure.is_empty()
            && self.heatmap_buckets.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EngineFullSnapshot {
    consumer_group: String,
    partitions: Vec<PartitionCheckpoint>,
    symbols: Vec<SymbolPipelineSnapshot>,
    total_accepted_events: u64,
    total_duplicate_events: u64,
    total_late_events: u64,
}

/// Top-level Single-Writer Exactly-Once Stream Aggregator Engine.
pub struct StreamAggregatorEngine {
    cfg: AggregatorConfig,
    /// Fast array slot for canonical symbols 1..=4 (`BTC-USDT`, `ETH-USDT`, `SOL-USDT`, `HYPE-USDT`)
    canonical_slots: [SymbolPipeline; 4],
    dynamic_symbols: FxHashMap<String, SymbolPipeline>,
    partitions: FxHashMap<(String, i32), PartitionCheckpoint>,
    collect_raw_events_for_db: bool,
    total_accepted_events: u64,
    total_duplicate_events: u64,
    total_late_events: u64,
}

impl StreamAggregatorEngine {
    pub fn new(cfg: AggregatorConfig) -> Self {
        Self::with_raw_collection(cfg, true)
    }

    pub fn with_raw_collection(cfg: AggregatorConfig, collect_raw_events_for_db: bool) -> Self {
        let canonical_slots = [
            SymbolPipeline::new(
                CANONICAL_INSTRUMENTS[0].symbol,
                &cfg,
                collect_raw_events_for_db,
            ),
            SymbolPipeline::new(
                CANONICAL_INSTRUMENTS[1].symbol,
                &cfg,
                collect_raw_events_for_db,
            ),
            SymbolPipeline::new(
                CANONICAL_INSTRUMENTS[2].symbol,
                &cfg,
                collect_raw_events_for_db,
            ),
            SymbolPipeline::new(
                CANONICAL_INSTRUMENTS[3].symbol,
                &cfg,
                collect_raw_events_for_db,
            ),
        ];
        Self {
            cfg,
            canonical_slots,
            dynamic_symbols: FxHashMap::default(),
            partitions: FxHashMap::default(),
            collect_raw_events_for_db,
            total_accepted_events: 0,
            total_duplicate_events: 0,
            total_late_events: 0,
        }
    }

    #[inline(always)]
    fn canonical_index_by_id(instrument_id: u32) -> Option<usize> {
        match instrument_id {
            1..=4 => Some((instrument_id - 1) as usize),
            _ => None,
        }
    }

    #[inline(always)]
    fn canonical_index_by_symbol(symbol: &str) -> Option<usize> {
        match symbol {
            "BTC-USDT" => Some(0),
            "ETH-USDT" => Some(1),
            "SOL-USDT" => Some(2),
            "HYPE-USDT" => Some(3),
            _ => None,
        }
    }

    #[inline]
    fn pipeline_mut(&mut self, symbol: &str, instrument_id: u32) -> &mut SymbolPipeline {
        if let Some(idx) = Self::canonical_index_by_id(instrument_id)
            .or_else(|| Self::canonical_index_by_symbol(symbol))
        {
            return &mut self.canonical_slots[idx];
        }
        let cfg = &self.cfg;
        let collect = self.collect_raw_events_for_db;
        self.dynamic_symbols
            .entry(symbol.to_string())
            .or_insert_with(|| SymbolPipeline::new(symbol, cfg, collect))
    }

    #[inline]
    pub fn pipeline(&self, symbol: &str) -> Option<&SymbolPipeline> {
        if let Some(idx) = Self::canonical_index_by_symbol(symbol) {
            return Some(&self.canonical_slots[idx]);
        }
        self.dynamic_symbols.get(symbol)
    }

    #[inline]
    fn record_outcome(&mut self, outcome: IngestOutcome) -> IngestOutcome {
        match outcome {
            IngestOutcome::Accepted => self.total_accepted_events += 1,
            IngestOutcome::DuplicateRejected | IngestOutcome::OffsetAlreadyCommitted => {
                self.total_duplicate_events += 1;
            }
            IngestOutcome::LateDropped { .. } => self.total_late_events += 1,
            IngestOutcome::InvalidPayload => {}
        }
        outcome
    }

    /// Hot-path zero-allocation ingestion for canonical `CompactFill` (`O(1)`).
    #[inline]
    pub fn ingest_compact_fill(&mut self, symbol: &str, fill: CompactFill) -> IngestOutcome {
        let out = self
            .pipeline_mut(symbol, fill.instrument_id)
            .ingest_compact_fill(fill);
        self.record_outcome(out)
    }

    /// Hot-path zero-allocation ingestion for canonical `CompactL3Delta` (`O(1)`).
    #[inline]
    pub fn ingest_compact_l3(&mut self, symbol: &str, delta: CompactL3Delta) -> IngestOutcome {
        let out = self
            .pipeline_mut(symbol, delta.instrument_id)
            .ingest_compact_l3(delta);
        self.record_outcome(out)
    }

    /// Ingests an `IngestEvent` without partition tracking (for direct unit/benchmark calls).
    pub fn ingest_event(&mut self, event: &IngestEvent) -> IngestOutcome {
        match event {
            IngestEvent::Execution(f) => {
                let mut cf = CompactFill::from_proto(f);
                if cf.instrument_id == 0 {
                    cf.instrument_id = instrument_spec_for(&f.symbol).instrument_id;
                }
                self.ingest_compact_fill(&f.symbol, cf)
            }
            IngestEvent::L3Delta(d) => {
                let mut cd = CompactL3Delta::from_proto(d);
                if cd.instrument_id == 0 {
                    cd.instrument_id = instrument_spec_for(&d.symbol).instrument_id;
                }
                self.ingest_compact_l3(&d.symbol, cd)
            }
            IngestEvent::L2Snapshot(s) => {
                let out = self
                    .pipeline_mut(&s.symbol, s.instrument_id)
                    .ingest_l2_snapshot(s);
                self.record_outcome(out)
            }
        }
    }

    /// Partition-aware transactional ingestion from Redpanda `(topic, partition_id, kafka_offset)`.
    /// Enforces both monotonic `kafka_offset > last_kafka_offset` AND sequence ID deduplication!
    pub fn ingest_broker_record(
        &mut self,
        topic: &str,
        partition_id: i32,
        kafka_offset: i64,
        event: &IngestEvent,
    ) -> IngestOutcome {
        let key = (topic.to_string(), partition_id);
        let group = self.cfg.consumer_group.clone();
        {
            let cp = self
                .partitions
                .entry(key.clone())
                .or_insert_with(|| PartitionCheckpoint::new(group, topic, partition_id));
            if !cp.accepts_offset(kafka_offset) {
                return self.record_outcome(IngestOutcome::OffsetAlreadyCommitted);
            }
        }

        let sym = event.symbol();
        let seq = event.engine_seq();
        let outcome = self.ingest_event(event);
        let wm = self
            .pipeline(sym)
            .map(|p| p.effective_watermark_ts_ns())
            .unwrap_or(0);

        if let Some(cp) = self.partitions.get_mut(&key) {
            cp.advance(kafka_offset, seq, wm);
        }
        outcome
    }

    /// Drains all buffered items inside watermark reorder queues across all symbols.
    pub fn flush_all_watermarks(&mut self) {
        for slot in &mut self.canonical_slots {
            slot.flush_watermark_horizon();
        }
        for pipe in self.dynamic_symbols.values_mut() {
            pipe.flush_watermark_horizon();
        }
    }

    /// Builds a deterministic `FlushBatch` containing all dirty candles, raw fills/deltas,
    /// latest microstructure snapshots, heatmap buckets, partition offsets, and a Blake3+CRC32C
    /// checkpoint state blob for atomic database persistence.
    pub fn prepare_flush_batch(&mut self) -> Result<FlushBatch, EngineError> {
        let mut candles = Vec::new();
        let mut executions = Vec::new();
        let mut l3_deltas = Vec::new();
        let mut microstructure = Vec::new();
        let mut heatmap_buckets = Vec::new();

        for pipe in self
            .canonical_slots
            .iter_mut()
            .chain(self.dynamic_symbols.values_mut())
        {
            if !pipe.dirty_for_flush {
                continue;
            }
            pipe.dirty_for_flush = false;
            let sym = pipe.symbol.clone();

            for c in pipe.candles.drain_all_dirty() {
                candles.push(c.to_proto(&sym));
            }
            for f in pipe.pending_executions.drain(..) {
                executions.push(f.to_proto(&sym));
            }
            for d in pipe.pending_l3_deltas.drain(..) {
                l3_deltas.push(d.to_proto(&sym));
            }
            let m = pipe.micro.snapshot_metrics();
            if m.engine_seq > 0 {
                microstructure.push(m);
            }
            for h in pipe.micro.drain_pending_heatmap() {
                heatmap_buckets.push((sym.clone(), h));
            }
        }

        let checkpoints: Vec<PartitionCheckpoint> = self.partitions.values().cloned().collect();
        let engine_checkpoint_blob = self.serialize_checkpoint()?;

        Ok(FlushBatch {
            consumer_group: self.cfg.consumer_group.clone(),
            checkpoints,
            executions,
            l3_deltas,
            candles,
            microstructure,
            heatmap_buckets,
            engine_checkpoint_blob,
        })
    }

    /// Serializes the exact deterministic engine state with `HYPAGG01` + `CRC32C` + `Blake3` framing.
    pub fn serialize_checkpoint(&self) -> Result<Vec<u8>, EngineError> {
        let mut symbols = Vec::with_capacity(4 + self.dynamic_symbols.len());
        for s in &self.canonical_slots {
            symbols.push(s.to_snapshot());
        }
        for s in self.dynamic_symbols.values() {
            symbols.push(s.to_snapshot());
        }

        let snap = EngineFullSnapshot {
            consumer_group: self.cfg.consumer_group.clone(),
            partitions: self.partitions.values().cloned().collect(),
            symbols,
            total_accepted_events: self.total_accepted_events,
            total_duplicate_events: self.total_duplicate_events,
            total_late_events: self.total_late_events,
        };

        let payload =
            bincode::serialize(&snap).map_err(|e| EngineError::Serialize(e.to_string()))?;
        let crc = crc32c::crc32c(&payload);
        let blake = blake3::hash(&payload);

        let mut frame = Vec::with_capacity(8 + 4 + 4 + 32 + payload.len());
        frame.extend_from_slice(CHECKPOINT_MAGIC);
        frame.extend_from_slice(&CHECKPOINT_VERSION.to_le_bytes());
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(blake.as_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Restores exact engine state from a persisted `HYPAGG01` checkpoint blob, verifying both
    /// Castagnoli `CRC32C` and `Blake3` digests before applying state.
    pub fn restore_from_checkpoint(&mut self, blob: &[u8]) -> Result<(), EngineError> {
        if blob.len() < 48 {
            return Err(EngineError::InvalidCheckpointHeader);
        }
        if &blob[0..8] != CHECKPOINT_MAGIC {
            return Err(EngineError::InvalidCheckpointHeader);
        }
        let version = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
        if version != CHECKPOINT_VERSION {
            return Err(EngineError::InvalidCheckpointHeader);
        }
        let expected_crc = u32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
        let expected_blake = &blob[16..48];
        let payload = &blob[48..];

        if crc32c::crc32c(payload) != expected_crc {
            return Err(EngineError::CorruptedCheckpoint);
        }
        if blake3::hash(payload).as_bytes() != expected_blake {
            return Err(EngineError::CorruptedCheckpoint);
        }

        let snap: EngineFullSnapshot =
            bincode::deserialize(payload).map_err(|_| EngineError::CorruptedCheckpoint)?;

        self.partitions.clear();
        for cp in snap.partitions {
            self.partitions
                .insert((cp.topic.clone(), cp.partition_id), cp);
        }

        self.dynamic_symbols.clear();
        for sym_snap in snap.symbols {
            if let Some(idx) = Self::canonical_index_by_symbol(&sym_snap.symbol) {
                self.canonical_slots[idx] = SymbolPipeline::from_snapshot(
                    sym_snap,
                    &self.cfg,
                    self.collect_raw_events_for_db,
                );
            } else {
                let name = sym_snap.symbol.clone();
                self.dynamic_symbols.insert(
                    name,
                    SymbolPipeline::from_snapshot(
                        sym_snap,
                        &self.cfg,
                        self.collect_raw_events_for_db,
                    ),
                );
            }
        }

        self.total_accepted_events = snap.total_accepted_events;
        self.total_duplicate_events = snap.total_duplicate_events;
        self.total_late_events = snap.total_late_events;
        Ok(())
    }

    pub fn partition_offset(&self, topic: &str, partition_id: i32) -> i64 {
        self.partitions
            .get(&(topic.to_string(), partition_id))
            .map(|c| c.last_kafka_offset)
            .unwrap_or(-1)
    }

    pub fn query_candles(
        &self,
        symbol: &str,
        tf: Timeframe,
        from_ts_ns: u64,
        to_ts_ns: u64,
        limit: usize,
    ) -> Vec<OhlcvCandle> {
        let Some(pipe) = self.pipeline(symbol) else {
            return Vec::new();
        };
        pipe.candles
            .series(tf)
            .query_candles(from_ts_ns, to_ts_ns, limit)
            .into_iter()
            .map(|c: CandleAccumulator| c.to_proto(symbol))
            .collect()
    }

    pub fn query_latest_microstructure(&self, symbol: &str) -> Option<MicrostructureMetrics> {
        let pipe = self.pipeline(symbol)?;
        Some(pipe.micro.snapshot_metrics())
    }

    pub fn query_volume_profile(
        &self,
        symbol: &str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        bucket_size_fp: u64,
    ) -> GetVolumeProfileResponse {
        let Some(pipe) = self.pipeline(symbol) else {
            return GetVolumeProfileResponse {
                symbol: symbol.to_string(),
                poc_price_fp: 0,
                value_area_high_fp: 0,
                value_area_low_fp: 0,
                bins: Vec::new(),
            };
        };
        pipe.micro
            .query_volume_profile(from_ts_ns, to_ts_ns, bucket_size_fp)
    }

    pub fn query_heatmap(
        &self,
        symbol: &str,
        from_ts_ns: u64,
        to_ts_ns: u64,
        price_bucket_size_fp: u64,
        time_bucket_ns: u64,
    ) -> Vec<LiquidityHeatmapBucket> {
        let Some(pipe) = self.pipeline(symbol) else {
            return Vec::new();
        };
        pipe.micro
            .query_heatmap(from_ts_ns, to_ts_ns, price_bucket_size_fp, time_bucket_ns)
    }

    #[inline(always)]
    pub fn total_accepted_events(&self) -> u64 {
        self.total_accepted_events
    }

    #[inline(always)]
    pub fn total_duplicate_events(&self) -> u64 {
        self.total_duplicate_events
    }

    #[inline(always)]
    pub fn total_late_events(&self) -> u64 {
        self.total_late_events
    }

    pub fn steady_state_capacity_units(&self) -> usize {
        self.canonical_slots
            .iter()
            .map(|s| s.memory_capacity_units())
            .sum::<usize>()
            + self
                .dynamic_symbols
                .values()
                .map(|s| s.memory_capacity_units())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_integrity_checks_both_crc32c_and_blake3() {
        let cfg = AggregatorConfig::default();
        let eng = StreamAggregatorEngine::new(cfg.clone());
        let valid = eng
            .serialize_checkpoint()
            .expect("serialize valid checkpoint");

        // 1. Valid checkpoint restores cleanly
        let mut dst = StreamAggregatorEngine::new(cfg.clone());
        dst.restore_from_checkpoint(&valid)
            .expect("valid checkpoint must restore");

        // 2. Tamper payload -> CRC32C check rejects
        let mut crc_bad = valid.clone();
        let last = crc_bad.len() - 1;
        crc_bad[last] ^= 0x01;
        assert!(matches!(
            dst.restore_from_checkpoint(&crc_bad),
            Err(EngineError::CorruptedCheckpoint)
        ));

        // 3. Tamper Blake3 digest only (CRC32C still matches payload!) -> Blake3 check rejects
        let mut blake_bad = valid;
        blake_bad[20] ^= 0xAA;
        assert!(matches!(
            dst.restore_from_checkpoint(&blake_bad),
            Err(EngineError::CorruptedCheckpoint)
        ));
    }
}
