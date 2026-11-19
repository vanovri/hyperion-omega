#![deny(clippy::float_arithmetic)]

use crate::arena_lob::{
    InstrumentOrderBook, OrderKind, OrderRecord, PriceLevelSummary, Side, StpPolicy, NULL_IDX,
};
use crate::fixed::{
    checked_buyer_reserve_fp, checked_fee_fp, checked_notional_fp, checked_slippage_limit_fp,
    AssetSymbol, ClientOrderId, InstrumentSymbol,
};
use crate::ledger::{
    AccountRiskStatus, AssetGlobalTotals, CompactJournalEntry, DoubleEntryLedger, LedgerError,
    TREASURY_ACCOUNT_ID,
};
use crate::merkle::{
    hash_execution_fill_leaf, verify_merkle_proof, AssetBalanceLeaf, ExecutionFillDigestInput,
    ExecutionInclusionProof, IncrementalMerkleState, LobStateAccumulator, ProofStep,
};
use crate::ring::SpscProducer;
use crate::wal::{
    SnapshotAccountV1, SnapshotAssetBalanceV1, SnapshotAssetTotalV1, SnapshotInstrumentV1,
    SnapshotOrderV1, SnapshotPayloadV1, SnapshotStore, WalError, WalFrame, WalSyncMode,
    WalWriter,
};
use std::path::{Path, PathBuf};

pub const DEFAULT_MAKER_FEE_PPM: i64 = 100; // 1.0 bps (0.01%)
pub const DEFAULT_TAKER_FEE_PPM: i64 = 250; // 2.5 bps (0.025%)
pub const MAX_INSTRUMENTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MarketRegimeState {
    Normal = 1,
    VolatilityGuard = 2,
    PostOnlyAuction = 3,
    Halted = 4,
}

impl MarketRegimeState {
    #[inline(always)]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            2 => Self::VolatilityGuard,
            3 => Self::PostOnlyAuction,
            4 => Self::Halted,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum AckStatus {
    Accepted = 1,
    PartiallyFilled = 2,
    Filled = 3,
    Canceled = 4,
    Modified = 5,
    Rejected = 6,
    Expired = 7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum RejectCode {
    None = 1,
    InsufficientBalance = 2,
    PostOnlyWouldCross = 3,
    FokInsufficientLiquidity = 4,
    SelfTradePrevented = 5,
    AccountFrozen = 6,
    MarketHalted = 7,
    InvalidPriceOrQty = 8,
    DuplicateIdempotencyKey = 9,
    OrderNotFound = 10,
    MaxOrderSizeExceeded = 11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum L3OpCode {
    Add = 1,
    Modify = 2,
    Cancel = 3,
    Fill = 4,
    Clear = 5,
}

/// Zero-allocation stack representation of an execution fill generated on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawFillEvent {
    pub fill_id: u64,
    pub engine_seq: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub maker_account_id: u64,
    pub taker_account_id: u64,
    pub aggressor_side: Side,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub notional_fp: u128,
    pub maker_fee_fp: i64,
    pub taker_fee_fp: i64,
    pub exec_ts_ns: u64,
    pub wal_blake3_hash: [u8; 32],
}

/// Zero-allocation stack representation of an L3 order book delta generated on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawL3Delta {
    pub engine_seq: u64,
    pub prev_engine_seq: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub op: L3OpCode,
    pub side: Side,
    pub order_id: u64,
    pub account_id: u64,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub delta_qty_fp: i64,
    pub event_ts_ns: u64,
    pub state_root_blake3: [u8; 32],
}

/// Outbound event pushed wait-free from the matching thread into the SPSC ring buffer
/// for asynchronous Redpanda & gRPC streaming fan-out (`0` heap allocs on push!).
#[derive(Debug, Clone, Copy)]
#[allow(clippy::large_enum_variant)]
pub enum OutboundRingEvent {
    L3Delta(RawL3Delta),
    Fill(RawFillEvent),
    Journal {
        symbol: InstrumentSymbol,
        entry: CompactJournalEntry,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct SubmitOrderCmd {
    pub client_order_id: ClientOrderId,
    pub account_id: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub side: Side,
    pub kind: OrderKind,
    pub stp_mode: StpPolicy,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub max_slippage_bps: u32,
    pub ts_ns: u64,
    pub idempotency_hash: Option<u128>,
}

#[derive(Debug, Clone, Copy)]
pub struct CancelOrderCmd {
    pub order_id: u64,
    pub client_order_id: ClientOrderId,
    pub account_id: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub ts_ns: u64,
    pub idempotency_hash: Option<u128>,
}

#[derive(Debug, Clone, Copy)]
pub struct ModifyOrderCmd {
    pub order_id: u64,
    pub client_order_id: ClientOrderId,
    pub account_id: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub new_price_fp: u64,
    pub new_qty_fp: u64,
    pub ts_ns: u64,
    pub idempotency_hash: Option<u128>,
}

#[derive(Debug, Clone, Copy)]
pub struct FastOrderAck {
    pub engine_seq: u64,
    pub order_id: u64,
    pub client_order_id: ClientOrderId,
    pub account_id: u64,
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub status: AckStatus,
    pub reject_reason: RejectCode,
    pub reject_detail: &'static str,
    pub filled_qty_fp: u64,
    pub remaining_qty_fp: u64,
    pub avg_fill_price_fp: u64,
    pub engine_ts_ns: u64,
    pub wal_blake3_hash: [u8; 32],
    pub state_root_blake3: [u8; 32],
    pub fills_count: usize,
}

#[derive(Clone)]
pub struct InstrumentRuntimeConfig {
    pub regime: MarketRegimeState,
    pub maker_fee_ppm: i64,
    pub taker_fee_ppm: i64,
    pub max_order_qty_fp: u64,
    pub volatility_guard_spread_bps: u32,
}

impl Default for InstrumentRuntimeConfig {
    fn default() -> Self {
        Self {
            regime: MarketRegimeState::Normal,
            maker_fee_ppm: DEFAULT_MAKER_FEE_PPM,
            taker_fee_ppm: DEFAULT_TAKER_FEE_PPM,
            max_order_qty_fp: 0, // 0 = unlimited unless policy sets cap
            volatility_guard_spread_bps: 500, // 5% max spread guard
        }
    }
}

/// Zero-allocation fixed-size circular idempotency key deduplicator (`O(1)` lookup, `0` heap allocs).
#[derive(Clone)]
struct IdempotencyRing {
    table: Box<[(u128, bool)]>,
    ring_order: Box<[usize]>,
    mask: usize,
    cursor: usize,
}

impl IdempotencyRing {
    fn with_capacity(pow2: usize) -> Self {
        let cap = pow2.max(64).next_power_of_two();
        Self {
            table: vec![(0u128, false); cap * 2].into_boxed_slice(),
            ring_order: vec![usize::MAX; cap].into_boxed_slice(),
            mask: (cap * 2) - 1,
            cursor: 0,
        }
    }

    fn clear(&mut self) {
        self.table.fill((0u128, false));
        self.ring_order.fill(usize::MAX);
        self.cursor = 0;
    }

    #[inline(always)]
    fn hash_slot(&self, key: u128) -> usize {
        let lo = key as u64;
        let hi = (key >> 64) as u64;
        ((lo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ hi) as usize) & self.mask
    }

    #[inline]
    fn check_and_record(&mut self, key: u128) -> bool {
        let mut idx = self.hash_slot(key);
        loop {
            let (stored, occ) = self.table[idx];
            if !occ {
                break;
            }
            if stored == key {
                return false; // Duplicate!
            }
            idx = (idx + 1) & self.mask;
        }

        // Evict oldest ring slot if wrapping
        let old_slot = self.ring_order[self.cursor];
        if old_slot != usize::MAX {
            self.evict_at(old_slot);
            // Re-probe insertion index after backward shift
            idx = self.hash_slot(key);
            while self.table[idx].1 {
                idx = (idx + 1) & self.mask;
            }
        }

        self.table[idx] = (key, true);
        self.ring_order[self.cursor] = idx;
        self.cursor = (self.cursor + 1) & (self.ring_order.len() - 1);
        true
    }

    #[inline]
    fn evict_at(&mut self, idx: usize) {
        if !self.table[idx].1 {
            return;
        }
        let mut empty = idx;
        let mut scan = (idx + 1) & self.mask;
        loop {
            if !self.table[scan].1 {
                self.table[empty] = (0, false);
                return;
            }
            let natural = self.hash_slot(self.table[scan].0);
            let dist_scan = scan.wrapping_sub(natural) & self.mask;
            let dist_empty = scan.wrapping_sub(empty) & self.mask;
            if dist_scan >= dist_empty {
                self.table[empty] = self.table[scan];
                // Update ring_order reference if present
                for r in self.ring_order.iter_mut() {
                    if *r == scan {
                        *r = empty;
                        break;
                    }
                }
                empty = scan;
            }
            scan = (scan + 1) & self.mask;
        }
    }
}

#[inline]
pub fn hash_idempotency_key(account_id: u64, key: &str) -> Option<u128> {
    if key.is_empty() {
        return None;
    }
    let mut h = blake3::Hasher::new();
    h.update(b"HYPERION_IDEMPOTENCY_V1");
    h.update(&account_id.to_le_bytes());
    h.update(key.as_bytes());
    let b = h.finalize();
    let bytes = b.as_bytes();
    let mut lo = [0u8; 16];
    lo.copy_from_slice(&bytes[0..16]);
    Some(u128::from_le_bytes(lo))
}

#[derive(Debug, Clone)]
pub struct EngineSizing {
    pub max_accounts: usize,
    pub max_orders_per_book: usize,
    pub max_levels_per_side: usize,
    pub snapshot_every_n_events: u64,
}

impl Default for EngineSizing {
    fn default() -> Self {
        Self {
            max_accounts: 16_384,
            max_orders_per_book: 131_072,
            max_levels_per_side: 16_384,
            snapshot_every_n_events: 0, // 0 = manual / explicit periodic trigger
        }
    }
}

/// LMAX Disruptor Single-Writer Deterministic Matching & Double-Entry Cryptographic Ledger Engine.
pub struct DeterministicExchangeEngine {
    books: Vec<InstrumentOrderBook>,
    configs: Vec<InstrumentRuntimeConfig>,
    ledger: DoubleEntryLedger,
    merkle: IncrementalMerkleState,
    lob_acc: LobStateAccumulator,
    idempotency: IdempotencyRing,
    wal: WalWriter,
    snapshot_dir: Option<PathBuf>,
    snapshot_every_n_events: u64,
    last_snapshot_seq: u64,
    outbound_ring: Option<SpscProducer<OutboundRingEvent>>,

    engine_seq: u64,
    last_ts_ns: u64,
    next_order_id: u64,
    next_fill_id: u64,
    sizing: EngineSizing,

    // Pre-allocated zero-allocation hot-path scratch buffers
    scratch_fills: Vec<RawFillEvent>,
    scratch_deltas: Vec<RawL3Delta>,
    scratch_journals: Vec<CompactJournalEntry>,
    scratch_timeline: Vec<OutboundRingEvent>,
    scratch_prost: Vec<u8>,
}

impl DeterministicExchangeEngine {
    pub fn new_in_memory(sizing: EngineSizing) -> Self {
        Self::with_wal_and_ring(sizing, WalWriter::in_memory(), None, None)
    }

    pub fn with_wal_and_ring(
        sizing: EngineSizing,
        wal: WalWriter,
        snapshot_dir: Option<PathBuf>,
        outbound_ring: Option<SpscProducer<OutboundRingEvent>>,
    ) -> Self {
        let mut engine = Self {
            books: Vec::with_capacity(MAX_INSTRUMENTS),
            configs: Vec::with_capacity(MAX_INSTRUMENTS),
            ledger: DoubleEntryLedger::with_capacity(sizing.max_accounts),
            merkle: IncrementalMerkleState::new(),
            lob_acc: LobStateAccumulator::new(),
            idempotency: IdempotencyRing::with_capacity(4096),
            wal,
            snapshot_dir,
            snapshot_every_n_events: sizing.snapshot_every_n_events,
            last_snapshot_seq: 0,
            outbound_ring,
            engine_seq: 0,
            last_ts_ns: 1_700_000_000_000_000_000,
            next_order_id: 1,
            next_fill_id: 1,
            sizing: sizing.clone(),
            scratch_fills: Vec::with_capacity(1024),
            scratch_deltas: Vec::with_capacity(2048),
            scratch_journals: Vec::with_capacity(2048),
            scratch_timeline: Vec::with_capacity(4096),
            scratch_prost: Vec::with_capacity(4096),
        };
        // Register the 4 canonical Hyperion instruments (00-hyperion-contract.md Section 4.3)
        engine.register_instrument(1, "BTC-USDT", "BTC", "USDT");
        engine.register_instrument(2, "ETH-USDT", "ETH", "USDT");
        engine.register_instrument(3, "SOL-USDT", "SOL", "USDT");
        engine.register_instrument(4, "HYPE-USDT", "HYPE", "USDT");
        engine.merkle.commit_state_root(&engine.lob_acc);
        engine
    }

    pub fn register_instrument(
        &mut self,
        instrument_id: u32,
        symbol: &str,
        base_asset: &str,
        quote_asset: &str,
    ) -> usize {
        let sym = InstrumentSymbol::from_str_trunc(symbol);
        for (i, b) in self.books.iter().enumerate() {
            if (instrument_id != 0 && b.instrument_id == instrument_id) || (!sym.is_empty() && b.symbol == sym) {
                return i;
            }
        }
        let assigned_id = if instrument_id != 0 {
            instrument_id
        } else {
            (self.books.len() as u32) + 1
        };
        let idx = self.books.len();
        self.books.push(InstrumentOrderBook::new(
            assigned_id,
            symbol,
            base_asset,
            quote_asset,
            self.sizing.max_orders_per_book,
            self.sizing.max_levels_per_side,
        ));
        self.configs.push(InstrumentRuntimeConfig::default());
        idx
    }

    #[inline]
    pub fn resolve_book_idx(&mut self, instrument_id: u32, symbol: InstrumentSymbol) -> usize {
        if instrument_id >= 1 && (instrument_id as usize) <= self.books.len() {
            let direct = (instrument_id - 1) as usize;
            if self.books[direct].instrument_id == instrument_id {
                return direct;
            }
        }
        for (i, b) in self.books.iter().enumerate() {
            if (instrument_id != 0 && b.instrument_id == instrument_id)
                || (!symbol.is_empty() && b.symbol == symbol)
            {
                return i;
            }
        }
        // Dynamically register valid "BASE-QUOTE" pair if room remains in MAX_INSTRUMENTS
        if self.books.len() < MAX_INSTRUMENTS && !symbol.is_empty() {
            let s = symbol.as_str();
            let (base, quote) = s.split_once('-').unwrap_or((s, "USDT"));
            let id = if instrument_id != 0 {
                instrument_id
            } else {
                (self.books.len() as u32) + 1
            };
            return self.register_instrument(id, s, base, quote);
        }
        0
    }

    #[inline]
    pub fn find_book_idx_readonly(&self, instrument_id: u32, symbol: &str) -> Option<usize> {
        let sym = InstrumentSymbol::from_str_trunc(symbol);
        for (i, b) in self.books.iter().enumerate() {
            if (instrument_id != 0 && b.instrument_id == instrument_id)
                || (!sym.is_empty() && b.symbol == sym)
            {
                return Some(i);
            }
        }
        None
    }

    pub fn set_instrument_fees(
        &mut self,
        instrument_id: u32,
        symbol: &str,
        maker_fee_ppm: i64,
        taker_fee_ppm: i64,
    ) {
        let idx = self.resolve_book_idx(instrument_id, InstrumentSymbol::from_str_trunc(symbol));
        self.configs[idx].maker_fee_ppm = maker_fee_ppm;
        self.configs[idx].taker_fee_ppm = taker_fee_ppm;
    }

    #[inline(always)]
    pub const fn engine_seq(&self) -> u64 {
        self.engine_seq
    }

    #[inline(always)]
    pub const fn last_ts_ns_val(&self) -> u64 {
        self.last_ts_ns
    }

    pub fn set_replayed_progress(&mut self, seq: u64, ts_ns: u64, wal_hash: [u8; 32]) {
        self.engine_seq = seq;
        self.last_ts_ns = self.last_ts_ns.max(ts_ns);
        self.wal.set_chain_anchor(seq, wal_hash);
    }

    pub fn replay_apply_journal(&mut self, entry: &CompactJournalEntry) -> Result<(), LedgerError> {
        self.ledger.apply_replayed_journal(&mut self.merkle, entry)
    }

    pub fn replay_append_fill(
        &mut self,
        fill_id: u64,
        maker_order_id: u64,
        taker_order_id: u64,
        fill_leaf: [u8; 32],
    ) {
        self.next_fill_id = self.next_fill_id.max(fill_id + 1);
        self.next_order_id = self
            .next_order_id
            .max(maker_order_id + 1)
            .max(taker_order_id + 1);
        self.merkle.append_execution_leaf(fill_leaf);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replay_apply_l3_delta(
        &mut self,
        instrument_id: u32,
        symbol: &str,
        engine_seq: u64,
        op: L3OpCode,
        side: Side,
        order_id: u64,
        account_id: u64,
        price_fp: u64,
        remaining_qty_fp: u64,
        locked_quote_fp: i128,
        ts_ns: u64,
    ) -> Result<(), WalError> {
        self.next_order_id = self.next_order_id.max(order_id + 1);
        let b_idx = self.resolve_book_idx(instrument_id, InstrumentSymbol::from_str_trunc(symbol));
        self.books[b_idx].prev_l3_seq = engine_seq;
        let inst_id = self.books[b_idx].instrument_id;

        match op {
            L3OpCode::Add => {
                let rec = OrderRecord {
                    order_id,
                    account_id,
                    instrument_id: inst_id,
                    side,
                    kind: OrderKind::Limit,
                    stp_mode: StpPolicy::None,
                    price_fp,
                    initial_qty_fp: remaining_qty_fp,
                    remaining_qty_fp,
                    locked_quote_fp,
                    created_ts_ns: ts_ns,
                    priority_seq: engine_seq,
                    client_order_id: ClientOrderId::empty(),
                };
                self.books[b_idx]
                    .insert_resting(rec, &mut self.lob_acc)
                    .map_err(|e| WalError::SnapshotCodec(e.to_string()))?;
            }
            L3OpCode::Modify | L3OpCode::Fill => {
                if let Some(slot_idx) = self.books[b_idx].order_index.get(order_id) {
                    if remaining_qty_fp == 0 {
                        self.books[b_idx].remove_resting_by_slot(slot_idx, &mut self.lob_acc);
                    } else {
                        self.books[b_idx].reduce_resting_qty_in_place(
                            slot_idx,
                            remaining_qty_fp,
                            locked_quote_fp,
                            &mut self.lob_acc,
                        );
                    }
                }
            }
            L3OpCode::Cancel => {
                if let Some(slot_idx) = self.books[b_idx].order_index.get(order_id) {
                    self.books[b_idx].remove_resting_by_slot(slot_idx, &mut self.lob_acc);
                }
            }
            L3OpCode::Clear => {
                self.books[b_idx].clear();
            }
        }
        Ok(())
    }

    pub fn replay_apply_order_ack(
        &mut self,
        ack: &hyperion_proto::OrderAck,
    ) -> Result<(), WalError> {
        if ack.order_id > 0 {
            self.next_order_id = self.next_order_id.max(ack.order_id + 1);
            if let Some(b_idx) = self.find_book_idx_readonly(ack.instrument_id, &ack.symbol) {
                if let Some(slot_idx) = self.books[b_idx].order_index.get(ack.order_id) {
                    self.books[b_idx].orders.cold_mut(slot_idx).client_order_id =
                        ClientOrderId::from_str_trunc(&ack.client_order_id);
                    let init_qty = ack.filled_qty_fp.saturating_add(ack.remaining_qty_fp);
                    if init_qty > 0 {
                        self.books[b_idx].orders.cold_mut(slot_idx).initial_qty_fp = init_qty;
                    }
                }
            }
        } else if let Some(rest) = ack.client_order_id.strip_prefix("RISK_ACTION:") {
            let parts: Vec<&str> = rest.split(':').collect();
            if parts.len() == 3 {
                let acc_id: u64 = parts[0].parse().unwrap_or(0);
                let st_u8: u8 = parts[1].parse().unwrap_or(0);
                let reg_u8: u8 = parts[2].parse().unwrap_or(0);
                if acc_id != 0 && st_u8 != 0 {
                    let st = match st_u8 {
                        2 => AccountRiskStatus::Throttled,
                        3 => AccountRiskStatus::Frozen,
                        4 => AccountRiskStatus::LiquidationOnly,
                        _ => AccountRiskStatus::Active,
                    };
                    let _ = self.ledger.set_account_status(acc_id, st);
                }
                if reg_u8 != 0 {
                    let reg = MarketRegimeState::from_u8(reg_u8);
                    if ack.symbol.is_empty() {
                        for c in &mut self.configs {
                            c.regime = reg;
                        }
                    } else {
                        let idx = self
                            .resolve_book_idx(0, InstrumentSymbol::from_str_trunc(&ack.symbol));
                        self.configs[idx].regime = reg;
                    }
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    pub fn state_root(&mut self) -> [u8; 32] {
        self.ledger.flush_merkle_batch(&mut self.merkle);
        self.merkle.commit_state_root(&self.lob_acc)
    }

    #[inline(always)]
    pub const fn cached_state_root(&self) -> [u8; 32] {
        self.merkle.state_root()
    }

    #[inline(always)]
    pub const fn ledger(&self) -> &DoubleEntryLedger {
        &self.ledger
    }

    #[inline(always)]
    pub fn books(&self) -> &[InstrumentOrderBook] {
        &self.books
    }

    #[inline(always)]
    pub fn last_fills(&self) -> &[RawFillEvent] {
        &self.scratch_fills
    }

    #[inline(always)]
    pub fn last_deltas(&self) -> &[RawL3Delta] {
        &self.scratch_deltas
    }

    #[inline(always)]
    pub fn last_journals(&self) -> &[CompactJournalEntry] {
        &self.scratch_journals
    }

    #[inline(always)]
    pub fn last_timeline(&self) -> &[OutboundRingEvent] {
        &self.scratch_timeline
    }

    #[inline(always)]
    pub fn scratch_prost_mut(&mut self) -> &mut Vec<u8> {
        &mut self.scratch_prost
    }

    #[inline(always)]
    pub fn wal_mut(&mut self) -> &mut WalWriter {
        &mut self.wal
    }

    #[inline(always)]
    fn clear_scratch(&mut self) {
        self.scratch_fills.clear();
        self.scratch_deltas.clear();
        self.scratch_journals.clear();
        self.scratch_timeline.clear();
    }

    #[inline(always)]
    fn next_monotonic_ts(&mut self, candidate_ts_ns: u64) -> u64 {
        if candidate_ts_ns > self.last_ts_ns {
            self.last_ts_ns = candidate_ts_ns;
        } else {
            self.last_ts_ns = self.last_ts_ns.saturating_add(1);
        }
        self.last_ts_ns
    }

    #[inline(always)]
    fn alloc_seq(&mut self) -> u64 {
        self.engine_seq += 1;
        self.engine_seq
    }

    #[inline(always)]
    fn push_ring_event(&mut self, ev: OutboundRingEvent) {
        self.scratch_timeline.push(ev);
    }

    /// Coalesces all touched ledger account leaves, commits the unified Blake3 Merkle state root
    /// ONCE per atomic engine command, stamps it on emitted L3 deltas, and publishes to the SPSC ring.
    #[inline]
    fn commit_and_publish_command_root(&mut self) -> [u8; 32] {
        self.ledger.flush_merkle_batch(&mut self.merkle);
        let root = self.merkle.commit_state_root(&self.lob_acc);
        for d in &mut self.scratch_deltas {
            d.state_root_blake3 = root;
        }
        if let Some(ring) = &mut self.outbound_ring {
            for ev in &mut self.scratch_timeline {
                if let OutboundRingEvent::L3Delta(d) = ev {
                    d.state_root_blake3 = root;
                }
                let _ = ring.try_push(*ev);
            }
        } else {
            for ev in &mut self.scratch_timeline {
                if let OutboundRingEvent::L3Delta(d) = ev {
                    d.state_root_blake3 = root;
                }
            }
        }
        root
    }

    /// Credits an external asset deposit on the fast path (`0` heap allocs once account/asset slot exists).
    pub fn credit_deposit_fast(
        &mut self,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: u128,
        ts_ns: u64,
    ) -> Result<(u64, CompactJournalEntry, AssetBalanceLeaf, [u8; 32]), LedgerError> {
        self.clear_scratch();

        let ts = self.next_monotonic_ts(ts_ns);
        let seq = self.alloc_seq();
        let (leaf, journal) = self.ledger.credit_deposit(
            &mut self.merkle,
            seq,
            seq,
            account_id,
            asset,
            amount_fp,
            ts,
        )?;
        let sym = InstrumentSymbol::from_str_trunc(asset.as_str());
        self.scratch_journals.push(journal);
        self.push_ring_event(OutboundRingEvent::Journal {
            symbol: sym,
            entry: journal,
        });
        let root = self.commit_and_publish_command_root();
        Ok((seq, journal, leaf, root))
    }

    /// Pre-flight zero-mutation crossing liquidity & buyer quote requirement calculator.
    /// Used by `FOK` (to verify 100% fillability under STP before mutating state) and `MARKET` Buy
    /// (to lock exact sweep collateral without over-locking).
    #[allow(clippy::too_many_arguments)]
    fn preflight_crossing_scan(
        &self,
        book_idx: usize,
        taker_account_id: u64,
        side: Side,
        target_qty_fp: u64,
        limit_price_fp: Option<u64>,
        taker_stp: StpPolicy,
        taker_fee_ppm: i64,
    ) -> Result<(u64, i128), RejectCode> {
        let book = &self.books[book_idx];
        let opp_book = book.side_book(side.opposite());
        let mut lvl_idx = opp_book.best_level_idx();
        let mut fillable_qty: u64 = 0;
        let mut buyer_quote_needed: i128 = 0;

        while lvl_idx != NULL_IDX && fillable_qty < target_qty_fp {
            let lvl = opp_book.get(lvl_idx);
            if let Some(lim) = limit_price_fp {
                let crosses = match side {
                    Side::Buy => lvl.price_fp <= lim,
                    Side::Sell => lvl.price_fp >= lim,
                };
                if !crosses {
                    break;
                }
            }

            let mut ord_idx = lvl.head_order;
            while ord_idx != NULL_IDX && fillable_qty < target_qty_fp {
                let maker = book.orders.hot(ord_idx);
                if maker.account_id == taker_account_id {
                    let eff_stp = if taker_stp != StpPolicy::None {
                        taker_stp
                    } else {
                        maker.stp_mode
                    };
                    match eff_stp {
                        StpPolicy::None => {}
                        StpPolicy::CancelResting => {
                            ord_idx = maker.next_in_level;
                            continue;
                        }
                        StpPolicy::CancelAggressing | StpPolicy::CancelBoth => {
                            return Ok((fillable_qty, buyer_quote_needed));
                        }
                    }
                }

                let rem_needed = target_qty_fp - fillable_qty;
                let step_qty = rem_needed.min(maker.remaining_qty_fp);
                fillable_qty += step_qty;

                if side == Side::Buy {
                    let notional = checked_notional_fp(maker.price_fp, step_qty)
                        .map_err(|_| RejectCode::InvalidPriceOrQty)?;
                    let fee = checked_fee_fp(notional, taker_fee_ppm)
                        .map_err(|_| RejectCode::InvalidPriceOrQty)?;
                    let leg_cost = (notional as i128)
                        .checked_add(fee.max(0) as i128)
                        .ok_or(RejectCode::InvalidPriceOrQty)?;
                    buyer_quote_needed = buyer_quote_needed
                        .checked_add(leg_cost)
                        .ok_or(RejectCode::InvalidPriceOrQty)?;
                }

                ord_idx = maker.next_in_level;
            }

            lvl_idx = lvl.worse_level;
        }

        Ok((fillable_qty, buyer_quote_needed))
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn reject_ack(
        &mut self,
        order_id: u64,
        client_order_id: ClientOrderId,
        account_id: u64,
        instrument_id: u32,
        symbol: InstrumentSymbol,
        reason: RejectCode,
        detail: &'static str,
        ts_ns: u64,
    ) -> FastOrderAck {
        let ts = self.next_monotonic_ts(ts_ns);
        let seq = self.alloc_seq();
        let root = self.commit_and_publish_command_root();
        FastOrderAck {
            engine_seq: seq,
            order_id,
            client_order_id,
            account_id,
            instrument_id,
            symbol,
            status: AckStatus::Rejected,
            reject_reason: reason,
            reject_detail: detail,
            filled_qty_fp: 0,
            remaining_qty_fp: 0,
            avg_fill_price_fp: 0,
            engine_ts_ns: ts,
            wal_blake3_hash: self.wal.last_blake3(),
            state_root_blake3: root,
            fills_count: 0,
        }
    }

    /// Ultra-low-latency zero-allocation hot-path order execution (`< 2 µs` p50).
    pub fn submit_order_fast(&mut self, cmd: SubmitOrderCmd) -> FastOrderAck {
        self.clear_scratch();

        let book_idx = self.resolve_book_idx(cmd.instrument_id, cmd.symbol);
        let inst_id = self.books[book_idx].instrument_id;
        let sym = self.books[book_idx].symbol;

        if let Some(idem) = cmd.idempotency_hash {
            if !self.idempotency.check_and_record(idem) {
                return self.reject_ack(
                    0,
                    cmd.client_order_id,
                    cmd.account_id,
                    inst_id,
                    sym,
                    RejectCode::DuplicateIdempotencyKey,
                    "duplicate idempotency_key",
                    cmd.ts_ns,
                );
            }
        }

        let order_id = self.next_order_id;
        self.next_order_id += 1;

        self.execute_order_internal(
            book_idx,
            order_id,
            cmd.client_order_id,
            cmd.account_id,
            cmd.side,
            cmd.kind,
            cmd.stp_mode,
            cmd.price_fp,
            cmd.qty_fp,
            cmd.qty_fp,
            cmd.max_slippage_bps,
            cmd.ts_ns,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_order_internal(
        &mut self,
        book_idx: usize,
        order_id: u64,
        client_order_id: ClientOrderId,
        account_id: u64,
        side: Side,
        kind: OrderKind,
        stp_mode: StpPolicy,
        price_fp: u64,
        initial_qty_fp: u64,
        qty_fp: u64,
        max_slippage_bps: u32,
        raw_ts_ns: u64,
        is_modify_reinsert: bool,
    ) -> FastOrderAck {
        let inst_id = self.books[book_idx].instrument_id;
        let sym = self.books[book_idx].symbol;
        let base_asset = self.books[book_idx].base_asset;
        let quote_asset = self.books[book_idx].quote_asset;
        let cfg = self.configs[book_idx].clone();

        // 1. Account & Market Regime risk checks
        if account_id == TREASURY_ACCOUNT_ID {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::AccountFrozen,
                "treasury account cannot submit orders",
                raw_ts_ns,
            );
        }
        match self.ledger.account_status(account_id) {
            AccountRiskStatus::Frozen | AccountRiskStatus::LiquidationOnly => {
                return self.reject_ack(
                    order_id,
                    client_order_id,
                    account_id,
                    inst_id,
                    sym,
                    RejectCode::AccountFrozen,
                    "account frozen or restricted by risk engine",
                    raw_ts_ns,
                );
            }
            AccountRiskStatus::Active | AccountRiskStatus::Throttled => {}
        }
        if cfg.regime == MarketRegimeState::Halted {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::MarketHalted,
                "instrument market regime is HALTED",
                raw_ts_ns,
            );
        }

        // 2. Price & Quantity bounds validation
        if qty_fp == 0 || (kind != OrderKind::Market && price_fp == 0) {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::InvalidPriceOrQty,
                "zero quantity or zero non-market price",
                raw_ts_ns,
            );
        }
        if cfg.max_order_qty_fp > 0 && qty_fp > cfg.max_order_qty_fp {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::MaxOrderSizeExceeded,
                "order quantity exceeds instrument max_order_qty_fp",
                raw_ts_ns,
            );
        }
        if cfg.regime == MarketRegimeState::VolatilityGuard && kind == OrderKind::Market {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::MarketHalted,
                "pure market orders blocked under VOLATILITY_GUARD",
                raw_ts_ns,
            );
        }

        // 3. Post-Only & Post-Only-Auction crossing check
        let effective_post_only =
            kind == OrderKind::PostOnly || cfg.regime == MarketRegimeState::PostOnlyAuction;
        if effective_post_only {
            let would_cross = match side {
                Side::Buy => self.books[book_idx]
                    .best_ask_fp()
                    .map(|ask| price_fp >= ask)
                    .unwrap_or(false),
                Side::Sell => self.books[book_idx]
                    .best_bid_fp()
                    .map(|bid| price_fp <= bid)
                    .unwrap_or(false),
            };
            if would_cross {
                return self.reject_ack(
                    order_id,
                    client_order_id,
                    account_id,
                    inst_id,
                    sym,
                    RejectCode::PostOnlyWouldCross,
                    "post-only order would cross resting book",
                    raw_ts_ns,
                );
            }
        }

        // 4. Effective crossing price limit & FOK / Market pre-flight checks
        let opp_best = match side {
            Side::Buy => self.books[book_idx].best_ask_fp(),
            Side::Sell => self.books[book_idx].best_bid_fp(),
        };

        if kind == OrderKind::Market && opp_best.is_none() {
            let ts = self.next_monotonic_ts(raw_ts_ns);
            let seq = self.alloc_seq();
            let root = self.merkle.commit_state_root(&self.lob_acc);
            return FastOrderAck {
                engine_seq: seq,
                order_id,
                client_order_id,
                account_id,
                instrument_id: inst_id,
                symbol: sym,
                status: AckStatus::Expired,
                reject_reason: RejectCode::None,
                reject_detail: "no opposite liquidity for market order",
                filled_qty_fp: 0,
                remaining_qty_fp: 0,
                avg_fill_price_fp: 0,
                engine_ts_ns: ts,
                wal_blake3_hash: self.wal.last_blake3(),
                state_root_blake3: root,
                fills_count: 0,
            };
        }

        let effective_limit_price_opt: Option<u64> = match kind {
            OrderKind::Market => {
                if max_slippage_bps > 0 {
                    let Some(ref_px) = opp_best else {
                        unreachable!()
                    };
                    match checked_slippage_limit_fp(ref_px, max_slippage_bps, side == Side::Buy) {
                        Ok(cap) => Some(cap),
                        Err(_) => {
                            return self.reject_ack(
                                order_id,
                                client_order_id,
                                account_id,
                                inst_id,
                                sym,
                                RejectCode::InvalidPriceOrQty,
                                "slippage limit overflow",
                                raw_ts_ns,
                            );
                        }
                    }
                } else if price_fp > 0 {
                    Some(price_fp)
                } else {
                    None
                }
            }
            _ => {
                if max_slippage_bps > 0 && opp_best.is_some() {
                    let ref_px = opp_best.unwrap_or(price_fp);
                    if let Ok(slip_cap) =
                        checked_slippage_limit_fp(ref_px, max_slippage_bps, side == Side::Buy)
                    {
                        Some(match side {
                            Side::Buy => price_fp.min(slip_cap),
                            Side::Sell => price_fp.max(slip_cap),
                        })
                    } else {
                        Some(price_fp)
                    }
                } else {
                    Some(price_fp)
                }
            }
        };

        // Compute required upfront lock
        let (lock_asset, required_lock_fp): (AssetSymbol, i128) = match (side, kind) {
            (Side::Sell, _) => (base_asset, qty_fp as i128),
            (Side::Buy, OrderKind::Fok) => {
                let (fillable, quote_needed) = match self.preflight_crossing_scan(
                    book_idx,
                    account_id,
                    Side::Buy,
                    qty_fp,
                    effective_limit_price_opt,
                    stp_mode,
                    cfg.taker_fee_ppm,
                ) {
                    Ok(res) => res,
                    Err(code) => {
                        return self.reject_ack(
                            order_id,
                            client_order_id,
                            account_id,
                            inst_id,
                            sym,
                            code,
                            "fok arithmetic overflow",
                            raw_ts_ns,
                        );
                    }
                };
                if fillable < qty_fp {
                    return self.reject_ack(
                        order_id,
                        client_order_id,
                        account_id,
                        inst_id,
                        sym,
                        RejectCode::FokInsufficientLiquidity,
                        "FOK order cannot be completely filled",
                        raw_ts_ns,
                    );
                }
                (quote_asset, quote_needed)
            }
            (Side::Buy, OrderKind::Market) => {
                let (_, quote_needed) = match self.preflight_crossing_scan(
                    book_idx,
                    account_id,
                    Side::Buy,
                    qty_fp,
                    effective_limit_price_opt,
                    stp_mode,
                    cfg.taker_fee_ppm,
                ) {
                    Ok(res) => res,
                    Err(code) => {
                        return self.reject_ack(
                            order_id,
                            client_order_id,
                            account_id,
                            inst_id,
                            sym,
                            code,
                            "market scan overflow",
                            raw_ts_ns,
                        );
                    }
                };
                (quote_asset, quote_needed)
            }
            (Side::Buy, _) => {
                match checked_buyer_reserve_fp(
                    price_fp,
                    qty_fp,
                    cfg.maker_fee_ppm,
                    cfg.taker_fee_ppm,
                ) {
                    Ok(res) => (quote_asset, res),
                    Err(_) => {
                        return self.reject_ack(
                            order_id,
                            client_order_id,
                            account_id,
                            inst_id,
                            sym,
                            RejectCode::InvalidPriceOrQty,
                            "order notional overflow",
                            raw_ts_ns,
                        );
                    }
                }
            }
        };

        if side == Side::Sell && kind == OrderKind::Fok {
            let (fillable, _) = match self.preflight_crossing_scan(
                book_idx,
                account_id,
                Side::Sell,
                qty_fp,
                effective_limit_price_opt,
                stp_mode,
                cfg.taker_fee_ppm,
            ) {
                Ok(res) => res,
                Err(code) => {
                    return self.reject_ack(
                        order_id,
                        client_order_id,
                        account_id,
                        inst_id,
                        sym,
                        code,
                        "fok sell scan overflow",
                        raw_ts_ns,
                    );
                }
            };
            if fillable < qty_fp {
                return self.reject_ack(
                    order_id,
                    client_order_id,
                    account_id,
                    inst_id,
                    sym,
                    RejectCode::FokInsufficientLiquidity,
                    "FOK sell order cannot be completely filled",
                    raw_ts_ns,
                );
            }
        }

        // 5. Upfront Double-Entry Collateral Hold
        let ts = self.next_monotonic_ts(raw_ts_ns);
        if self.ledger.available_balance(account_id, lock_asset) < required_lock_fp {
            return self.reject_ack(
                order_id,
                client_order_id,
                account_id,
                inst_id,
                sym,
                RejectCode::InsufficientBalance,
                "insufficient available balance to cover order reserve",
                ts,
            );
        }
        self.ledger.begin_merkle_batch();
        if required_lock_fp > 0 {
            let hold_seq = self.alloc_seq();
            match self.ledger.lock_funds(
                &mut self.merkle,
                hold_seq,
                order_id,
                account_id,
                lock_asset,
                required_lock_fp,
                ts,
            ) {
                Ok(Some(j)) => {
                    self.scratch_journals.push(j);
                    self.push_ring_event(OutboundRingEvent::Journal {
                        symbol: sym,
                        entry: j,
                    });
                }
                Ok(None) => {}
                Err(_) => {
                    return self.reject_ack(
                        order_id,
                        client_order_id,
                        account_id,
                        inst_id,
                        sym,
                        RejectCode::InsufficientBalance,
                        "failed to lock collateral",
                        ts,
                    );
                }
            }
        }

        let mut taker_rem_qty = qty_fp;
        let mut taker_locked_quote_rem: i128 = if side == Side::Buy { required_lock_fp } else { 0 };
        let mut cumulative_filled_qty: u64 = 0;
        let mut weighted_px_qty_sum: u128 = 0;
        let mut stp_canceled_taker = false;

        // 6. Price-Time Priority Crossing Loop (O(1) per best level, 64B cache-aligned nodes)
        while taker_rem_qty > 0 && !effective_post_only {
            let opp_side = side.opposite();
            let best_lvl_idx = self.books[book_idx].side_book(opp_side).best_level_idx();
            if best_lvl_idx == NULL_IDX {
                break;
            }
            let best_px = self.books[book_idx]
                .side_book(opp_side)
                .get(best_lvl_idx)
                .price_fp;
            if let Some(lim_px) = effective_limit_price_opt {
                let crosses = match side {
                    Side::Buy => best_px <= lim_px,
                    Side::Sell => best_px >= lim_px,
                };
                if !crosses {
                    break;
                }
            }

            let head_slot = self.books[book_idx]
                .side_book(opp_side)
                .get(best_lvl_idx)
                .head_order;
            let maker_hot = *self.books[book_idx].orders.hot(head_slot);

            // Self-Trade Prevention (STP) check
            if maker_hot.account_id == account_id {
                let eff_stp = if stp_mode != StpPolicy::None {
                    stp_mode
                } else {
                    maker_hot.stp_mode
                };
                match eff_stp {
                    StpPolicy::None => {}
                    StpPolicy::CancelResting => {
                        self.cancel_resting_slot_internal(
                            book_idx,
                            head_slot,
                            "STP_CANCEL_RESTING",
                            ts,
                        );
                        continue;
                    }
                    StpPolicy::CancelAggressing => {
                        stp_canceled_taker = true;
                        break;
                    }
                    StpPolicy::CancelBoth => {
                        self.cancel_resting_slot_internal(
                            book_idx,
                            head_slot,
                            "STP_CANCEL_BOTH_RESTING",
                            ts,
                        );
                        stp_canceled_taker = true;
                        break;
                    }
                }
            }

            let fill_qty = taker_rem_qty.min(maker_hot.remaining_qty_fp);
            let exec_price = maker_hot.price_fp;
            let notional_fp = checked_notional_fp(exec_price, fill_qty)
                .expect("validated notional fits in u128");
            let maker_fee_fp = checked_fee_fp(notional_fp, cfg.maker_fee_ppm)
                .expect("validated maker fee fits in i64");
            let taker_fee_fp = checked_fee_fp(notional_fp, cfg.taker_fee_ppm)
                .expect("validated taker fee fits in i64");

            let (buyer_acc, seller_acc, buyer_fee, seller_fee) = match side {
                Side::Buy => (account_id, maker_hot.account_id, taker_fee_fp, maker_fee_fp),
                Side::Sell => (maker_hot.account_id, account_id, maker_fee_fp, taker_fee_fp),
            };

            let fill_id = self.next_fill_id;
            self.next_fill_id += 1;

            let journal_seq_ev = self.alloc_seq();
            let (buyer_quote_spent, trade_journal) = self
                .ledger
                .settle_fill(
                    &mut self.merkle,
                    journal_seq_ev,
                    fill_id,
                    buyer_acc,
                    seller_acc,
                    base_asset,
                    quote_asset,
                    fill_qty,
                    notional_fp,
                    buyer_fee,
                    seller_fee,
                    ts,
                )
                .expect("pre-locked reserves guarantee balanced settlement");
            self.scratch_journals.push(trade_journal);
            self.push_ring_event(OutboundRingEvent::Journal {
                symbol: sym,
                entry: trade_journal,
            });

            if side == Side::Buy {
                taker_locked_quote_rem -= buyer_quote_spent;
                debug_assert!(taker_locked_quote_rem >= 0);
            }

            // Record execution fill in Merkle accumulator & scratch buffer
            let fill_seq = self.alloc_seq();
            let fill_leaf = hash_execution_fill_leaf(&ExecutionFillDigestInput {
                fill_id,
                engine_seq: fill_seq,
                instrument_id: inst_id,
                maker_order_id: maker_hot.order_id,
                taker_order_id: order_id,
                maker_account_id: maker_hot.account_id,
                taker_account_id: account_id,
                aggressor_side: side as u8,
                price_fp: exec_price,
                qty_fp: fill_qty,
                notional_fp,
                maker_fee_fp,
                taker_fee_fp,
                exec_ts_ns: ts,
            });
            self.merkle.append_execution_leaf(fill_leaf);

            let raw_fill = RawFillEvent {
                fill_id,
                engine_seq: fill_seq,
                instrument_id: inst_id,
                symbol: sym,
                maker_order_id: maker_hot.order_id,
                taker_order_id: order_id,
                maker_account_id: maker_hot.account_id,
                taker_account_id: account_id,
                aggressor_side: side,
                price_fp: exec_price,
                qty_fp: fill_qty,
                notional_fp,
                maker_fee_fp,
                taker_fee_fp,
                exec_ts_ns: ts,
                wal_blake3_hash: [0u8; 32],
            };
            self.scratch_fills.push(raw_fill);
            self.push_ring_event(OutboundRingEvent::Fill(raw_fill));

            // Update or remove resting Maker order in LOB
            let maker_old_locked_quote = self.books[book_idx].orders.cold(head_slot).locked_quote_fp;
            let maker_new_locked_quote = if maker_hot.side == Side::Buy {
                let rem = maker_old_locked_quote - buyer_quote_spent;
                debug_assert!(rem >= 0);
                rem
            } else {
                0
            };

            if maker_hot.remaining_qty_fp == fill_qty {
                self.books[book_idx].remove_resting_by_slot(head_slot, &mut self.lob_acc);
                if maker_hot.side == Side::Buy && maker_new_locked_quote > 0 {
                    let rel_seq = self.alloc_seq();
                    if let Some(rj) = self
                        .ledger
                        .unlock_funds(
                            &mut self.merkle,
                            rel_seq,
                            maker_hot.order_id,
                            maker_hot.account_id,
                            quote_asset,
                            maker_new_locked_quote,
                            "MAKER_FILL_RESIDUAL_RELEASE",
                            ts,
                        )
                        .expect("maker residual unlock valid")
                    {
                        self.scratch_journals.push(rj);
                        self.push_ring_event(OutboundRingEvent::Journal {
                            symbol: sym,
                            entry: rj,
                        });
                    }
                }
                self.emit_l3_delta(
                    book_idx,
                    L3OpCode::Fill,
                    maker_hot.side,
                    maker_hot.order_id,
                    maker_hot.account_id,
                    maker_hot.price_fp,
                    0,
                    -(fill_qty as i64),
                    ts,
                );
            } else {
                let new_maker_rem = maker_hot.remaining_qty_fp - fill_qty;
                self.books[book_idx].reduce_resting_qty_in_place(
                    head_slot,
                    new_maker_rem,
                    maker_new_locked_quote,
                    &mut self.lob_acc,
                );
                self.emit_l3_delta(
                    book_idx,
                    L3OpCode::Fill,
                    maker_hot.side,
                    maker_hot.order_id,
                    maker_hot.account_id,
                    maker_hot.price_fp,
                    new_maker_rem,
                    -(fill_qty as i64),
                    ts,
                );
            }

            taker_rem_qty -= fill_qty;
            cumulative_filled_qty += fill_qty;
            weighted_px_qty_sum = weighted_px_qty_sum.saturating_add((exec_price as u128) * (fill_qty as u128));
        }

        // 7. Post-match remainder & unused collateral settlement
        let (final_status, reject_reason, reject_detail, final_rem_on_book) = if taker_rem_qty == 0 {
            if side == Side::Buy && taker_locked_quote_rem > 0 {
                let rel_seq = self.alloc_seq();
                if let Some(rj) = self
                    .ledger
                    .unlock_funds(
                        &mut self.merkle,
                        rel_seq,
                        order_id,
                        account_id,
                        quote_asset,
                        taker_locked_quote_rem,
                        "TAKER_PRICE_IMPROVEMENT_RELEASE",
                        ts,
                    )
                    .expect("taker residual unlock valid")
                {
                    self.scratch_journals.push(rj);
                    self.push_ring_event(OutboundRingEvent::Journal {
                        symbol: sym,
                        entry: rj,
                    });
                }
            }
            (AckStatus::Filled, RejectCode::None, "", 0u64)
        } else if stp_canceled_taker {
            let unlock_amt = if side == Side::Buy {
                taker_locked_quote_rem
            } else {
                taker_rem_qty as i128
            };
            if unlock_amt > 0 {
                let rel_seq = self.alloc_seq();
                if let Some(rj) = self
                    .ledger
                    .unlock_funds(
                        &mut self.merkle,
                        rel_seq,
                        order_id,
                        account_id,
                        lock_asset,
                        unlock_amt,
                        "STP_TAKER_RELEASE",
                        ts,
                    )
                    .expect("stp taker unlock valid")
                {
                    self.scratch_journals.push(rj);
                    self.push_ring_event(OutboundRingEvent::Journal {
                        symbol: sym,
                        entry: rj,
                    });
                }
            }
            let st = if cumulative_filled_qty == 0 {
                AckStatus::Rejected
            } else {
                AckStatus::Canceled
            };
            (
                st,
                RejectCode::SelfTradePrevented,
                "self-trade prevented",
                0u64,
            )
        } else if kind == OrderKind::Ioc || kind == OrderKind::Market {
            let unlock_amt = if side == Side::Buy {
                taker_locked_quote_rem
            } else {
                taker_rem_qty as i128
            };
            if unlock_amt > 0 {
                let rel_seq = self.alloc_seq();
                if let Some(rj) = self
                    .ledger
                    .unlock_funds(
                        &mut self.merkle,
                        rel_seq,
                        order_id,
                        account_id,
                        lock_asset,
                        unlock_amt,
                        "IOC_MARKET_REMAINDER_RELEASE",
                        ts,
                    )
                    .expect("ioc/market unlock valid")
                {
                    self.scratch_journals.push(rj);
                    self.push_ring_event(OutboundRingEvent::Journal {
                        symbol: sym,
                        entry: rj,
                    });
                }
            }
            let st = if cumulative_filled_qty > 0 {
                AckStatus::Canceled
            } else {
                AckStatus::Expired
            };
            (st, RejectCode::None, "", 0u64)
        } else {
            // Rest remaining quantity on the book!
            // Set `priority_seq` equal to the exact `engine_seq` of the `L3_DELTA_OP_ADD` event so
            // WAL replay reconstructs 100% bit-identical `LobStateAccumulator` without hidden counters!
            let add_seq = self.alloc_seq();
            let rec = OrderRecord {
                order_id,
                account_id,
                instrument_id: inst_id,
                side,
                kind,
                stp_mode,
                price_fp,
                initial_qty_fp,
                remaining_qty_fp: taker_rem_qty,
                locked_quote_fp: taker_locked_quote_rem,
                created_ts_ns: ts,
                priority_seq: add_seq,
                client_order_id,
            };
            self.books[book_idx]
                .insert_resting(rec, &mut self.lob_acc)
                .expect("OrderArena pre-allocated capacity sufficient");

            let prev_l3 = self.books[book_idx].prev_l3_seq;
            self.books[book_idx].prev_l3_seq = add_seq;
            let add_delta = RawL3Delta {
                engine_seq: add_seq,
                prev_engine_seq: prev_l3,
                instrument_id: inst_id,
                symbol: sym,
                op: L3OpCode::Add,
                side,
                order_id,
                account_id,
                price_fp,
                remaining_qty_fp: taker_rem_qty,
                delta_qty_fp: taker_rem_qty as i64,
                event_ts_ns: ts,
                state_root_blake3: [0u8; 32],
            };
            self.scratch_deltas.push(add_delta);
            self.push_ring_event(OutboundRingEvent::L3Delta(add_delta));

            let st = if cumulative_filled_qty > 0 {
                AckStatus::PartiallyFilled
            } else if is_modify_reinsert {
                AckStatus::Modified
            } else {
                AckStatus::Accepted
            };
            (st, RejectCode::None, "", taker_rem_qty)
        };

        let avg_fill_price_fp = if cumulative_filled_qty > 0 {
            (weighted_px_qty_sum / (cumulative_filled_qty as u128)) as u64
        } else {
            0
        };

        let ack_seq = self.alloc_seq();
        let final_root = self.commit_and_publish_command_root();
        FastOrderAck {
            engine_seq: ack_seq,
            order_id,
            client_order_id,
            account_id,
            instrument_id: inst_id,
            symbol: sym,
            status: final_status,
            reject_reason,
            reject_detail,
            filled_qty_fp: cumulative_filled_qty,
            remaining_qty_fp: final_rem_on_book,
            avg_fill_price_fp,
            engine_ts_ns: ts,
            wal_blake3_hash: self.wal.last_blake3(),
            state_root_blake3: final_root,
            fills_count: self.scratch_fills.len(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn emit_l3_delta(
        &mut self,
        book_idx: usize,
        op: L3OpCode,
        side: Side,
        order_id: u64,
        account_id: u64,
        price_fp: u64,
        remaining_qty_fp: u64,
        delta_qty_fp: i64,
        ts_ns: u64,
    ) {
        let seq = self.alloc_seq();
        let prev = self.books[book_idx].prev_l3_seq;
        self.books[book_idx].prev_l3_seq = seq;
        let inst_id = self.books[book_idx].instrument_id;
        let sym = self.books[book_idx].symbol;
        let d = RawL3Delta {
            engine_seq: seq,
            prev_engine_seq: prev,
            instrument_id: inst_id,
            symbol: sym,
            op,
            side,
            order_id,
            account_id,
            price_fp,
            remaining_qty_fp,
            delta_qty_fp,
            event_ts_ns: ts_ns,
            state_root_blake3: [0u8; 32],
        };
        self.scratch_deltas.push(d);
        self.push_ring_event(OutboundRingEvent::L3Delta(d));
    }

    fn cancel_resting_slot_internal(
        &mut self,
        book_idx: usize,
        slot_idx: u32,
        release_reason: &'static str,
        ts_ns: u64,
    ) -> OrderRecord {
        let base_asset = self.books[book_idx].base_asset;
        let quote_asset = self.books[book_idx].quote_asset;
        let sym = self.books[book_idx].symbol;
        let removed = self.books[book_idx].remove_resting_by_slot(slot_idx, &mut self.lob_acc);

        let (asset, unlock_amt) = match removed.side {
            Side::Buy => (quote_asset, removed.locked_quote_fp),
            Side::Sell => (base_asset, removed.remaining_qty_fp as i128),
        };
        if unlock_amt > 0 {
            let rel_seq = self.alloc_seq();
            if let Some(rj) = self
                .ledger
                .unlock_funds(
                    &mut self.merkle,
                    rel_seq,
                    removed.order_id,
                    removed.account_id,
                    asset,
                    unlock_amt,
                    release_reason,
                    ts_ns,
                )
                .expect("resting order unlock valid")
            {
                self.scratch_journals.push(rj);
                self.push_ring_event(OutboundRingEvent::Journal {
                    symbol: sym,
                    entry: rj,
                });
            }
        }

        self.emit_l3_delta(
            book_idx,
            L3OpCode::Cancel,
            removed.side,
            removed.order_id,
            removed.account_id,
            removed.price_fp,
            0,
            -(removed.remaining_qty_fp as i64),
            ts_ns,
        );
        removed
    }

    /// O(1) zero-allocation hot-path cancellation (`< 1.5 µs`).
    pub fn cancel_order_fast(&mut self, cmd: CancelOrderCmd) -> FastOrderAck {
        self.clear_scratch();

        let book_idx = self.resolve_book_idx(cmd.instrument_id, cmd.symbol);
        let inst_id = self.books[book_idx].instrument_id;
        let sym = self.books[book_idx].symbol;

        if let Some(idem) = cmd.idempotency_hash {
            if !self.idempotency.check_and_record(idem) {
                return self.reject_ack(
                    cmd.order_id,
                    cmd.client_order_id,
                    cmd.account_id,
                    inst_id,
                    sym,
                    RejectCode::DuplicateIdempotencyKey,
                    "duplicate cancel idempotency_key",
                    cmd.ts_ns,
                );
            }
        }

        let Some(slot_idx) = self.books[book_idx].order_index.get(cmd.order_id) else {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::OrderNotFound,
                "resting order_id not found",
                cmd.ts_ns,
            );
        };

        let existing = self.books[book_idx].orders.record(slot_idx);
        if cmd.account_id != 0 && existing.account_id != cmd.account_id {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::OrderNotFound,
                "order does not belong to account_id",
                cmd.ts_ns,
            );
        }

        let ts = self.next_monotonic_ts(cmd.ts_ns);
        self.ledger.begin_merkle_batch();
        let removed =
            self.cancel_resting_slot_internal(book_idx, slot_idx, "ORDER_CANCEL_RELEASE", ts);
        let ack_seq = self.alloc_seq();
        let root = self.commit_and_publish_command_root();

        FastOrderAck {
            engine_seq: ack_seq,
            order_id: removed.order_id,
            client_order_id: removed.client_order_id,
            account_id: removed.account_id,
            instrument_id: inst_id,
            symbol: sym,
            status: AckStatus::Canceled,
            reject_reason: RejectCode::None,
            reject_detail: "",
            filled_qty_fp: removed.initial_qty_fp.saturating_sub(removed.remaining_qty_fp),
            remaining_qty_fp: 0,
            avg_fill_price_fp: 0,
            engine_ts_ns: ts,
            wal_blake3_hash: self.wal.last_blake3(),
            state_root_blake3: root,
            fills_count: 0,
        }
    }

    /// Atomic Cancel-Replace (`ModifyOrder`):
    /// - Size reduction at identical price preserves FIFO queue `priority_seq` in O(1)!
    /// - Price change or size increase validates balance upfront (never dropping existing order on failure),
    ///   then cancels old order and re-executes through matching engine with new priority.
    pub fn modify_order_fast(&mut self, cmd: ModifyOrderCmd) -> FastOrderAck {
        self.clear_scratch();

        let book_idx = self.resolve_book_idx(cmd.instrument_id, cmd.symbol);
        let inst_id = self.books[book_idx].instrument_id;
        let sym = self.books[book_idx].symbol;

        if let Some(idem) = cmd.idempotency_hash {
            if !self.idempotency.check_and_record(idem) {
                return self.reject_ack(
                    cmd.order_id,
                    cmd.client_order_id,
                    cmd.account_id,
                    inst_id,
                    sym,
                    RejectCode::DuplicateIdempotencyKey,
                    "duplicate modify idempotency_key",
                    cmd.ts_ns,
                );
            }
        }

        if cmd.new_price_fp == 0 || cmd.new_qty_fp == 0 {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::InvalidPriceOrQty,
                "modify new_price_fp and new_qty_fp must be > 0",
                cmd.ts_ns,
            );
        }

        if self.configs[book_idx].regime == MarketRegimeState::Halted {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::MarketHalted,
                "cannot modify order while market is HALTED",
                cmd.ts_ns,
            );
        }

        let Some(slot_idx) = self.books[book_idx].order_index.get(cmd.order_id) else {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::OrderNotFound,
                "order_id not found for modify",
                cmd.ts_ns,
            );
        };

        let existing = self.books[book_idx].orders.record(slot_idx);
        if cmd.account_id != 0 && existing.account_id != cmd.account_id {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                cmd.account_id,
                inst_id,
                sym,
                RejectCode::OrderNotFound,
                "order ownership mismatch on modify",
                cmd.ts_ns,
            );
        }
        if matches!(
            self.ledger.account_status(existing.account_id),
            AccountRiskStatus::Frozen | AccountRiskStatus::LiquidationOnly
        ) {
            return self.reject_ack(
                cmd.order_id,
                cmd.client_order_id,
                existing.account_id,
                inst_id,
                sym,
                RejectCode::AccountFrozen,
                "account frozen on modify",
                cmd.ts_ns,
            );
        }

        let already_filled = existing
            .initial_qty_fp
            .saturating_sub(existing.remaining_qty_fp);
        if cmd.new_qty_fp <= already_filled {
            // Already filled up to or past requested total quantity -> cancel remaining open leg
            return self.cancel_order_fast(CancelOrderCmd {
                order_id: cmd.order_id,
                client_order_id: existing.client_order_id,
                account_id: existing.account_id,
                instrument_id: inst_id,
                symbol: sym,
                ts_ns: cmd.ts_ns,
                idempotency_hash: None,
            });
        }
        let target_rem_qty = cmd.new_qty_fp - already_filled;
        let cfg = self.configs[book_idx].clone();
        let base_asset = self.books[book_idx].base_asset;
        let quote_asset = self.books[book_idx].quote_asset;

        // Case A: Same price & quantity <= current remaining -> preserve FIFO priority in-place!
        if cmd.new_price_fp == existing.price_fp && target_rem_qty <= existing.remaining_qty_fp {
            let ts = self.next_monotonic_ts(cmd.ts_ns);
            self.ledger.begin_merkle_batch();
            if target_rem_qty < existing.remaining_qty_fp {
                let qty_reduction = existing.remaining_qty_fp - target_rem_qty;
                let new_locked_quote = match existing.side {
                    Side::Sell => {
                        let rel_seq = self.alloc_seq();
                        if let Some(rj) = self
                            .ledger
                            .unlock_funds(
                                &mut self.merkle,
                                rel_seq,
                                existing.order_id,
                                existing.account_id,
                                base_asset,
                                qty_reduction as i128,
                                "ORDER_MODIFY_SIZE_DOWN_RELEASE",
                                ts,
                            )
                            .expect("modify base unlock valid")
                        {
                            self.scratch_journals.push(rj);
                            self.push_ring_event(OutboundRingEvent::Journal {
                                symbol: sym,
                                entry: rj,
                            });
                        }
                        0
                    }
                    Side::Buy => {
                        let target_reserve = checked_buyer_reserve_fp(
                            existing.price_fp,
                            target_rem_qty,
                            cfg.maker_fee_ppm,
                            cfg.taker_fee_ppm,
                        )
                        .unwrap_or(existing.locked_quote_fp);
                        let new_lock = target_reserve.min(existing.locked_quote_fp);
                        let release_quote = existing.locked_quote_fp - new_lock;
                        if release_quote > 0 {
                            let rel_seq = self.alloc_seq();
                            if let Some(rj) = self
                                .ledger
                                .unlock_funds(
                                    &mut self.merkle,
                                    rel_seq,
                                    existing.order_id,
                                    existing.account_id,
                                    quote_asset,
                                    release_quote,
                                    "ORDER_MODIFY_SIZE_DOWN_RELEASE",
                                    ts,
                                )
                                .expect("modify quote unlock valid")
                            {
                                self.scratch_journals.push(rj);
                                self.push_ring_event(OutboundRingEvent::Journal {
                                    symbol: sym,
                                    entry: rj,
                                });
                            }
                        }
                        new_lock
                    }
                };

                self.books[book_idx].reduce_resting_qty_in_place(
                    slot_idx,
                    target_rem_qty,
                    new_locked_quote,
                    &mut self.lob_acc,
                );
                self.books[book_idx].orders.cold_mut(slot_idx).initial_qty_fp = cmd.new_qty_fp;
                self.emit_l3_delta(
                    book_idx,
                    L3OpCode::Modify,
                    existing.side,
                    existing.order_id,
                    existing.account_id,
                    existing.price_fp,
                    target_rem_qty,
                    -(qty_reduction as i64),
                    ts,
                );
            }

            let ack_seq = self.alloc_seq();
            let root = self.commit_and_publish_command_root();
            return FastOrderAck {
                engine_seq: ack_seq,
                order_id: existing.order_id,
                client_order_id: existing.client_order_id,
                account_id: existing.account_id,
                instrument_id: inst_id,
                symbol: sym,
                status: AckStatus::Modified,
                reject_reason: RejectCode::None,
                reject_detail: "",
                filled_qty_fp: already_filled,
                remaining_qty_fp: target_rem_qty,
                avg_fill_price_fp: 0,
                engine_ts_ns: ts,
                wal_blake3_hash: self.wal.last_blake3(),
                state_root_blake3: root,
                fills_count: 0,
            };
        }

        // Case B: Price changed OR size increased -> pre-verify balance + Post-Only BEFORE touching existing order!
        if existing.kind == OrderKind::PostOnly || cfg.regime == MarketRegimeState::PostOnlyAuction {
            let would_cross = match existing.side {
                Side::Buy => self.books[book_idx]
                    .best_ask_fp()
                    .map(|ask| cmd.new_price_fp >= ask)
                    .unwrap_or(false),
                Side::Sell => self.books[book_idx]
                    .best_bid_fp()
                    .map(|bid| cmd.new_price_fp <= bid)
                    .unwrap_or(false),
            };
            if would_cross {
                return self.reject_ack(
                    existing.order_id,
                    existing.client_order_id,
                    existing.account_id,
                    inst_id,
                    sym,
                    RejectCode::PostOnlyWouldCross,
                    "modified post-only order would cross resting book",
                    cmd.ts_ns,
                );
            }
        }

        match existing.side {
            Side::Sell => {
                let avail_after_unlock = self
                    .ledger
                    .available_balance(existing.account_id, base_asset)
                    .saturating_add(existing.remaining_qty_fp as i128);
                if avail_after_unlock < (target_rem_qty as i128) {
                    return self.reject_ack(
                        existing.order_id,
                        existing.client_order_id,
                        existing.account_id,
                        inst_id,
                        sym,
                        RejectCode::InsufficientBalance,
                        "insufficient base balance for size increase",
                        cmd.ts_ns,
                    );
                }
            }
            Side::Buy => {
                let Ok(new_res) = checked_buyer_reserve_fp(
                    cmd.new_price_fp,
                    target_rem_qty,
                    cfg.maker_fee_ppm,
                    cfg.taker_fee_ppm,
                ) else {
                    return self.reject_ack(
                        existing.order_id,
                        existing.client_order_id,
                        existing.account_id,
                        inst_id,
                        sym,
                        RejectCode::InvalidPriceOrQty,
                        "modified order notional overflow",
                        cmd.ts_ns,
                    );
                };
                let avail_after_unlock = self
                    .ledger
                    .available_balance(existing.account_id, quote_asset)
                    .saturating_add(existing.locked_quote_fp);
                if avail_after_unlock < new_res {
                    return self.reject_ack(
                        existing.order_id,
                        existing.client_order_id,
                        existing.account_id,
                        inst_id,
                        sym,
                        RejectCode::InsufficientBalance,
                        "insufficient quote balance for modified order",
                        cmd.ts_ns,
                    );
                }
            }
        }

        let ts = self.next_monotonic_ts(cmd.ts_ns);
        self.ledger.begin_merkle_batch();
        self.cancel_resting_slot_internal(book_idx, slot_idx, "ORDER_MODIFY_CANCEL_OLD", ts);

        let cid = if !cmd.client_order_id.is_empty() {
            cmd.client_order_id
        } else {
            existing.client_order_id
        };
        let mut ack = self.execute_order_internal(
            book_idx,
            existing.order_id,
            cid,
            existing.account_id,
            existing.side,
            existing.kind,
            existing.stp_mode,
            cmd.new_price_fp,
            cmd.new_qty_fp,
            target_rem_qty,
            0,
            ts,
            true,
        );
        ack.filled_qty_fp = already_filled.saturating_add(ack.filled_qty_fp);
        ack
    }

    /// Applies an autonomous SRE / Sentinel risk mitigation action (`Freeze`, `MarketRegime`, mass open-order cancel).
    pub fn apply_risk_action_fast(
        &mut self,
        account_id: u64,
        new_status: Option<AccountRiskStatus>,
        regime_override: Option<MarketRegimeState>,
        target_symbol: &str,
        cancel_all_open_orders: bool,
        ts_ns: u64,
    ) -> Result<(u64, u32, AccountRiskStatus, [u8; 32]), LedgerError> {
        self.clear_scratch();

        let ts = self.next_monotonic_ts(ts_ns);
        let eff_status = if account_id != 0 {
            if let Some(st) = new_status {
                self.ledger.set_account_status(account_id, st)?
            } else {
                self.ledger.account_status(account_id)
            }
        } else {
            AccountRiskStatus::Active
        };

        if let Some(reg) = regime_override {
            if target_symbol.is_empty() {
                for c in &mut self.configs {
                    c.regime = reg;
                }
            } else {
                let idx =
                    self.resolve_book_idx(0, InstrumentSymbol::from_str_trunc(target_symbol));
                self.configs[idx].regime = reg;
            }
        }

        let mut canceled_count = 0u32;
        self.ledger.begin_merkle_batch();
        if cancel_all_open_orders && account_id != 0 {
            let book_indices: Vec<usize> = if target_symbol.is_empty() {
                (0..self.books.len()).collect()
            } else if let Some(idx) = self.find_book_idx_readonly(0, target_symbol) {
                vec![idx]
            } else {
                Vec::new()
            };

            for b_idx in book_indices {
                let mut to_cancel_slots = Vec::new();
                for side in [Side::Buy, Side::Sell] {
                    let sb = self.books[b_idx].side_book(side);
                    let mut lvl_idx = sb.best_level_idx();
                    while lvl_idx != NULL_IDX {
                        let lvl = sb.get(lvl_idx);
                        let mut ord_idx = lvl.head_order;
                        while ord_idx != NULL_IDX {
                            let h = self.books[b_idx].orders.hot(ord_idx);
                            if h.account_id == account_id {
                                to_cancel_slots.push(ord_idx);
                            }
                            ord_idx = h.next_in_level;
                        }
                        lvl_idx = lvl.worse_level;
                    }
                }
                for slot_idx in to_cancel_slots {
                    self.cancel_resting_slot_internal(b_idx, slot_idx, "RISK_ACTION_CANCEL", ts);
                    canceled_count += 1;
                }
            }
        }

        let seq = self.alloc_seq();
        let root = self.commit_and_publish_command_root();
        Ok((seq, canceled_count, eff_status, root))
    }

    /// Generates a cryptographic Blake3 Merkle proof for `account_id` + global double-entry conservation attestation.
    #[allow(clippy::type_complexity)]
    pub fn generate_account_merkle_proof(
        &mut self,
        account_id: u64,
        primary_quote_asset: &str,
    ) -> (
        u64,
        Vec<AssetBalanceLeaf>,
        [u8; 32],
        [u8; 32],
        Vec<ProofStep>,
        AssetGlobalTotals,
        bool,
    ) {
        // Ensure account exists in Merkle tree if queried before first deposit so its leaf hash is explicit
        let balances = if let Some(acc) = self.ledger.get_account(account_id) {
            acc.assets_slice().to_vec()
        } else {
            Vec::new()
        };
        let (leaf_hash, proof, state_root) = if self.ledger.get_account(account_id).is_some() {
            self.merkle.generate_account_proof(account_id, &self.lob_acc)
        } else {
            // Non-existent account proves clean emptiness at its bucket against `state_root`
            self.merkle.generate_account_proof(account_id, &self.lob_acc)
        };

        let quote_sym = AssetSymbol::from_str_trunc(primary_quote_asset);
        let (quote_totals, quote_ok) = self.ledger.verify_asset_conservation(quote_sym);
        let mut all_assets_balanced = quote_ok;
        for t in self.ledger.all_asset_totals() {
            let (_, ok) = self.ledger.verify_asset_conservation(t.asset);
            if !ok {
                all_assets_balanced = false;
                break;
            }
        }
        debug_assert!(verify_merkle_proof(&leaf_hash, &proof, &state_root));
        (
            self.engine_seq,
            balances,
            leaf_hash,
            state_root,
            proof,
            quote_totals,
            all_assets_balanced,
        )
    }

    /// Generates a cryptographic Blake3 Merkle inclusion proof for historical trade fill `fill_id`.
    pub fn generate_execution_merkle_proof(
        &mut self,
        fill_id: u64,
    ) -> Option<ExecutionInclusionProof> {
        self.merkle
            .generate_execution_proof(fill_id, self.engine_seq, &self.lob_acc)
    }

    /// Verifies both system-wide Double-Entry Conservation (`0` atomic unit error) and Uncrossed Book Law across all books.
    pub fn verify_system_invariants(
        &mut self,
        asset_name: &str,
    ) -> (u64, AssetGlobalTotals, bool, bool, [u8; 32]) {
        let asset = AssetSymbol::from_str_trunc(asset_name);
        let (totals, mut inv_ok) = self.ledger.verify_asset_conservation(asset);
        // If `asset_name` was empty, verify ALL assets!
        if asset_name.is_empty() {
            for t in self.ledger.all_asset_totals() {
                if !self.ledger.verify_asset_conservation(t.asset).1 {
                    inv_ok = false;
                    break;
                }
            }
        }
        let uncrossed = self.books.iter().all(|b| b.is_uncrossed());
        let root = self.merkle.commit_state_root(&self.lob_acc);
        (self.engine_seq, totals, inv_ok, uncrossed, root)
    }

    #[allow(clippy::type_complexity)]
    pub fn l2_snapshot(
        &mut self,
        symbol: &str,
        depth: usize,
    ) -> Option<(
        u64,
        u32,
        InstrumentSymbol,
        Vec<PriceLevelSummary>,
        Vec<PriceLevelSummary>,
        u64,
        u64,
        u64,
        u64,
        u64,
        [u8; 32],
    )> {
        let idx = self.find_book_idx_readonly(0, symbol)?;
        let eff_depth = if depth == 0 { 50 } else { depth };
        let (bids, asks) = self.books[idx].snapshot_l2(eff_depth);
        let best_bid = bids.first().map(|l| l.price_fp).unwrap_or(0);
        let best_ask = asks.first().map(|l| l.price_fp).unwrap_or(0);
        let (mid, spread) = if best_bid > 0 && best_ask >= best_bid {
            (
                ((best_bid as u128 + best_ask as u128) / 2) as u64,
                best_ask - best_bid,
            )
        } else {
            (0, 0)
        };
        let inst_id = self.books[idx].instrument_id;
        let sym = self.books[idx].symbol;
        let root = self.merkle.commit_state_root(&self.lob_acc);
        Some((
            self.engine_seq,
            inst_id,
            sym,
            bids,
            asks,
            best_bid,
            best_ask,
            mid,
            spread,
            self.last_ts_ns,
            root,
        ))
    }

    /// Exports complete lossless exchange state as a `SnapshotPayloadV1` for `HYPSNP01` persistence.
    pub fn build_snapshot_payload(&mut self) -> SnapshotPayloadV1 {
        let root = self.merkle.commit_state_root(&self.lob_acc);
        let accounts = self
            .ledger
            .active_accounts_slice()
            .iter()
            .map(|a| SnapshotAccountV1 {
                account_id: a.account_id,
                status: a.status as u8,
                balances: a
                    .assets_slice()
                    .iter()
                    .map(|b| SnapshotAssetBalanceV1 {
                        asset: b.asset.as_str().to_string(),
                        available_fp: b.available_fp.to_string(),
                        locked_fp: b.locked_fp.to_string(),
                    })
                    .collect(),
            })
            .collect();

        let asset_totals = self
            .ledger
            .all_asset_totals()
            .iter()
            .map(|t| SnapshotAssetTotalV1 {
                asset: t.asset.as_str().to_string(),
                total_deposits_fp: t.total_deposits_fp.to_string(),
                total_available_fp: t.total_available_fp.to_string(),
                total_locked_fp: t.total_locked_fp.to_string(),
                treasury_fee_pool_fp: t.treasury_fee_pool_fp.to_string(),
            })
            .collect();

        let instruments = self
            .books
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let c = &self.configs[i];
                SnapshotInstrumentV1 {
                    instrument_id: b.instrument_id,
                    symbol: b.symbol.as_str().to_string(),
                    base_asset: b.base_asset.as_str().to_string(),
                    quote_asset: b.quote_asset.as_str().to_string(),
                    regime: c.regime as u8,
                    maker_fee_ppm: c.maker_fee_ppm,
                    taker_fee_ppm: c.taker_fee_ppm,
                    prev_l3_seq: b.prev_l3_seq,
                    resting_orders: b
                        .export_resting_orders_deterministic()
                        .into_iter()
                        .map(|o| SnapshotOrderV1 {
                            order_id: o.order_id,
                            client_order_id: o.client_order_id.as_str().to_string(),
                            account_id: o.account_id,
                            side: o.side as u8,
                            kind: o.kind as u8,
                            stp_mode: o.stp_mode as u8,
                            price_fp: o.price_fp,
                            initial_qty_fp: o.initial_qty_fp,
                            remaining_qty_fp: o.remaining_qty_fp,
                            locked_quote_fp: o.locked_quote_fp.to_string(),
                            created_ts_ns: o.created_ts_ns,
                            priority_seq: o.priority_seq,
                        })
                        .collect(),
                }
            })
            .collect();

        SnapshotPayloadV1 {
            engine_seq: self.engine_seq,
            ts_ns: self.last_ts_ns,
            journal_seq: self.ledger.journal_seq(),
            last_wal_blake3: self.wal.last_blake3(),
            last_journal_blake3: self.ledger.last_journal_blake3(),
            next_order_id: self.next_order_id,
            next_fill_id: self.next_fill_id,
            state_root_blake3: root,
            accounts,
            asset_totals,
            instruments,
            exec_leaf_hashes: self.merkle.execution_leaves().to_vec(),
        }
    }

    /// Writes an atomic `HYPSNP01` snapshot to `dir` (and flushes WAL first so WAL >= snapshot!).
    pub fn write_snapshot_to_dir(&mut self, dir: impl AsRef<Path>) -> Result<PathBuf, WalError> {
        self.wal.flush_and_sync()?;
        let payload = self.build_snapshot_payload();
        let path = SnapshotStore::write_snapshot(dir, &payload)?;
        self.last_snapshot_seq = payload.engine_seq;
        Ok(path)
    }

    /// Restores bit-identical deterministic engine state from a verified `SnapshotPayloadV1`.
    pub fn restore_from_snapshot(&mut self, snap: &SnapshotPayloadV1) -> Result<(), WalError> {
        self.merkle.clear_and_reset();
        self.lob_acc = LobStateAccumulator::new();
        self.idempotency.clear();
        self.books.clear();
        self.configs.clear();

        let mut parsed_accounts = Vec::with_capacity(snap.accounts.len());
        for a in &snap.accounts {
            let st = match a.status {
                2 => AccountRiskStatus::Throttled,
                3 => AccountRiskStatus::Frozen,
                4 => AccountRiskStatus::LiquidationOnly,
                _ => AccountRiskStatus::Active,
            };
            let mut bals = Vec::with_capacity(a.balances.len());
            for b in &a.balances {
                let avail: i128 = b
                    .available_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("avail parse: {e}")))?;
                let locked: i128 = b
                    .locked_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("locked parse: {e}")))?;
                bals.push(AssetBalanceLeaf {
                    asset: AssetSymbol::from_str_trunc(&b.asset),
                    available_fp: avail,
                    locked_fp: locked,
                });
            }
            bals.sort_by_key(|x| x.asset);
            parsed_accounts.push((a.account_id, st, bals));
        }

        let mut parsed_totals = Vec::with_capacity(snap.asset_totals.len());
        for t in &snap.asset_totals {
            parsed_totals.push(AssetGlobalTotals {
                asset: AssetSymbol::from_str_trunc(&t.asset),
                total_deposits_fp: t
                    .total_deposits_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("dep parse: {e}")))?,
                total_available_fp: t
                    .total_available_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("tot avail parse: {e}")))?,
                total_locked_fp: t
                    .total_locked_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("tot lock parse: {e}")))?,
                treasury_fee_pool_fp: t
                    .treasury_fee_pool_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("treasury parse: {e}")))?,
            });
        }

        self.ledger
            .restore_from_snapshot_parts(
                &mut self.merkle,
                snap.journal_seq,
                snap.last_journal_blake3,
                &parsed_accounts,
                &parsed_totals,
            )
            .map_err(|e| WalError::SnapshotCodec(e.to_string()))?;

        for inst in &snap.instruments {
            let b_idx = self.register_instrument(
                inst.instrument_id,
                &inst.symbol,
                &inst.base_asset,
                &inst.quote_asset,
            );
            self.configs[b_idx].regime = MarketRegimeState::from_u8(inst.regime);
            self.configs[b_idx].maker_fee_ppm = inst.maker_fee_ppm;
            self.configs[b_idx].taker_fee_ppm = inst.taker_fee_ppm;
            self.books[b_idx].prev_l3_seq = inst.prev_l3_seq;

            for o in &inst.resting_orders {
                let locked_q: i128 = o
                    .locked_quote_fp
                    .parse()
                    .map_err(|e| WalError::SnapshotCodec(format!("order lock parse: {e}")))?;
                let rec = OrderRecord {
                    order_id: o.order_id,
                    account_id: o.account_id,
                    instrument_id: inst.instrument_id,
                    side: Side::from_u8(o.side).unwrap_or(Side::Buy),
                    kind: OrderKind::from_u8(o.kind).unwrap_or(OrderKind::Limit),
                    stp_mode: StpPolicy::from_u8(o.stp_mode),
                    price_fp: o.price_fp,
                    initial_qty_fp: o.initial_qty_fp,
                    remaining_qty_fp: o.remaining_qty_fp,
                    locked_quote_fp: locked_q,
                    created_ts_ns: o.created_ts_ns,
                    priority_seq: o.priority_seq,
                    client_order_id: ClientOrderId::from_str_trunc(&o.client_order_id),
                };
                self.books[b_idx]
                    .insert_resting(rec, &mut self.lob_acc)
                    .map_err(|e| WalError::SnapshotCodec(e.to_string()))?;
            }
        }

        for h in &snap.exec_leaf_hashes {
            self.merkle.append_execution_leaf(*h);
        }

        self.engine_seq = snap.engine_seq;
        self.last_ts_ns = snap.ts_ns;
        self.next_order_id = snap.next_order_id;
        self.next_fill_id = snap.next_fill_id;
        self.last_snapshot_seq = snap.engine_seq;
        self.wal
            .set_chain_anchor(snap.engine_seq, snap.last_wal_blake3);

        let computed_root = self.merkle.commit_state_root(&self.lob_acc);
        if computed_root != snap.state_root_blake3 {
            return Err(WalError::HashChainBroken {
                engine_seq: snap.engine_seq,
            });
        }
        Ok(())
    }

    #[inline]
    pub fn maybe_auto_snapshot(&mut self) -> Result<Option<PathBuf>, WalError> {
        if self.snapshot_every_n_events == 0 {
            return Ok(None);
        }
        let Some(dir) = self.snapshot_dir.clone() else {
            return Ok(None);
        };
        if self.engine_seq.saturating_sub(self.last_snapshot_seq) >= self.snapshot_every_n_events {
            let p = self.write_snapshot_to_dir(dir)?;
            return Ok(Some(p));
        }
        Ok(None)
    }

    /// Recovers an engine instance from `${data_dir}/snapshots` + `${data_dir}/wal/segment_00000001.wal`,
    /// replaying all tail `HYPWAL01` Protobuf `MarketEventEnvelope` frames via `replay_callback`.
    pub fn open_and_recover_with<F>(
        sizing: EngineSizing,
        data_dir: impl AsRef<Path>,
        sync_mode: WalSyncMode,
        outbound_ring: Option<SpscProducer<OutboundRingEvent>>,
        mut replay_frame_fn: F,
    ) -> Result<(Self, usize, bool), WalError>
    where
        F: FnMut(&mut Self, &WalFrame) -> Result<(), WalError>,
    {
        let data_dir = data_dir.as_ref();
        let wal_path = data_dir.join("wal").join("segment_00000001.wal");
        let snap_dir = data_dir.join("snapshots");

        let (mut wal_writer, frames, truncated_tail) = WalWriter::open_segment(&wal_path, sync_mode)?;
        let max_wal_seq = frames.last().map(|f| f.engine_seq).unwrap_or(u64::MAX);
        let latest_snap = SnapshotStore::find_latest_snapshot_before(&snap_dir, max_wal_seq)?;

        let mut engine =
            Self::with_wal_and_ring(sizing, WalWriter::in_memory(), Some(snap_dir), outbound_ring);

        let mut start_after_seq = 0u64;
        if let Some(snap) = &latest_snap {
            engine.restore_from_snapshot(snap)?;
            start_after_seq = snap.engine_seq;
        }

        let mut replayed_count = 0usize;
        for frame in &frames {
            if frame.engine_seq <= start_after_seq {
                continue;
            }
            if frame.engine_seq == start_after_seq + 1 && start_after_seq > 0 {
                if let Some(snap) = &latest_snap {
                    if frame.prev_blake3 != snap.last_wal_blake3 {
                        return Err(WalError::HashChainBroken {
                            engine_seq: frame.engine_seq,
                        });
                    }
                }
            }
            replay_frame_fn(&mut engine, frame)?;
            engine.engine_seq = frame.engine_seq;
            engine.last_ts_ns = engine.last_ts_ns.max(frame.ts_ns);
            replayed_count += 1;
        }

        if frames.is_empty() {
            if let Some(snap) = &latest_snap {
                wal_writer.set_chain_anchor(snap.engine_seq, snap.last_wal_blake3);
            }
        }
        engine.wal = wal_writer;
        engine.merkle.commit_state_root(&engine.lob_acc);
        Ok((engine, replayed_count, truncated_tail))
    }
}
