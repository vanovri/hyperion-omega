use crate::fixed_point::{calc_notional_fp, calc_taker_buy_ratio_ppm, calc_vwap_fp};
use hyperion_proto::{CandleInterval, OhlcvCandle, OrderSide, U128};
use serde::{Deserialize, Serialize};

/// Number of sub-buckets per candle for deterministic, order-independent TWAP calculation.
pub const TWAP_SUB_SLICES: usize = 16;

/// Active direct-mapped power-of-two ring slots per timeframe (`64` slots -> tracked by a single `u64` dirty bitmask!).
pub const ACTIVE_RING_SLOTS: usize = 64;
const ACTIVE_RING_MASK: usize = ACTIVE_RING_SLOTS - 1;

/// Canonical supported candlestick timeframes (`1s`, `5s`, `1m`, `5m`, `15m`, `1h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Timeframe {
    S1 = 0,
    S5 = 1,
    M1 = 2,
    M5 = 3,
    M15 = 4,
    H1 = 5,
}

pub const ALL_TIMEFRAMES: [Timeframe; 6] = [
    Timeframe::S1,
    Timeframe::S5,
    Timeframe::M1,
    Timeframe::M5,
    Timeframe::M15,
    Timeframe::H1,
];

impl Timeframe {
    #[inline(always)]
    pub const fn duration_ns(self) -> u64 {
        match self {
            Self::S1 => 1_000_000_000,
            Self::S5 => 5_000_000_000,
            Self::M1 => 60_000_000_000,
            Self::M5 => 300_000_000_000,
            Self::M15 => 900_000_000_000,
            Self::H1 => 3_600_000_000_000,
        }
    }

    /// Exact `duration_ns / 16` divisor (every canonical interval is divisible by 16),
    /// eliminating software `u128` division on the hot path.
    #[inline(always)]
    pub const fn sub_slice_ns(self) -> u64 {
        match self {
            Self::S1 => 62_500_000,
            Self::S5 => 312_500_000,
            Self::M1 => 3_750_000_000,
            Self::M5 => 18_750_000_000,
            Self::M15 => 56_250_000_000,
            Self::H1 => 225_000_000_000,
        }
    }

    #[inline(always)]
    pub const fn code(self) -> &'static str {
        match self {
            Self::S1 => "1s",
            Self::S5 => "5s",
            Self::M1 => "1m",
            Self::M5 => "5m",
            Self::M15 => "15m",
            Self::H1 => "1h",
        }
    }

    #[inline(always)]
    pub const fn proto_interval(self) -> CandleInterval {
        match self {
            Self::S1 => CandleInterval::CandleInterval1s,
            Self::S5 => CandleInterval::CandleInterval5s,
            Self::M1 => CandleInterval::CandleInterval1m,
            Self::M5 => CandleInterval::CandleInterval5m,
            Self::M15 => CandleInterval::CandleInterval15m,
            Self::H1 => CandleInterval::CandleInterval1h,
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "1s" => Some(Self::S1),
            "5s" => Some(Self::S5),
            "1m" => Some(Self::M1),
            "5m" => Some(Self::M5),
            "15m" => Some(Self::M15),
            "1h" => Some(Self::H1),
            _ => None,
        }
    }

    pub fn from_proto_i32(val: i32) -> Option<Self> {
        match CandleInterval::try_from(val).ok()? {
            CandleInterval::CandleInterval1s => Some(Self::S1),
            CandleInterval::CandleInterval5s => Some(Self::S5),
            CandleInterval::CandleInterval1m => Some(Self::M1),
            CandleInterval::CandleInterval5m => Some(Self::M5),
            CandleInterval::CandleInterval15m => Some(Self::M15),
            CandleInterval::CandleInterval1h => Some(Self::H1),
            CandleInterval::Unspecified => None,
        }
    }
}

/// Sub-slice latest-price tracker for commutative 16-point TWAP inside a candle bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TwapSubSlice {
    pub latest_key: (u64, u64, u64),
    pub price_fp: u64,
}

/// Cache-line aligned `O(1)` incremental candlestick state.
/// Strictly zero heap fields; commutative under arbitrary out-of-order fill arrival!
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandleAccumulator {
    pub timeframe: Timeframe,
    pub occupied: bool,
    pub is_closed: bool,
    pub bucket_open_ts_ns: u64,
    pub bucket_close_ts_ns: u64,
    pub open_key: (u64, u64, u64),
    pub close_key: (u64, u64, u64),
    pub open_fp: u64,
    pub high_fp: u64,
    pub low_fp: u64,
    pub close_fp: u64,
    pub base_volume_fp: u128,
    pub quote_volume_fp: u128,
    pub raw_px_qty_sum: u128,
    pub taker_buy_base_volume_fp: u128,
    pub taker_sell_base_volume_fp: u128,
    pub trade_count: u64,
    pub last_engine_seq: u64,
    pub sub_slices: [TwapSubSlice; TWAP_SUB_SLICES],
}

impl Default for CandleAccumulator {
    fn default() -> Self {
        Self::empty(Timeframe::S1)
    }
}

impl CandleAccumulator {
    #[inline]
    pub const fn empty(timeframe: Timeframe) -> Self {
        Self {
            timeframe,
            occupied: false,
            is_closed: false,
            bucket_open_ts_ns: 0,
            bucket_close_ts_ns: 0,
            open_key: (u64::MAX, u64::MAX, u64::MAX),
            close_key: (0, 0, 0),
            open_fp: 0,
            high_fp: 0,
            low_fp: u64::MAX,
            close_fp: 0,
            base_volume_fp: 0,
            quote_volume_fp: 0,
            raw_px_qty_sum: 0,
            taker_buy_base_volume_fp: 0,
            taker_sell_base_volume_fp: 0,
            trade_count: 0,
            last_engine_seq: 0,
            sub_slices: [TwapSubSlice {
                latest_key: (0, 0, 0),
                price_fp: 0,
            }; TWAP_SUB_SLICES],
        }
    }

    #[inline]
    pub fn reset_for_bucket(&mut self, timeframe: Timeframe, bucket_open_ts_ns: u64) {
        let dur = timeframe.duration_ns();
        *self = Self::empty(timeframe);
        self.occupied = true;
        self.bucket_open_ts_ns = bucket_open_ts_ns;
        self.bucket_close_ts_ns = bucket_open_ts_ns.saturating_add(dur);
    }

    /// Updates this candle in `O(1)` time with zero allocations.
    /// Commutative & order-independent: `open_fp`, `close_fp`, `high_fp`, `low_fp`,
    /// `vwap_fp`, `twap_fp`, and volumes converge to identical values regardless of arrival permutation!
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill(
        &mut self,
        exec_ts_ns: u64,
        engine_seq: u64,
        fill_id: u64,
        price_fp: u64,
        qty_fp: u64,
        notional_override_fp: Option<u128>,
        aggressor_side: OrderSide,
    ) {
        let key = (exec_ts_ns, engine_seq, fill_id);
        if self.trade_count == 0 {
            self.open_key = key;
            self.close_key = key;
            self.open_fp = price_fp;
            self.high_fp = price_fp;
            self.low_fp = price_fp;
            self.close_fp = price_fp;
        } else {
            if key < self.open_key {
                self.open_key = key;
                self.open_fp = price_fp;
            }
            if key >= self.close_key {
                self.close_key = key;
                self.close_fp = price_fp;
            }
            if price_fp > self.high_fp {
                self.high_fp = price_fp;
            }
            if price_fp < self.low_fp {
                self.low_fp = price_fp;
            }
        }

        let qty_u128 = qty_fp as u128;
        let notional_fp =
            notional_override_fp.unwrap_or_else(|| calc_notional_fp(price_fp, qty_fp));
        self.base_volume_fp = self.base_volume_fp.saturating_add(qty_u128);
        self.quote_volume_fp = self.quote_volume_fp.saturating_add(notional_fp);
        self.raw_px_qty_sum = self
            .raw_px_qty_sum
            .saturating_add((price_fp as u128) * qty_u128);

        match aggressor_side {
            OrderSide::Buy => {
                self.taker_buy_base_volume_fp =
                    self.taker_buy_base_volume_fp.saturating_add(qty_u128);
            }
            OrderSide::Sell | OrderSide::Unspecified => {
                self.taker_sell_base_volume_fp =
                    self.taker_sell_base_volume_fp.saturating_add(qty_u128);
            }
        }

        self.trade_count = self.trade_count.saturating_add(1);
        if engine_seq > self.last_engine_seq {
            self.last_engine_seq = engine_seq;
        }

        // O(1) sub-slice update for 16-point time-weighted average price (pure 64-bit fast division)
        let rel_ns = exec_ts_ns.saturating_sub(self.bucket_open_ts_ns);
        let sub_idx =
            (rel_ns / self.timeframe.sub_slice_ns()).min((TWAP_SUB_SLICES - 1) as u64) as usize;
        let sub = &mut self.sub_slices[sub_idx];
        if sub.price_fp == 0 || key >= sub.latest_key {
            sub.latest_key = key;
            sub.price_fp = price_fp;
        }
    }

    #[inline]
    pub fn vwap_fp(&self) -> u64 {
        calc_vwap_fp(self.raw_px_qty_sum, self.base_volume_fp, self.close_fp)
    }

    /// Deterministic 16-slice carry-forward TWAP (`O(1)` 16-element unrolled sum).
    /// Leading slices prior to the first intra-bucket print anchor to `open_fp`.
    #[inline]
    pub fn twap_fp(&self) -> u64 {
        if self.trade_count == 0 {
            return 0;
        }
        let mut carry_px = self.open_fp;
        let mut sum: u128 = 0;
        for slice in &self.sub_slices {
            if slice.price_fp > 0 {
                carry_px = slice.price_fp;
            }
            sum += carry_px as u128;
        }
        (sum / (TWAP_SUB_SLICES as u128)) as u64
    }

    #[inline]
    pub fn taker_buy_ratio_ppm(&self) -> u32 {
        calc_taker_buy_ratio_ppm(self.taker_buy_base_volume_fp, self.base_volume_fp)
    }

    pub fn to_proto(&self, symbol: &str) -> OhlcvCandle {
        OhlcvCandle {
            symbol: symbol.to_string(),
            interval: self.timeframe.proto_interval() as i32,
            bucket_open_ts_ns: self.bucket_open_ts_ns,
            bucket_close_ts_ns: self.bucket_close_ts_ns,
            open_fp: self.open_fp,
            high_fp: self.high_fp,
            low_fp: self.low_fp,
            close_fp: self.close_fp,
            base_volume_fp: Some(U128::from(self.base_volume_fp)),
            quote_volume_fp: Some(U128::from(self.quote_volume_fp)),
            taker_buy_base_volume_fp: Some(U128::from(self.taker_buy_base_volume_fp)),
            taker_sell_base_volume_fp: Some(U128::from(self.taker_sell_base_volume_fp)),
            vwap_fp: self.vwap_fp(),
            twap_fp: self.twap_fp(),
            trade_count: self.trade_count,
            last_engine_seq: self.last_engine_seq,
            is_closed: self.is_closed,
        }
    }
}

/// Pre-allocated circular buffer for closed historical candles + `64`-slot active ring for a single timeframe.
/// Zero steady-state heap allocations!
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeframeCandleSeries {
    timeframe: Timeframe,
    active_ring: Vec<CandleAccumulator>,
    dirty_active_mask: u64,
    last_wm_bucket: u64,
    closed_history: Vec<CandleAccumulator>,
    history_head: usize,
    history_len: usize,
    history_capacity: usize,
    pending_closed_flush: Vec<CandleAccumulator>,
}

impl TimeframeCandleSeries {
    pub fn new(timeframe: Timeframe, history_capacity: usize) -> Self {
        let cap = history_capacity.max(64);
        Self {
            timeframe,
            active_ring: vec![CandleAccumulator::empty(timeframe); ACTIVE_RING_SLOTS],
            dirty_active_mask: 0,
            last_wm_bucket: 0,
            closed_history: vec![CandleAccumulator::empty(timeframe); cap],
            history_head: 0,
            history_len: 0,
            history_capacity: cap,
            pending_closed_flush: Vec::with_capacity(64),
        }
    }

    #[inline]
    fn push_closed_history(&mut self, mut candle: CandleAccumulator, queue_for_flush: bool) {
        if !candle.occupied || candle.trade_count == 0 {
            return;
        }
        candle.is_closed = true;
        // Overwrite if most recent closed bucket has identical bucket_open_ts_ns
        if self.history_len > 0 {
            let last_idx = (self.history_head + self.history_capacity - 1) % self.history_capacity;
            if self.closed_history[last_idx].bucket_open_ts_ns == candle.bucket_open_ts_ns {
                self.closed_history[last_idx] = candle;
                if queue_for_flush && self.pending_closed_flush.len() < self.history_capacity {
                    self.pending_closed_flush.push(candle);
                }
                return;
            }
        }
        self.closed_history[self.history_head] = candle;
        self.history_head = (self.history_head + 1) % self.history_capacity;
        if self.history_len < self.history_capacity {
            self.history_len += 1;
        }
        if queue_for_flush && self.pending_closed_flush.len() < self.history_capacity {
            self.pending_closed_flush.push(candle);
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill(
        &mut self,
        exec_ts_ns: u64,
        engine_seq: u64,
        fill_id: u64,
        price_fp: u64,
        qty_fp: u64,
        notional_override_fp: Option<u128>,
        aggressor_side: OrderSide,
        queue_evicted_for_flush: bool,
    ) {
        let dur = self.timeframe.duration_ns();
        let bucket_idx = exec_ts_ns / dur;
        let bucket_open_ts_ns = bucket_idx * dur;
        let slot = (bucket_idx as usize) & ACTIVE_RING_MASK;

        let existing = &self.active_ring[slot];
        if !existing.occupied {
            self.active_ring[slot].reset_for_bucket(self.timeframe, bucket_open_ts_ns);
        } else if existing.bucket_open_ts_ns != bucket_open_ts_ns {
            if bucket_open_ts_ns > existing.bucket_open_ts_ns {
                let evicted = *existing;
                self.push_closed_history(evicted, queue_evicted_for_flush);
                self.active_ring[slot].reset_for_bucket(self.timeframe, bucket_open_ts_ns);
            } else {
                // Event older than active ring horizon (64 intervals ago) -> ignore safely.
                return;
            }
        }

        self.active_ring[slot].apply_fill(
            exec_ts_ns,
            engine_seq,
            fill_id,
            price_fp,
            qty_fp,
            notional_override_fp,
            aggressor_side,
        );
        self.dirty_active_mask |= 1_u64 << slot;
    }

    /// Marks active buckets whose `bucket_close_ts_ns <= watermark_ts_ns` as `is_closed = true`
    /// and promotes them into `closed_history` while keeping them addressable until slot reuse.
    #[inline]
    pub fn advance_watermark(&mut self, watermark_ts_ns: u64, queue_for_flush: bool) {
        let wm_bucket = watermark_ts_ns / self.timeframe.duration_ns();
        if wm_bucket <= self.last_wm_bucket {
            return;
        }
        self.last_wm_bucket = wm_bucket;
        for slot in 0..ACTIVE_RING_SLOTS {
            let c = &mut self.active_ring[slot];
            if c.occupied && !c.is_closed && c.bucket_close_ts_ns <= watermark_ts_ns {
                c.is_closed = true;
                self.dirty_active_mask |= 1_u64 << slot;
                let closed_copy = *c;
                self.push_closed_history(closed_copy, queue_for_flush);
            }
        }
    }

    /// Collects all dirty candles (both evicted/closed and currently open/updated) since last drain,
    /// resetting `dirty_active_mask` and `pending_closed_flush`.
    pub fn drain_dirty_candles(&mut self) -> Vec<CandleAccumulator> {
        let mut out = Vec::with_capacity(self.pending_closed_flush.len() + 8);
        out.append(&mut self.pending_closed_flush);
        let mut mask = self.dirty_active_mask;
        self.dirty_active_mask = 0;
        while mask != 0 {
            let slot = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let c = &self.active_ring[slot];
            if c.occupied && c.trade_count > 0 {
                out.push(*c);
            }
        }
        out.sort_by_key(|c| {
            (
                c.bucket_open_ts_ns,
                std::cmp::Reverse((c.last_engine_seq, c.is_closed)),
            )
        });
        out.dedup_by(|a, b| a.bucket_open_ts_ns == b.bucket_open_ts_ns);
        out
    }

    /// Queries merged candles (historical ring + active open ring) sorted ascending by `bucket_open_ts_ns`.
    pub fn query_candles(
        &self,
        from_ts_ns: u64,
        to_ts_ns: u64,
        limit: usize,
    ) -> Vec<CandleAccumulator> {
        let upper = if to_ts_ns == 0 { u64::MAX } else { to_ts_ns };
        let mut map: std::collections::BTreeMap<u64, CandleAccumulator> =
            std::collections::BTreeMap::new();

        let start = if self.history_len == self.history_capacity {
            self.history_head
        } else {
            0
        };
        for i in 0..self.history_len {
            let idx = (start + i) % self.history_capacity;
            let c = &self.closed_history[idx];
            if c.occupied
                && c.trade_count > 0
                && c.bucket_open_ts_ns >= from_ts_ns
                && c.bucket_open_ts_ns <= upper
            {
                map.insert(c.bucket_open_ts_ns, *c);
            }
        }
        for c in &self.active_ring {
            if c.occupied
                && c.trade_count > 0
                && c.bucket_open_ts_ns >= from_ts_ns
                && c.bucket_open_ts_ns <= upper
            {
                map.insert(c.bucket_open_ts_ns, *c);
            }
        }

        let mut vec: Vec<CandleAccumulator> = map.into_values().collect();
        if limit > 0 && vec.len() > limit {
            vec = vec.split_off(vec.len() - limit);
        }
        vec
    }

    #[inline]
    pub fn latest_candle(&self) -> Option<CandleAccumulator> {
        let mut best: Option<CandleAccumulator> = None;
        for c in &self.active_ring {
            if c.occupied && c.trade_count > 0 {
                match best {
                    None => best = Some(*c),
                    Some(prev) if c.bucket_open_ts_ns > prev.bucket_open_ts_ns => best = Some(*c),
                    _ => {}
                }
            }
        }
        best
    }

    #[inline]
    pub fn allocated_capacity_slots(&self) -> usize {
        self.active_ring.capacity()
            + self.closed_history.capacity()
            + self.pending_closed_flush.capacity()
    }
}

/// Multi-timeframe (`1s`, `5s`, `1m`, `5m`, `15m`, `1h`) candle book for a single instrument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiTimeframeCandleBook {
    series: [TimeframeCandleSeries; 6],
}

impl MultiTimeframeCandleBook {
    pub fn new(history_capacity: usize) -> Self {
        Self {
            series: [
                TimeframeCandleSeries::new(Timeframe::S1, history_capacity),
                TimeframeCandleSeries::new(Timeframe::S5, history_capacity),
                TimeframeCandleSeries::new(Timeframe::M1, history_capacity),
                TimeframeCandleSeries::new(Timeframe::M5, history_capacity),
                TimeframeCandleSeries::new(Timeframe::M15, history_capacity),
                TimeframeCandleSeries::new(Timeframe::H1, history_capacity),
            ],
        }
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_fill(
        &mut self,
        exec_ts_ns: u64,
        engine_seq: u64,
        fill_id: u64,
        price_fp: u64,
        qty_fp: u64,
        notional_override_fp: Option<u128>,
        aggressor_side: OrderSide,
        queue_evicted_for_flush: bool,
    ) {
        for s in &mut self.series {
            s.apply_fill(
                exec_ts_ns,
                engine_seq,
                fill_id,
                price_fp,
                qty_fp,
                notional_override_fp,
                aggressor_side,
                queue_evicted_for_flush,
            );
        }
    }

    #[inline]
    pub fn advance_watermark(&mut self, watermark_ts_ns: u64, queue_for_flush: bool) {
        for s in &mut self.series {
            s.advance_watermark(watermark_ts_ns, queue_for_flush);
        }
    }

    #[inline]
    pub fn series(&self, tf: Timeframe) -> &TimeframeCandleSeries {
        &self.series[tf as usize]
    }

    #[inline]
    pub fn drain_all_dirty(&mut self) -> Vec<CandleAccumulator> {
        let mut all = Vec::new();
        for s in &mut self.series {
            all.extend(s.drain_dirty_candles());
        }
        all
    }

    #[inline]
    pub fn allocated_capacity_slots(&self) -> usize {
        self.series
            .iter()
            .map(|s| s.allocated_capacity_slots())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candle_out_of_order_commutativity() {
        let fills = [
            // (ts_ns, seq, fill_id, price_fp, qty_fp, side)
            (
                1_050_000_000_u64,
                1_u64,
                1_u64,
                100_000_000_000_u64,
                200_000_000_u64,
                OrderSide::Buy,
            ),
            (
                1_250_000_000_u64,
                2_u64,
                2_u64,
                105_000_000_000_u64,
                100_000_000_u64,
                OrderSide::Sell,
            ),
            (
                1_600_000_000_u64,
                3_u64,
                3_u64,
                98_000_000_000_u64,
                300_000_000_u64,
                OrderSide::Buy,
            ),
            (
                1_950_000_000_u64,
                4_u64,
                4_u64,
                102_000_000_000_u64,
                400_000_000_u64,
                OrderSide::Sell,
            ),
        ];

        let mut ordered = CandleAccumulator::empty(Timeframe::S1);
        ordered.reset_for_bucket(Timeframe::S1, 1_000_000_000);
        for f in fills {
            ordered.apply_fill(f.0, f.1, f.2, f.3, f.4, None, f.5);
        }

        let mut shuffled = CandleAccumulator::empty(Timeframe::S1);
        shuffled.reset_for_bucket(Timeframe::S1, 1_000_000_000);
        for idx in [2_usize, 0, 3, 1] {
            let f = fills[idx];
            shuffled.apply_fill(f.0, f.1, f.2, f.3, f.4, None, f.5);
        }

        assert_eq!(ordered.open_fp, 100_000_000_000);
        assert_eq!(ordered.high_fp, 105_000_000_000);
        assert_eq!(ordered.low_fp, 98_000_000_000);
        assert_eq!(ordered.close_fp, 102_000_000_000);
        assert_eq!(ordered.base_volume_fp, 1_000_000_000);
        assert!(ordered.twap_fp() >= ordered.low_fp && ordered.twap_fp() <= ordered.high_fp);
        assert_eq!(ordered, shuffled);

        // Verify watermark candle closing boundary
        let mut series = TimeframeCandleSeries::new(Timeframe::S1, 64);
        series.apply_fill(
            1_200_000_000,
            1,
            1,
            100_000_000_000,
            100_000_000,
            None,
            OrderSide::Buy,
            false,
        );
        series.advance_watermark(1_500_000_000, false);
        assert!(!series.latest_candle().unwrap().is_closed);
        series.advance_watermark(2_000_000_000, false);
        assert!(series.latest_candle().unwrap().is_closed);
    }
}
