#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::cmp::Ordering;

use crate::error::ChronosError;
use crate::fixed::{
    checked_buyer_reserve_fp, checked_fee_fp, checked_notional_fp, compute_realized_volatility_bps,
    compute_slippage_bps, i128_to_pb, pb_to_i128, pb_to_u128, u128_to_pb, AssetSymbol, InlineStr,
    InstrumentSpec, DEFAULT_MAKER_FEE_PPM, DEFAULT_TAKER_FEE_PPM,
};
use crate::merkle::{
    hash_account_leaf, hash_execution_fill_leaf, AssetBalanceLeaf, ExecutionFillDigestInput,
    LobStateAccumulator, StateCommitmentTree,
};
use crate::pb::{
    market_event_envelope::Payload as EnvelopePayload, AccountAssetBalance, AccountStatus,
    ExecutionFill, L2BookSnapshot, L3BookDelta, L3DeltaOp, LedgerJournalEntry, MerkleLedgerProof,
    MerkleProofNode, OrderSide, PriceLevel,
};
use crate::wal::{
    CanonicalSnapshotPayload, SnapshotAccountBalance, SnapshotAccountStatus, SnapshotAssetPool,
    SnapshotRestingOrder, VerifiedSnapshot,
};

pub const NULL_SLOT: u32 = u32::MAX;
pub const MAX_INLINE_ASSETS: usize = 8;

/// 64-byte L1 cache-line aligned hot resting order node (`std::mem::size_of == 64`).
/// Separates hot matching/traversal fields from cold string/collateral metadata.
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotOrderSlot {
    pub order_id: u64,
    pub account_id: u64,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub priority_seq: u64,
    pub ts_ns: u64,
    pub instrument_id: u32,
    pub prev_slot: u32,
    pub next_slot: u32,
    pub side: u8, // 1 = BUY, 2 = SELL
    pub active: u8,
    pub _pad: [u8; 2],
}

const _: () = assert!(std::mem::size_of::<HotOrderSlot>() == 64);
const _: () = assert!(std::mem::align_of::<HotOrderSlot>() == 64);

#[derive(Debug, Clone, Copy, Default)]
pub struct ColdOrderMeta {
    pub client_order_id: InlineStr<48>,
    pub symbol: InlineStr<24>,
    pub locked_collateral_fp: i128,
}

/// Contiguous 32-byte price-level queue head (2 levels per 64-byte cache line).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceLevelHead {
    pub price_fp: u64,
    pub total_qty_fp: u64,
    pub head_slot: u32,
    pub tail_slot: u32,
    pub order_count: u32,
    pub _pad: u32,
}

const _: () = assert!(std::mem::size_of::<PriceLevelHead>() == 32);

/// Open-addressed `u64 -> u32` hash table with backward-shift deletion (zero tombstones, zero steady-state allocs).
#[derive(Debug, Clone)]
pub struct FlatU64Map {
    keys: Vec<u64>,
    vals: Vec<u32>,
    occupied: Vec<bool>,
    mask: usize,
    len: usize,
}

impl FlatU64Map {
    pub fn with_capacity(min_cap: usize) -> Self {
        let cap = (min_cap.saturating_mul(2)).max(16).next_power_of_two();
        Self {
            keys: vec![0; cap],
            vals: vec![NULL_SLOT; cap],
            occupied: vec![false; cap],
            mask: cap - 1,
            len: 0,
        }
    }

    #[inline(always)]
    fn bucket(&self, key: u64) -> usize {
        let mut h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 32;
        (h as usize) & self.mask
    }

    #[inline]
    pub fn clear_keep_capacity(&mut self) {
        self.occupied.fill(false);
        self.len = 0;
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, key: u64) -> Option<u32> {
        let mut idx = self.bucket(key);
        while self.occupied[idx] {
            if self.keys[idx] == key {
                return Some(self.vals[idx]);
            }
            idx = (idx + 1) & self.mask;
        }
        None
    }

    pub fn insert(&mut self, key: u64, val: u32) -> Option<u32> {
        if (self.len + 1) * 4 >= self.keys.len() * 3 {
            self.grow();
        }
        let mut idx = self.bucket(key);
        while self.occupied[idx] {
            if self.keys[idx] == key {
                let prev = self.vals[idx];
                self.vals[idx] = val;
                return Some(prev);
            }
            idx = (idx + 1) & self.mask;
        }
        self.occupied[idx] = true;
        self.keys[idx] = key;
        self.vals[idx] = val;
        self.len += 1;
        None
    }

    pub fn remove(&mut self, key: u64) -> Option<u32> {
        let mut idx = self.bucket(key);
        while self.occupied[idx] {
            if self.keys[idx] == key {
                let removed = self.vals[idx];
                self.len -= 1;
                let mut hole = idx;
                let mut scan = (hole + 1) & self.mask;
                while self.occupied[scan] {
                    let natural = self.bucket(self.keys[scan]);
                    let dist_scan = (scan.wrapping_sub(natural)) & self.mask;
                    let dist_hole = (hole.wrapping_sub(natural)) & self.mask;
                    if dist_scan >= dist_hole {
                        self.keys[hole] = self.keys[scan];
                        self.vals[hole] = self.vals[scan];
                        hole = scan;
                    }
                    scan = (scan + 1) & self.mask;
                }
                self.occupied[hole] = false;
                return Some(removed);
            }
            idx = (idx + 1) & self.mask;
        }
        None
    }

    fn grow(&mut self) {
        let new_cap = (self.keys.len() * 2).max(16);
        let old_keys = std::mem::replace(&mut self.keys, vec![0; new_cap]);
        let old_vals = std::mem::replace(&mut self.vals, vec![NULL_SLOT; new_cap]);
        let old_occ = std::mem::replace(&mut self.occupied, vec![false; new_cap]);
        self.mask = new_cap - 1;
        self.len = 0;
        for i in 0..old_occ.len() {
            if old_occ[i] {
                self.insert(old_keys[i], old_vals[i]);
            }
        }
    }
}

/// Per-symbol cache-friendly L2/L3 Order Book:
/// - `bids` sorted ASCENDING by `price_fp` so `bids.last()` is Best Bid (`O(1)` pop on full sweep!).
/// - `asks` sorted DESCENDING by `price_fp` so `asks.last()` is Best Ask (`O(1)` pop on full sweep!).
#[derive(Debug, Clone)]
pub struct SymbolBook {
    pub spec: InstrumentSpec,
    pub bids: Vec<PriceLevelHead>,
    pub asks: Vec<PriceLevelHead>,
    pub last_clearing_price_fp: u64,
    pub cumulative_base_vol_fp: u128,
    pub cumulative_quote_vol_fp: u128,
}

impl SymbolBook {
    pub fn new(symbol: &str) -> Self {
        Self {
            spec: InstrumentSpec::from_symbol(symbol),
            bids: Vec::with_capacity(128),
            asks: Vec::with_capacity(128),
            last_clearing_price_fp: 0,
            cumulative_base_vol_fp: 0,
            cumulative_quote_vol_fp: 0,
        }
    }

    pub fn clear_keep_capacity(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.last_clearing_price_fp = 0;
        self.cumulative_base_vol_fp = 0;
        self.cumulative_quote_vol_fp = 0;
    }

    #[inline(always)]
    pub fn best_bid_fp(&self) -> u64 {
        self.bids.last().map(|l| l.price_fp).unwrap_or(0)
    }

    #[inline(always)]
    pub fn best_ask_fp(&self) -> u64 {
        self.asks.last().map(|l| l.price_fp).unwrap_or(0)
    }

    #[inline(always)]
    pub fn mid_price_fp(&self) -> u64 {
        let bid = self.best_bid_fp();
        let ask = self.best_ask_fp();
        if bid > 0 && ask > 0 {
            ((bid as u128 + ask as u128) / 2) as u64
        } else if bid > 0 {
            bid
        } else if ask > 0 {
            ask
        } else {
            self.last_clearing_price_fp
        }
    }

    #[inline(always)]
    pub fn spread_fp(&self) -> u64 {
        let bid = self.best_bid_fp();
        let ask = self.best_ask_fp();
        if bid > 0 && ask > bid {
            ask - bid
        } else {
            0
        }
    }

    #[inline(always)]
    pub fn spread_bps(&self) -> u32 {
        let mid = self.mid_price_fp();
        let spread = self.spread_fp();
        if mid == 0 || spread == 0 {
            0
        } else {
            ((spread as u128 * 10_000) / (mid as u128)) as u32
        }
    }

    #[inline]
    pub fn find_level_idx(&self, side: u8, price_fp: u64) -> Result<usize, usize> {
        if side == 1 {
            // Bids sorted ascending
            self.bids
                .binary_search_by(|lvl| lvl.price_fp.cmp(&price_fp))
        } else {
            // Asks sorted descending
            self.asks
                .binary_search_by(|lvl| price_fp.cmp(&lvl.price_fp))
        }
    }
}

/// Zero-heap-allocation inline multi-asset account record.
#[derive(Debug, Clone)]
pub struct AccountRecord {
    pub account_id: u64,
    pub status: i32,
    pub assets: [AssetBalanceLeaf; MAX_INLINE_ASSETS],
    pub asset_count: u8,
    pub rate_window_sec: u64,
    pub rate_window_count: u32,
}

impl AccountRecord {
    pub fn new(account_id: u64) -> Self {
        Self {
            account_id,
            status: AccountStatus::Active as i32,
            assets: [AssetBalanceLeaf {
                asset: AssetSymbol::empty(),
                available_fp: 0,
                locked_fp: 0,
            }; MAX_INLINE_ASSETS],
            asset_count: 0,
            rate_window_sec: 0,
            rate_window_count: 0,
        }
    }

    #[inline]
    pub fn get_asset(&self, asset: AssetSymbol) -> (i128, i128) {
        let cnt = self.asset_count as usize;
        for i in 0..cnt {
            if self.assets[i].asset == asset {
                return (self.assets[i].available_fp, self.assets[i].locked_fp);
            }
        }
        (0, 0)
    }

    #[inline]
    pub fn get_or_insert_asset_mut(
        &mut self,
        asset: AssetSymbol,
    ) -> Result<&mut AssetBalanceLeaf, ChronosError> {
        let cnt = self.asset_count as usize;
        for i in 0..cnt {
            if self.assets[i].asset == asset {
                return Ok(&mut self.assets[i]);
            }
        }
        if cnt >= MAX_INLINE_ASSETS {
            return Err(ChronosError::InvalidArgument(format!(
                "account {} exceeded max inline assets ({})",
                self.account_id, MAX_INLINE_ASSETS
            )));
        }
        // Keep assets sorted lexicographically so `hash_account_leaf` is always canonical without allocating!
        let mut insert_pos = cnt;
        for i in 0..cnt {
            if asset < self.assets[i].asset {
                insert_pos = i;
                break;
            }
        }
        for j in (insert_pos..cnt).rev() {
            self.assets[j + 1] = self.assets[j];
        }
        self.assets[insert_pos] = AssetBalanceLeaf {
            asset,
            available_fp: 0,
            locked_fp: 0,
        };
        self.asset_count += 1;
        Ok(&mut self.assets[insert_pos])
    }

    #[inline]
    pub fn sorted_balances(&self) -> &[AssetBalanceLeaf] {
        &self.assets[..self.asset_count as usize]
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AssetPoolRecord {
    pub asset: AssetSymbol,
    pub total_deposits_fp: u128,
    pub treasury_fee_pool_fp: i128,
}

/// Quantitative liquidity & microstructure summary reconstructed at moment `T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconstructedLiquidityMetrics {
    pub best_bid_fp: u64,
    pub best_ask_fp: u64,
    pub mid_price_fp: u64,
    pub spread_fp: u64,
    pub spread_bps: u32,
    pub total_bid_depth_fp: u128,
    pub total_ask_depth_fp: u128,
    pub book_imbalance_top5_ppm: i64,
    pub last_clearing_price_fp: u64,
    pub vwap_fp: u64,
}

/// Deterministic Exchange & Double-Entry Ledger State Machine used by both:
/// 1. Snapshot + WAL Fast-Forward State Reconstruction (`TimeTravelSeek` & `StreamReplayFrames`)
/// 2. Isolated Counterfactual Sandbox Matching (`TimelineFork`)
#[derive(Clone)]
pub struct ExchangeState {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub hot_orders: Vec<HotOrderSlot>,
    pub cold_orders: Vec<ColdOrderMeta>,
    pub free_slots: Vec<u32>,
    pub order_index: FlatU64Map,
    pub books: Vec<SymbolBook>,
    pub accounts: Vec<AccountRecord>,
    pub account_index: FlatU64Map,
    pub dirty_account_flags: Vec<bool>,
    pub dirty_account_indices: Vec<u32>,
    pub asset_pools: Vec<AssetPoolRecord>,
    pub merkle: StateCommitmentTree,
    pub next_fill_id: u64,
    pub maker_fee_ppm: i64,
    pub taker_fee_ppm: i64,
    pub fee_schedule_known: bool,
    /// Tracks window-local execution prices & max slippage during counterfactual interval `(T1, T2]`.
    pub window_fill_prices: Vec<u64>,
    pub window_max_slippage_bps: u32,
    pub window_fills: Vec<ExecutionFill>,
    pub track_window_fills: bool,
}

impl ExchangeState {
    pub fn with_capacity(order_cap: usize, account_cap: usize) -> Self {
        Self {
            engine_seq: 0,
            ts_ns: 0,
            hot_orders: Vec::with_capacity(order_cap),
            cold_orders: Vec::with_capacity(order_cap),
            free_slots: Vec::with_capacity(order_cap / 2),
            order_index: FlatU64Map::with_capacity(order_cap),
            books: vec![SymbolBook::new("BTC-USDT")],
            accounts: Vec::with_capacity(account_cap),
            account_index: FlatU64Map::with_capacity(account_cap),
            dirty_account_flags: Vec::with_capacity(account_cap),
            dirty_account_indices: Vec::with_capacity(256),
            asset_pools: Vec::with_capacity(8),
            merkle: StateCommitmentTree::new(),
            next_fill_id: 1,
            maker_fee_ppm: DEFAULT_MAKER_FEE_PPM,
            taker_fee_ppm: DEFAULT_TAKER_FEE_PPM,
            fee_schedule_known: false,
            window_fill_prices: Vec::new(),
            window_max_slippage_bps: 0,
            window_fills: Vec::new(),
            track_window_fills: false,
        }
    }

    pub fn new() -> Self {
        Self::with_capacity(4096, 512)
    }

    /// Resets state without deallocating arena vectors (used by zero-leak soak tests & fast reuse).
    pub fn reset_keep_capacity(&mut self) {
        self.engine_seq = 0;
        self.ts_ns = 0;
        self.hot_orders.clear();
        self.cold_orders.clear();
        self.free_slots.clear();
        self.order_index.clear_keep_capacity();
        for b in &mut self.books {
            b.clear_keep_capacity();
        }
        self.accounts.clear();
        self.account_index.clear_keep_capacity();
        self.dirty_account_flags.clear();
        self.dirty_account_indices.clear();
        self.asset_pools.clear();
        self.merkle.clear_keep_capacity();
        self.next_fill_id = 1;
        self.maker_fee_ppm = DEFAULT_MAKER_FEE_PPM;
        self.taker_fee_ppm = DEFAULT_TAKER_FEE_PPM;
        self.fee_schedule_known = false;
        self.window_fill_prices.clear();
        self.window_max_slippage_bps = 0;
        self.window_fills.clear();
    }

    pub fn clear_window_trackers(&mut self) {
        self.window_fill_prices.clear();
        self.window_max_slippage_bps = 0;
        self.window_fills.clear();
    }

    /// Restores full deterministic state from a verified `HYPSNP01` snapshot.
    pub fn from_snapshot(snap: &VerifiedSnapshot) -> Result<Self, ChronosError> {
        let p = &snap.payload;
        let mut st = Self::with_capacity(
            p.resting_orders.len().saturating_add(1024),
            p.account_balances.len().saturating_add(128),
        );
        st.engine_seq = snap.engine_seq;
        st.ts_ns = snap.ts_ns;
        st.next_fill_id = p.next_fill_id.max(1);
        st.maker_fee_ppm = if p.maker_fee_ppm != 0 || p.taker_fee_ppm != 0 {
            p.maker_fee_ppm
        } else {
            DEFAULT_MAKER_FEE_PPM
        };
        st.taker_fee_ppm = if p.maker_fee_ppm != 0 || p.taker_fee_ppm != 0 {
            p.taker_fee_ppm
        } else {
            DEFAULT_TAKER_FEE_PPM
        };

        for pool in &p.asset_pools {
            let sym = AssetSymbol::from_str_trunc(&pool.asset);
            st.asset_pools.push(AssetPoolRecord {
                asset: sym,
                total_deposits_fp: pool.total_deposits_fp,
                treasury_fee_pool_fp: pool.treasury_fee_pool_fp,
            });
        }

        for bal in &p.account_balances {
            let acc_idx = st.ensure_account(bal.account_id);
            let sym = AssetSymbol::from_str_trunc(&bal.asset);
            let entry = st.accounts[acc_idx as usize].get_or_insert_asset_mut(sym)?;
            entry.available_fp = bal.available_fp;
            entry.locked_fp = bal.locked_fp;
            st.mark_account_dirty(acc_idx);
        }

        for st_rec in &p.account_statuses {
            let acc_idx = st.ensure_account(st_rec.account_id);
            st.accounts[acc_idx as usize].status = st_rec.status;
        }

        // Restore resting orders in exact `(priority_seq, order_id)` FIFO order.
        let mut sorted_orders = p.resting_orders.clone();
        sorted_orders.sort_by(|a, b| {
            a.priority_seq
                .cmp(&b.priority_seq)
                .then_with(|| a.order_id.cmp(&b.order_id))
        });
        for ro in &sorted_orders {
            st.insert_resting_order_raw(
                ro.order_id,
                &ro.client_order_id,
                ro.instrument_id,
                &ro.symbol,
                ro.account_id,
                ro.side,
                ro.price_fp,
                ro.remaining_qty_fp,
                ro.locked_collateral_fp,
                ro.priority_seq,
                ro.ts_ns,
            );
        }

        for (sym, px) in &p.last_clearing_prices {
            let b_idx = st.ensure_book(sym);
            st.books[b_idx].last_clearing_price_fp = *px;
        }

        // Restore execution MMR leaves / frontier if present in snapshot.
        if !p.exec_leaf_hashes.is_empty() {
            for h in &p.exec_leaf_hashes {
                st.merkle.append_execution_leaf(*h);
            }
        } else {
            st.merkle
                .restore_exec_frontier(p.exec_leaf_count, &p.exec_frontier);
        }

        let computed_root = st.compute_state_root();
        if snap.state_root_blake3 != [0u8; 32] && computed_root != snap.state_root_blake3 {
            return Err(ChronosError::SnapshotStateRootMismatch {
                engine_seq: snap.engine_seq,
            });
        }

        Ok(st)
    }

    /// Exports current `ExchangeState` into a `CanonicalSnapshotPayload` suitable for `HYPSNP01`.
    pub fn to_snapshot_payload(&mut self) -> CanonicalSnapshotPayload {
        let mut resting_orders = Vec::with_capacity(self.order_index.len());
        for (idx, slot) in self.hot_orders.iter().enumerate() {
            if slot.active != 0 && slot.remaining_qty_fp > 0 {
                let cold = &self.cold_orders[idx];
                resting_orders.push(SnapshotRestingOrder {
                    order_id: slot.order_id,
                    client_order_id: cold.client_order_id.as_str().to_string(),
                    instrument_id: slot.instrument_id,
                    symbol: cold.symbol.as_str().to_string(),
                    account_id: slot.account_id,
                    side: slot.side,
                    price_fp: slot.price_fp,
                    remaining_qty_fp: slot.remaining_qty_fp,
                    locked_collateral_fp: cold.locked_collateral_fp,
                    priority_seq: slot.priority_seq,
                    ts_ns: slot.ts_ns,
                });
            }
        }
        resting_orders.sort_by(|a, b| {
            a.priority_seq
                .cmp(&b.priority_seq)
                .then_with(|| a.order_id.cmp(&b.order_id))
        });

        let mut account_balances = Vec::new();
        let mut account_statuses = Vec::new();
        for acc in &self.accounts {
            if acc.status != AccountStatus::Active as i32 {
                account_statuses.push(SnapshotAccountStatus {
                    account_id: acc.account_id,
                    status: acc.status,
                });
            }
            for b in acc.sorted_balances() {
                account_balances.push(SnapshotAccountBalance {
                    account_id: acc.account_id,
                    asset: b.asset.as_str().to_string(),
                    available_fp: b.available_fp,
                    locked_fp: b.locked_fp,
                });
            }
        }
        account_balances.sort_by(|a, b| {
            a.account_id
                .cmp(&b.account_id)
                .then_with(|| a.asset.cmp(&b.asset))
        });
        account_statuses.sort_by_key(|s| s.account_id);

        let mut asset_pools: Vec<SnapshotAssetPool> = self
            .asset_pools
            .iter()
            .map(|p| SnapshotAssetPool {
                asset: p.asset.as_str().to_string(),
                total_deposits_fp: p.total_deposits_fp,
                treasury_fee_pool_fp: p.treasury_fee_pool_fp,
            })
            .collect();
        asset_pools.sort_by(|a, b| a.asset.cmp(&b.asset));

        let last_clearing_prices = self
            .books
            .iter()
            .filter(|b| b.last_clearing_price_fp > 0)
            .map(|b| (b.spec.symbol.clone(), b.last_clearing_price_fp))
            .collect();

        let (exec_leaf_count, exec_frontier) = self.merkle.export_exec_frontier();

        CanonicalSnapshotPayload {
            engine_seq: self.engine_seq,
            ts_ns: self.ts_ns,
            resting_orders,
            account_balances,
            account_statuses,
            asset_pools,
            last_clearing_prices,
            exec_leaf_count,
            exec_frontier,
            exec_leaf_hashes: Vec::new(),
            last_wal_blake3: [0u8; 32],
            next_fill_id: self.next_fill_id,
            maker_fee_ppm: self.maker_fee_ppm,
            taker_fee_ppm: self.taker_fee_ppm,
        }
    }

    #[inline]
    pub fn ensure_book(&mut self, symbol: &str) -> usize {
        for (i, b) in self.books.iter().enumerate() {
            if b.spec.symbol == symbol {
                return i;
            }
        }
        let idx = self.books.len();
        self.books.push(SymbolBook::new(symbol));
        idx
    }

    #[inline]
    pub fn find_book(&self, symbol: &str) -> Option<&SymbolBook> {
        self.books.iter().find(|b| b.spec.symbol == symbol)
    }

    #[inline]
    pub fn ensure_account(&mut self, account_id: u64) -> u32 {
        if let Some(idx) = self.account_index.get(account_id) {
            return idx;
        }
        let idx = self.accounts.len() as u32;
        self.accounts.push(AccountRecord::new(account_id));
        self.dirty_account_flags.push(false);
        self.account_index.insert(account_id, idx);
        idx
    }

    #[inline]
    fn mark_account_dirty(&mut self, acc_idx: u32) {
        let i = acc_idx as usize;
        if !self.dirty_account_flags[i] {
            self.dirty_account_flags[i] = true;
            self.dirty_account_indices.push(acc_idx);
        }
    }

    #[inline]
    fn ensure_asset_pool_mut(&mut self, asset: AssetSymbol) -> &mut AssetPoolRecord {
        for i in 0..self.asset_pools.len() {
            if self.asset_pools[i].asset == asset {
                return &mut self.asset_pools[i];
            }
        }
        self.asset_pools.push(AssetPoolRecord {
            asset,
            total_deposits_fp: 0,
            treasury_fee_pool_fp: 0,
        });
        let last = self.asset_pools.len() - 1;
        &mut self.asset_pools[last]
    }

    fn alloc_order_slot(&mut self, hot: HotOrderSlot, cold: ColdOrderMeta) -> u32 {
        if let Some(slot_idx) = self.free_slots.pop() {
            let i = slot_idx as usize;
            self.hot_orders[i] = hot;
            self.cold_orders[i] = cold;
            slot_idx
        } else {
            let slot_idx = self.hot_orders.len() as u32;
            self.hot_orders.push(hot);
            self.cold_orders.push(cold);
            slot_idx
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_resting_order_raw(
        &mut self,
        order_id: u64,
        client_order_id: &str,
        instrument_id: u32,
        symbol: &str,
        account_id: u64,
        side: u8,
        price_fp: u64,
        remaining_qty_fp: u64,
        locked_collateral_fp: i128,
        priority_seq: u64,
        ts_ns: u64,
    ) -> u32 {
        if let Some(existing) = self.order_index.get(order_id) {
            self.remove_resting_order_by_id(order_id);
            let _ = existing;
        }
        let b_idx = self.ensure_book(symbol);
        let eff_inst_id = if instrument_id != 0 {
            instrument_id
        } else {
            self.books[b_idx].spec.instrument_id
        };
        let slot_idx = self.alloc_order_slot(
            HotOrderSlot {
                order_id,
                account_id,
                price_fp,
                remaining_qty_fp,
                priority_seq,
                ts_ns,
                instrument_id: eff_inst_id,
                prev_slot: NULL_SLOT,
                next_slot: NULL_SLOT,
                side,
                active: 1,
                _pad: [0; 2],
            },
            ColdOrderMeta {
                client_order_id: InlineStr::from_str_trunc(client_order_id),
                symbol: InlineStr::from_str_trunc(symbol),
                locked_collateral_fp,
            },
        );
        self.order_index.insert(order_id, slot_idx);

        let levels = if side == 1 {
            &mut self.books[b_idx].bids
        } else {
            &mut self.books[b_idx].asks
        };
        let search = if side == 1 {
            levels.binary_search_by(|l| l.price_fp.cmp(&price_fp))
        } else {
            levels.binary_search_by(|l| price_fp.cmp(&l.price_fp))
        };
        match search {
            Ok(l_idx) => {
                let tail = levels[l_idx].tail_slot;
                self.hot_orders[slot_idx as usize].prev_slot = tail;
                self.hot_orders[slot_idx as usize].next_slot = NULL_SLOT;
                if tail != NULL_SLOT {
                    self.hot_orders[tail as usize].next_slot = slot_idx;
                } else {
                    levels[l_idx].head_slot = slot_idx;
                }
                levels[l_idx].tail_slot = slot_idx;
                levels[l_idx].total_qty_fp =
                    levels[l_idx].total_qty_fp.saturating_add(remaining_qty_fp);
                levels[l_idx].order_count += 1;
            }
            Err(ins_idx) => {
                levels.insert(
                    ins_idx,
                    PriceLevelHead {
                        price_fp,
                        total_qty_fp: remaining_qty_fp,
                        head_slot: slot_idx,
                        tail_slot: slot_idx,
                        order_count: 1,
                        _pad: 0,
                    },
                );
            }
        }
        slot_idx
    }

    pub fn remove_resting_order_by_id(
        &mut self,
        order_id: u64,
    ) -> Option<(HotOrderSlot, ColdOrderMeta)> {
        let slot_idx = self.order_index.remove(order_id)?;
        let hot = self.hot_orders[slot_idx as usize];
        let cold = self.cold_orders[slot_idx as usize];
        self.hot_orders[slot_idx as usize].active = 0;
        self.hot_orders[slot_idx as usize].remaining_qty_fp = 0;

        if let Some(b_idx) = self
            .books
            .iter()
            .position(|b| b.spec.symbol == cold.symbol.as_str())
        {
            let levels = if hot.side == 1 {
                &mut self.books[b_idx].bids
            } else {
                &mut self.books[b_idx].asks
            };
            let search = if hot.side == 1 {
                levels.binary_search_by(|l| l.price_fp.cmp(&hot.price_fp))
            } else {
                levels.binary_search_by(|l| hot.price_fp.cmp(&l.price_fp))
            };
            if let Ok(l_idx) = search {
                if hot.prev_slot != NULL_SLOT {
                    self.hot_orders[hot.prev_slot as usize].next_slot = hot.next_slot;
                } else {
                    levels[l_idx].head_slot = hot.next_slot;
                }
                if hot.next_slot != NULL_SLOT {
                    self.hot_orders[hot.next_slot as usize].prev_slot = hot.prev_slot;
                } else {
                    levels[l_idx].tail_slot = hot.prev_slot;
                }
                levels[l_idx].total_qty_fp = levels[l_idx]
                    .total_qty_fp
                    .saturating_sub(hot.remaining_qty_fp);
                levels[l_idx].order_count = levels[l_idx].order_count.saturating_sub(1);
                if levels[l_idx].order_count == 0 || levels[l_idx].head_slot == NULL_SLOT {
                    levels.remove(l_idx);
                }
            }
        }
        self.free_slots.push(slot_idx);
        Some((hot, cold))
    }

    /// Applies a single `MarketEventEnvelope` delta event during fast-forward replay.
    /// Zero Blake3 hashing is performed per intermediate event; dirty accounts and active LOB
    /// orders are committed once at the target sequence `T` (`compute_state_root`).
    pub fn apply_wal_envelope(
        &mut self,
        env: &crate::pb::MarketEventEnvelope,
    ) -> Result<(), ChronosError> {
        if env.engine_seq > 0 {
            self.engine_seq = env.engine_seq;
        }
        if env.ts_ns > 0 {
            self.ts_ns = env.ts_ns;
        }

        match &env.payload {
            Some(EnvelopePayload::L3Delta(d)) => self.apply_l3_delta(d)?,
            Some(EnvelopePayload::Fill(f)) => self.apply_execution_fill_record(f)?,
            Some(EnvelopePayload::JournalEntry(j)) => self.apply_journal_entry(j)?,
            Some(EnvelopePayload::OrderAck(ack)) => {
                if let Some(rest) = ack.client_order_id.strip_prefix("RISK_ACTION:") {
                    let parts: Vec<&str> = rest.split(':').collect();
                    if parts.len() == 3 {
                        let acc_id: u64 = parts[0].parse().unwrap_or(0);
                        let st_u8: i32 = parts[1].parse().unwrap_or(0);
                        if acc_id != 0 && st_u8 != 0 {
                            let idx = self.ensure_account(acc_id);
                            self.accounts[idx as usize].status = st_u8;
                        }
                    }
                }
                for f in &ack.fills {
                    self.apply_execution_fill_record(f)?;
                }
            }
            Some(EnvelopePayload::L2Snapshot(snap)) => {
                let _ = self.ensure_book(&snap.symbol);
            }
            None => {}
        }
        Ok(())
    }

    fn apply_l3_delta(&mut self, d: &L3BookDelta) -> Result<(), ChronosError> {
        let sym = if d.symbol.is_empty() {
            "BTC-USDT"
        } else {
            d.symbol.as_str()
        };
        let side_u8 = if d.side == OrderSide::Sell as i32 {
            2
        } else {
            1
        };
        let op = L3DeltaOp::try_from(d.op).unwrap_or(L3DeltaOp::Unspecified);

        match op {
            L3DeltaOp::Add => {
                if !self.fee_schedule_known {
                    if let Some((m_ppm, t_ppm)) = extract_fee_ppm_from_trace(d.trace.as_ref()) {
                        self.maker_fee_ppm = m_ppm;
                        self.taker_fee_ppm = t_ppm;
                        self.fee_schedule_known = true;
                    }
                }
                if d.remaining_qty_fp > 0 {
                    let locked_col = if side_u8 == 1 {
                        checked_buyer_reserve_fp(
                            d.price_fp,
                            d.remaining_qty_fp,
                            self.maker_fee_ppm,
                            self.taker_fee_ppm,
                        )
                        .unwrap_or(0)
                    } else {
                        d.remaining_qty_fp as i128
                    };
                    self.insert_resting_order_raw(
                        d.order_id,
                        "",
                        d.instrument_id,
                        sym,
                        d.account_id,
                        side_u8,
                        d.price_fp,
                        d.remaining_qty_fp,
                        locked_col,
                        d.engine_seq,
                        d.event_ts_ns,
                    );
                }
            }
            L3DeltaOp::Fill => {
                if d.remaining_qty_fp == 0 {
                    self.remove_resting_order_by_id(d.order_id);
                } else if let Some(slot_idx) = self.order_index.get(d.order_id) {
                    let old_qty = self.hot_orders[slot_idx as usize].remaining_qty_fp;
                    let price_fp = self.hot_orders[slot_idx as usize].price_fp;
                    let side = self.hot_orders[slot_idx as usize].side;
                    self.hot_orders[slot_idx as usize].remaining_qty_fp = d.remaining_qty_fp;
                    self.cold_orders[slot_idx as usize].locked_collateral_fp = if side == 1 {
                        checked_buyer_reserve_fp(
                            price_fp,
                            d.remaining_qty_fp,
                            self.maker_fee_ppm,
                            self.taker_fee_ppm,
                        )
                        .unwrap_or(0)
                    } else {
                        d.remaining_qty_fp as i128
                    };

                    let b_idx = self.ensure_book(sym);
                    let levels = if side == 1 {
                        &mut self.books[b_idx].bids
                    } else {
                        &mut self.books[b_idx].asks
                    };
                    if let Ok(l_idx) = if side == 1 {
                        levels.binary_search_by(|l| l.price_fp.cmp(&price_fp))
                    } else {
                        levels.binary_search_by(|l| price_fp.cmp(&l.price_fp))
                    } {
                        if d.remaining_qty_fp < old_qty {
                            levels[l_idx].total_qty_fp = levels[l_idx]
                                .total_qty_fp
                                .saturating_sub(old_qty - d.remaining_qty_fp);
                        } else {
                            levels[l_idx].total_qty_fp = levels[l_idx]
                                .total_qty_fp
                                .saturating_add(d.remaining_qty_fp - old_qty);
                        }
                    }
                }
            }
            L3DeltaOp::Modify => {
                if d.remaining_qty_fp == 0 {
                    self.remove_resting_order_by_id(d.order_id);
                } else if let Some(slot_idx) = self.order_index.get(d.order_id) {
                    let cur = self.hot_orders[slot_idx as usize];
                    if cur.price_fp == d.price_fp && d.remaining_qty_fp <= cur.remaining_qty_fp {
                        // Size reduction preserves FIFO time priority!
                        let diff = cur.remaining_qty_fp - d.remaining_qty_fp;
                        self.hot_orders[slot_idx as usize].remaining_qty_fp = d.remaining_qty_fp;
                        let b_idx = self.ensure_book(sym);
                        let levels = if cur.side == 1 {
                            &mut self.books[b_idx].bids
                        } else {
                            &mut self.books[b_idx].asks
                        };
                        if let Ok(l_idx) = if cur.side == 1 {
                            levels.binary_search_by(|l| l.price_fp.cmp(&cur.price_fp))
                        } else {
                            levels.binary_search_by(|l| cur.price_fp.cmp(&l.price_fp))
                        } {
                            levels[l_idx].total_qty_fp =
                                levels[l_idx].total_qty_fp.saturating_sub(diff);
                        }
                    } else {
                        // Price change or size increase loses time priority -> re-insert at tail with new priority_seq.
                        let (_, cold) = self
                            .remove_resting_order_by_id(d.order_id)
                            .unwrap_or((cur, ColdOrderMeta::default()));
                        self.insert_resting_order_raw(
                            d.order_id,
                            cold.client_order_id.as_str(),
                            d.instrument_id,
                            sym,
                            d.account_id,
                            side_u8,
                            d.price_fp,
                            d.remaining_qty_fp,
                            cold.locked_collateral_fp,
                            d.engine_seq,
                            d.event_ts_ns,
                        );
                    }
                } else {
                    self.insert_resting_order_raw(
                        d.order_id,
                        "",
                        d.instrument_id,
                        sym,
                        d.account_id,
                        side_u8,
                        d.price_fp,
                        d.remaining_qty_fp,
                        0,
                        d.engine_seq,
                        d.event_ts_ns,
                    );
                }
            }
            L3DeltaOp::Cancel => {
                self.remove_resting_order_by_id(d.order_id);
            }
            L3DeltaOp::Clear => {
                let order_ids: Vec<u64> = self
                    .hot_orders
                    .iter()
                    .enumerate()
                    .filter(|(idx, s)| {
                        s.active != 0 && self.cold_orders[*idx].symbol.as_str() == sym
                    })
                    .map(|(_, s)| s.order_id)
                    .collect();
                for oid in order_ids {
                    self.remove_resting_order_by_id(oid);
                }
            }
            L3DeltaOp::Unspecified => {}
        }
        Ok(())
    }

    fn apply_execution_fill_record(&mut self, f: &ExecutionFill) -> Result<(), ChronosError> {
        // Deduplicate if already processed via both Fill envelope and OrderAck.fills
        if f.fill_id > 0 && f.fill_id < self.next_fill_id {
            return Ok(());
        }
        let sym = if f.symbol.is_empty() {
            "BTC-USDT"
        } else {
            f.symbol.as_str()
        };
        let b_idx = self.ensure_book(sym);
        let inst_id = if f.instrument_id != 0 {
            f.instrument_id
        } else {
            self.books[b_idx].spec.instrument_id
        };
        let notional = {
            let n = pb_to_u128(f.notional_fp);
            if n > 0 {
                n
            } else {
                checked_notional_fp(f.price_fp, f.qty_fp)?
            }
        };

        // Extract exact fee schedule from trace tag (once, 0 allocs) or infer via nearest-integer rounding
        if !self.fee_schedule_known {
            if let Some((m, t)) = extract_fee_ppm_from_trace(f.trace.as_ref()) {
                self.maker_fee_ppm = m;
                self.taker_fee_ppm = t;
                self.fee_schedule_known = true;
            } else if notional >= 100_000_000 {
                let n_i128 = notional as i128;
                let half = n_i128 / 2;
                let num_m = (f.maker_fee_fp as i128) * 1_000_000;
                let num_t = (f.taker_fee_fp as i128) * 1_000_000;
                let inferred_maker = if num_m >= 0 {
                    (num_m + half) / n_i128
                } else {
                    (num_m - half) / n_i128
                };
                let inferred_taker = if num_t >= 0 {
                    (num_t + half) / n_i128
                } else {
                    (num_t - half) / n_i128
                };
                if let (Ok(m), Ok(t)) =
                    (i64::try_from(inferred_maker), i64::try_from(inferred_taker))
                {
                    self.maker_fee_ppm = m;
                    self.taker_fee_ppm = t;
                }
            }
        }

        let prev_px = self.books[b_idx].last_clearing_price_fp;
        if prev_px > 0 {
            let is_buy = f.aggressor_side != OrderSide::Sell as i32;
            let slip = compute_slippage_bps(prev_px, f.price_fp, is_buy);
            if slip > self.window_max_slippage_bps {
                self.window_max_slippage_bps = slip;
            }
        }
        self.books[b_idx].last_clearing_price_fp = f.price_fp;
        self.books[b_idx].cumulative_base_vol_fp = self.books[b_idx]
            .cumulative_base_vol_fp
            .saturating_add(f.qty_fp as u128);
        self.books[b_idx].cumulative_quote_vol_fp = self.books[b_idx]
            .cumulative_quote_vol_fp
            .saturating_add(notional);

        self.window_fill_prices.push(f.price_fp);
        if self.track_window_fills {
            self.window_fills.push(f.clone());
        }

        let agg_side = if f.aggressor_side == OrderSide::Sell as i32 {
            2u8
        } else {
            1u8
        };
        let fill_id = if f.fill_id > 0 {
            f.fill_id
        } else {
            self.next_fill_id
        };
        self.next_fill_id = fill_id.saturating_add(1);

        let leaf = hash_execution_fill_leaf(&ExecutionFillDigestInput {
            fill_id,
            engine_seq: if f.engine_seq > 0 {
                f.engine_seq
            } else {
                self.engine_seq
            },
            instrument_id: inst_id,
            maker_order_id: f.maker_order_id,
            taker_order_id: f.taker_order_id,
            maker_account_id: f.maker_account_id,
            taker_account_id: f.taker_account_id,
            aggressor_side: agg_side,
            price_fp: f.price_fp,
            qty_fp: f.qty_fp,
            notional_fp: notional,
            maker_fee_fp: f.maker_fee_fp,
            taker_fee_fp: f.taker_fee_fp,
            exec_ts_ns: f.exec_ts_ns,
        });
        self.merkle.append_execution_leaf(leaf);
        Ok(())
    }

    fn apply_journal_entry(&mut self, j: &LedgerJournalEntry) -> Result<(), ChronosError> {
        let is_deposit = j.reference_type.eq_ignore_ascii_case("DEPOSIT");
        let has_vault_leg = j.lines.iter().any(|l| l.sub_account == "EXCHANGE_VAULT");
        for line in &j.lines {
            let asset = AssetSymbol::from_str_trunc(&line.asset);
            let after_fp = pb_to_i128(line.balance_after_fp);
            let debit_fp = pb_to_i128(line.debit_fp);
            let credit_fp = pb_to_i128(line.credit_fp);

            match line.sub_account.as_str() {
                "EXCHANGE_VAULT" => {
                    let pool = self.ensure_asset_pool_mut(asset);
                    if line.balance_after_fp.is_some() {
                        pool.total_deposits_fp = after_fp.max(0) as u128;
                    } else if debit_fp > credit_fp {
                        pool.total_deposits_fp = pool
                            .total_deposits_fp
                            .saturating_add((debit_fp - credit_fp) as u128);
                    }
                }
                "AVAILABLE" => {
                    let acc_idx = self.ensure_account(line.account_id);
                    let entry = self.accounts[acc_idx as usize].get_or_insert_asset_mut(asset)?;
                    if line.balance_after_fp.is_some() {
                        entry.available_fp = after_fp;
                    } else {
                        entry.available_fp = entry
                            .available_fp
                            .saturating_add(credit_fp)
                            .saturating_sub(debit_fp);
                    }
                    self.mark_account_dirty(acc_idx);
                    if is_deposit && !has_vault_leg && credit_fp > debit_fp {
                        let net = (credit_fp - debit_fp) as u128;
                        let pool = self.ensure_asset_pool_mut(asset);
                        pool.total_deposits_fp = pool.total_deposits_fp.saturating_add(net);
                    }
                }
                "LOCKED_MARGIN" => {
                    let acc_idx = self.ensure_account(line.account_id);
                    let entry = self.accounts[acc_idx as usize].get_or_insert_asset_mut(asset)?;
                    if line.balance_after_fp.is_some() {
                        entry.locked_fp = after_fp;
                    } else {
                        entry.locked_fp = entry
                            .locked_fp
                            .saturating_add(credit_fp)
                            .saturating_sub(debit_fp);
                    }
                    self.mark_account_dirty(acc_idx);
                }
                "TREASURY_FEE" => {
                    let new_treasury = {
                        let pool = self.ensure_asset_pool_mut(asset);
                        if line.balance_after_fp.is_some() {
                            pool.treasury_fee_pool_fp = after_fp;
                        } else {
                            pool.treasury_fee_pool_fp = pool
                                .treasury_fee_pool_fp
                                .saturating_add(credit_fp)
                                .saturating_sub(debit_fp);
                        }
                        pool.treasury_fee_pool_fp
                    };
                    if new_treasury != 0 || self.account_index.get(0).is_some() {
                        let t_idx = self.ensure_account(0);
                        let t_entry =
                            self.accounts[t_idx as usize].get_or_insert_asset_mut(asset)?;
                        t_entry.available_fp = new_treasury;
                        self.mark_account_dirty(t_idx);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    // =========================================================================
    // DIRECT LEDGER OPERATIONS (USED BY SCENARIO BUILDER & SANDBOX MATCHER)
    // =========================================================================

    pub fn credit_deposit(
        &mut self,
        account_id: u64,
        asset_str: &str,
        amount_fp: u128,
    ) -> Result<(), ChronosError> {
        if amount_fp == 0 || amount_fp > i128::MAX as u128 {
            return Err(ChronosError::InvalidArgument(
                "deposit amount out of valid positive i128 range".to_string(),
            ));
        }
        let asset = AssetSymbol::from_str_trunc(asset_str);
        let acc_idx = self.ensure_account(account_id);
        let bal = self.accounts[acc_idx as usize].get_or_insert_asset_mut(asset)?;
        bal.available_fp = bal
            .available_fp
            .checked_add(amount_fp as i128)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
        self.mark_account_dirty(acc_idx);

        let pool = self.ensure_asset_pool_mut(asset);
        pool.total_deposits_fp = pool
            .total_deposits_fp
            .checked_add(amount_fp)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
        Ok(())
    }

    pub fn lock_funds(
        &mut self,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: i128,
    ) -> Result<bool, ChronosError> {
        if amount_fp <= 0 {
            return Ok(true);
        }
        let acc_idx = self.ensure_account(account_id);
        let bal = self.accounts[acc_idx as usize].get_or_insert_asset_mut(asset)?;
        if bal.available_fp < amount_fp {
            return Ok(false);
        }
        bal.available_fp -= amount_fp;
        bal.locked_fp = bal
            .locked_fp
            .checked_add(amount_fp)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
        self.mark_account_dirty(acc_idx);
        Ok(true)
    }

    pub fn unlock_funds(
        &mut self,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: i128,
    ) -> Result<(), ChronosError> {
        if amount_fp <= 0 {
            return Ok(());
        }
        let acc_idx = self.ensure_account(account_id);
        let bal = self.accounts[acc_idx as usize].get_or_insert_asset_mut(asset)?;
        let unlock = amount_fp.min(bal.locked_fp.max(0));
        bal.locked_fp -= unlock;
        bal.available_fp = bal
            .available_fp
            .checked_add(unlock)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
        self.mark_account_dirty(acc_idx);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn settle_trade_fill(
        &mut self,
        base_asset: AssetSymbol,
        quote_asset: AssetSymbol,
        buyer_account_id: u64,
        seller_account_id: u64,
        buyer_from_locked_quote: i128,
        seller_from_locked_base: i128,
        qty_fp: u64,
        notional_fp: u128,
        buyer_fee_fp: i64,
        seller_fee_fp: i64,
    ) -> Result<(), ChronosError> {
        let qty_i128 = qty_fp as i128;
        let notional_i128 = notional_fp as i128;
        let buyer_quote_cost = notional_i128
            .checked_add(buyer_fee_fp as i128)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
        let seller_quote_proceeds = notional_i128
            .checked_sub(seller_fee_fp as i128)
            .ok_or(ChronosError::Math(crate::fixed::MathError::Underflow))?;

        // 1. Buyer settlement: consume locked quote (or available if direct aggressive taker), credit base
        {
            let b_idx = self.ensure_account(buyer_account_id);
            {
                let q = self.accounts[b_idx as usize].get_or_insert_asset_mut(quote_asset)?;
                if buyer_from_locked_quote > 0 {
                    q.locked_fp -= buyer_from_locked_quote;
                    let refund_or_extra = buyer_from_locked_quote - buyer_quote_cost;
                    q.available_fp += refund_or_extra;
                } else {
                    q.available_fp -= buyer_quote_cost;
                }
            }
            {
                let b = self.accounts[b_idx as usize].get_or_insert_asset_mut(base_asset)?;
                b.available_fp += qty_i128;
            }
            self.mark_account_dirty(b_idx);
        }

        // 2. Seller settlement: consume locked base (or available if direct aggressive taker), credit quote proceeds
        {
            let s_idx = self.ensure_account(seller_account_id);
            {
                let b = self.accounts[s_idx as usize].get_or_insert_asset_mut(base_asset)?;
                if seller_from_locked_base > 0 {
                    b.locked_fp -= seller_from_locked_base;
                    let refund = seller_from_locked_base - qty_i128;
                    b.available_fp += refund;
                } else {
                    b.available_fp -= qty_i128;
                }
            }
            {
                let q = self.accounts[s_idx as usize].get_or_insert_asset_mut(quote_asset)?;
                q.available_fp += seller_quote_proceeds;
            }
            self.mark_account_dirty(s_idx);
        }

        // 3. Treasury fee pool settlement (exact double-entry conservation + CH02 Treasury account_id=0 leaf!)
        let net_treasury_fee = (buyer_fee_fp as i128) + (seller_fee_fp as i128);
        let new_treasury_pool = {
            let q_pool = self.ensure_asset_pool_mut(quote_asset);
            q_pool.treasury_fee_pool_fp = q_pool
                .treasury_fee_pool_fp
                .checked_add(net_treasury_fee)
                .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
            q_pool.treasury_fee_pool_fp
        };
        if net_treasury_fee != 0 || self.account_index.get(0).is_some() {
            let t_idx = self.ensure_account(0);
            let t_q = self.accounts[t_idx as usize].get_or_insert_asset_mut(quote_asset)?;
            t_q.available_fp = new_treasury_pool;
            self.mark_account_dirty(t_idx);
        }
        let _ = self.ensure_asset_pool_mut(base_asset);
        Ok(())
    }

    // =========================================================================
    // CRYPTOGRAPHIC STATE ROOT, L2/L3 SNAPSHOTS & CONSERVATION PROOFS
    // =========================================================================

    #[inline]
    pub fn build_lob_accumulator(&self) -> LobStateAccumulator {
        let mut acc = LobStateAccumulator::new();
        for slot in &self.hot_orders {
            if slot.active != 0 && slot.remaining_qty_fp > 0 {
                acc.add_lanes(LobStateAccumulator::order_state_lanes(
                    slot.instrument_id,
                    slot.order_id,
                    slot.account_id,
                    slot.side,
                    slot.price_fp,
                    slot.remaining_qty_fp,
                    slot.priority_seq,
                ));
            }
        }
        acc
    }

    pub fn flush_dirty_accounts_to_merkle(&mut self) {
        for &acc_idx in &self.dirty_account_indices {
            self.dirty_account_flags[acc_idx as usize] = false;
            let rec = &self.accounts[acc_idx as usize];
            let leaf = hash_account_leaf(rec.account_id, rec.sorted_balances());
            self.merkle.upsert_account_leaf(rec.account_id, leaf);
        }
        self.dirty_account_indices.clear();
    }

    pub fn compute_state_root(&mut self) -> [u8; 32] {
        self.flush_dirty_accounts_to_merkle();
        let lob_acc = self.build_lob_accumulator();
        self.merkle.commit_state_root(&lob_acc)
    }

    /// Verifies exact Double-Entry Conservation Law across all assets (`0` atomic tick drift).
    pub fn verify_double_entry_conservation(&self) -> Result<(), ChronosError> {
        for pool in &self.asset_pools {
            let mut sum_user_fp: i128 = 0;
            for acc in &self.accounts {
                if acc.account_id == 0 {
                    continue;
                }
                let (avail, locked) = acc.get_asset(pool.asset);
                sum_user_fp = sum_user_fp
                    .checked_add(avail)
                    .and_then(|v| v.checked_add(locked))
                    .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
            }
            let lhs = sum_user_fp
                .checked_add(pool.treasury_fee_pool_fp)
                .ok_or(ChronosError::Math(crate::fixed::MathError::Overflow))?;
            let rhs = pool.total_deposits_fp as i128;
            if lhs != rhs {
                return Err(ChronosError::InvariantViolation(format!(
                    "asset {} imbalance: user_equity({}) + treasury({}) = {} != deposits({})",
                    pool.asset, sum_user_fp, pool.treasury_fee_pool_fp, lhs, rhs
                )));
            }
        }
        Ok(())
    }

    /// Builds Protobuf `L2BookSnapshot` for `symbol` up to `depth` levels (0 = full book).
    pub fn build_l2_snapshot(&mut self, symbol: &str, depth: usize) -> L2BookSnapshot {
        let state_root = self.compute_state_root();
        let sym = if symbol.is_empty() {
            "BTC-USDT"
        } else {
            symbol
        };
        let b_idx = self.ensure_book(sym);
        let book = &self.books[b_idx];
        let max_levels = if depth == 0 { usize::MAX } else { depth };

        let bids: Vec<PriceLevel> = book
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|l| PriceLevel {
                price_fp: l.price_fp,
                total_qty_fp: l.total_qty_fp,
                order_count: l.order_count,
            })
            .collect();

        let asks: Vec<PriceLevel> = book
            .asks
            .iter()
            .rev()
            .take(max_levels)
            .map(|l| PriceLevel {
                price_fp: l.price_fp,
                total_qty_fp: l.total_qty_fp,
                order_count: l.order_count,
            })
            .collect();

        L2BookSnapshot {
            engine_seq: self.engine_seq,
            symbol: book.spec.symbol.clone(),
            instrument_id: book.spec.instrument_id,
            bids,
            asks,
            best_bid_fp: book.best_bid_fp(),
            best_ask_fp: book.best_ask_fp(),
            mid_price_fp: book.mid_price_fp(),
            spread_fp: book.spread_fp(),
            snapshot_ts_ns: self.ts_ns,
            state_root_blake3: state_root.to_vec(),
        }
    }

    /// Reconstructs full L3 individual resting orders for `symbol` in strict price-time priority.
    pub fn build_l3_resting_orders(&self, symbol: &str) -> Vec<SnapshotRestingOrder> {
        let sym = if symbol.is_empty() {
            "BTC-USDT"
        } else {
            symbol
        };
        let Some(book) = self.find_book(sym) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        // Best bids first, FIFO within each price tick
        for lvl in book.bids.iter().rev() {
            let mut cur = lvl.head_slot;
            while cur != NULL_SLOT {
                let hot = &self.hot_orders[cur as usize];
                let cold = &self.cold_orders[cur as usize];
                out.push(SnapshotRestingOrder {
                    order_id: hot.order_id,
                    client_order_id: cold.client_order_id.as_str().to_string(),
                    instrument_id: hot.instrument_id,
                    symbol: cold.symbol.as_str().to_string(),
                    account_id: hot.account_id,
                    side: hot.side,
                    price_fp: hot.price_fp,
                    remaining_qty_fp: hot.remaining_qty_fp,
                    locked_collateral_fp: cold.locked_collateral_fp,
                    priority_seq: hot.priority_seq,
                    ts_ns: hot.ts_ns,
                });
                cur = hot.next_slot;
            }
        }
        // Best asks first, FIFO within each price tick
        for lvl in book.asks.iter().rev() {
            let mut cur = lvl.head_slot;
            while cur != NULL_SLOT {
                let hot = &self.hot_orders[cur as usize];
                let cold = &self.cold_orders[cur as usize];
                out.push(SnapshotRestingOrder {
                    order_id: hot.order_id,
                    client_order_id: cold.client_order_id.as_str().to_string(),
                    instrument_id: hot.instrument_id,
                    symbol: cold.symbol.as_str().to_string(),
                    account_id: hot.account_id,
                    side: hot.side,
                    price_fp: hot.price_fp,
                    remaining_qty_fp: hot.remaining_qty_fp,
                    locked_collateral_fp: cold.locked_collateral_fp,
                    priority_seq: hot.priority_seq,
                    ts_ns: hot.ts_ns,
                });
                cur = hot.next_slot;
            }
        }
        out
    }

    /// Computes reconstructed liquidity & microstructure metrics at the current moment `T`.
    pub fn compute_liquidity_metrics(&self, symbol: &str) -> ReconstructedLiquidityMetrics {
        let sym = if symbol.is_empty() {
            "BTC-USDT"
        } else {
            symbol
        };
        let Some(book) = self.find_book(sym) else {
            return ReconstructedLiquidityMetrics::default();
        };
        let total_bid_depth_fp: u128 = book.bids.iter().map(|l| l.total_qty_fp as u128).sum();
        let total_ask_depth_fp: u128 = book.asks.iter().map(|l| l.total_qty_fp as u128).sum();

        let top5_bid: i128 = book
            .bids
            .iter()
            .rev()
            .take(5)
            .map(|l| l.total_qty_fp as i128)
            .sum();
        let top5_ask: i128 = book
            .asks
            .iter()
            .rev()
            .take(5)
            .map(|l| l.total_qty_fp as i128)
            .sum();
        let denom = top5_bid + top5_ask;
        let book_imbalance_top5_ppm = if denom > 0 {
            (((top5_bid - top5_ask) * 1_000_000) / denom) as i64
        } else {
            0
        };
        let vwap_fp = book
            .cumulative_quote_vol_fp
            .saturating_mul(100_000_000)
            .checked_div(book.cumulative_base_vol_fp)
            .map(|v| v as u64)
            .unwrap_or(book.last_clearing_price_fp);

        ReconstructedLiquidityMetrics {
            best_bid_fp: book.best_bid_fp(),
            best_ask_fp: book.best_ask_fp(),
            mid_price_fp: book.mid_price_fp(),
            spread_fp: book.spread_fp(),
            spread_bps: book.spread_bps(),
            total_bid_depth_fp,
            total_ask_depth_fp,
            book_imbalance_top5_ppm,
            last_clearing_price_fp: book.last_clearing_price_fp,
            vwap_fp,
        }
    }

    /// Generates `MerkleLedgerProof` records for all active accounts (sorted by `account_id`).
    pub fn build_all_account_merkle_proofs(
        &mut self,
        quote_asset_str: &str,
    ) -> Vec<MerkleLedgerProof> {
        self.flush_dirty_accounts_to_merkle();
        let lob_acc = self.build_lob_accumulator();
        let quote_sym = AssetSymbol::from_str_trunc(quote_asset_str);

        let (sys_deposits, sys_treasury) = self
            .asset_pools
            .iter()
            .find(|p| p.asset == quote_sym)
            .map(|p| (p.total_deposits_fp, p.treasury_fee_pool_fp))
            .unwrap_or((0, 0));

        let mut sys_equity: i128 = 0;
        let mut account_ids: Vec<u64> = self
            .accounts
            .iter()
            .filter(|a| a.account_id != 0)
            .map(|a| a.account_id)
            .collect();
        account_ids.sort_unstable();

        for acc in &self.accounts {
            if acc.account_id == 0 {
                continue;
            }
            let (av, lk) = acc.get_asset(quote_sym);
            sys_equity = sys_equity.saturating_add(av).saturating_add(lk);
        }
        let balanced = self.verify_double_entry_conservation().is_ok();

        let mut out = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            let Some(acc_idx) = self.account_index.get(account_id) else {
                continue;
            };
            let balances_pb: Vec<AccountAssetBalance> = self.accounts[acc_idx as usize]
                .sorted_balances()
                .iter()
                .map(|b| AccountAssetBalance {
                    asset: b.asset.as_str().to_string(),
                    available_fp: Some(i128_to_pb(b.available_fp)),
                    locked_fp: Some(i128_to_pb(b.locked_fp)),
                    total_fp: Some(i128_to_pb(b.total_fp())),
                })
                .collect();

            let (leaf_hash, steps, state_root) =
                self.merkle.generate_account_proof(account_id, &lob_acc);
            let proof_path = steps
                .into_iter()
                .map(|s| MerkleProofNode {
                    sibling_hash: s.sibling_hash.to_vec(),
                    is_left_sibling: s.is_left_sibling,
                    depth: s.depth,
                })
                .collect();

            out.push(MerkleLedgerProof {
                engine_seq: self.engine_seq,
                account_id,
                balances: balances_pb,
                leaf_blake3_hash: leaf_hash.to_vec(),
                state_root_blake3: state_root.to_vec(),
                proof_path,
                system_total_deposits_fp: Some(u128_to_pb(sys_deposits)),
                system_total_user_equity_fp: Some(u128_to_pb(sys_equity.max(0) as u128)),
                treasury_fee_pool_fp: Some(i128_to_pb(sys_treasury)),
                double_entry_balanced: balanced,
                proof_ts_ns: self.ts_ns,
            });
        }
        out
    }

    #[inline]
    pub fn window_realized_volatility_bps(&self) -> u32 {
        compute_realized_volatility_bps(&self.window_fill_prices)
    }

    #[inline]
    pub fn quote_treasury_fee_pool(&self, quote_asset_str: &str) -> i128 {
        let sym = AssetSymbol::from_str_trunc(quote_asset_str);
        self.asset_pools
            .iter()
            .find(|p| p.asset == sym)
            .map(|p| p.treasury_fee_pool_fp)
            .unwrap_or(0)
    }

    #[inline]
    pub fn can_lock_buyer_reserve(
        &self,
        account_id: u64,
        quote_asset: AssetSymbol,
        limit_price_fp: u64,
        qty_fp: u64,
        maker_fee_ppm: i64,
        taker_fee_ppm: i64,
    ) -> Result<i128, ChronosError> {
        let reserve =
            checked_buyer_reserve_fp(limit_price_fp, qty_fp, maker_fee_ppm, taker_fee_ppm)?;
        let Some(acc_idx) = self.account_index.get(account_id) else {
            return Ok(-1);
        };
        let (avail, _) = self.accounts[acc_idx as usize].get_asset(quote_asset);
        if avail >= reserve {
            Ok(reserve)
        } else {
            Ok(-1)
        }
    }

    #[inline]
    pub fn compute_fill_fees(
        &self,
        notional_fp: u128,
        maker_fee_ppm: i64,
        taker_fee_ppm: i64,
    ) -> Result<(i64, i64), ChronosError> {
        Ok((
            checked_fee_fp(notional_fp, maker_fee_ppm)?,
            checked_fee_fp(notional_fp, taker_fee_ppm)?,
        ))
    }

    #[inline]
    pub fn is_book_uncrossed(&self, symbol: &str) -> bool {
        let Some(book) = self.find_book(symbol) else {
            return true;
        };
        let bid = book.best_bid_fp();
        let ask = book.best_ask_fp();
        if bid == 0 || ask == 0 {
            true
        } else {
            bid < ask
        }
    }
}

impl Default for ExchangeState {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderingHelper for HotOrderSlot {}
trait OrderingHelper: Sized {
    #[allow(dead_code)]
    fn cmp_seq(a: u64, b: u64) -> Ordering {
        a.cmp(&b)
    }
}

/// Zero-allocation byte-slice scanner extracting `(maker_fee_ppm, taker_fee_ppm)` from `chronos-cmd:` trace tags.
#[inline]
fn extract_fee_ppm_from_trace(trace: Option<&crate::pb::TraceMetadata>) -> Option<(i64, i64)> {
    let raw = trace?.source_service.strip_prefix("chronos-cmd:")?;
    let m = parse_json_i64_field(raw, "\"maker_fee_ppm\":")?;
    let t = parse_json_i64_field(raw, "\"taker_fee_ppm\":")?;
    Some((m, t))
}

#[inline]
fn parse_json_i64_field(haystack: &str, needle: &str) -> Option<i64> {
    let pos = haystack.find(needle)?;
    let bytes = &haystack.as_bytes()[pos + needle.len()..];
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let neg = if bytes[i] == b'-' {
        i += 1;
        true
    } else {
        false
    };
    let start = i;
    let mut acc: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        acc = acc.checked_mul(10)?.checked_add((bytes[i] - b'0') as i64)?;
        i += 1;
    }
    if i == start {
        return None;
    }
    Some(if neg { -acc } else { acc })
}
