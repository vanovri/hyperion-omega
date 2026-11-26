//! # Internal Pre-Trade Risk Gate (`100% Fixed-Point 1e8`, Zero-Alloc Per-Tick Transactional Staging)
//!
//! Validates every guest `submit_limit_order` and `cancel_order` synchronously *before* forwarding
//! to `gateway-go` / `ledger-rs`. Enforces:
//! - Canonical tick-size & lot-size integer divisibility (`00-hyperion-contract.md` §4.3 & §5.1)
//! - Fat-finger price collar in basis points relative to reference mid-price
//! - `POST_ONLY` immediate-cross prevention
//! - Single-order base quantity & 128-bit quote notional caps
//! - Open-order count & duplicate `client_order_id` prevention
//! - Worst-case net base inventory exposure cap (`|inv + open_side + qty| <= max_abs_position_qty_fp`)
//! - **Transactional Per-Tick Spam Cap (`01-hyperion-omega.md` §4.5)**: Exceeding `max_orders_per_tick`
//!   (default `16`) immediately aborts the tick with `WASM_TRAP_CODE_ORDER_SPAM_LIMIT_EXCEEDED` and
//!   rolls back all staged orders from that tick (`0` orders leaked downstream).

use hyperion_quant_abi::*;

pub const INLINE_STAGE_CAP: usize = 32;
pub const MAX_TRACKED_OPEN_ORDERS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstrumentSpec {
    pub instrument_id: u32,
    pub tick_size_fp: u64,
    pub lot_size_fp: u64,
}

impl InstrumentSpec {
    pub const fn canonical(instrument_id: u32) -> Option<Self> {
        match instrument_id {
            1 => Some(Self {
                instrument_id: 1,
                tick_size_fp: 1_000_000, // BTC-USDT: 0.01 USDT
                lot_size_fp: 1_000,      // 0.00001 BTC
            }),
            2 => Some(Self {
                instrument_id: 2,
                tick_size_fp: 1_000_000, // ETH-USDT: 0.01 USDT
                lot_size_fp: 10_000,     // 0.0001 ETH
            }),
            3 => Some(Self {
                instrument_id: 3,
                tick_size_fp: 100_000, // SOL-USDT: 0.001 USDT
                lot_size_fp: 100_000, // 0.001 SOL
            }),
            4 => Some(Self {
                instrument_id: 4,
                tick_size_fp: 10_000,    // HYPE-USDT: 0.0001 USDT
                lot_size_fp: 1_000_000, // 0.01 HYPE
            }),
            _ => None,
        }
    }

    pub const fn symbol_str(instrument_id: u32) -> &'static str {
        match instrument_id {
            1 => "BTC-USDT",
            2 => "ETH-USDT",
            3 => "SOL-USDT",
            4 => "HYPE-USDT",
            _ => "UNKNOWN",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RiskPolicy {
    pub account_id: u64,
    pub allowed_instrument_mask: u32,
    pub max_orders_per_tick: u32,
    pub max_open_orders: u32,
    pub max_order_qty_fp: u64,
    pub max_order_notional_fp: u128,
    pub max_abs_position_qty_fp: u64,
    pub fat_finger_collar_bps: u32,
    pub account_active: bool,
    pub market_halted: bool,
}

impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            account_id: 10_001,
            allowed_instrument_mask: 0b1_1110, // instruments 1..=4
            max_orders_per_tick: MAX_ORDERS_PER_TICK, // 16
            max_open_orders: 64,
            max_order_qty_fp: 500_000_000,                  // 5.00 BTC
            max_order_notional_fp: 50_000_000_000_000,      // 500,000.00 USDT (scale 1e8)
            max_abs_position_qty_fp: 1_000_000_000,         // 10.00 BTC
            fat_finger_collar_bps: 500,                     // 5.00% collar
            account_active: true,
            market_halted: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOrderRecord {
    pub order_id: u64,
    pub client_order_id: u64,
    pub instrument_id: u32,
    pub side: u8,
    pub order_type: u8,
    pub price_fp: u64,
    pub remaining_qty_fp: u64,
    pub created_ts_ns: u64,
}

impl OpenOrderRecord {
    const ZERO: Self = Self {
        order_id: 0,
        client_order_id: 0,
        instrument_id: 0,
        side: SIDE_UNSPECIFIED,
        order_type: ORDER_TYPE_UNSPECIFIED,
        price_fp: 0,
        remaining_qty_fp: 0,
        created_ts_ns: 0,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatedOrderAction {
    SubmitLimit {
        order_id: u64,
        client_order_id: u64,
        account_id: u64,
        instrument_id: u32,
        side: u8,
        order_type: u8,
        stp_mode: u8,
        price_fp: u64,
        qty_fp: u64,
        max_slippage_bps: u32,
        client_ts_ns: u64,
        strategy_tag: u64,
    },
    Cancel {
        order_id: u64,
        client_order_id: u64,
        account_id: u64,
        instrument_id: u32,
        client_ts_ns: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskGateOutcome {
    AcceptedSubmit { order_id: u64 },
    AcceptedCancel,
    SoftRejected { risk_code: i32 },
    HardTrapOrderSpam { attempts: u32, max_allowed: u32 },
}

/// Zero-heap-allocation Pre-Trade Risk Gate bound to a single strategy sandbox lineage.
#[derive(Clone)]
pub struct PreTradeRiskGate {
    pub policy: RiskPolicy,
    next_order_id: u64,
    net_inventory_fp: i64,
    ref_mid_fp: u64,
    ref_best_bid_fp: u64,
    ref_best_ask_fp: u64,
    open_orders: [OpenOrderRecord; MAX_TRACKED_OPEN_ORDERS],
    open_len: usize,
    // Per-tick transactional state (zero heap allocations)
    tick_attempts: u32,
    tick_rejected: u32,
    staged: [Option<ValidatedOrderAction>; INLINE_STAGE_CAP],
    staged_len: usize,
}

impl PreTradeRiskGate {
    pub fn new(policy: RiskPolicy, starting_order_id: u64) -> Self {
        Self {
            policy,
            next_order_id: starting_order_id.max(1),
            net_inventory_fp: 0,
            ref_mid_fp: 0,
            ref_best_bid_fp: 0,
            ref_best_ask_fp: 0,
            open_orders: [OpenOrderRecord::ZERO; MAX_TRACKED_OPEN_ORDERS],
            open_len: 0,
            tick_attempts: 0,
            tick_rejected: 0,
            staged: [None; INLINE_STAGE_CAP],
            staged_len: 0,
        }
    }

    #[inline]
    pub fn begin_tick(&mut self, mid_fp: u64, best_bid_fp: u64, best_ask_fp: u64) {
        if mid_fp > 0 {
            self.ref_mid_fp = mid_fp;
        }
        if best_bid_fp > 0 {
            self.ref_best_bid_fp = best_bid_fp;
        }
        if best_ask_fp > 0 {
            self.ref_best_ask_fp = best_ask_fp;
        }
        self.tick_attempts = 0;
        self.tick_rejected = 0;
        self.staged_len = 0;
    }

    #[inline]
    pub const fn tick_rejected_count(&self) -> u32 {
        self.tick_rejected
    }

    #[inline]
    pub const fn tick_attempts_count(&self) -> u32 {
        self.tick_attempts
    }

    #[inline]
    pub const fn open_order_count(&self) -> usize {
        self.open_len
    }

    #[inline]
    pub fn open_orders_slice(&self) -> &[OpenOrderRecord] {
        &self.open_orders[..self.open_len]
    }

    #[inline]
    pub const fn net_inventory_fp(&self) -> i64 {
        self.net_inventory_fp
    }

    #[inline]
    pub fn set_net_inventory_fp(&mut self, inv_fp: i64) {
        self.net_inventory_fp = inv_fp;
    }

    /// Updates open-order tracking & net inventory when an `OrderFill` arrives from `ledger-rs`.
    pub fn apply_fill_update(&mut self, order_id: u64, remaining_qty_fp: u64, net_inventory_fp: i64) {
        self.net_inventory_fp = net_inventory_fp;
        let mut i = 0;
        while i < self.open_len {
            if self.open_orders[i].order_id == order_id {
                if remaining_qty_fp == 0 {
                    self.open_len -= 1;
                    self.open_orders[i] = self.open_orders[self.open_len];
                    self.open_orders[self.open_len] = OpenOrderRecord::ZERO;
                } else {
                    self.open_orders[i].remaining_qty_fp = remaining_qty_fp;
                }
                return;
            }
            i += 1;
        }
    }

    /// Cancels all tracked open orders (used on quarantine trap or permanent strategy termination).
    pub fn drain_cancel_all_open_orders(&mut self, ts_ns: u64) -> Vec<ValidatedOrderAction> {
        let mut out = Vec::with_capacity(self.open_len);
        for i in 0..self.open_len {
            let rec = self.open_orders[i];
            out.push(ValidatedOrderAction::Cancel {
                order_id: rec.order_id,
                client_order_id: rec.client_order_id,
                account_id: self.policy.account_id,
                instrument_id: rec.instrument_id,
                client_ts_ns: ts_ns,
            });
            self.open_orders[i] = OpenOrderRecord::ZERO;
        }
        self.open_len = 0;
        out
    }

    #[inline]
    fn reject(&mut self, risk_code: i32) -> RiskGateOutcome {
        self.tick_rejected = self.tick_rejected.saturating_add(1);
        RiskGateOutcome::SoftRejected { risk_code }
    }

    /// Synchronously validates `EgressOrderSlot64` before accepting into the current tick's batch.
    pub fn validate_submit_limit(&mut self, slot: &EgressOrderSlot64) -> RiskGateOutcome {
        self.tick_attempts = self.tick_attempts.saturating_add(1);
        let max_per_tick = self.policy.max_orders_per_tick.min(INLINE_STAGE_CAP as u32);
        if self.tick_attempts > max_per_tick {
            self.tick_rejected = self.tick_rejected.saturating_add(1);
            return RiskGateOutcome::HardTrapOrderSpam {
                attempts: self.tick_attempts,
                max_allowed: max_per_tick,
            };
        }

        if !self.policy.account_active || self.policy.market_halted {
            return self.reject(RISK_ERR_ACCOUNT_OR_REGIME_HALTED);
        }

        if slot.instrument_id == 0
            || slot.instrument_id >= 31
            || (self.policy.allowed_instrument_mask & (1u32 << slot.instrument_id)) == 0
        {
            return self.reject(RISK_ERR_INSTRUMENT_MISMATCH);
        }

        let Some(spec) = InstrumentSpec::canonical(slot.instrument_id) else {
            return self.reject(RISK_ERR_INSTRUMENT_MISMATCH);
        };

        if slot.side != SIDE_BUY && slot.side != SIDE_SELL {
            return self.reject(RISK_ERR_INVALID_SIDE_OR_STP);
        }
        if slot.stp_mode < STP_NONE || slot.stp_mode > STP_CANCEL_BOTH {
            return self.reject(RISK_ERR_INVALID_SIDE_OR_STP);
        }

        // Sandbox bots must always specify explicit limit prices (pure unpriced MARKET orders forbidden).
        if slot.order_type != ORDER_TYPE_LIMIT
            && slot.order_type != ORDER_TYPE_IOC
            && slot.order_type != ORDER_TYPE_FOK
            && slot.order_type != ORDER_TYPE_POST_ONLY
        {
            return self.reject(RISK_ERR_MARKET_ORDER_FORBIDDEN);
        }

        if slot.price_fp == 0 || slot.qty_fp == 0 || slot.client_order_id == 0 {
            return self.reject(RISK_ERR_INVALID_PRICE_OR_QTY);
        }

        if slot.price_fp % spec.tick_size_fp != 0 {
            return self.reject(RISK_ERR_TICK_SIZE_ALIGNMENT);
        }
        if slot.qty_fp % spec.lot_size_fp != 0 {
            return self.reject(RISK_ERR_LOT_SIZE_ALIGNMENT);
        }

        // Fat-finger collar check against latest reference mid-price.
        if self.ref_mid_fp > 0 && self.policy.fat_finger_collar_bps > 0 {
            let diff = slot.price_fp.abs_diff(self.ref_mid_fp) as u128;
            let bps = (diff * (BPS_SCALE as u128)) / (self.ref_mid_fp as u128);
            if bps > (self.policy.fat_finger_collar_bps as u128) {
                return self.reject(RISK_ERR_FAT_FINGER_COLLAR);
            }
        }

        // POST_ONLY immediate crossing guard.
        if slot.order_type == ORDER_TYPE_POST_ONLY {
            if slot.side == SIDE_BUY && self.ref_best_ask_fp > 0 && slot.price_fp >= self.ref_best_ask_fp {
                return self.reject(RISK_ERR_POST_ONLY_WOULD_CROSS);
            }
            if slot.side == SIDE_SELL && self.ref_best_bid_fp > 0 && slot.price_fp <= self.ref_best_bid_fp {
                return self.reject(RISK_ERR_POST_ONLY_WOULD_CROSS);
            }
        }

        if slot.qty_fp > self.policy.max_order_qty_fp {
            return self.reject(RISK_ERR_MAX_ORDER_QTY);
        }

        let notional_fp = ((slot.price_fp as u128) * (slot.qty_fp as u128)) / FP_SCALE_U128;
        if notional_fp == 0 || notional_fp > self.policy.max_order_notional_fp {
            return self.reject(RISK_ERR_MAX_ORDER_NOTIONAL);
        }

        // Check duplicate client_order_id across open + staged orders, and compute pending open exposure.
        let mut resting_after = self.open_len;
        let mut same_side_open_qty: u128 = 0;

        for i in 0..self.open_len {
            let o = &self.open_orders[i];
            if o.client_order_id == slot.client_order_id {
                return self.reject(RISK_ERR_DUPLICATE_CLIENT_ORDER_ID);
            }
            // Check if already scheduled for cancel in this tick:
            let mut being_canceled = false;
            for staged in &self.staged[..self.staged_len] {
                if let Some(ValidatedOrderAction::Cancel { order_id, .. }) = *staged {
                    if order_id == o.order_id {
                        being_canceled = true;
                        break;
                    }
                }
            }
            if being_canceled {
                resting_after = resting_after.saturating_sub(1);
            } else if o.side == slot.side {
                same_side_open_qty += o.remaining_qty_fp as u128;
            }
        }

        for s_idx in 0..self.staged_len {
            if let Some(ValidatedOrderAction::SubmitLimit {
                order_id: staged_oid,
                client_order_id,
                side,
                order_type,
                qty_fp,
                ..
            }) = self.staged[s_idx]
            {
                if client_order_id == slot.client_order_id {
                    return self.reject(RISK_ERR_DUPLICATE_CLIENT_ORDER_ID);
                }
                let mut canceled_later = false;
                for later in &self.staged[(s_idx + 1)..self.staged_len] {
                    if let Some(ValidatedOrderAction::Cancel { order_id, .. }) = *later {
                        if order_id == staged_oid {
                            canceled_later = true;
                            break;
                        }
                    }
                }
                if !canceled_later {
                    if order_type == ORDER_TYPE_LIMIT || order_type == ORDER_TYPE_POST_ONLY {
                        resting_after += 1;
                    }
                    if side == slot.side {
                        same_side_open_qty += qty_fp as u128;
                    }
                }
            }
        }

        let is_resting =
            slot.order_type == ORDER_TYPE_LIMIT || slot.order_type == ORDER_TYPE_POST_ONLY;
        if is_resting {
            let max_open = (self.policy.max_open_orders as usize).min(MAX_TRACKED_OPEN_ORDERS);
            if resting_after >= max_open {
                return self.reject(RISK_ERR_MAX_OPEN_ORDERS);
            }
        }

        // Worst-case projected net position check:
        let signed_order_delta: i128 = if slot.side == SIDE_BUY {
            (same_side_open_qty + (slot.qty_fp as u128)) as i128
        } else {
            -((same_side_open_qty + (slot.qty_fp as u128)) as i128)
        };
        let worst_case_pos = (self.net_inventory_fp as i128) + signed_order_delta;
        if worst_case_pos.unsigned_abs() > (self.policy.max_abs_position_qty_fp as u128) {
            return self.reject(RISK_ERR_MAX_POSITION_DELTA);
        }

        let assigned_id = self.next_order_id;
        self.next_order_id = self.next_order_id.wrapping_add(1);

        self.staged[self.staged_len] = Some(ValidatedOrderAction::SubmitLimit {
            order_id: assigned_id,
            client_order_id: slot.client_order_id,
            account_id: self.policy.account_id,
            instrument_id: slot.instrument_id,
            side: slot.side,
            order_type: slot.order_type,
            stp_mode: slot.stp_mode,
            price_fp: slot.price_fp,
            qty_fp: slot.qty_fp,
            max_slippage_bps: slot.max_slippage_bps,
            client_ts_ns: slot.client_ts_ns,
            strategy_tag: slot.strategy_tag,
        });
        self.staged_len += 1;

        RiskGateOutcome::AcceptedSubmit {
            order_id: assigned_id,
        }
    }

    /// Synchronously validates `cancel_order(order_id, client_order_id)`.
    pub fn validate_cancel(&mut self, order_id: u64, client_order_id: u64, ts_ns: u64) -> RiskGateOutcome {
        self.tick_attempts = self.tick_attempts.saturating_add(1);
        let max_per_tick = self.policy.max_orders_per_tick.min(INLINE_STAGE_CAP as u32);
        if self.tick_attempts > max_per_tick {
            self.tick_rejected = self.tick_rejected.saturating_add(1);
            return RiskGateOutcome::HardTrapOrderSpam {
                attempts: self.tick_attempts,
                max_allowed: max_per_tick,
            };
        }

        if order_id == 0 {
            return self.reject(RISK_ERR_UNKNOWN_OR_FOREIGN_ORDER);
        }

        let mut found_instrument: Option<u32> = None;
        for order in &self.open_orders[..self.open_len] {
            if order.order_id == order_id {
                found_instrument = Some(order.instrument_id);
                break;
            }
        }
        if found_instrument.is_none() {
            for staged in &self.staged[..self.staged_len] {
                if let Some(ValidatedOrderAction::SubmitLimit {
                    order_id: staged_oid,
                    instrument_id,
                    ..
                }) = *staged
                {
                    if staged_oid == order_id {
                        found_instrument = Some(instrument_id);
                        break;
                    }
                }
            }
        }

        let Some(instrument_id) = found_instrument else {
            return self.reject(RISK_ERR_UNKNOWN_OR_FOREIGN_ORDER);
        };

        self.staged[self.staged_len] = Some(ValidatedOrderAction::Cancel {
            order_id,
            client_order_id,
            account_id: self.policy.account_id,
            instrument_id,
            client_ts_ns: ts_ns,
        });
        self.staged_len += 1;

        RiskGateOutcome::AcceptedCancel
    }

    /// Rolls back all staged mutations from a trapped tick in O(1) (`0` orders emitted).
    #[inline]
    pub fn rollback_tick(&mut self) {
        self.staged_len = 0;
    }

    /// Atomically commits the current tick's validated mutations into `open_orders` and invokes `emit`
    /// for each outbound action without allocating any heap vectors on the hot path!
    #[inline]
    pub fn commit_tick_with<F: FnMut(ValidatedOrderAction)>(&mut self, mut emit: F) -> u32 {
        let count = self.staged_len as u32;
        for idx in 0..self.staged_len {
            if let Some(action) = self.staged[idx].take() {
                match action {
                    ValidatedOrderAction::SubmitLimit {
                        order_id,
                        client_order_id,
                        instrument_id,
                        side,
                        order_type,
                        price_fp,
                        qty_fp,
                        client_ts_ns,
                        ..
                    } => {
                        if (order_type == ORDER_TYPE_LIMIT || order_type == ORDER_TYPE_POST_ONLY)
                            && self.open_len < MAX_TRACKED_OPEN_ORDERS
                        {
                            self.open_orders[self.open_len] = OpenOrderRecord {
                                order_id,
                                client_order_id,
                                instrument_id,
                                side,
                                order_type,
                                price_fp,
                                remaining_qty_fp: qty_fp,
                                created_ts_ns: client_ts_ns,
                            };
                            self.open_len += 1;
                        }
                    }
                    ValidatedOrderAction::Cancel { order_id, .. } => {
                        let mut i = 0;
                        while i < self.open_len {
                            if self.open_orders[i].order_id == order_id {
                                self.open_len -= 1;
                                self.open_orders[i] = self.open_orders[self.open_len];
                                self.open_orders[self.open_len] = OpenOrderRecord::ZERO;
                                break;
                            }
                            i += 1;
                        }
                    }
                }
                emit(action);
            }
        }
        self.staged_len = 0;
        count
    }
}
