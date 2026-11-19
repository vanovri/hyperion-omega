#![no_std]

//! # `stat_arb_zscore.wasm` — Rolling Z-Score Spread & OFI Mean-Reversion Bot
//!
//! Institutional co-located statistical arbitrage strategy operating in pure `1e8` fixed-point
//! integer arithmetic (`0` floating-point instructions).
//!
//! ## Quantitative Signal Formulation
//! 1. **Synthetic Basis Spread (`spread_fp`)**:
//!    - If `book.fair_ref_price_fp > 0` (correlated index / synthetic basket leg),
//!      $\text{spread\_fp}_t = P^{\text{mid}}_t - P^{\text{ref}}_t$.
//!    - Otherwise, tracks dislocation against a slow exponentially weighted equilibrium anchor
//!      $\mu^{\text{slow}}_t$ ($\alpha = 0.08$), $\text{spread\_fp}_t = P^{\text{mid}}_t - \mu^{\text{slow}}_t$.
//! 2. **Rolling Centered Z-Score (`zscore_ppm`)**:
//!    - Computed over a configurable rolling window $W \in [4, 64]$ via 128-bit integer variance
//!      and Newton-Raphson `isqrt_u128`:
//!      $$Z_t = \frac{\text{spread\_fp}_t - \bar{s}_t}{\sigma_t} \times 10^6 \quad (\text{in ppm})$$
//! 3. **Order Flow Imbalance (`OFI`) Confirmation**:
//!    - Normalizes signed `ofi_fp` against top-5 total resting liquidity to avoid catching a falling
//!      knife during toxic informed flow, blending `alpha_ppm = -Z_t + w_{\text{ofi}} \cdot \text{OFI}^{\text{norm}}_t$.
//! 4. **Execution & Mean-Reversion Unwind**:
//!    - Enters passive `POST_ONLY` / `LIMIT` orders when $|\alpha_t| \ge Z_{\text{entry}}$ within position caps.
//!    - Unwinds inventory toward zero once $|Z_t| \le Z_{\text{exit}}$.

use hyperion_quant_sdk::abi::*;
use hyperion_quant_sdk::{
    export_quant_strategy, fp, BookL2View, LimitOrderRequest, OrderFillView, QuantContext,
    QuantStrategy, RollingWindowFp, StateReader, StateWriter, TradeTickView,
};

const MAX_WINDOW: usize = 64;

pub struct StatArbZScoreBot {
    cfg: StrategyInitBlock,
    spread_window: RollingWindowFp<MAX_WINDOW>,
    slow_anchor_fp: i64,
    ofi_ewma_fp: i64,
    net_inventory_fp: i64,
    active_order_id: u64,
    active_order_side: u8,
    active_order_price_fp: u64,
    next_client_order_id: u64,
    last_order_ts_ns: u64,
    last_zscore_ppm: i64,
}

impl StatArbZScoreBot {
    pub const fn const_new() -> Self {
        Self {
            cfg: StrategyInitBlock::default_btc(),
            spread_window: RollingWindowFp::new(),
            slow_anchor_fp: 0,
            ofi_ewma_fp: 0,
            net_inventory_fp: 0,
            active_order_id: 0,
            active_order_side: SIDE_UNSPECIFIED,
            active_order_price_fp: 0,
            next_client_order_id: 1,
            last_order_ts_ns: 0,
            last_zscore_ppm: 0,
        }
    }

    #[inline]
    fn total_top5_depth_fp(book: &BookL2View) -> u64 {
        let mut sum: u64 = 0;
        let mut i = 0;
        while i < L2_BOOK_LEVELS {
            sum = sum
                .saturating_add(book.bids_qty_fp[i])
                .saturating_add(book.asks_qty_fp[i]);
            i += 1;
        }
        sum
    }
}

impl QuantStrategy for StatArbZScoreBot {
    const STRATEGY_KIND: u32 = STRATEGY_KIND_STAT_ARB_ZSCORE;

    fn new() -> Self {
        Self::const_new()
    }

    fn on_init(&mut self, cfg: &StrategyInitBlock, _ctx: &mut QuantContext<'_>) -> i32 {
        if cfg.tick_size_fp == 0 || cfg.lot_size_fp == 0 || cfg.order_qty_fp == 0 {
            return CALLBACK_ERR_BAD_CONFIG;
        }
        self.cfg = *cfg;
        CALLBACK_OK
    }

    fn on_book_l2_tick(&mut self, book: &BookL2View, ctx: &mut QuantContext<'_>) -> i32 {
        if book.best_bid_fp == 0 || book.best_ask_fp == 0 || book.best_bid_fp >= book.best_ask_fp {
            return CALLBACK_OK;
        }

        let mid_fp = book.mid_price_fp as i64;
        if self.slow_anchor_fp == 0 {
            self.slow_anchor_fp = mid_fp;
        } else {
            self.slow_anchor_fp = fp::ewma_fp(self.slow_anchor_fp, mid_fp, 80_000);
        }

        let anchor_fp = if book.fair_ref_price_fp > 0 {
            book.fair_ref_price_fp as i64
        } else {
            self.slow_anchor_fp
        };
        let spread_fp = mid_fp - anchor_fp;

        let win_cap = (self.cfg.zscore_window as usize).clamp(4, MAX_WINDOW);
        // Compute Z-score against existing distribution before pushing extreme outlier, then update window.
        let ready = self.spread_window.len() >= 4;
        let zscore_ppm = if ready {
            self.spread_window.zscore_ppm(spread_fp, win_cap)
        } else {
            0
        };
        self.spread_window.push_with_cap(spread_fp, win_cap);
        self.last_zscore_ppm = zscore_ppm;

        // Smooth OFI and normalize to [-1_000_000, +1_000_000] ppm relative to top-5 book depth.
        self.ofi_ewma_fp = fp::ewma_fp(self.ofi_ewma_fp, book.ofi_fp, 350_000);
        let depth_fp = Self::total_top5_depth_fp(book).max(self.cfg.order_qty_fp) as i128;
        let ofi_norm_ppm = fp::ratio_ppm(self.ofi_ewma_fp as i128, depth_fp)
            .clamp(-PPM_SCALE, PPM_SCALE);

        if !ready {
            return CALLBACK_OK;
        }

        // Cooldown guard to prevent quote-stuffing.
        if self.last_order_ts_ns > 0
            && book.ts_ns < self.last_order_ts_ns.saturating_add(self.cfg.cooldown_ns)
        {
            return CALLBACK_OK;
        }

        // Composite mean-reversion signal: buy negative spread dislocation + positive OFI turn.
        let ofi_contrib_ppm =
            ((ofi_norm_ppm as i128) * (self.cfg.ofi_weight_ppm as i128) / PPM_SCALE_I128) as i64;
        let alpha_ppm = (-zscore_ppm).saturating_add(ofi_contrib_ppm);

        let clip_qty = fp::align_lot_qty(
            self.cfg.order_qty_fp.min(self.cfg.max_order_qty_fp),
            self.cfg.lot_size_fp,
        );
        if clip_qty == 0 {
            return CALLBACK_OK;
        }

        // 1. Mean-reversion exit / inventory flatten check when spread returns inside exit band.
        if zscore_ppm.abs() <= self.cfg.zscore_exit_ppm && self.net_inventory_fp != 0 {
            if self.active_order_id != 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.active_order_id, cid, book.ts_ns);
                self.active_order_id = 0;
            }
            let unwind_abs = (self.net_inventory_fp.unsigned_abs()).min(self.cfg.max_order_qty_fp);
            let unwind_qty = fp::align_lot_qty(unwind_abs, self.cfg.lot_size_fp);
            if unwind_qty > 0 {
                let (side, px) = if self.net_inventory_fp > 0 {
                    (SIDE_SELL, fp::align_ask_tick(book.best_ask_fp, self.cfg.tick_size_fp))
                } else {
                    (SIDE_BUY, fp::align_bid_tick(book.best_bid_fp, self.cfg.tick_size_fp))
                };
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side,
                    order_type: ORDER_TYPE_LIMIT,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: px,
                    qty_fp: unwind_qty,
                    max_slippage_bps: 25,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x5354_4152_425F_5557, // "STARB_UW"
                }) {
                    self.active_order_id = oid;
                    self.active_order_side = side;
                    self.active_order_price_fp = px;
                    self.last_order_ts_ns = book.ts_ns;
                }
            }
            return CALLBACK_OK;
        }

        // 2. Dislocation entry check.
        if alpha_ppm >= self.cfg.zscore_entry_ppm
            && self.net_inventory_fp.saturating_add(clip_qty as i64) <= self.cfg.max_position_fp
        {
            let target_bid = fp::align_bid_tick(book.best_bid_fp, self.cfg.tick_size_fp);
            if self.active_order_id != 0
                && (self.active_order_side != SIDE_BUY || self.active_order_price_fp != target_bid)
            {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.active_order_id, cid, book.ts_ns);
                self.active_order_id = 0;
            }
            if self.active_order_id == 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_BUY,
                    order_type: ORDER_TYPE_POST_ONLY,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: target_bid,
                    qty_fp: clip_qty,
                    max_slippage_bps: 0,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x5354_4152_425F_4C47, // "STARB_LG"
                }) {
                    self.active_order_id = oid;
                    self.active_order_side = SIDE_BUY;
                    self.active_order_price_fp = target_bid;
                    self.last_order_ts_ns = book.ts_ns;
                }
            }
        } else if alpha_ppm <= -self.cfg.zscore_entry_ppm
            && self.net_inventory_fp.saturating_sub(clip_qty as i64) >= -self.cfg.max_position_fp
        {
            let target_ask = fp::align_ask_tick(book.best_ask_fp, self.cfg.tick_size_fp);
            if self.active_order_id != 0
                && (self.active_order_side != SIDE_SELL || self.active_order_price_fp != target_ask)
            {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.active_order_id, cid, book.ts_ns);
                self.active_order_id = 0;
            }
            if self.active_order_id == 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_SELL,
                    order_type: ORDER_TYPE_POST_ONLY,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: target_ask,
                    qty_fp: clip_qty,
                    max_slippage_bps: 0,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x5354_4152_425F_5348, // "STARB_SH"
                }) {
                    self.active_order_id = oid;
                    self.active_order_side = SIDE_SELL;
                    self.active_order_price_fp = target_ask;
                    self.last_order_ts_ns = book.ts_ns;
                }
            }
        }

        CALLBACK_OK
    }

    fn on_trade_tick(&mut self, trade: &TradeTickView, _ctx: &mut QuantContext<'_>) -> i32 {
        let signed_flow = if trade.aggressor_side == SIDE_BUY {
            trade.qty_fp as i64
        } else if trade.aggressor_side == SIDE_SELL {
            -(trade.qty_fp as i64)
        } else {
            trade.ofi_delta_fp
        };
        self.ofi_ewma_fp = fp::ewma_fp(self.ofi_ewma_fp, signed_flow, 250_000);
        CALLBACK_OK
    }

    fn on_order_fill(&mut self, fill: &OrderFillView, _ctx: &mut QuantContext<'_>) -> i32 {
        self.net_inventory_fp = fill.net_inventory_fp;
        if fill.order_id == self.active_order_id && fill.remaining_qty_fp == 0 {
            self.active_order_id = 0;
            self.active_order_side = SIDE_UNSPECIFIED;
            self.active_order_price_fp = 0;
        }
        CALLBACK_OK
    }

    fn export_state(&self, writer: &mut StateWriter<'_>) -> (i64, u64, u64, bool) {
        let ok = self.spread_window.write_snapshot(writer)
            && writer.write_i64(self.slow_anchor_fp)
            && writer.write_i64(self.ofi_ewma_fp)
            && writer.write_i64(self.net_inventory_fp)
            && writer.write_u64(self.active_order_id)
            && writer.write_u8(self.active_order_side)
            && writer.write_u64(self.active_order_price_fp)
            && writer.write_u64(self.next_client_order_id)
            && writer.write_u64(self.last_order_ts_ns)
            && writer.write_i64(self.last_zscore_ppm);
        (
            self.net_inventory_fp,
            self.active_order_id,
            self.next_client_order_id,
            ok,
        )
    }

    fn import_state(&mut self, reader: &mut StateReader<'_>, _hdr: &StateSnapshotRegion) -> bool {
        if !self.spread_window.read_snapshot(reader) {
            return false;
        }
        let Some(slow_anchor_fp) = reader.read_i64() else { return false };
        let Some(ofi_ewma_fp) = reader.read_i64() else { return false };
        let Some(net_inventory_fp) = reader.read_i64() else { return false };
        let Some(active_order_id) = reader.read_u64() else { return false };
        let Some(active_order_side) = reader.read_u8() else { return false };
        let Some(active_order_price_fp) = reader.read_u64() else { return false };
        let Some(next_client_order_id) = reader.read_u64() else { return false };
        let Some(last_order_ts_ns) = reader.read_u64() else { return false };
        let Some(last_zscore_ppm) = reader.read_i64() else { return false };

        self.slow_anchor_fp = slow_anchor_fp;
        self.ofi_ewma_fp = ofi_ewma_fp;
        self.net_inventory_fp = net_inventory_fp;
        self.active_order_id = active_order_id;
        self.active_order_side = active_order_side;
        self.active_order_price_fp = active_order_price_fp;
        self.next_client_order_id = next_client_order_id;
        self.last_order_ts_ns = last_order_ts_ns;
        self.last_zscore_ppm = last_zscore_ppm;
        true
    }
}

export_quant_strategy!(StatArbZScoreBot, StatArbZScoreBot::const_new());
