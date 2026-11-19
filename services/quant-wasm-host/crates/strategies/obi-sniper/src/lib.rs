#![no_std]

//! # `obi_sniper.wasm` — 5-Level Liquidity Slope & Order-Book Imbalance Microstructure Sniper
//!
//! Ultra-low-latency microstructure taker strategy evaluating 5-level depth decay and
//! price-distance liquidity slope (`0` floating-point instructions).
//!
//! ## Quantitative Signal Formulation
//! 1. **Exponentially Weighted 5-Level OBI (`dw_obi_ppm`)**:
//!    - Level weights $w = [100, 75, 50, 30, 15]$ emphasize near-touch queue pressure:
//!      $$\text{OBI}^{\text{dw}}_t = \frac{\sum_{k=0}^{4} w_k (Q^{\text{bid}}_k - Q^{\text{ask}}_k)}{\sum_{k=0}^{4} w_k (Q^{\text{bid}}_k + Q^{\text{ask}}_k)} \times 10^6$$
//! 2. **5-Level Liquidity Slope Asymmetry (`slope_ppm`)**:
//!    - Measures elasticity of resting depth per tick away from mid-price:
//!      $$S^{\text{bid}}_t = \sum_{k=0}^{4} \frac{Q^{\text{bid}}_k \cdot \text{tick}}{\max(\text{tick}, P^{\text{mid}}_t - P^{\text{bid}}_k)}, \quad S^{\text{ask}}_t = \sum_{k=0}^{4} \frac{Q^{\text{ask}}_k \cdot \text{tick}}{\max(\text{tick}, P^{\text{ask}}_k - P^{\text{mid}}_t)}$$
//!      $$\text{SlopeAsym}_t = \frac{S^{\text{bid}}_t - S^{\text{ask}}_t}{S^{\text{bid}}_t + S^{\text{ask}}_t} \times 10^6$$
//! 3. **Sniper Trigger**:
//!    - Fires an immediate-or-cancel (`ORDER_TYPE_IOC`) sweep at `best_ask_fp` (BUY) or
//!      `best_bid_fp` (SELL) iff spread $\le \text{max\_spread\_fp}$, both `dw_obi_ppm` and
//!      `slope_ppm` exceed threshold, aggressive trade flow aligns, and cooldown has elapsed.

use hyperion_quant_sdk::abi::*;
use hyperion_quant_sdk::{
    export_quant_strategy, fp, BookL2View, LimitOrderRequest, OrderFillView, QuantContext,
    QuantStrategy, StateReader, StateWriter, TradeTickView,
};

const LEVEL_WEIGHTS: [u128; L2_BOOK_LEVELS] = [100, 75, 50, 30, 15];

pub struct ObiSniperBot {
    cfg: StrategyInitBlock,
    trade_impulse_fp: i64,
    net_inventory_fp: i64,
    last_snipe_ts_ns: u64,
    next_client_order_id: u64,
    snipe_count: u64,
    last_dw_obi_ppm: i64,
    last_slope_ppm: i64,
}

impl ObiSniperBot {
    pub const fn const_new() -> Self {
        Self {
            cfg: StrategyInitBlock::default_btc(),
            trade_impulse_fp: 0,
            net_inventory_fp: 0,
            last_snipe_ts_ns: 0,
            next_client_order_id: 1,
            snipe_count: 0,
            last_dw_obi_ppm: 0,
            last_slope_ppm: 0,
        }
    }

    #[inline]
    pub fn compute_5level_obi_and_slope(book: &BookL2View, tick_fp: u64) -> (i64, i64) {
        let safe_tick = tick_fp.max(1) as u128;
        let mid = book.mid_price_fp as u128;

        let mut w_bid: u128 = 0;
        let mut w_ask: u128 = 0;
        let mut slope_bid: u128 = 0;
        let mut slope_ask: u128 = 0;

        let mut i = 0;
        while i < L2_BOOK_LEVELS {
            let w = LEVEL_WEIGHTS[i];
            let bq = book.bids_qty_fp[i] as u128;
            let aq = book.asks_qty_fp[i] as u128;
            w_bid += w * bq;
            w_ask += w * aq;

            let bp = book.bids_price_fp[i] as u128;
            if bp > 0 && bq > 0 {
                let dist_b = mid.saturating_sub(bp).max(safe_tick);
                slope_bid += (bq * safe_tick) / dist_b;
            }
            let ap = book.asks_price_fp[i] as u128;
            if ap > 0 && aq > 0 {
                let dist_a = ap.saturating_sub(mid).max(safe_tick);
                slope_ask += (aq * safe_tick) / dist_a;
            }
            i += 1;
        }

        let dw_obi_ppm = fp::ratio_ppm(
            (w_bid as i128) - (w_ask as i128),
            (w_bid + w_ask) as i128,
        );
        let slope_ppm = fp::ratio_ppm(
            (slope_bid as i128) - (slope_ask as i128),
            (slope_bid + slope_ask) as i128,
        );
        (dw_obi_ppm, slope_ppm)
    }
}

impl QuantStrategy for ObiSniperBot {
    const STRATEGY_KIND: u32 = STRATEGY_KIND_OBI_SNIPER;

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
        if self.cfg.max_spread_fp > 0 && book.spread_fp > self.cfg.max_spread_fp {
            return CALLBACK_OK;
        }
        if self.last_snipe_ts_ns > 0
            && book.ts_ns < self.last_snipe_ts_ns.saturating_add(self.cfg.cooldown_ns)
        {
            return CALLBACK_OK;
        }

        let (dw_obi_ppm, slope_ppm) =
            Self::compute_5level_obi_and_slope(book, self.cfg.tick_size_fp);
        self.last_dw_obi_ppm = dw_obi_ppm;
        self.last_slope_ppm = slope_ppm;

        let thresh = self.cfg.obi_threshold_ppm;
        let slope_thresh = (thresh * 3) / 5; // 60% slope confirmation
        let snipe_qty = fp::align_lot_qty(
            self.cfg.order_qty_fp.min(self.cfg.max_order_qty_fp),
            self.cfg.lot_size_fp,
        );
        if snipe_qty == 0 {
            return CALLBACK_OK;
        }

        // Decay trade impulse slightly on passive book updates.
        self.trade_impulse_fp = fp::ewma_fp(self.trade_impulse_fp, 0, 100_000);

        if dw_obi_ppm >= thresh
            && slope_ppm >= slope_thresh
            && self.trade_impulse_fp >= -(snipe_qty as i64)
            && self.net_inventory_fp.saturating_add(snipe_qty as i64) <= self.cfg.max_position_fp
        {
            let ask_px = fp::align_ask_tick(book.best_ask_fp, self.cfg.tick_size_fp);
            let cid = self.next_client_order_id;
            self.next_client_order_id = cid.wrapping_add(1);
            if ctx
                .submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_BUY,
                    order_type: ORDER_TYPE_IOC,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: ask_px,
                    qty_fp: snipe_qty,
                    max_slippage_bps: 15,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x4F42_495F_534E_5042, // "OBI_SNPB"
                })
                .is_ok()
            {
                self.last_snipe_ts_ns = book.ts_ns;
                self.snipe_count = self.snipe_count.saturating_add(1);
            }
        } else if dw_obi_ppm <= -thresh
            && slope_ppm <= -slope_thresh
            && self.trade_impulse_fp <= (snipe_qty as i64)
            && self.net_inventory_fp.saturating_sub(snipe_qty as i64) >= -self.cfg.max_position_fp
        {
            let bid_px = fp::align_bid_tick(book.best_bid_fp, self.cfg.tick_size_fp);
            let cid = self.next_client_order_id;
            self.next_client_order_id = cid.wrapping_add(1);
            if ctx
                .submit_limit_order(LimitOrderRequest {
                    client_order_id: cid,
                    instrument_id: book.instrument_id,
                    side: SIDE_SELL,
                    order_type: ORDER_TYPE_IOC,
                    stp_mode: self.cfg.stp_mode,
                    price_fp: bid_px,
                    qty_fp: snipe_qty,
                    max_slippage_bps: 15,
                    client_ts_ns: book.ts_ns,
                    strategy_tag: 0x4F42_495F_534E_5053, // "OBI_SNPS"
                })
                .is_ok()
            {
                self.last_snipe_ts_ns = book.ts_ns;
                self.snipe_count = self.snipe_count.saturating_add(1);
            }
        }

        CALLBACK_OK
    }

    fn on_trade_tick(&mut self, trade: &TradeTickView, _ctx: &mut QuantContext<'_>) -> i32 {
        let signed = if trade.aggressor_side == SIDE_BUY {
            trade.qty_fp as i64
        } else if trade.aggressor_side == SIDE_SELL {
            -(trade.qty_fp as i64)
        } else {
            0
        };
        self.trade_impulse_fp = fp::ewma_fp(self.trade_impulse_fp, signed, 450_000);
        CALLBACK_OK
    }

    fn on_order_fill(&mut self, fill: &OrderFillView, _ctx: &mut QuantContext<'_>) -> i32 {
        self.net_inventory_fp = fill.net_inventory_fp;
        CALLBACK_OK
    }

    fn export_state(&self, writer: &mut StateWriter<'_>) -> (i64, u64, u64, bool) {
        let ok = writer.write_i64(self.trade_impulse_fp)
            && writer.write_i64(self.net_inventory_fp)
            && writer.write_u64(self.last_snipe_ts_ns)
            && writer.write_u64(self.next_client_order_id)
            && writer.write_u64(self.snipe_count)
            && writer.write_i64(self.last_dw_obi_ppm)
            && writer.write_i64(self.last_slope_ppm);
        (self.net_inventory_fp, 0, self.next_client_order_id, ok)
    }

    fn import_state(&mut self, reader: &mut StateReader<'_>, _hdr: &StateSnapshotRegion) -> bool {
        let Some(trade_impulse_fp) = reader.read_i64() else { return false };
        let Some(net_inventory_fp) = reader.read_i64() else { return false };
        let Some(last_snipe_ts_ns) = reader.read_u64() else { return false };
        let Some(next_client_order_id) = reader.read_u64() else { return false };
        let Some(snipe_count) = reader.read_u64() else { return false };
        let Some(last_dw_obi_ppm) = reader.read_i64() else { return false };
        let Some(last_slope_ppm) = reader.read_i64() else { return false };

        self.trade_impulse_fp = trade_impulse_fp;
        self.net_inventory_fp = net_inventory_fp;
        self.last_snipe_ts_ns = last_snipe_ts_ns;
        self.next_client_order_id = next_client_order_id;
        self.snipe_count = snipe_count;
        self.last_dw_obi_ppm = last_dw_obi_ppm;
        self.last_slope_ppm = last_slope_ppm;
        true
    }
}

export_quant_strategy!(ObiSniperBot, ObiSniperBot::const_new());
