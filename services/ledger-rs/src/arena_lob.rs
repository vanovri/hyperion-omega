#![deny(clippy::float_arithmetic)]

use crate::fixed::{AssetSymbol, ClientOrderId, InstrumentSymbol};
use crate::merkle::LobStateAccumulator;

pub const NULL_IDX: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Side {
    Buy = 1,
    Sell = 2,
}

impl Side {
    #[inline(always)]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }

    #[inline(always)]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Buy),
            2 => Some(Self::Sell),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OrderKind {
    Limit = 1,
    Market = 2,
    Ioc = 3,
    Fok = 4,
    PostOnly = 5,
}

impl OrderKind {
    #[inline(always)]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Limit),
            2 => Some(Self::Market),
            3 => Some(Self::Ioc),
            4 => Some(Self::Fok),
            5 => Some(Self::PostOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StpPolicy {
    None = 1,
    CancelResting = 2,
    CancelAggressing = 3,
    CancelBoth = 4,
}

impl StpPolicy {
    #[inline(always)]
    pub const fn from_u8(v: u8) -> Self {
        match v {
            2 => Self::CancelResting,
            3 => Self::CancelAggressing,
            4 => Self::CancelBoth,
            _ => Self::None,
        }
    }
}

/// Zero-allocation open-addressed u64 -> u32 hash map with backward-shift deletion (zero tombstones).
#[derive(Clone)]
pub struct FixedU64Map {
    keys: Box<[u64]>,
    vals: Box<[u32]>,
    occupied: Box<[bool]>,
    mask: usize,
    len: usize,
    max_load: usize,
}

impl FixedU64Map {
    pub fn with_capacity(min_entries: usize) -> Self {
        let cap = (min_entries.saturating_mul(2)).max(16).next_power_of_two();
        Self {
            keys: vec![0u64; cap].into_boxed_slice(),
            vals: vec![NULL_IDX; cap].into_boxed_slice(),
            occupied: vec![false; cap].into_boxed_slice(),
            mask: cap - 1,
            len: 0,
            max_load: (cap * 3) / 4,
        }
    }

    #[inline(always)]
    fn hash_idx(&self, key: u64) -> usize {
        let h = key.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        ((h ^ (h >> 32)) as usize) & self.mask
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.occupied.fill(false);
        self.len = 0;
    }

    #[inline(always)]
    pub fn get(&self, key: u64) -> Option<u32> {
        let mut idx = self.hash_idx(key);
        loop {
            if !self.occupied[idx] {
                return None;
            }
            if self.keys[idx] == key {
                return Some(self.vals[idx]);
            }
            idx = (idx + 1) & self.mask;
        }
    }

    #[inline(always)]
    pub fn insert(&mut self, key: u64, val: u32) -> Result<Option<u32>, &'static str> {
        let mut idx = self.hash_idx(key);
        loop {
            if !self.occupied[idx] {
                if self.len >= self.max_load {
                    return Err("FixedU64Map capacity exhausted");
                }
                self.occupied[idx] = true;
                self.keys[idx] = key;
                self.vals[idx] = val;
                self.len += 1;
                return Ok(None);
            }
            if self.keys[idx] == key {
                let prev = self.vals[idx];
                self.vals[idx] = val;
                return Ok(Some(prev));
            }
            idx = (idx + 1) & self.mask;
        }
    }

    #[inline(always)]
    pub fn remove(&mut self, key: u64) -> Option<u32> {
        let mut idx = self.hash_idx(key);
        loop {
            if !self.occupied[idx] {
                return None;
            }
            if self.keys[idx] == key {
                break;
            }
            idx = (idx + 1) & self.mask;
        }
        let removed_val = self.vals[idx];
        self.len -= 1;

        let mut empty = idx;
        let mut scan = (idx + 1) & self.mask;
        loop {
            if !self.occupied[scan] {
                self.occupied[empty] = false;
                return Some(removed_val);
            }
            let natural = self.hash_idx(self.keys[scan]);
            let dist_scan = scan.wrapping_sub(natural) & self.mask;
            let dist_empty = scan.wrapping_sub(empty) & self.mask;
            if dist_scan >= dist_empty {
                self.keys[empty] = self.keys[scan];
                self.vals[empty] = self.vals[scan];
                empty = scan;
            }
            scan = (scan + 1) & self.mask;
        }
    }
}

/// Exact 64-byte L1-cache-line-aligned hot order node traversed during crossing sweeps, FOK pre-flight
/// liquidity checks, and STP matching (`1` cache line per resting order).
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct HotOrderNode {
    pub order_id: u64,
    pub account_id: u64,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub priority_seq: u64,
    pub instrument_id: u32,
    pub level_idx: u32,
    pub prev_in_level: u32,
    pub next_in_level: u32,
    pub side: Side,
    pub kind: OrderKind,
    pub stp_mode: StpPolicy,
    pub occupied: bool,
    pub _pad: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<HotOrderNode>() == 64);

/// Cold order metadata indexed by the same `slot_idx` (`OrderArena` SoA split), touched only on
/// settlement/cancel/snapshot instead of polluting L1 cache lines during inner queue scans.
#[derive(Clone, Copy, Default)]
pub struct ColdOrderMeta {
    pub locked_quote_fp: i128,
    pub initial_qty_fp: u64,
    pub created_ts_ns: u64,
    pub lt_lanes: [u64; 4],
    pub client_order_id: ClientOrderId,
}

/// Logical combined order view used at API / snapshot boundaries.
#[derive(Clone, Copy, Debug)]
pub struct OrderRecord {
    pub order_id: u64,
    pub account_id: u64,
    pub instrument_id: u32,
    pub side: Side,
    pub kind: OrderKind,
    pub stp_mode: StpPolicy,
    pub price_fp: u64,
    pub initial_qty_fp: u64,
    pub remaining_qty_fp: u64,
    pub locked_quote_fp: i128,
    pub created_ts_ns: u64,
    pub priority_seq: u64,
    pub client_order_id: ClientOrderId,
}

impl Default for HotOrderNode {
    fn default() -> Self {
        Self {
            order_id: 0,
            account_id: 0,
            price_fp: 0,
            remaining_qty_fp: 0,
            priority_seq: 0,
            instrument_id: 0,
            level_idx: NULL_IDX,
            prev_in_level: NULL_IDX,
            next_in_level: NULL_IDX,
            side: Side::Buy,
            kind: OrderKind::Limit,
            stp_mode: StpPolicy::None,
            occupied: false,
            _pad: [0u8; 4],
        }
    }
}

#[derive(Clone)]
pub struct OrderArena {
    hot: Box<[HotOrderNode]>,
    cold: Box<[ColdOrderMeta]>,
    free_head: u32,
    active_len: u32,
}

impl OrderArena {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0 && capacity < (NULL_IDX as usize));
        let mut hot = vec![HotOrderNode::default(); capacity].into_boxed_slice();
        let cold = vec![ColdOrderMeta::default(); capacity].into_boxed_slice();
        for (i, node) in hot.iter_mut().enumerate() {
            node.next_in_level = if i + 1 < capacity {
                (i + 1) as u32
            } else {
                NULL_IDX
            };
        }
        Self {
            hot,
            cold,
            free_head: 0,
            active_len: 0,
        }
    }

    pub fn clear(&mut self) {
        let cap = self.hot.len();
        for (i, node) in self.hot.iter_mut().enumerate() {
            node.occupied = false;
            node.next_in_level = if i + 1 < cap {
                (i + 1) as u32
            } else {
                NULL_IDX
            };
        }
        self.free_head = 0;
        self.active_len = 0;
    }

    #[inline(always)]
    pub fn alloc(
        &mut self,
        mut hot_node: HotOrderNode,
        cold_meta: ColdOrderMeta,
    ) -> Result<u32, &'static str> {
        let idx = self.free_head;
        if idx == NULL_IDX {
            return Err("OrderArena pre-allocated slab exhausted");
        }
        self.free_head = self.hot[idx as usize].next_in_level;
        hot_node.occupied = true;
        hot_node.prev_in_level = NULL_IDX;
        hot_node.next_in_level = NULL_IDX;
        self.hot[idx as usize] = hot_node;
        self.cold[idx as usize] = cold_meta;
        self.active_len += 1;
        Ok(idx)
    }

    #[inline(always)]
    pub fn free(&mut self, idx: u32) {
        let h = &mut self.hot[idx as usize];
        debug_assert!(h.occupied);
        h.occupied = false;
        h.level_idx = NULL_IDX;
        h.prev_in_level = NULL_IDX;
        h.next_in_level = self.free_head;
        self.free_head = idx;
        self.active_len -= 1;
    }

    #[inline(always)]
    pub fn hot(&self, idx: u32) -> &HotOrderNode {
        &self.hot[idx as usize]
    }

    #[inline(always)]
    pub fn hot_mut(&mut self, idx: u32) -> &mut HotOrderNode {
        &mut self.hot[idx as usize]
    }

    #[inline(always)]
    pub fn cold(&self, idx: u32) -> &ColdOrderMeta {
        &self.cold[idx as usize]
    }

    #[inline(always)]
    pub fn cold_mut(&mut self, idx: u32) -> &mut ColdOrderMeta {
        &mut self.cold[idx as usize]
    }

    #[inline]
    pub fn record(&self, idx: u32) -> OrderRecord {
        let h = &self.hot[idx as usize];
        let c = &self.cold[idx as usize];
        OrderRecord {
            order_id: h.order_id,
            account_id: h.account_id,
            instrument_id: h.instrument_id,
            side: h.side,
            kind: h.kind,
            stp_mode: h.stp_mode,
            price_fp: h.price_fp,
            initial_qty_fp: c.initial_qty_fp,
            remaining_qty_fp: h.remaining_qty_fp,
            locked_quote_fp: c.locked_quote_fp,
            created_ts_ns: c.created_ts_ns,
            priority_seq: h.priority_seq,
            client_order_id: c.client_order_id,
        }
    }

    #[inline(always)]
    pub const fn active_len(&self) -> u32 {
        self.active_len
    }
}

/// Exact 64-byte L1-cache-line-aligned Price Level node combining:
/// 1. Intrusive FIFO doubly-linked order queue (`head_order`, `tail_order`)
/// 2. Intrusive Andersson AA-Tree balanced BST (`left`, `right`, `aa_level`)
/// 3. Threaded sorted doubly-linked list (`better_level`, `worse_level`)
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct PriceLevelNode {
    pub total_qty_fp: u128,
    pub price_fp: u64,
    pub order_count: u32,
    pub head_order: u32,
    pub tail_order: u32,
    pub left: u32,
    pub right: u32,
    pub aa_level: u32,
    pub better_level: u32,
    pub worse_level: u32,
    pub occupied: bool,
    pub _pad: [u8; 7],
}

const _: () = assert!(std::mem::size_of::<PriceLevelNode>() == 64);

impl Default for PriceLevelNode {
    fn default() -> Self {
        Self {
            total_qty_fp: 0,
            price_fp: 0,
            order_count: 0,
            head_order: NULL_IDX,
            tail_order: NULL_IDX,
            left: NULL_IDX,
            right: NULL_IDX,
            aa_level: 0,
            better_level: NULL_IDX,
            worse_level: NULL_IDX,
            occupied: false,
            _pad: [0u8; 7],
        }
    }
}

#[derive(Clone)]
pub struct PriceSideBook {
    is_bid: bool,
    nodes: Box<[PriceLevelNode]>,
    price_index: FixedU64Map,
    free_head: u32,
    root: u32,
    best_idx: u32,
    active_levels: u32,
}

impl PriceSideBook {
    pub fn with_capacity(is_bid: bool, capacity: usize) -> Self {
        assert!(capacity > 0 && capacity < (NULL_IDX as usize));
        let mut nodes = vec![PriceLevelNode::default(); capacity].into_boxed_slice();
        for (i, n) in nodes.iter_mut().enumerate() {
            n.worse_level = if i + 1 < capacity {
                (i + 1) as u32
            } else {
                NULL_IDX
            };
        }
        Self {
            is_bid,
            nodes,
            price_index: FixedU64Map::with_capacity(capacity),
            free_head: 0,
            root: NULL_IDX,
            best_idx: NULL_IDX,
            active_levels: 0,
        }
    }

    pub fn clear(&mut self) {
        let cap = self.nodes.len();
        for (i, n) in self.nodes.iter_mut().enumerate() {
            *n = PriceLevelNode::default();
            n.worse_level = if i + 1 < cap {
                (i + 1) as u32
            } else {
                NULL_IDX
            };
        }
        self.price_index.clear();
        self.free_head = 0;
        self.root = NULL_IDX;
        self.best_idx = NULL_IDX;
        self.active_levels = 0;
    }

    #[inline(always)]
    pub const fn best_level_idx(&self) -> u32 {
        self.best_idx
    }

    #[inline(always)]
    pub fn best_price_fp(&self) -> Option<u64> {
        if self.best_idx == NULL_IDX {
            None
        } else {
            Some(self.nodes[self.best_idx as usize].price_fp)
        }
    }

    #[inline(always)]
    pub const fn active_levels(&self) -> u32 {
        self.active_levels
    }

    #[inline(always)]
    pub fn get(&self, idx: u32) -> &PriceLevelNode {
        &self.nodes[idx as usize]
    }

    #[inline(always)]
    pub fn get_mut(&mut self, idx: u32) -> &mut PriceLevelNode {
        &mut self.nodes[idx as usize]
    }

    #[inline(always)]
    fn cmp_priority(&self, a_price: u64, b_price: u64) -> std::cmp::Ordering {
        if self.is_bid {
            b_price.cmp(&a_price)
        } else {
            a_price.cmp(&b_price)
        }
    }

    #[inline(always)]
    fn node_level(&self, idx: u32) -> u32 {
        if idx == NULL_IDX {
            0
        } else {
            self.nodes[idx as usize].aa_level
        }
    }

    #[inline(always)]
    fn skew(&mut self, t: u32) -> u32 {
        if t == NULL_IDX {
            return NULL_IDX;
        }
        let left = self.nodes[t as usize].left;
        if left != NULL_IDX && self.nodes[left as usize].aa_level == self.nodes[t as usize].aa_level
        {
            let left_right = self.nodes[left as usize].right;
            self.nodes[t as usize].left = left_right;
            self.nodes[left as usize].right = t;
            left
        } else {
            t
        }
    }

    #[inline(always)]
    fn split(&mut self, t: u32) -> u32 {
        if t == NULL_IDX {
            return NULL_IDX;
        }
        let right = self.nodes[t as usize].right;
        if right == NULL_IDX {
            return t;
        }
        let right_right = self.nodes[right as usize].right;
        if right_right != NULL_IDX
            && self.nodes[t as usize].aa_level == self.nodes[right_right as usize].aa_level
        {
            let right_left = self.nodes[right as usize].left;
            self.nodes[t as usize].right = right_left;
            self.nodes[right as usize].left = t;
            self.nodes[right as usize].aa_level += 1;
            right
        } else {
            t
        }
    }

    #[inline(always)]
    pub fn find_level(&self, price_fp: u64) -> u32 {
        if self.best_idx != NULL_IDX && self.nodes[self.best_idx as usize].price_fp == price_fp {
            return self.best_idx;
        }
        self.price_index.get(price_fp).unwrap_or(NULL_IDX)
    }

    pub fn get_or_insert_level(&mut self, price_fp: u64) -> Result<u32, &'static str> {
        if self.best_idx != NULL_IDX && self.nodes[self.best_idx as usize].price_fp == price_fp {
            return Ok(self.best_idx);
        }
        if let Some(existing_idx) = self.price_index.get(price_fp) {
            return Ok(existing_idx);
        }
        let mut cur = self.root;
        let mut last_better = NULL_IDX;
        let mut last_worse = NULL_IDX;
        while cur != NULL_IDX {
            let node_price = self.nodes[cur as usize].price_fp;
            match self.cmp_priority(price_fp, node_price) {
                std::cmp::Ordering::Equal => return Ok(cur),
                std::cmp::Ordering::Less => {
                    last_worse = cur;
                    cur = self.nodes[cur as usize].left;
                }
                std::cmp::Ordering::Greater => {
                    last_better = cur;
                    cur = self.nodes[cur as usize].right;
                }
            }
        }

        let new_idx = self.free_head;
        if new_idx == NULL_IDX {
            return Err("PriceSideBook pre-allocated level slab exhausted");
        }
        self.price_index.insert(price_fp, new_idx)?;
        self.free_head = self.nodes[new_idx as usize].worse_level;
        self.nodes[new_idx as usize] = PriceLevelNode {
            total_qty_fp: 0,
            price_fp,
            order_count: 0,
            head_order: NULL_IDX,
            tail_order: NULL_IDX,
            left: NULL_IDX,
            right: NULL_IDX,
            aa_level: 1,
            better_level: last_better,
            worse_level: last_worse,
            occupied: true,
            _pad: [0u8; 7],
        };
        if last_better != NULL_IDX {
            self.nodes[last_better as usize].worse_level = new_idx;
        } else {
            self.best_idx = new_idx;
        }
        if last_worse != NULL_IDX {
            self.nodes[last_worse as usize].better_level = new_idx;
        }
        self.active_levels += 1;
        self.root = self.aa_insert_node(self.root, new_idx, price_fp);
        Ok(new_idx)
    }

    fn aa_insert_node(&mut self, t: u32, new_idx: u32, price_fp: u64) -> u32 {
        if t == NULL_IDX {
            return new_idx;
        }
        let node_price = self.nodes[t as usize].price_fp;
        if self.cmp_priority(price_fp, node_price) == std::cmp::Ordering::Less {
            let left = self.nodes[t as usize].left;
            self.nodes[t as usize].left = self.aa_insert_node(left, new_idx, price_fp);
        } else {
            let right = self.nodes[t as usize].right;
            self.nodes[t as usize].right = self.aa_insert_node(right, new_idx, price_fp);
        }
        let t = self.skew(t);
        self.split(t)
    }

    pub fn remove_empty_level(&mut self, level_idx: u32) {
        let price_fp = self.nodes[level_idx as usize].price_fp;
        debug_assert_eq!(self.nodes[level_idx as usize].order_count, 0);
        self.price_index.remove(price_fp);
        let better = self.nodes[level_idx as usize].better_level;
        let worse = self.nodes[level_idx as usize].worse_level;
        if better != NULL_IDX {
            self.nodes[better as usize].worse_level = worse;
        } else {
            self.best_idx = worse;
        }
        if worse != NULL_IDX {
            self.nodes[worse as usize].better_level = better;
        }

        self.root = self.aa_delete_price(self.root, price_fp);
        let node = &mut self.nodes[level_idx as usize];
        node.occupied = false;
        node.aa_level = 0;
        node.left = NULL_IDX;
        node.right = NULL_IDX;
        node.better_level = NULL_IDX;
        node.worse_level = self.free_head;
        self.free_head = level_idx;
        self.active_levels -= 1;
    }

    fn aa_delete_price(&mut self, mut t: u32, price_fp: u64) -> u32 {
        if t == NULL_IDX {
            return NULL_IDX;
        }
        let node_price = self.nodes[t as usize].price_fp;
        match self.cmp_priority(price_fp, node_price) {
            std::cmp::Ordering::Less => {
                let left = self.nodes[t as usize].left;
                self.nodes[t as usize].left = self.aa_delete_price(left, price_fp);
            }
            std::cmp::Ordering::Greater => {
                let right = self.nodes[t as usize].right;
                self.nodes[t as usize].right = self.aa_delete_price(right, price_fp);
            }
            std::cmp::Ordering::Equal => {
                let left = self.nodes[t as usize].left;
                let right = self.nodes[t as usize].right;
                if left == NULL_IDX {
                    return right;
                }
                if right == NULL_IDX {
                    return left;
                }
                let mut succ = right;
                while self.nodes[succ as usize].left != NULL_IDX {
                    succ = self.nodes[succ as usize].left;
                }
                let succ_price = self.nodes[succ as usize].price_fp;
                let new_right = self.aa_delete_price(right, succ_price);
                self.nodes[succ as usize].left = left;
                self.nodes[succ as usize].right = new_right;
                self.nodes[succ as usize].aa_level = self.nodes[t as usize].aa_level;
                t = succ;
            }
        }
        self.rebalance_after_delete(t)
    }

    #[inline]
    fn rebalance_after_delete(&mut self, mut t: u32) -> u32 {
        let left_lvl = self.node_level(self.nodes[t as usize].left);
        let right_lvl = self.node_level(self.nodes[t as usize].right);
        let target_lvl = left_lvl.min(right_lvl) + 1;
        if target_lvl < self.nodes[t as usize].aa_level {
            self.nodes[t as usize].aa_level = target_lvl;
            let right = self.nodes[t as usize].right;
            if right != NULL_IDX && target_lvl < self.nodes[right as usize].aa_level {
                self.nodes[right as usize].aa_level = target_lvl;
            }
        }
        t = self.skew(t);
        let right = self.nodes[t as usize].right;
        if right != NULL_IDX {
            let new_right = self.skew(right);
            self.nodes[t as usize].right = new_right;
            let rr = self.nodes[new_right as usize].right;
            if rr != NULL_IDX {
                self.nodes[new_right as usize].right = self.skew(rr);
            }
        }
        t = self.split(t);
        let right2 = self.nodes[t as usize].right;
        if right2 != NULL_IDX {
            self.nodes[t as usize].right = self.split(right2);
        }
        t
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceLevelSummary {
    pub price_fp: u64,
    pub total_qty_fp: u64,
    pub order_count: u32,
}

#[derive(Clone)]
pub struct InstrumentOrderBook {
    pub instrument_id: u32,
    pub symbol: InstrumentSymbol,
    pub base_asset: AssetSymbol,
    pub quote_asset: AssetSymbol,
    pub prev_l3_seq: u64,
    pub bids: PriceSideBook,
    pub asks: PriceSideBook,
    pub orders: OrderArena,
    pub order_index: FixedU64Map,
}

impl InstrumentOrderBook {
    pub fn new(
        instrument_id: u32,
        symbol: &str,
        base_asset: &str,
        quote_asset: &str,
        max_orders: usize,
        max_levels_per_side: usize,
    ) -> Self {
        Self {
            instrument_id,
            symbol: InstrumentSymbol::from_str_trunc(symbol),
            base_asset: AssetSymbol::from_str_trunc(base_asset),
            quote_asset: AssetSymbol::from_str_trunc(quote_asset),
            prev_l3_seq: 0,
            bids: PriceSideBook::with_capacity(true, max_levels_per_side),
            asks: PriceSideBook::with_capacity(false, max_levels_per_side),
            orders: OrderArena::with_capacity(max_orders),
            order_index: FixedU64Map::with_capacity(max_orders),
        }
    }

    pub fn clear(&mut self) {
        self.prev_l3_seq = 0;
        self.bids.clear();
        self.asks.clear();
        self.orders.clear();
        self.order_index.clear();
    }

    #[inline(always)]
    pub fn best_bid_fp(&self) -> Option<u64> {
        self.bids.best_price_fp()
    }

    #[inline(always)]
    pub fn best_ask_fp(&self) -> Option<u64> {
        self.asks.best_price_fp()
    }

    #[inline(always)]
    pub fn is_uncrossed(&self) -> bool {
        match (self.best_bid_fp(), self.best_ask_fp()) {
            (Some(bid), Some(ask)) => bid < ask,
            _ => true,
        }
    }

    #[inline(always)]
    pub fn side_book(&self, side: Side) -> &PriceSideBook {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    #[inline(always)]
    pub fn side_book_mut(&mut self, side: Side) -> &mut PriceSideBook {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    pub fn insert_resting(
        &mut self,
        rec: OrderRecord,
        global_lob_acc: &mut LobStateAccumulator,
    ) -> Result<u32, &'static str> {
        let side = rec.side;
        let price_fp = rec.price_fp;
        let order_id = rec.order_id;

        let level_idx = self.side_book_mut(side).get_or_insert_level(price_fp)?;
        let lanes = LobStateAccumulator::order_state_lanes(
            self.instrument_id,
            rec.order_id,
            rec.account_id,
            rec.side as u8,
            rec.price_fp,
            rec.remaining_qty_fp,
            rec.priority_seq,
        );
        let hot_node = HotOrderNode {
            order_id: rec.order_id,
            account_id: rec.account_id,
            price_fp: rec.price_fp,
            remaining_qty_fp: rec.remaining_qty_fp,
            priority_seq: rec.priority_seq,
            instrument_id: self.instrument_id,
            level_idx,
            prev_in_level: NULL_IDX,
            next_in_level: NULL_IDX,
            side: rec.side,
            kind: rec.kind,
            stp_mode: rec.stp_mode,
            occupied: true,
            _pad: [0u8; 4],
        };
        let cold_meta = ColdOrderMeta {
            locked_quote_fp: rec.locked_quote_fp,
            initial_qty_fp: rec.initial_qty_fp,
            created_ts_ns: rec.created_ts_ns,
            lt_lanes: lanes,
            client_order_id: rec.client_order_id,
        };

        let slot_idx = match self.orders.alloc(hot_node, cold_meta) {
            Ok(idx) => idx,
            Err(e) => {
                if self.side_book(side).get(level_idx).order_count == 0 {
                    self.side_book_mut(side).remove_empty_level(level_idx);
                }
                return Err(e);
            }
        };

        if let Err(e) = self.order_index.insert(order_id, slot_idx) {
            self.orders.free(slot_idx);
            if self.side_book(side).get(level_idx).order_count == 0 {
                self.side_book_mut(side).remove_empty_level(level_idx);
            }
            return Err(e);
        }

        let tail = self.side_book(side).get(level_idx).tail_order;
        if tail == NULL_IDX {
            let lvl = self.side_book_mut(side).get_mut(level_idx);
            lvl.head_order = slot_idx;
            lvl.tail_order = slot_idx;
        } else {
            self.orders.hot_mut(tail).next_in_level = slot_idx;
            self.orders.hot_mut(slot_idx).prev_in_level = tail;
            self.side_book_mut(side).get_mut(level_idx).tail_order = slot_idx;
        }
        let lvl = self.side_book_mut(side).get_mut(level_idx);
        lvl.total_qty_fp += rec.remaining_qty_fp as u128;
        lvl.order_count += 1;
        global_lob_acc.add_lanes(lanes);
        Ok(slot_idx)
    }

    #[inline]
    pub fn reduce_resting_qty_in_place(
        &mut self,
        slot_idx: u32,
        new_remaining_qty_fp: u64,
        new_locked_quote_fp: i128,
        global_lob_acc: &mut LobStateAccumulator,
    ) {
        let inst_id = self.instrument_id;
        let old_hot = *self.orders.hot(slot_idx);
        let old_lanes = self.orders.cold(slot_idx).lt_lanes;
        debug_assert!(new_remaining_qty_fp > 0 && new_remaining_qty_fp <= old_hot.remaining_qty_fp);
        let qty_diff = (old_hot.remaining_qty_fp - new_remaining_qty_fp) as u128;

        global_lob_acc.sub_lanes(old_lanes);
        let new_lanes = LobStateAccumulator::order_state_lanes(
            inst_id,
            old_hot.order_id,
            old_hot.account_id,
            old_hot.side as u8,
            old_hot.price_fp,
            new_remaining_qty_fp,
            old_hot.priority_seq,
        );
        global_lob_acc.add_lanes(new_lanes);

        self.orders.hot_mut(slot_idx).remaining_qty_fp = new_remaining_qty_fp;
        let c = self.orders.cold_mut(slot_idx);
        c.locked_quote_fp = new_locked_quote_fp;
        c.lt_lanes = new_lanes;

        let lvl = self.side_book_mut(old_hot.side).get_mut(old_hot.level_idx);
        lvl.total_qty_fp -= qty_diff;
    }

    #[inline]
    pub fn remove_resting_by_slot(
        &mut self,
        slot_idx: u32,
        global_lob_acc: &mut LobStateAccumulator,
    ) -> OrderRecord {
        let removed_rec = self.orders.record(slot_idx);
        let old_hot = *self.orders.hot(slot_idx);
        let old_lanes = self.orders.cold(slot_idx).lt_lanes;

        self.order_index.remove(old_hot.order_id);
        global_lob_acc.sub_lanes(old_lanes);

        let prev = old_hot.prev_in_level;
        let next = old_hot.next_in_level;
        if prev != NULL_IDX {
            self.orders.hot_mut(prev).next_in_level = next;
        } else {
            self.side_book_mut(old_hot.side)
                .get_mut(old_hot.level_idx)
                .head_order = next;
        }
        if next != NULL_IDX {
            self.orders.hot_mut(next).prev_in_level = prev;
        } else {
            self.side_book_mut(old_hot.side)
                .get_mut(old_hot.level_idx)
                .tail_order = prev;
        }

        let is_empty_now = {
            let lvl = self.side_book_mut(old_hot.side).get_mut(old_hot.level_idx);
            lvl.total_qty_fp -= old_hot.remaining_qty_fp as u128;
            lvl.order_count -= 1;
            lvl.order_count == 0
        };
        if is_empty_now {
            self.side_book_mut(old_hot.side)
                .remove_empty_level(old_hot.level_idx);
        }

        self.orders.free(slot_idx);
        removed_rec
    }

    pub fn snapshot_l2(&self, depth: usize) -> (Vec<PriceLevelSummary>, Vec<PriceLevelSummary>) {
        let cap = depth.min(4096);
        let mut bids_out = Vec::with_capacity(cap.min(self.bids.active_levels() as usize));
        let mut cur = self.bids.best_level_idx();
        while cur != NULL_IDX && bids_out.len() < depth {
            let n = self.bids.get(cur);
            bids_out.push(PriceLevelSummary {
                price_fp: n.price_fp,
                total_qty_fp: n.total_qty_fp.min(u64::MAX as u128) as u64,
                order_count: n.order_count,
            });
            cur = n.worse_level;
        }

        let mut asks_out = Vec::with_capacity(cap.min(self.asks.active_levels() as usize));
        let mut cur = self.asks.best_level_idx();
        while cur != NULL_IDX && asks_out.len() < depth {
            let n = self.asks.get(cur);
            asks_out.push(PriceLevelSummary {
                price_fp: n.price_fp,
                total_qty_fp: n.total_qty_fp.min(u64::MAX as u128) as u64,
                order_count: n.order_count,
            });
            cur = n.worse_level;
        }
        (bids_out, asks_out)
    }

    pub fn export_resting_orders_deterministic(&self) -> Vec<OrderRecord> {
        let mut out = Vec::with_capacity(self.orders.active_len() as usize);
        for side in [Side::Buy, Side::Sell] {
            let side_book = self.side_book(side);
            let mut lvl_idx = side_book.best_level_idx();
            while lvl_idx != NULL_IDX {
                let lvl = side_book.get(lvl_idx);
                let mut ord_idx = lvl.head_order;
                while ord_idx != NULL_IDX {
                    out.push(self.orders.record(ord_idx));
                    ord_idx = self.orders.hot(ord_idx).next_in_level;
                }
                lvl_idx = lvl.worse_level;
            }
        }
        out
    }
}
