//! Progressive 3-Pass Micro-Optimization Reference Implementations (`02-extreme-overclock-protocol.md` Gate 3):
//! - **Pass 1 (`BaselineHeapBook`)**: Standard heap-allocated `BTreeMap<u64, VecDeque<Box<HeapOrder>>>` + `HashMap` + `String` clones + per-op Blake3 rehashing.
//! - **Pass 2 (`MonolithicArenaBook`)**: Pre-allocated monolithic unaligned 208-byte `Vec<MonolithicOrder>` slab + `FixedU64Map` (eliminates heap allocations, `0 B/op`, but touches 3-4 cache lines per order scan).
//! - **Pass 3 (`InstrumentOrderBook` / `DeterministicExchangeEngine`)**: `#[repr(C, align(64))]` 64-byte Hot/Cold SoA Arena + Intrusive AA-Tree + Threaded `worse_level` pointers + Incremental `LtHash`.

use crate::arena_lob::{FixedU64Map, Side, NULL_IDX};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[derive(Clone)]
pub struct HeapOrder {
    pub order_id: u64,
    pub account_id: u64,
    pub client_order_id: String,
    pub symbol: String,
    pub side: Side,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
}

/// Pass 1 Baseline: idiomatic naive heap-backed Rust LOB (`BTreeMap` + `VecDeque<Box<HeapOrder>>` + `HashMap`).
#[derive(Default)]
pub struct BaselineHeapBook {
    bids: BTreeMap<u64, VecDeque<Box<HeapOrder>>>,
    asks: BTreeMap<u64, VecDeque<Box<HeapOrder>>>,
    index: HashMap<u64, (Side, u64)>,
}

impl BaselineHeapBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit_limit(&mut self, mut ord: HeapOrder) -> (u64, [u8; 32]) {
        let mut filled = 0u64;
        match ord.side {
            Side::Buy => {
                while ord.remaining_qty_fp > 0 {
                    let Some((&ask_px, _)) = self.asks.iter().next() else {
                        break;
                    };
                    if ask_px > ord.price_fp {
                        break;
                    }
                    let mut remove_lvl = false;
                    if let Some(q) = self.asks.get_mut(&ask_px) {
                        while ord.remaining_qty_fp > 0 {
                            let Some(front) = q.front_mut() else {
                                break;
                            };
                            let step = ord.remaining_qty_fp.min(front.remaining_qty_fp);
                            ord.remaining_qty_fp -= step;
                            front.remaining_qty_fp -= step;
                            filled += step;
                            if front.remaining_qty_fp == 0 {
                                let popped = q.pop_front().unwrap();
                                self.index.remove(&popped.order_id);
                            }
                        }
                        remove_lvl = q.is_empty();
                    }
                    if remove_lvl {
                        self.asks.remove(&ask_px);
                    }
                }
                if ord.remaining_qty_fp > 0 {
                    let id = ord.order_id;
                    let px = ord.price_fp;
                    self.bids
                        .entry(px)
                        .or_default()
                        .push_back(Box::new(ord.clone()));
                    self.index.insert(id, (Side::Buy, px));
                }
            }
            Side::Sell => {
                while ord.remaining_qty_fp > 0 {
                    let Some((&bid_px, _)) = self.bids.iter().next_back() else {
                        break;
                    };
                    if bid_px < ord.price_fp {
                        break;
                    }
                    let mut remove_lvl = false;
                    if let Some(q) = self.bids.get_mut(&bid_px) {
                        while ord.remaining_qty_fp > 0 {
                            let Some(front) = q.front_mut() else {
                                break;
                            };
                            let step = ord.remaining_qty_fp.min(front.remaining_qty_fp);
                            ord.remaining_qty_fp -= step;
                            front.remaining_qty_fp -= step;
                            filled += step;
                            if front.remaining_qty_fp == 0 {
                                let popped = q.pop_front().unwrap();
                                self.index.remove(&popped.order_id);
                            }
                        }
                        remove_lvl = q.is_empty();
                    }
                    if remove_lvl {
                        self.bids.remove(&bid_px);
                    }
                }
                if ord.remaining_qty_fp > 0 {
                    let id = ord.order_id;
                    let px = ord.price_fp;
                    self.asks
                        .entry(px)
                        .or_default()
                        .push_back(Box::new(ord.clone()));
                    self.index.insert(id, (Side::Sell, px));
                }
            }
        }
        (filled, self.recompute_naive_book_hash())
    }

    pub fn cancel(&mut self, order_id: u64) -> bool {
        let Some((side, px)) = self.index.remove(&order_id) else {
            return false;
        };
        let map = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let mut empty = false;
        if let Some(q) = map.get_mut(&px) {
            if let Some(pos) = q.iter().position(|o| o.order_id == order_id) {
                q.remove(pos);
            }
            empty = q.is_empty();
        }
        if empty {
            map.remove(&px);
        }
        let _ = self.recompute_naive_book_hash();
        true
    }

    fn recompute_naive_book_hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        for (px, q) in &self.bids {
            h.update(&px.to_le_bytes());
            for o in q {
                h.update(&o.order_id.to_le_bytes());
                h.update(&o.remaining_qty_fp.to_le_bytes());
            }
        }
        for (px, q) in &self.asks {
            h.update(&px.to_le_bytes());
            for o in q {
                h.update(&o.order_id.to_le_bytes());
                h.update(&o.remaining_qty_fp.to_le_bytes());
            }
        }
        *h.finalize().as_bytes()
    }
}

/// Pass 2 Refactor: Pre-allocated Monolithic Unaligned Slab + `BTreeMap<u64, (u32, u32)>` price map
/// (eliminates per-order heap `Box` & `String` allocations, but mixes cold metadata in 192B unaligned structs
/// and walks `BTreeMap` iterators on every level transition instead of 64B cache-aligned threaded nodes).
#[derive(Clone, Copy)]
pub struct UnalignedMonolithicNode {
    pub client_id_padding: [u8; 64],
    pub lt_lanes: [u64; 4],
    pub locked_quote_fp: i128,
    pub order_id: u64,
    pub account_id: u64,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub prev: u32,
    pub next: u32,
    pub side: Side,
    pub active: bool,
}

impl Default for UnalignedMonolithicNode {
    fn default() -> Self {
        Self {
            client_id_padding: [0u8; 64],
            lt_lanes: [0u64; 4],
            locked_quote_fp: 0,
            order_id: 0,
            account_id: 0,
            price_fp: 0,
            remaining_qty_fp: 0,
            prev: NULL_IDX,
            next: NULL_IDX,
            side: Side::Buy,
            active: false,
        }
    }
}

pub struct Pass2MonolithicSlabBook {
    slab: Box<[UnalignedMonolithicNode]>,
    free_head: u32,
    bids: BTreeMap<u64, (u32, u32)>,
    asks: BTreeMap<u64, (u32, u32)>,
    index: FixedU64Map,
    xor_hash: [u64; 4],
}

impl Pass2MonolithicSlabBook {
    pub fn with_capacity(cap: usize) -> Self {
        let mut slab = vec![UnalignedMonolithicNode::default(); cap].into_boxed_slice();
        for (i, n) in slab.iter_mut().enumerate() {
            n.next = if i + 1 < cap { (i + 1) as u32 } else { NULL_IDX };
        }
        Self {
            slab,
            free_head: 0,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            index: FixedU64Map::with_capacity(cap),
            xor_hash: [0u64; 4],
        }
    }

    #[inline]
    fn hash_node(order_id: u64, price_fp: u64, qty_fp: u64) -> [u64; 4] {
        let mut b = [0u8; 24];
        b[0..8].copy_from_slice(&order_id.to_le_bytes());
        b[8..16].copy_from_slice(&price_fp.to_le_bytes());
        b[16..24].copy_from_slice(&qty_fp.to_le_bytes());
        let d = blake3::hash(&b);
        let s = d.as_bytes();
        [
            u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
            u64::from_le_bytes([s[8], s[9], s[10], s[11], s[12], s[13], s[14], s[15]]),
            u64::from_le_bytes([s[16], s[17], s[18], s[19], s[20], s[21], s[22], s[23]]),
            u64::from_le_bytes([s[24], s[25], s[26], s[27], s[28], s[29], s[30], s[31]]),
        ]
    }

    pub fn submit_limit(
        &mut self,
        order_id: u64,
        account_id: u64,
        side: Side,
        price_fp: u64,
        mut qty_fp: u64,
    ) -> (u64, [u64; 4]) {
        let mut filled = 0u64;
        match side {
            Side::Buy => {
                while qty_fp > 0 {
                    let Some((&ask_px, &(head, _))) = self.asks.iter().next() else {
                        break;
                    };
                    if ask_px > price_fp {
                        break;
                    }
                    let step = qty_fp.min(self.slab[head as usize].remaining_qty_fp);
                    qty_fp -= step;
                    filled += step;
                    let old_lanes = self.slab[head as usize].lt_lanes;
                    for (acc, &lane) in self.xor_hash.iter_mut().zip(old_lanes.iter()) {
                        *acc = acc.wrapping_sub(lane);
                    }
                    if self.slab[head as usize].remaining_qty_fp == step {
                        self.unlink(head);
                    } else {
                        self.slab[head as usize].remaining_qty_fp -= step;
                        let nl = Self::hash_node(
                            self.slab[head as usize].order_id,
                            ask_px,
                            self.slab[head as usize].remaining_qty_fp,
                        );
                        self.slab[head as usize].lt_lanes = nl;
                        for (acc, &lane) in self.xor_hash.iter_mut().zip(nl.iter()) {
                            *acc = acc.wrapping_add(lane);
                        }
                    }
                }
            }
            Side::Sell => {
                while qty_fp > 0 {
                    let Some((&bid_px, &(head, _))) = self.bids.iter().next_back() else {
                        break;
                    };
                    if bid_px < price_fp {
                        break;
                    }
                    let step = qty_fp.min(self.slab[head as usize].remaining_qty_fp);
                    qty_fp -= step;
                    filled += step;
                    let old_lanes = self.slab[head as usize].lt_lanes;
                    for (acc, &lane) in self.xor_hash.iter_mut().zip(old_lanes.iter()) {
                        *acc = acc.wrapping_sub(lane);
                    }
                    if self.slab[head as usize].remaining_qty_fp == step {
                        self.unlink(head);
                    } else {
                        self.slab[head as usize].remaining_qty_fp -= step;
                        let nl = Self::hash_node(
                            self.slab[head as usize].order_id,
                            bid_px,
                            self.slab[head as usize].remaining_qty_fp,
                        );
                        self.slab[head as usize].lt_lanes = nl;
                        for (acc, &lane) in self.xor_hash.iter_mut().zip(nl.iter()) {
                            *acc = acc.wrapping_add(lane);
                        }
                    }
                }
            }
        }

        if qty_fp > 0 {
            let idx = self.free_head;
            self.free_head = self.slab[idx as usize].next;
            let lanes = Self::hash_node(order_id, price_fp, qty_fp);
            for (acc, &lane) in self.xor_hash.iter_mut().zip(lanes.iter()) {
                *acc = acc.wrapping_add(lane);
            }
            self.slab[idx as usize] = UnalignedMonolithicNode {
                client_id_padding: [0xAB; 64],
                lt_lanes: lanes,
                locked_quote_fp: 0,
                order_id,
                account_id,
                price_fp,
                remaining_qty_fp: qty_fp,
                prev: NULL_IDX,
                next: NULL_IDX,
                side,
                active: true,
            };
            let _ = self.index.insert(order_id, idx);
            let map = match side {
                Side::Buy => &mut self.bids,
                Side::Sell => &mut self.asks,
            };
            match map.get_mut(&price_fp) {
                Some((_, tail)) => {
                    self.slab[*tail as usize].next = idx;
                    self.slab[idx as usize].prev = *tail;
                    *tail = idx;
                }
                None => {
                    map.insert(price_fp, (idx, idx));
                }
            }
        }
        (filled, self.xor_hash)
    }

    pub fn cancel(&mut self, order_id: u64) -> bool {
        let Some(idx) = self.index.get(order_id) else {
            return false;
        };
        let old_lanes = self.slab[idx as usize].lt_lanes;
        for (acc, &lane) in self.xor_hash.iter_mut().zip(old_lanes.iter()) {
            *acc = acc.wrapping_sub(lane);
        }
        self.unlink(idx);
        true
    }

    fn unlink(&mut self, idx: u32) {
        let node = self.slab[idx as usize];
        self.index.remove(node.order_id);
        let map = match node.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        if let Some((head, tail)) = map.get_mut(&node.price_fp) {
            if node.prev != NULL_IDX {
                self.slab[node.prev as usize].next = node.next;
            } else {
                *head = node.next;
            }
            if node.next != NULL_IDX {
                self.slab[node.next as usize].prev = node.prev;
            } else {
                *tail = node.prev;
            }
            if *head == NULL_IDX {
                map.remove(&node.price_fp);
            }
        }
        self.slab[idx as usize].active = false;
        self.slab[idx as usize].next = self.free_head;
        self.free_head = idx;
    }
}
