#![no_std]

//! # `delta_hedger.wasm` — Automated Inventory & Portfolio Delta-Neutral Hedger
//!
//! Maintains strict delta neutrality across spot inventory (`spot_inventory_fp`) and
//! external derivatives/options book exposure (`external_delta_fp` from `margin-risk-rs`),
//! operating in pure `1e8` fixed-point arithmetic (`0` floating-point instructions).
//!
//! ## Control Law
//! 1. **Net Exposure Excess**:
//!    $$\Delta_{\text{excess}} = (\text{spot\_inventory\_fp} + \text{external\_delta\_fp}) - \text{target\_delta\_fp}$$
//! 2. **Deadband Hysteresis (`|excess| <= hedge_band_fp`)**:
//!    - Suppresses churn inside the deadband; cancels any leftover passive hedge quote.
//! 3. **Two-Regime Urgency Execution**:
//!    - **Passive Maker Band (`hedge_band_fp < |excess| <= 3 * hedge_band_fp`)**:
//!      Quotes `ORDER_TYPE_POST_ONLY` on the passive touch (`best_ask_fp` to sell excess positive delta,
//!      `best_bid_fp` to buy against negative delta), repricing only when top-of-book drifts by $\ge 2$ ticks.
//!    - **Urgent Taker Band (`|excess| > 3 * hedge_band_fp`)**:
//!      Cancels resting passive quote and sweeps across the spread via `ORDER_TYPE_IOC` clipped to
//!      `max_order_qty_fp` and aligned to `lot_size_fp`.

use hyperion_quant_sdk::abi::*;
use hyperion_quant_sdk::{
    export_quant_strategy, fp, BookL2View, LimitOrderRequest, OrderFillView, QuantContext,
    QuantStrategy, StateReader, StateWriter, TradeTickView,
};

pub struct DeltaHedgerBot {
    cfg: StrategyInitBlock,
    spot_inventory_fp: i64,
    external_delta_fp: i64,
    last_best_bid_fp: u64,
    last_best_ask_fp: u64,
    resting_hedge_oid: u64,
    resting_hedge_side: u8,
    resting_hedge_price_fp: u64,
    next_client_order_id: u64,
    last_hedge_ts_ns: u64,
    hedge_exec_count: u64,
}

impl DeltaHedgerBot {
    pub const fn const_new() -> Self {
        Self {
            cfg: StrategyInitBlock::default_btc(),
            spot_inventory_fp: 0,
            external_delta_fp: 0,
            last_best_bid_fp: 0,
            last_best_ask_fp: 0,
            resting_hedge_oid: 0,
            resting_hedge_side: SIDE_UNSPECIFIED,
            resting_hedge_price_fp: 0,
            next_client_order_id: 1,
            last_hedge_ts_ns: 0,
            hedge_exec_count: 0,
        }
    }

    #[inline]
    fn net_delta_excess_fp(&self) -> i64 {
        self.spot_inventory_fp
            .saturating_add(self.external_delta_fp)
            .saturating_sub(self.cfg.target_delta_fp)
    }

    fn evaluate_hedge(
        &mut self,
        instrument_id: u32,
        best_bid_fp: u64,
        best_ask_fp: u64,
        ts_ns: u64,
        ctx: &mut QuantContext<'_>,
    ) {
        if best_bid_fp == 0 || best_ask_fp == 0 || best_bid_fp >= best_ask_fp {
            return;
        }
        let excess_fp = self.net_delta_excess_fp();
        let abs_excess = excess_fp.unsigned_abs();
        let band = self.cfg.hedge_band_fp.unsigned_abs().max(self.cfg.lot_size_fp);

        // Inside neutrality deadband: cancel stale resting hedge if any and hold flat.
        if abs_excess <= band {
            if self.resting_hedge_oid != 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.resting_hedge_oid, cid, ts_ns);
                self.resting_hedge_oid = 0;
                self.resting_hedge_side = SIDE_UNSPECIFIED;
                self.resting_hedge_price_fp = 0;
            }
            return;
        }

        if self.last_hedge_ts_ns > 0
            && ts_ns < self.last_hedge_ts_ns.saturating_add(self.cfg.cooldown_ns)
        {
            return;
        }

        let desired_side = if excess_fp > 0 { SIDE_SELL } else { SIDE_BUY };
        let raw_clip = abs_excess.min(self.cfg.max_order_qty_fp);
        let hedge_qty = fp::align_lot_qty(raw_clip, self.cfg.lot_size_fp);
        if hedge_qty == 0 {
            return;
        }

        let urgent = abs_excess > band.saturating_mul(3);
        if urgent {
            if self.resting_hedge_oid != 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.resting_hedge_oid, cid, ts_ns);
                self.resting_hedge_oid = 0;
                self.resting_hedge_side = SIDE_UNSPECIFIED;
                self.resting_hedge_price_fp = 0;
            }
            let cross_px = if desired_side == SIDE_SELL {
                fp::align_bid_tick(best_bid_fp, self.cfg.tick_size_fp)
            } else {
                fp::align_ask_tick(best_ask_fp, self.cfg.tick_size_fp)
            };
            let cid = self.next_client_order_id;
            self.next_client_order_id = cid.wrapping_add(1);
            if ctx
                .submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id,
                    side: desired_side,
                    order_type: ORDER_TYPE_IOC,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: cross_px,
                    qty_fp: hedge_qty,
                    max_slippage_bps: 30,
                    client_ts_ns: ts_ns,
                    strategy_tag: 0x4448_4447_5F55_5247, // "DHDG_URG"
                })
                .is_ok()
            {
                self.last_hedge_ts_ns = ts_ns;
                self.hedge_exec_count = self.hedge_exec_count.saturating_add(1);
            }
        } else {
            let passive_px = if desired_side == SIDE_SELL {
                fp::align_ask_tick(best_ask_fp, self.cfg.tick_size_fp)
            } else {
                fp::align_bid_tick(best_bid_fp, self.cfg.tick_size_fp)
            };
            let two_ticks = self.cfg.tick_size_fp.saturating_mul(2);
            let drifted = self.resting_hedge_price_fp.abs_diff(passive_px) >= two_ticks;

            if self.resting_hedge_oid != 0
                && (self.resting_hedge_side != desired_side || drifted)
            {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                let _ = ctx.cancel_order(self.resting_hedge_oid, cid, ts_ns);
                self.resting_hedge_oid = 0;
            }

            if self.resting_hedge_oid == 0 {
                let cid = self.next_client_order_id;
                self.next_client_order_id = cid.wrapping_add(1);
                if let Ok(oid) = ctx.submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id,
                    side: desired_side,
                    order_type: ORDER_TYPE_POST_ONLY,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: passive_px,
                    qty_fp: hedge_qty,
                    max_slippage_bps: 0,
                    client_ts_ns: ts_ns,
                    strategy_tag: 0x4448_4447_5F4D_4B52, // "DHDG_MKR"
                }) {
                    self.resting_hedge_oid = oid;
                    self.resting_hedge_side = desired_side;
                    self.resting_hedge_price_fp = passive_px;
                    self.last_hedge_ts_ns = ts_ns;
                    self.hedge_exec_count = self.hedge_exec_count.saturating_add(1);
                }
            }
        }
    }
}

impl QuantStrategy for DeltaHedgerBot {
    const STRATEGY_KIND: u32 = STRATEGY_KIND_DELTA_HEDGER;

    fn new() -> Self {
        Self::const_new()
    }

    fn on_init(&mut self, cfg: &StrategyInitBlock, _ctx: &mut QuantContext<'_>) -> i32 {
        if cfg.tick_size_fp == 0 || cfg.lot_size_fp == 0 || cfg.max_order_qty_fp == 0 {
            return CALLBACK_ERR_BAD_CONFIG;
        }
        self.cfg = *cfg;
        CALLBACK_OK
    }

    fn on_book_l2_tick(&mut self, book: &BookL2View, ctx: &mut QuantContext<'_>) -> i32 {
        self.last_best_bid_fp = book.best_bid_fp;
        self.last_best_ask_fp = book.best_ask_fp;
        self.evaluate_hedge(
            book.instrument_id,
            book.best_bid_fp,
            book.best_ask_fp,
            book.ts_ns,
            ctx,
        );
        CALLBACK_OK
    }

    fn on_trade_tick(&mut self, _trade: &TradeTickView, _ctx: &mut QuantContext<'_>) -> i32 {
        CALLBACK_OK
    }

    fn on_order_fill(&mut self, fill: &OrderFillView, ctx: &mut QuantContext<'_>) -> i32 {
        self.spot_inventory_fp = fill.net_inventory_fp;
        self.external_delta_fp = fill.external_delta_fp;
        if fill.order_id == self.resting_hedge_oid && fill.remaining_qty_fp == 0 {
            self.resting_hedge_oid = 0;
            self.resting_hedge_side = SIDE_UNSPECIFIED;
            self.resting_hedge_price_fp = 0;
        }
        // Immediately react to unhedged fill delta if book quotes are already cached!
        if self.last_best_bid_fp > 0 && self.last_best_ask_fp > self.last_best_bid_fp {
            self.evaluate_hedge(
                fill.instrument_id,
                self.last_best_bid_fp,
                self.last_best_ask_fp,
                fill.ts_ns,
                ctx,
            );
        }
        CALLBACK_OK
    }

    fn export_state(&self, writer: &mut StateWriter<'_>) -> (i64, u64, u64, bool) {
        let ok = writer.write_i64(self.spot_inventory_fp)
            && writer.write_i64(self.external_delta_fp)
            && writer.write_u64(self.last_best_bid_fp)
            && writer.write_u64(self.last_best_ask_fp)
            && writer.write_u64(self.resting_hedge_oid)
            && writer.write_u8(self.resting_hedge_side)
            && writer.write_u64(self.resting_hedge_price_fp)
            && writer.write_u64(self.next_client_order_id)
            && writer.write_u64(self.last_hedge_ts_ns)
            && writer.write_u64(self.hedge_exec_count);
        (
            self.spot_inventory_fp,
            self.resting_hedge_oid,
            self.next_client_order_id,
            ok,
        )
    }

    fn import_state(&mut self, reader: &mut StateReader<'_>, _hdr: &StateSnapshotRegion) -> bool {
        let Some(spot_inventory_fp) = reader.read_i64() else { return false };
        let Some(external_delta_fp) = reader.read_i64() else { return false };
        let Some(last_best_bid_fp) = reader.read_u64() else { return false };
        let Some(last_best_ask_fp) = reader.read_u64() else { return false };
        let Some(resting_hedge_oid) = reader.read_u64() else { return false };
        let Some(resting_hedge_side) = reader.read_u8() else { return false };
        let Some(resting_hedge_price_fp) = reader.read_u64() else { return false };
        let Some(next_client_order_id) = reader.read_u64() else { return false };
        let Some(last_hedge_ts_ns) = reader.read_u64() else { return false };
        let Some(hedge_exec_count) = reader.read_u64() else { return false };

        self.spot_inventory_fp = spot_inventory_fp;
        self.external_delta_fp = external_delta_fp;
        self.last_best_bid_fp = last_best_bid_fp;
        self.last_best_ask_fp = last_best_ask_fp;
        self.resting_hedge_oid = resting_hedge_oid;
        self.resting_hedge_side = resting_hedge_side;
        self.resting_hedge_price_fp = resting_hedge_price_fp;
        self.next_client_order_id = next_client_order_id;
        self.last_hedge_ts_ns = last_hedge_ts_ns;
        self.hedge_exec_count = hedge_exec_count;
        true
    }
}

export_quant_strategy!(DeltaHedgerBot, DeltaHedgerBot::const_new());
