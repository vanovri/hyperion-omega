use crate::config::instrument_spec_for;
use crate::fixed_point::{
    calc_imbalance_ppm, calc_mid_price_fp, calc_spread, calc_vwap_fp, quantize_price_bucket,
    FP_SCALE_I128,
};
use hyperion_proto::{
    GetVolumeProfileResponse, I128, L2BookSnapshot, L3BookDelta, L3DeltaOp,
    LiquidityHeatmapBucket, MicrostructureMetrics, OrderSide, U128, VolumeProfileBin,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Rolling window size in 1-second buckets (`64` seconds — power of two for bitmask indexing!).
pub const ROLLING_SEC_SLOTS: usize = 64;
const ROLLING_SEC_MASK: usize = ROLLING_SEC_SLOTS - 1;

/// Rolling OLS sample window for Kyle's Lambda (`64` observations).
pub const LAMBDA_RING_SLOTS: usize = 64;

/// Maximum distinct active price levels retained per book side to guarantee bounded memory.
pub const MAX_BOOK_LEVELS: usize = 2048;

/// Maximum heatmap price cells stored per historical time column (`64` levels around mid-price).
pub const MAX_HEATMAP_CELLS_PER_COL: usize = 64;

/// Number of circular Volume Profile time slices (`256` slices x `15s` = 64 minutes bounded window).
pub const VOLUME_PROFILE_SLICES: usize = 256;
pub const VOLUME_PROFILE_SLICE_NS: u64 = 15_000_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VwapSecSlot {
    pub sec: u64,
    pub raw_pq_sum: u128,
    pub qty_sum: u128,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfiSecSlot {
    pub sec: u64,
    pub ofi_delta_fp: i128,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LambdaSample {
    pub x_flow_scaled: i128,
    pub y_dp_scaled: i128,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatmapCell {
    pub price_bucket_fp: u64,
    pub resting_bid_qty_fp: u64,
    pub resting_ask_qty_fp: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeatmapColumn {
    pub occupied: bool,
    pub bucket_ts_ns: u64,
    pub cells: Vec<HeatmapCell>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileSlice {
    pub occupied: bool,
    pub slice_open_ts_ns: u64,
    /// `price_fp -> (buy_vol_fp, sell_vol_fp)`
    pub bins: FxHashMap<u64, (u128, u128)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RestingOrderEntry {
    side: i32,
    price_fp: u64,
    qty_fp: u64,
}

mod serde_array64 {
    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::fmt;
    use std::marker::PhantomData;

    pub fn serialize<S, T>(arr: &[T; 64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let mut tup = serializer.serialize_tuple(64)?;
        for elem in arr {
            tup.serialize_element(elem)?;
        }
        tup.end()
    }

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<[T; 64], D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Copy + Default,
    {
        struct Array64Visitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de> + Copy + Default> Visitor<'de> for Array64Visitor<T> {
            type Value = [T; 64];
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array of length 64")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = [T::default(); 64];
                for item in &mut out {
                    *item = seq
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(64, &self))?;
                }
                Ok(out)
            }
        }
        deserializer.deserialize_tuple(64, Array64Visitor(PhantomData))
    }
}

/// Cache-line aligned quantitative microstructure & L2/L3 order-book tracker per symbol.
#[repr(C, align(64))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolMicrostructureState {
    symbol: String,
    default_heatmap_bucket_fp: u64,
    heatmap_time_bucket_ns: u64,

    // Order book price levels & cached top-20 arrays
    bids: BTreeMap<u64, u64>,
    asks: BTreeMap<u64, u64>,
    resting_orders: FxHashMap<u64, RestingOrderEntry>,
    top_bids: [(u64, u64); 20],
    top_bids_len: usize,
    top_asks: [(u64, u64); 20],
    top_asks_len: usize,
    bid_sum_5: u128,
    bid_sum_10: u128,
    bid_sum_20: u128,
    ask_sum_5: u128,
    ask_sum_10: u128,
    ask_sum_20: u128,

    // Previous BBO for Cont-Kukanov-Stoikov (2014) OFI transitions
    has_prev_bbo: bool,
    prev_best_bid_px: u64,
    prev_best_bid_qty: u64,
    prev_best_ask_px: u64,
    prev_best_ask_qty: u64,

    // Cumulative & 64-second rolling OFI
    cumulative_ofi_fp: i128,
    rolling_ofi_fp: i128,
    #[serde(with = "serde_array64")]
    ofi_ring: [OfiSecSlot; ROLLING_SEC_SLOTS],
    last_ofi_sec: u64,
    has_ofi_sec: bool,

    // Cumulative & 64-second rolling VWAP
    last_trade_price_fp: u64,
    session_raw_pq_sum: u128,
    session_qty_sum: u128,
    rolling_raw_pq_sum: u128,
    rolling_qty_sum: u128,
    #[serde(with = "serde_array64")]
    vwap_ring: [VwapSecSlot; ROLLING_SEC_SLOTS],
    last_vwap_sec: u64,
    has_vwap_sec: bool,

    // 64-second rolling carry-forward TWAP
    #[serde(with = "serde_array64")]
    twap_ring: [u64; ROLLING_SEC_SLOTS],
    twap_sum: u128,
    twap_filled_len: usize,
    last_twap_sec: u64,
    last_ref_price_fp: u64,

    // O(1) Rolling OLS Kyle's Lambda (`Cov(dP, Q_signed) / Var(Q_signed)`)
    #[serde(with = "serde_array64")]
    lambda_ring: [LambdaSample; LAMBDA_RING_SLOTS],
    lambda_head: usize,
    lambda_len: usize,
    lambda_sum_x: i128,
    lambda_sum_y: i128,
    lambda_sum_xx: i128,
    lambda_sum_xy: i128,
    bar_anchor_price_fp: u64,
    bar_last_price_fp: u64,
    bar_signed_flow_fp: i128,
    bar_event_count: u32,
    bar_sec: u64,
    kyles_lambda_fp: i64,

    // Latest snapshot scalars
    last_engine_seq: u64,
    last_ts_ns: u64,
    mid_price_fp: u64,
    spread_fp: u64,
    spread_bps: u32,
    imbalance_top5_ppm: i64,
    imbalance_top10_ppm: i64,
    imbalance_top20_ppm: i64,

    // 60fps Historical Liquidity Heatmap Matrix (bounded ring)
    heatmap_ring: Vec<HeatmapColumn>,
    heatmap_head: usize,
    heatmap_len: usize,
    heatmap_capacity: usize,
    current_heatmap_bucket_ts: u64,
    heatmap_current_dirty: bool,
    #[serde(skip)]
    pending_heatmap_flush: Vec<LiquidityHeatmapBucket>,

    // Bounded circular Volume Profile slices
    profile_ring: Vec<ProfileSlice>,
    profile_head: usize,
    profile_len: usize,
}

impl SymbolMicrostructureState {
    pub fn new(symbol: impl Into<String>, heatmap_time_bucket_ns: u64, heatmap_columns: usize) -> Self {
        let sym = symbol.into();
        let spec = instrument_spec_for(&sym);
        let hm_cap = heatmap_columns.max(32);
        let mut heatmap_ring = Vec::with_capacity(hm_cap);
        for _ in 0..hm_cap {
            heatmap_ring.push(HeatmapColumn {
                occupied: false,
                bucket_ts_ns: 0,
                cells: Vec::with_capacity(MAX_HEATMAP_CELLS_PER_COL),
            });
        }

        let mut profile_ring = Vec::with_capacity(VOLUME_PROFILE_SLICES);
        for _ in 0..VOLUME_PROFILE_SLICES {
            profile_ring.push(ProfileSlice {
                occupied: false,
                slice_open_ts_ns: 0,
                bins: FxHashMap::with_capacity_and_hasher(128, Default::default()),
            });
        }

        Self {
            symbol: sym,
            default_heatmap_bucket_fp: spec.default_heatmap_bucket_fp,
            heatmap_time_bucket_ns: heatmap_time_bucket_ns.max(10_000_000),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            resting_orders: FxHashMap::with_capacity_and_hasher(2048, Default::default()),
            top_bids: [(0, 0); 20],
            top_bids_len: 0,
            top_asks: [(0, 0); 20],
            top_asks_len: 0,
            bid_sum_5: 0,
            bid_sum_10: 0,
            bid_sum_20: 0,
            ask_sum_5: 0,
            ask_sum_10: 0,
            ask_sum_20: 0,
            has_prev_bbo: false,
            prev_best_bid_px: 0,
            prev_best_bid_qty: 0,
            prev_best_ask_px: 0,
            prev_best_ask_qty: 0,
            cumulative_ofi_fp: 0,
            rolling_ofi_fp: 0,
            ofi_ring: [OfiSecSlot::default(); ROLLING_SEC_SLOTS],
            last_ofi_sec: 0,
            has_ofi_sec: false,
            last_trade_price_fp: 0,
            session_raw_pq_sum: 0,
            session_qty_sum: 0,
            rolling_raw_pq_sum: 0,
            rolling_qty_sum: 0,
            vwap_ring: [VwapSecSlot::default(); ROLLING_SEC_SLOTS],
            last_vwap_sec: 0,
            has_vwap_sec: false,
            twap_ring: [0; ROLLING_SEC_SLOTS],
            twap_sum: 0,
            twap_filled_len: 0,
            last_twap_sec: 0,
            last_ref_price_fp: 0,
            lambda_ring: [LambdaSample::default(); LAMBDA_RING_SLOTS],
            lambda_head: 0,
            lambda_len: 0,
            lambda_sum_x: 0,
            lambda_sum_y: 0,
            lambda_sum_xx: 0,
            lambda_sum_xy: 0,
            bar_anchor_price_fp: 0,
            bar_last_price_fp: 0,
            bar_signed_flow_fp: 0,
            bar_event_count: 0,
            bar_sec: 0,
            kyles_lambda_fp: 0,
            last_engine_seq: 0,
            last_ts_ns: 0,
            mid_price_fp: 0,
            spread_fp: 0,
            spread_bps: 0,
            imbalance_top5_ppm: 0,
            imbalance_top10_ppm: 0,
            imbalance_top20_ppm: 0,
            heatmap_ring,
            heatmap_head: 0,
            heatmap_len: 0,
            heatmap_capacity: hm_cap,
            current_heatmap_bucket_ts: 0,
            heatmap_current_dirty: false,
            pending_heatmap_flush: Vec::with_capacity(512),
            profile_ring,
            profile_head: 0,
            profile_len: 0,
        }
    }

    #[inline(always)]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    #[inline(always)]
    pub fn cumulative_ofi_fp(&self) -> i128 {
        self.cumulative_ofi_fp
    }

    #[inline(always)]
    pub fn rolling_ofi_fp(&self) -> i128 {
        self.rolling_ofi_fp
    }

    #[inline(always)]
    pub fn session_vwap_fp(&self) -> u64 {
        calc_vwap_fp(self.session_raw_pq_sum, self.session_qty_sum, self.mid_price_fp)
    }

    #[inline(always)]
    pub fn rolling_vwap_fp(&self) -> u64 {
        let fallback = if self.last_trade_price_fp > 0 {
            self.last_trade_price_fp
        } else {
            self.mid_price_fp
        };
        calc_vwap_fp(self.rolling_raw_pq_sum, self.rolling_qty_sum, fallback)
    }

    #[inline(always)]
    pub fn rolling_twap_fp(&self) -> u64 {
        if self.twap_filled_len == 0 {
            return if self.mid_price_fp > 0 {
                self.mid_price_fp
            } else {
                self.last_trade_price_fp
            };
        }
        (self.twap_sum / (self.twap_filled_len as u128)) as u64
    }

    #[inline]
    fn refresh_bids_top20(&mut self) {
        self.top_bids_len = 0;
        let mut b5: u128 = 0;
        let mut b10: u128 = 0;
        let mut b20: u128 = 0;
        for (&px, &qty) in self.bids.iter().rev().take(20) {
            let idx = self.top_bids_len;
            self.top_bids[idx] = (px, qty);
            self.top_bids_len = idx + 1;
            let q = qty as u128;
            if idx < 5 {
                b5 += q;
            }
            if idx < 10 {
                b10 += q;
            }
            b20 += q;
        }
        self.bid_sum_5 = b5;
        self.bid_sum_10 = b10;
        self.bid_sum_20 = b20;
    }

    #[inline]
    fn refresh_asks_top20(&mut self) {
        self.top_asks_len = 0;
        let mut a5: u128 = 0;
        let mut a10: u128 = 0;
        let mut a20: u128 = 0;
        for (&px, &qty) in self.asks.iter().take(20) {
            let idx = self.top_asks_len;
            self.top_asks[idx] = (px, qty);
            self.top_asks_len = idx + 1;
            let q = qty as u128;
            if idx < 5 {
                a5 += q;
            }
            if idx < 10 {
                a10 += q;
            }
            a20 += q;
        }
        self.ask_sum_5 = a5;
        self.ask_sum_10 = a10;
        self.ask_sum_20 = a20;
    }

    #[inline]
    fn recompute_imbalance_and_spread(&mut self) {
        self.imbalance_top5_ppm = calc_imbalance_ppm(self.bid_sum_5, self.ask_sum_5);
        self.imbalance_top10_ppm = calc_imbalance_ppm(self.bid_sum_10, self.ask_sum_10);
        self.imbalance_top20_ppm = calc_imbalance_ppm(self.bid_sum_20, self.ask_sum_20);

        let best_bid = if self.top_bids_len > 0 {
            self.top_bids[0].0
        } else {
            0
        };
        let best_ask = if self.top_asks_len > 0 {
            self.top_asks[0].0
        } else {
            0
        };
        self.mid_price_fp = calc_mid_price_fp(best_bid, best_ask, self.last_trade_price_fp);
        let (sp, bps) = calc_spread(best_bid, best_ask, self.mid_price_fp);
        self.spread_fp = sp;
        self.spread_bps = bps;
    }

    #[inline]
    fn refresh_top20_and_imbalances(&mut self) {
        self.refresh_bids_top20();
        self.refresh_asks_top20();
        self.recompute_imbalance_and_spread();
    }

    #[inline]
    fn refresh_top20_for_side(&mut self, side: OrderSide, price_fp: u64) {
        match side {
            OrderSide::Buy => {
                if self.top_bids_len == 20 && price_fp < self.top_bids[19].0 {
                    return;
                }
                self.refresh_bids_top20();
                self.recompute_imbalance_and_spread();
            }
            OrderSide::Sell => {
                if self.top_asks_len == 20 && price_fp > self.top_asks[19].0 {
                    return;
                }
                self.refresh_asks_top20();
                self.recompute_imbalance_and_spread();
            }
            OrderSide::Unspecified => {}
        }
    }

    /// Exact Cont-Kukanov-Stoikov (2014) BBO transition step (`O(1)`).
    #[inline]
    fn record_bbo_ofi_transition(&mut self, ts_ns: u64) {
        let (cur_b_px, cur_b_qty) = if self.top_bids_len > 0 {
            self.top_bids[0]
        } else {
            (0, 0)
        };
        let (cur_a_px, cur_a_qty) = if self.top_asks_len > 0 {
            self.top_asks[0]
        } else {
            (0, 0)
        };

        if !self.has_prev_bbo {
            if cur_b_px > 0 || cur_a_px > 0 {
                self.has_prev_bbo = true;
                self.prev_best_bid_px = cur_b_px;
                self.prev_best_bid_qty = cur_b_qty;
                self.prev_best_ask_px = cur_a_px;
                self.prev_best_ask_qty = cur_a_qty;
            }
            return;
        }

        let e_bid: i128 = {
            let add_term = if cur_b_px > 0 && cur_b_px >= self.prev_best_bid_px {
                cur_b_qty as i128
            } else {
                0
            };
            let sub_term = if self.prev_best_bid_px > 0
                && (cur_b_px == 0 || cur_b_px <= self.prev_best_bid_px)
            {
                self.prev_best_bid_qty as i128
            } else {
                0
            };
            add_term - sub_term
        };

        let e_ask: i128 = {
            let add_term = if cur_a_px > 0
                && (self.prev_best_ask_px == 0 || cur_a_px <= self.prev_best_ask_px)
            {
                cur_a_qty as i128
            } else {
                0
            };
            let sub_term = if self.prev_best_ask_px > 0
                && (cur_a_px == 0 || cur_a_px >= self.prev_best_ask_px)
            {
                self.prev_best_ask_qty as i128
            } else {
                0
            };
            add_term - sub_term
        };

        let delta = e_bid - e_ask;
        self.prev_best_bid_px = cur_b_px;
        self.prev_best_bid_qty = cur_b_qty;
        self.prev_best_ask_px = cur_a_px;
        self.prev_best_ask_qty = cur_a_qty;

        if delta != 0 {
            self.add_ofi_delta(ts_ns, delta);
        }
    }

    #[inline]
    fn add_ofi_delta(&mut self, ts_ns: u64, delta: i128) {
        self.cumulative_ofi_fp = self.cumulative_ofi_fp.saturating_add(delta);
        let sec = ts_ns / 1_000_000_000;
        if !self.has_ofi_sec {
            self.has_ofi_sec = true;
            self.last_ofi_sec = sec;
            let slot = (sec as usize) & ROLLING_SEC_MASK;
            self.ofi_ring[slot] = OfiSecSlot {
                sec,
                ofi_delta_fp: delta,
            };
            self.rolling_ofi_fp = delta;
            return;
        }

        if sec > self.last_ofi_sec {
            let jump = sec - self.last_ofi_sec;
            if jump >= (ROLLING_SEC_SLOTS as u64) {
                self.ofi_ring = [OfiSecSlot::default(); ROLLING_SEC_SLOTS];
                self.rolling_ofi_fp = 0;
            } else {
                for s in (self.last_ofi_sec + 1)..=sec {
                    let idx = (s as usize) & ROLLING_SEC_MASK;
                    self.rolling_ofi_fp -= self.ofi_ring[idx].ofi_delta_fp;
                    self.ofi_ring[idx] = OfiSecSlot {
                        sec: s,
                        ofi_delta_fp: 0,
                    };
                }
            }
            self.last_ofi_sec = sec;
        }

        if self.last_ofi_sec.saturating_sub(sec) < (ROLLING_SEC_SLOTS as u64) {
            let idx = (sec as usize) & ROLLING_SEC_MASK;
            if self.ofi_ring[idx].sec != sec {
                self.rolling_ofi_fp -= self.ofi_ring[idx].ofi_delta_fp;
                self.ofi_ring[idx] = OfiSecSlot {
                    sec,
                    ofi_delta_fp: 0,
                };
            }
            self.ofi_ring[idx].ofi_delta_fp += delta;
            self.rolling_ofi_fp += delta;
        }
    }

    #[inline]
    fn update_rolling_twap(&mut self, ts_ns: u64, price_fp: u64) {
        if price_fp == 0 {
            return;
        }
        let sec = ts_ns / 1_000_000_000;
        if self.twap_filled_len == 0 {
            let idx = (sec as usize) & ROLLING_SEC_MASK;
            self.twap_ring[idx] = price_fp;
            self.twap_sum = price_fp as u128;
            self.twap_filled_len = 1;
            self.last_twap_sec = sec;
            self.last_ref_price_fp = price_fp;
            return;
        }

        if sec > self.last_twap_sec {
            let gap = sec - self.last_twap_sec;
            if gap >= (ROLLING_SEC_SLOTS as u64) {
                // Fast O(1) bulk fill when gap >= 64s
                self.twap_ring.fill(self.last_ref_price_fp);
                self.twap_filled_len = ROLLING_SEC_SLOTS;
                self.twap_sum = (self.last_ref_price_fp as u128) * (ROLLING_SEC_SLOTS as u128);
                let cur_idx = (sec as usize) & ROLLING_SEC_MASK;
                self.twap_sum -= self.twap_ring[cur_idx] as u128;
                self.twap_ring[cur_idx] = price_fp;
                self.twap_sum += price_fp as u128;
            } else {
                for s in (self.last_twap_sec + 1)..=sec {
                    let idx = (s as usize) & ROLLING_SEC_MASK;
                    let fill_px = if s == sec {
                        price_fp
                    } else {
                        self.last_ref_price_fp
                    };
                    if self.twap_filled_len < ROLLING_SEC_SLOTS {
                        self.twap_ring[idx] = fill_px;
                        self.twap_sum += fill_px as u128;
                        self.twap_filled_len += 1;
                    } else {
                        self.twap_sum -= self.twap_ring[idx] as u128;
                        self.twap_ring[idx] = fill_px;
                        self.twap_sum += fill_px as u128;
                    }
                }
            }
            self.last_twap_sec = sec;
            self.last_ref_price_fp = price_fp;
        } else if sec == self.last_twap_sec {
            let idx = (sec as usize) & ROLLING_SEC_MASK;
            self.twap_sum -= self.twap_ring[idx] as u128;
            self.twap_ring[idx] = price_fp;
            self.twap_sum += price_fp as u128;
            self.last_ref_price_fp = price_fp;
        }
    }

    #[inline]
    fn push_lambda_sample(&mut self, signed_flow_fp: i128, delta_price_fp: i128) {
        // Normalize by 1e4 so squares in OLS stay strictly < 1e20 inside i128 (max 1.7e38).
        let x = signed_flow_fp / 10_000;
        let y = delta_price_fp / 10_000;
        if x == 0 && y == 0 {
            return;
        }

        if self.lambda_len == LAMBDA_RING_SLOTS {
            let old = self.lambda_ring[self.lambda_head];
            self.lambda_sum_x -= old.x_flow_scaled;
            self.lambda_sum_y -= old.y_dp_scaled;
            self.lambda_sum_xx -= old.x_flow_scaled * old.x_flow_scaled;
            self.lambda_sum_xy -= old.x_flow_scaled * old.y_dp_scaled;
        } else {
            self.lambda_len += 1;
        }

        self.lambda_ring[self.lambda_head] = LambdaSample {
            x_flow_scaled: x,
            y_dp_scaled: y,
        };
        self.lambda_head = (self.lambda_head + 1) % LAMBDA_RING_SLOTS;

        self.lambda_sum_x += x;
        self.lambda_sum_y += y;
        self.lambda_sum_xx += x * x;
        self.lambda_sum_xy += x * y;

        let n = self.lambda_len as i128;
        if n >= 2 {
            let denom = n * self.lambda_sum_xx - (self.lambda_sum_x * self.lambda_sum_x);
            if denom > 0 {
                let numer = n * self.lambda_sum_xy - (self.lambda_sum_x * self.lambda_sum_y);
                let scaled = (numer.saturating_mul(FP_SCALE_I128)) / denom;
                self.kyles_lambda_fp = scaled.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            }
        }
    }

    #[inline]
    fn maybe_snapshot_heatmap(&mut self, ts_ns: u64, queue_for_flush: bool) {
        if self.bids.is_empty() && self.asks.is_empty() {
            return;
        }
        let col_ts = (ts_ns / self.heatmap_time_bucket_ns) * self.heatmap_time_bucket_ns;
        if self.heatmap_len > 0 && col_ts == self.current_heatmap_bucket_ts {
            // Lazy O(1) dirty flag — defer 64-bucket matrix packing until column rollover or flush/query!
            self.heatmap_current_dirty = true;
            return;
        }

        if self.heatmap_len > 0 {
            let prev_idx = (self.heatmap_head + self.heatmap_capacity - 1) % self.heatmap_capacity;
            if self.heatmap_current_dirty {
                let prev_ts = self.current_heatmap_bucket_ts;
                self.populate_heatmap_column(prev_idx, prev_ts, false);
                self.heatmap_current_dirty = false;
            }
            if queue_for_flush {
                let prev_col = &self.heatmap_ring[prev_idx];
                if prev_col.occupied && self.pending_heatmap_flush.len() < 4096 {
                    for cell in &prev_col.cells {
                        self.pending_heatmap_flush.push(LiquidityHeatmapBucket {
                            bucket_ts_ns: prev_col.bucket_ts_ns,
                            price_bucket_fp: cell.price_bucket_fp,
                            resting_bid_qty_fp: cell.resting_bid_qty_fp,
                            resting_ask_qty_fp: cell.resting_ask_qty_fp,
                        });
                    }
                }
            }
        }

        let write_idx = self.heatmap_head;
        self.heatmap_head = (self.heatmap_head + 1) % self.heatmap_capacity;
        if self.heatmap_len < self.heatmap_capacity {
            self.heatmap_len += 1;
        }
        self.current_heatmap_bucket_ts = col_ts;
        self.populate_heatmap_column(write_idx, col_ts, false);
        self.heatmap_current_dirty = false;
    }

    #[inline]
    fn populate_heatmap_column(&mut self, col_idx: usize, bucket_ts_ns: u64, _unused: bool) {
        let step = self.default_heatmap_bucket_fp;
        let col = &mut self.heatmap_ring[col_idx];
        col.occupied = true;
        col.bucket_ts_ns = bucket_ts_ns;
        col.cells.clear();

        // Aggregate top 32 bid levels and top 32 ask levels into quantized heatmap buckets
        for (&px, &qty) in self.bids.iter().rev().take(32) {
            let b_px = quantize_price_bucket(px, step);
            if let Some(last) = col.cells.iter_mut().find(|c| c.price_bucket_fp == b_px) {
                last.resting_bid_qty_fp = last.resting_bid_qty_fp.saturating_add(qty);
            } else if col.cells.len() < MAX_HEATMAP_CELLS_PER_COL {
                col.cells.push(HeatmapCell {
                    price_bucket_fp: b_px,
                    resting_bid_qty_fp: qty,
                    resting_ask_qty_fp: 0,
                });
            }
        }
        for (&px, &qty) in self.asks.iter().take(32) {
            let b_px = quantize_price_bucket(px, step);
            if let Some(last) = col.cells.iter_mut().find(|c| c.price_bucket_fp == b_px) {
                last.resting_ask_qty_fp = last.resting_ask_qty_fp.saturating_add(qty);
            } else if col.cells.len() < MAX_HEATMAP_CELLS_PER_COL {
                col.cells.push(HeatmapCell {
                    price_bucket_fp: b_px,
                    resting_bid_qty_fp: 0,
                    resting_ask_qty_fp: qty,
                });
            }
        }
        col.cells.sort_unstable_by_key(|c| c.price_bucket_fp);
    }

    fn enforce_book_level_cap(&mut self) {
        while self.bids.len() > MAX_BOOK_LEVELS {
            if let Some((&worst_bid, _)) = self.bids.iter().next() {
                self.bids.remove(&worst_bid);
            } else {
                break;
            }
        }
        while self.asks.len() > MAX_BOOK_LEVELS {
            if let Some((&worst_ask, _)) = self.asks.iter().next_back() {
                self.asks.remove(&worst_ask);
            } else {
                break;
            }
        }
        if self.resting_orders.len() > MAX_BOOK_LEVELS * 2 {
            self.resting_orders.clear();
        }
    }

    #[inline]
    fn apply_level_delta(&mut self, side: OrderSide, price_fp: u64, delta_qty_fp: i64) {
        if price_fp == 0 || delta_qty_fp == 0 {
            return;
        }
        let book = match side {
            OrderSide::Buy => &mut self.bids,
            OrderSide::Sell => &mut self.asks,
            OrderSide::Unspecified => return,
        };
        let cur = book.get(&price_fp).copied().unwrap_or(0);
        let next = if delta_qty_fp >= 0 {
            cur.saturating_add(delta_qty_fp as u64)
        } else {
            cur.saturating_sub(delta_qty_fp.unsigned_abs())
        };
        if next == 0 {
            book.remove(&price_fp);
        } else {
            book.insert(price_fp, next);
        }
    }

    /// Applies an `L2BookSnapshot` idempotently.
    pub fn apply_l2_snapshot(&mut self, snap: &L2BookSnapshot, queue_for_flush: bool) {
        self.bids.clear();
        self.asks.clear();
        self.resting_orders.clear();
        for lvl in &snap.bids {
            if lvl.price_fp > 0 && lvl.total_qty_fp > 0 {
                self.bids.insert(lvl.price_fp, lvl.total_qty_fp);
            }
        }
        for lvl in &snap.asks {
            if lvl.price_fp > 0 && lvl.total_qty_fp > 0 {
                self.asks.insert(lvl.price_fp, lvl.total_qty_fp);
            }
        }
        self.enforce_book_level_cap();
        self.refresh_top20_and_imbalances();
        self.record_bbo_ofi_transition(snap.snapshot_ts_ns);
        self.update_rolling_twap(snap.snapshot_ts_ns, self.mid_price_fp);
        self.maybe_snapshot_heatmap(snap.snapshot_ts_ns, queue_for_flush);
        if snap.engine_seq > self.last_engine_seq {
            self.last_engine_seq = snap.engine_seq;
        }
        if snap.snapshot_ts_ns > self.last_ts_ns {
            self.last_ts_ns = snap.snapshot_ts_ns;
        }
    }

    /// Applies an incremental `L3BookDelta` in `O(1)` bounded time.
    pub fn apply_l3_delta(&mut self, delta: &L3BookDelta, queue_for_flush: bool) {
        self.apply_l3_fields(
            delta.engine_seq,
            delta.op,
            delta.side,
            delta.order_id,
            delta.price_fp,
            delta.remaining_qty_fp,
            delta.delta_qty_fp,
            delta.event_ts_ns,
            queue_for_flush,
        );
    }

    /// Zero-allocation scalar fast path for `CompactL3Delta` (`O(1)` bounded time).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_l3_fields(
        &mut self,
        engine_seq: u64,
        op_raw: i32,
        side_raw: i32,
        order_id: u64,
        price_fp: u64,
        remaining_qty_fp: u64,
        delta_qty_fp: i64,
        event_ts_ns: u64,
        queue_for_flush: bool,
    ) {
        let op = L3DeltaOp::try_from(op_raw).unwrap_or(L3DeltaOp::Unspecified);
        let side = OrderSide::try_from(side_raw).unwrap_or(OrderSide::Unspecified);

        let mut full_refresh = false;
        match op {
            L3DeltaOp::Clear => {
                self.bids.clear();
                self.asks.clear();
                self.resting_orders.clear();
                full_refresh = true;
            }
            L3DeltaOp::Add | L3DeltaOp::Modify | L3DeltaOp::Cancel | L3DeltaOp::Fill => {
                if delta_qty_fp != 0 {
                    self.apply_level_delta(side, price_fp, delta_qty_fp);
                } else if order_id > 0 {
                    // Fallback order-state inference when producer omits signed delta_qty_fp
                    let prev = self.resting_orders.remove(&order_id);
                    if let Some(old) = prev {
                        let old_side =
                            OrderSide::try_from(old.side).unwrap_or(OrderSide::Unspecified);
                        self.apply_level_delta(old_side, old.price_fp, -(old.qty_fp as i64));
                        full_refresh = true;
                    }
                    if remaining_qty_fp > 0 && !matches!(op, L3DeltaOp::Cancel) {
                        self.apply_level_delta(side, price_fp, remaining_qty_fp as i64);
                        self.resting_orders.insert(
                            order_id,
                            RestingOrderEntry {
                                side: side_raw,
                                price_fp,
                                qty_fp: remaining_qty_fp,
                            },
                        );
                    }
                }
            }
            L3DeltaOp::Unspecified => {}
        }

        if self.bids.len() > MAX_BOOK_LEVELS || self.asks.len() > MAX_BOOK_LEVELS {
            self.enforce_book_level_cap();
        }
        if full_refresh {
            self.refresh_top20_and_imbalances();
        } else {
            self.refresh_top20_for_side(side, price_fp);
        }
        self.record_bbo_ofi_transition(event_ts_ns);
        if self.mid_price_fp > 0 {
            self.update_rolling_twap(event_ts_ns, self.mid_price_fp);
        }
        self.maybe_snapshot_heatmap(event_ts_ns, queue_for_flush);

        if engine_seq > self.last_engine_seq {
            self.last_engine_seq = engine_seq;
        }
        if event_ts_ns > self.last_ts_ns {
            self.last_ts_ns = event_ts_ns;
        }
    }

    /// Updates trade-driven microstructure (`Rolling VWAP`, `Rolling TWAP`, `Kyle's Lambda`, `Volume Profile`,
    /// and trade-fallback `OFI` if no L2/L3 book levels exist).
    pub fn apply_execution_fill(
        &mut self,
        exec_ts_ns: u64,
        engine_seq: u64,
        price_fp: u64,
        qty_fp: u64,
        aggressor_side: OrderSide,
    ) {
        let raw_pq = (price_fp as u128) * (qty_fp as u128);
        let qty_u128 = qty_fp as u128;
        self.last_trade_price_fp = price_fp;
        self.session_raw_pq_sum = self.session_raw_pq_sum.saturating_add(raw_pq);
        self.session_qty_sum = self.session_qty_sum.saturating_add(qty_u128);

        // 1. Update 64-second rolling VWAP ring (`O(1)`)
        let sec = exec_ts_ns / 1_000_000_000;
        if !self.has_vwap_sec {
            self.has_vwap_sec = true;
            self.last_vwap_sec = sec;
            let idx = (sec as usize) & ROLLING_SEC_MASK;
            self.vwap_ring[idx] = VwapSecSlot {
                sec,
                raw_pq_sum: raw_pq,
                qty_sum: qty_u128,
            };
            self.rolling_raw_pq_sum = raw_pq;
            self.rolling_qty_sum = qty_u128;
        } else {
            if sec > self.last_vwap_sec {
                let jump = sec - self.last_vwap_sec;
                if jump >= (ROLLING_SEC_SLOTS as u64) {
                    self.vwap_ring = [VwapSecSlot::default(); ROLLING_SEC_SLOTS];
                    self.rolling_raw_pq_sum = 0;
                    self.rolling_qty_sum = 0;
                } else {
                    for s in (self.last_vwap_sec + 1)..=sec {
                        let idx = (s as usize) & ROLLING_SEC_MASK;
                        self.rolling_raw_pq_sum = self
                            .rolling_raw_pq_sum
                            .saturating_sub(self.vwap_ring[idx].raw_pq_sum);
                        self.rolling_qty_sum = self
                            .rolling_qty_sum
                            .saturating_sub(self.vwap_ring[idx].qty_sum);
                        self.vwap_ring[idx] = VwapSecSlot {
                            sec: s,
                            raw_pq_sum: 0,
                            qty_sum: 0,
                        };
                    }
                }
                self.last_vwap_sec = sec;
            }
            if self.last_vwap_sec.saturating_sub(sec) < (ROLLING_SEC_SLOTS as u64) {
                let idx = (sec as usize) & ROLLING_SEC_MASK;
                if self.vwap_ring[idx].sec != sec {
                    self.rolling_raw_pq_sum = self
                        .rolling_raw_pq_sum
                        .saturating_sub(self.vwap_ring[idx].raw_pq_sum);
                    self.rolling_qty_sum = self
                        .rolling_qty_sum
                        .saturating_sub(self.vwap_ring[idx].qty_sum);
                    self.vwap_ring[idx] = VwapSecSlot {
                        sec,
                        raw_pq_sum: 0,
                        qty_sum: 0,
                    };
                }
                self.vwap_ring[idx].raw_pq_sum =
                    self.vwap_ring[idx].raw_pq_sum.saturating_add(raw_pq);
                self.vwap_ring[idx].qty_sum =
                    self.vwap_ring[idx].qty_sum.saturating_add(qty_u128);
                self.rolling_raw_pq_sum = self.rolling_raw_pq_sum.saturating_add(raw_pq);
                self.rolling_qty_sum = self.rolling_qty_sum.saturating_add(qty_u128);
            }
        }

        // 2. If book is empty (execution-only stream), fallback OFI uses signed taker flow
        let signed_flow: i128 = match aggressor_side {
            OrderSide::Buy => qty_fp as i128,
            OrderSide::Sell | OrderSide::Unspecified => -(qty_fp as i128),
        };
        if !self.has_prev_bbo && self.bids.is_empty() && self.asks.is_empty() {
            self.add_ofi_delta(exec_ts_ns, signed_flow);
            self.mid_price_fp = price_fp;
        }

        // 3. Rolling TWAP update
        let ref_px = if self.mid_price_fp > 0 {
            self.mid_price_fp
        } else {
            price_fp
        };
        self.update_rolling_twap(exec_ts_ns, ref_px);

        // 4. Kyle's Lambda bar sampling (contemporaneous dP_k = close_k - close_{k-1} vs Q_k)
        if self.bar_anchor_price_fp == 0 {
            self.bar_anchor_price_fp = ref_px;
            self.bar_last_price_fp = ref_px;
            self.bar_sec = sec;
            self.bar_signed_flow_fp = signed_flow;
            self.bar_event_count = 1;
        } else if sec > self.bar_sec || self.bar_event_count >= 32 {
            let dp = (self.bar_last_price_fp as i128) - (self.bar_anchor_price_fp as i128);
            self.push_lambda_sample(self.bar_signed_flow_fp, dp);
            self.bar_anchor_price_fp = self.bar_last_price_fp;
            self.bar_last_price_fp = ref_px;
            self.bar_sec = sec;
            self.bar_signed_flow_fp = signed_flow;
            self.bar_event_count = 1;
        } else {
            self.bar_last_price_fp = ref_px;
            self.bar_signed_flow_fp += signed_flow;
            self.bar_event_count += 1;
        }

        // 5. Bounded Volume Profile slice update (`O(1)`)
        self.record_volume_profile_fill(exec_ts_ns, price_fp, qty_u128, aggressor_side);

        if engine_seq > self.last_engine_seq {
            self.last_engine_seq = engine_seq;
        }
        if exec_ts_ns > self.last_ts_ns {
            self.last_ts_ns = exec_ts_ns;
        }
    }

    #[inline]
    fn record_volume_profile_fill(
        &mut self,
        exec_ts_ns: u64,
        price_fp: u64,
        qty_u128: u128,
        aggressor_side: OrderSide,
    ) {
        let slice_open = (exec_ts_ns / VOLUME_PROFILE_SLICE_NS) * VOLUME_PROFILE_SLICE_NS;
        let cur_idx = if self.profile_len > 0 {
            let prev = (self.profile_head + VOLUME_PROFILE_SLICES - 1) % VOLUME_PROFILE_SLICES;
            if self.profile_ring[prev].occupied
                && self.profile_ring[prev].slice_open_ts_ns == slice_open
            {
                Some(prev)
            } else {
                None
            }
        } else {
            None
        };

        let target_idx = match cur_idx {
            Some(idx) => idx,
            None => {
                let idx = self.profile_head;
                self.profile_head = (self.profile_head + 1) % VOLUME_PROFILE_SLICES;
                if self.profile_len < VOLUME_PROFILE_SLICES {
                    self.profile_len += 1;
                }
                let slot = &mut self.profile_ring[idx];
                slot.occupied = true;
                slot.slice_open_ts_ns = slice_open;
                slot.bins.clear();
                idx
            }
        };

        let bucket_px = quantize_price_bucket(price_fp, self.default_heatmap_bucket_fp / 10);
        let bins = &mut self.profile_ring[target_idx].bins;
        if bins.len() >= 512 && !bins.contains_key(&bucket_px) {
            return;
        }
        let entry = bins.entry(bucket_px).or_insert((0, 0));
        match aggressor_side {
            OrderSide::Buy => entry.0 = entry.0.saturating_add(qty_u128),
            OrderSide::Sell | OrderSide::Unspecified => entry.1 = entry.1.saturating_add(qty_u128),
        }
    }

    pub fn snapshot_metrics(&self) -> MicrostructureMetrics {
        MicrostructureMetrics {
            symbol: self.symbol.clone(),
            engine_seq: self.last_engine_seq,
            ts_ns: self.last_ts_ns,
            mid_price_fp: self.mid_price_fp,
            spread_fp: self.spread_fp,
            spread_bps: self.spread_bps,
            rolling_vwap_fp: self.rolling_vwap_fp(),
            rolling_twap_fp: self.rolling_twap_fp(),
            ofi_fp: Some(I128::from(self.rolling_ofi_fp)),
            book_imbalance_top5_ppm: self.imbalance_top5_ppm,
            book_imbalance_top10_ppm: self.imbalance_top10_ppm,
            book_imbalance_top20_ppm: self.imbalance_top20_ppm,
            kyles_lambda_fp: self.kyles_lambda_fp,
        }
    }

    pub fn drain_pending_heatmap(&mut self) -> Vec<LiquidityHeatmapBucket> {
        let mut out = Vec::with_capacity(self.pending_heatmap_flush.len() + 32);
        out.append(&mut self.pending_heatmap_flush);
        if self.heatmap_len > 0 {
            let cur_idx = (self.heatmap_head + self.heatmap_capacity - 1) % self.heatmap_capacity;
            if self.heatmap_current_dirty {
                let cur_ts = self.current_heatmap_bucket_ts;
                self.populate_heatmap_column(cur_idx, cur_ts, false);
                self.heatmap_current_dirty = false;
            }
            let cur = &self.heatmap_ring[cur_idx];
            if cur.occupied {
                for cell in &cur.cells {
                    out.push(LiquidityHeatmapBucket {
                        bucket_ts_ns: cur.bucket_ts_ns,
                        price_bucket_fp: cell.price_bucket_fp,
                        resting_bid_qty_fp: cell.resting_bid_qty_fp,
                        resting_ask_qty_fp: cell.resting_ask_qty_fp,
                    });
                }
            }
        }
        out
    }

    pub fn query_heatmap(
        &self,
        from_ts_ns: u64,
        to_ts_ns: u64,
        price_bucket_size_fp: u64,
        time_bucket_ns: u64,
    ) -> Vec<LiquidityHeatmapBucket> {
        let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
        let px_step = if price_bucket_size_fp == 0 {
            self.default_heatmap_bucket_fp
        } else {
            price_bucket_size_fp
        };
        let t_step = if time_bucket_ns == 0 {
            self.heatmap_time_bucket_ns
        } else {
            time_bucket_ns
        };

        let mut agg: BTreeMap<(u64, u64), (u64, u64)> = BTreeMap::new();
        let start = if self.heatmap_len == self.heatmap_capacity {
            self.heatmap_head
        } else {
            0
        };
        for i in 0..self.heatmap_len {
            let idx = (start + i) % self.heatmap_capacity;
            let col = &self.heatmap_ring[idx];
            if !col.occupied || col.bucket_ts_ns < from_ts_ns || col.bucket_ts_ns > upper {
                continue;
            }
            let t_key = (col.bucket_ts_ns / t_step) * t_step;
            for cell in &col.cells {
                let p_key = quantize_price_bucket(cell.price_bucket_fp, px_step);
                let e = agg.entry((t_key, p_key)).or_insert((0, 0));
                // Latest column within t_key wins when downsampling time; sum across merged price bins
                e.0 = e.0.saturating_add(cell.resting_bid_qty_fp);
                e.1 = e.1.saturating_add(cell.resting_ask_qty_fp);
            }
        }

        agg.into_iter()
            .map(|((t, p), (b, a))| LiquidityHeatmapBucket {
                bucket_ts_ns: t,
                price_bucket_fp: p,
                resting_bid_qty_fp: b,
                resting_ask_qty_fp: a,
            })
            .collect()
    }

    /// Computes horizontal Volume Profile + Point of Control (`POC`) + 70% Value Area (`VAH`, `VAL`).
    pub fn query_volume_profile(
        &self,
        from_ts_ns: u64,
        to_ts_ns: u64,
        bucket_size_fp: u64,
    ) -> GetVolumeProfileResponse {
        let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
        let px_step = if bucket_size_fp == 0 {
            self.default_heatmap_bucket_fp
        } else {
            bucket_size_fp
        };

        let mut hist: BTreeMap<u64, (u128, u128)> = BTreeMap::new();
        let start = if self.profile_len == VOLUME_PROFILE_SLICES {
            self.profile_head
        } else {
            0
        };
        for i in 0..self.profile_len {
            let idx = (start + i) % VOLUME_PROFILE_SLICES;
            let slice = &self.profile_ring[idx];
            if !slice.occupied {
                continue;
            }
            let slice_end = slice.slice_open_ts_ns.saturating_add(VOLUME_PROFILE_SLICE_NS);
            if slice_end < from_ts_ns || slice.slice_open_ts_ns > upper {
                continue;
            }
            for (&raw_px, &(buy_v, sell_v)) in &slice.bins {
                let b_px = quantize_price_bucket(raw_px, px_step);
                let e = hist.entry(b_px).or_insert((0, 0));
                e.0 = e.0.saturating_add(buy_v);
                e.1 = e.1.saturating_add(sell_v);
            }
        }

        if hist.is_empty() {
            return GetVolumeProfileResponse {
                symbol: self.symbol.clone(),
                poc_price_fp: 0,
                value_area_high_fp: 0,
                value_area_low_fp: 0,
                bins: Vec::new(),
            };
        }

        let mut raw_bins: Vec<(u64, u128, u128, u128)> = Vec::with_capacity(hist.len());
        let mut total_vol_all: u128 = 0;
        let mut poc_idx: usize = 0;
        let mut max_bin_vol: u128 = 0;

        for (px, (bv, sv)) in hist {
            let tv = bv.saturating_add(sv);
            total_vol_all = total_vol_all.saturating_add(tv);
            if tv >= max_bin_vol {
                max_bin_vol = tv;
                poc_idx = raw_bins.len();
            }
            raw_bins.push((px, bv, sv, tv));
        }

        // Standard 70% Value Area outward expansion from POC
        let target_va_vol = (total_vol_all.saturating_mul(700_000)) / 1_000_000;
        let mut low_idx = poc_idx;
        let mut high_idx = poc_idx;
        let mut acc_vol = raw_bins[poc_idx].3;

        while acc_vol < target_va_vol && (low_idx > 0 || high_idx + 1 < raw_bins.len()) {
            let next_up = if high_idx + 1 < raw_bins.len() {
                Some(raw_bins[high_idx + 1].3)
            } else {
                None
            };
            let next_down = if low_idx > 0 {
                Some(raw_bins[low_idx - 1].3)
            } else {
                None
            };

            match (next_up, next_down) {
                (Some(uv), Some(dv)) => {
                    if uv >= dv {
                        high_idx += 1;
                        acc_vol = acc_vol.saturating_add(uv);
                    } else {
                        low_idx -= 1;
                        acc_vol = acc_vol.saturating_add(dv);
                    }
                }
                (Some(uv), None) => {
                    high_idx += 1;
                    acc_vol = acc_vol.saturating_add(uv);
                }
                (None, Some(dv)) => {
                    low_idx -= 1;
                    acc_vol = acc_vol.saturating_add(dv);
                }
                (None, None) => break,
            }
        }

        let poc_price_fp = raw_bins[poc_idx].0;
        let value_area_low_fp = raw_bins[low_idx].0;
        let value_area_high_fp = raw_bins[high_idx].0;

        let bins = raw_bins
            .into_iter()
            .map(|(px, bv, sv, tv)| VolumeProfileBin {
                price_bucket_fp: px,
                buy_volume_fp: Some(U128::from(bv)),
                sell_volume_fp: Some(U128::from(sv)),
                total_volume_fp: Some(U128::from(tv)),
            })
            .collect();

        GetVolumeProfileResponse {
            symbol: self.symbol.clone(),
            poc_price_fp,
            value_area_high_fp,
            value_area_low_fp,
            bins,
        }
    }

    pub fn allocated_capacity_proxy(&self) -> usize {
        self.heatmap_ring
            .iter()
            .map(|c| c.cells.capacity())
            .sum::<usize>()
            + self
                .profile_ring
                .iter()
                .map(|p| p.bins.capacity())
                .sum::<usize>()
            + self.resting_orders.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperion_proto::PriceLevel;

    #[test]
    fn cont_kukanov_stoikov_ofi_and_imbalance_exactness() {
        let mut st = SymbolMicrostructureState::new("BTC-USDT", 1_000_000_000, 32);
        let snap = L2BookSnapshot {
            engine_seq: 1,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            bids: vec![PriceLevel {
                price_fp: 6_800_000_000_000,
                total_qty_fp: 500_000_000, // 5 BTC
                order_count: 1,
            }],
            asks: vec![PriceLevel {
                price_fp: 6_801_000_000_000,
                total_qty_fp: 500_000_000, // 5 BTC
                order_count: 1,
            }],
            best_bid_fp: 6_800_000_000_000,
            best_ask_fp: 6_801_000_000_000,
            mid_price_fp: 6_800_500_000_000,
            spread_fp: 1_000_000_000,
            snapshot_ts_ns: 1_000_000_000,
            state_root_blake3: vec![],
        };
        st.apply_l2_snapshot(&snap, false);
        assert_eq!(st.imbalance_top5_ppm, 0);
        assert_eq!(st.cumulative_ofi_fp(), 0);

        // Add +3 BTC to Best Bid -> Bid=8, Ask=5 -> OFI = +3 BTC (300_000_000), Imbalance = (8-5)/(8+5) = 230_769 ppm
        let add_bid = L3BookDelta {
            engine_seq: 2,
            prev_engine_seq: 1,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            op: L3DeltaOp::Add as i32,
            side: OrderSide::Buy as i32,
            order_id: 101,
            account_id: 1,
            price_fp: 6_800_000_000_000,
            remaining_qty_fp: 300_000_000,
            delta_qty_fp: 300_000_000,
            event_ts_ns: 1_100_000_000,
            state_root_blake3: vec![],
            trace: None,
        };
        st.apply_l3_delta(&add_bid, false);
        assert_eq!(st.cumulative_ofi_fp(), 300_000_000);
        assert_eq!(st.rolling_ofi_fp(), 300_000_000);
        assert_eq!(st.imbalance_top5_ppm, 230_769);

        // Add +2 BTC to Best Ask -> e_ask = +2 BTC -> OFI delta = -2 BTC -> cumulative = +1 BTC
        let add_ask = L3BookDelta {
            engine_seq: 3,
            prev_engine_seq: 2,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            op: L3DeltaOp::Add as i32,
            side: OrderSide::Sell as i32,
            order_id: 102,
            account_id: 2,
            price_fp: 6_801_000_000_000,
            remaining_qty_fp: 200_000_000,
            delta_qty_fp: 200_000_000,
            event_ts_ns: 1_200_000_000,
            state_root_blake3: vec![],
            trace: None,
        };
        st.apply_l3_delta(&add_ask, false);
        assert_eq!(st.cumulative_ofi_fp(), 100_000_000);

        // Kyle's Lambda exact analytical OLS slope test (kills OB-03 numerator/covariance mutation)
        let mut km = SymbolMicrostructureState::new("BTC-USDT", 1_000_000_000, 32);
        let steps: [(u64, u64, u64, OrderSide); 6] = [
            (1_000_000_000, 10_000_000_000, 100_000_000, OrderSide::Buy),
            (2_000_000_000, 10_200_000_000, 200_000_000, OrderSide::Buy),
            (3_000_000_000, 10_600_000_000, 300_000_000, OrderSide::Buy),
            (4_000_000_000, 10_400_000_000, 100_000_000, OrderSide::Sell),
            (5_000_000_000, 10_100_000_000, 150_000_000, OrderSide::Sell),
            (6_000_000_000, 10_500_000_000, 250_000_000, OrderSide::Buy),
        ];
        for (idx, (ts, px, qty, side)) in steps.into_iter().enumerate() {
            km.apply_execution_fill(ts, (idx + 1) as u64, px, qty, side);
        }
        let metrics = km.snapshot_metrics();
        assert_eq!(
            metrics.kyles_lambda_fp, 147_297_297,
            "Kyle's Lambda must match exact analytical OLS slope (10.9e9 * 1e8 / 7.4e9)"
        );

        // 70% Value Area expansion test across 5 price bins: [10, 20, 40, 20, 10] -> POC=middle, VA=[2..4]
        let mut vp_st = SymbolMicrostructureState::new("BTC-USDT", 1_000_000_000, 32);
        let bins_data = [
            (100_000_000_u64, 10_000_000_u64),
            (200_000_000, 20_000_000),
            (300_000_000, 40_000_000),
            (400_000_000, 20_000_000),
            (500_000_000, 10_000_000),
        ];
        for (i, (px, qty)) in bins_data.into_iter().enumerate() {
            vp_st.apply_execution_fill(1_000_000_000 + i as u64, (i + 1) as u64, px, qty, OrderSide::Buy);
        }
        let vp = vp_st.query_volume_profile(0, u64::MAX, 100_000_000);
        assert_eq!(vp.poc_price_fp, 300_000_000);
        assert_eq!(vp.value_area_low_fp, 200_000_000);
        assert_eq!(vp.value_area_high_fp, 400_000_000);
    }
}
