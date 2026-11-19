//! # Zero-Allocation Nanosecond Latency & CPU Fuel Telemetry Profiler (`profiler.rs`)
//!
//! Tracks exact deterministic instruction fuel ("gas") consumption and Host -> Guest -> Host
//! wall-clock execution latency down to `25 ns` resolution without heap allocations on the hot path.

use serde::Serialize;

/// Fine-grained `25 ns` linear buckets up to `50,000 ns` (`50 µs` = `2,000` buckets)
/// plus `32` coarse `10 µs` overflow buckets up to `> 370 µs`.
const FINE_BUCKET_NS: u64 = 25;
const FINE_BUCKETS: usize = 2_000; // 0 .. 50,000 ns
const COARSE_BUCKET_NS: u64 = 10_000;
const COARSE_BUCKETS: usize = 64;
const TOTAL_BUCKETS: usize = FINE_BUCKETS + COARSE_BUCKETS;

#[derive(Clone, Debug, Serialize)]
pub struct ProfilerSummary {
    pub total_invocations: u64,
    pub total_traps: u64,
    pub total_fuel_consumed: u64,
    pub mean_fuel_per_call: u64,
    pub max_fuel_single_call: u64,
    pub min_latency_ns: u64,
    pub p50_latency_ns: u64,
    pub p90_latency_ns: u64,
    pub p99_latency_ns: u64,
    pub p999_latency_ns: u64,
    pub max_latency_ns: u64,
    pub total_orders_emitted: u64,
    pub total_orders_rejected: u64,
    pub hot_swap_count: u64,
}

#[derive(Clone)]
pub struct StrategyProfiler {
    buckets: Box<[u64; TOTAL_BUCKETS]>,
    total_invocations: u64,
    total_traps: u64,
    total_fuel_consumed: u64,
    max_fuel_single_call: u64,
    min_latency_ns: u64,
    max_latency_ns: u64,
    total_orders_emitted: u64,
    total_orders_rejected: u64,
    hot_swap_count: u64,
}

impl Default for StrategyProfiler {
    fn default() -> Self {
        Self::new()
    }
}

impl StrategyProfiler {
    pub fn new() -> Self {
        Self {
            buckets: Box::new([0u64; TOTAL_BUCKETS]),
            total_invocations: 0,
            total_traps: 0,
            total_fuel_consumed: 0,
            max_fuel_single_call: 0,
            min_latency_ns: u64::MAX,
            max_latency_ns: 0,
            total_orders_emitted: 0,
            total_orders_rejected: 0,
            hot_swap_count: 0,
        }
    }

    #[inline]
    fn latency_to_bucket(ns: u64) -> usize {
        if ns < (FINE_BUCKETS as u64) * FINE_BUCKET_NS {
            (ns / FINE_BUCKET_NS) as usize
        } else {
            let excess = ns - (FINE_BUCKETS as u64) * FINE_BUCKET_NS;
            let c = (excess / COARSE_BUCKET_NS) as usize;
            FINE_BUCKETS + c.min(COARSE_BUCKETS - 1)
        }
    }

    #[inline]
    fn bucket_upper_bound_ns(idx: usize) -> u64 {
        if idx < FINE_BUCKETS {
            ((idx as u64) + 1) * FINE_BUCKET_NS
        } else {
            let c = (idx - FINE_BUCKETS) as u64;
            (FINE_BUCKETS as u64) * FINE_BUCKET_NS + (c + 1) * COARSE_BUCKET_NS
        }
    }

    /// Zero-allocation hot-path telemetry update.
    #[inline]
    pub fn record_tick(
        &mut self,
        latency_ns: u64,
        fuel_consumed: u64,
        orders_emitted: u32,
        orders_rejected: u32,
    ) {
        self.total_invocations = self.total_invocations.saturating_add(1);
        self.total_fuel_consumed = self.total_fuel_consumed.saturating_add(fuel_consumed);
        if fuel_consumed > self.max_fuel_single_call {
            self.max_fuel_single_call = fuel_consumed;
        }
        if latency_ns < self.min_latency_ns {
            self.min_latency_ns = latency_ns;
        }
        if latency_ns > self.max_latency_ns {
            self.max_latency_ns = latency_ns;
        }
        self.total_orders_emitted = self
            .total_orders_emitted
            .saturating_add(orders_emitted as u64);
        self.total_orders_rejected = self
            .total_orders_rejected
            .saturating_add(orders_rejected as u64);
        let b = Self::latency_to_bucket(latency_ns);
        self.buckets[b] = self.buckets[b].saturating_add(1);
    }

    #[inline]
    pub fn record_trap(&mut self, elapsed_ns: u64, fuel_consumed: u64, orders_rejected: u32) {
        self.total_traps = self.total_traps.saturating_add(1);
        self.total_fuel_consumed = self.total_fuel_consumed.saturating_add(fuel_consumed);
        self.total_orders_rejected = self
            .total_orders_rejected
            .saturating_add(orders_rejected as u64);
        if elapsed_ns > self.max_latency_ns {
            self.max_latency_ns = elapsed_ns;
        }
    }

    #[inline]
    pub fn record_hot_swap(&mut self) {
        self.hot_swap_count = self.hot_swap_count.saturating_add(1);
    }

    pub fn quantile_ns(&self, permille_x10: u64) -> u64 {
        // `permille_x10` in [0, 10_000]: 5000 = p50, 9900 = p99, 9990 = p99.9
        if self.total_invocations == 0 {
            return 0;
        }
        let target = ((self.total_invocations as u128 * permille_x10 as u128).div_ceil(10_000))
            .max(1) as u64;
        let mut cumulative: u64 = 0;
        for (idx, &count) in self.buckets.iter().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return Self::bucket_upper_bound_ns(idx).min(self.max_latency_ns.max(FINE_BUCKET_NS));
            }
        }
        self.max_latency_ns
    }

    pub fn summary(&self) -> ProfilerSummary {
        let min_ns = if self.total_invocations == 0 {
            0
        } else {
            self.min_latency_ns
        };
        let mean_fuel = self
            .total_fuel_consumed
            .checked_div(self.total_invocations)
            .unwrap_or(0);
        ProfilerSummary {
            total_invocations: self.total_invocations,
            total_traps: self.total_traps,
            total_fuel_consumed: self.total_fuel_consumed,
            mean_fuel_per_call: mean_fuel,
            max_fuel_single_call: self.max_fuel_single_call,
            min_latency_ns: min_ns,
            p50_latency_ns: self.quantile_ns(5_000),
            p90_latency_ns: self.quantile_ns(9_000),
            p99_latency_ns: self.quantile_ns(9_900),
            p999_latency_ns: self.quantile_ns(9_990),
            max_latency_ns: self.max_latency_ns,
            total_orders_emitted: self.total_orders_emitted,
            total_orders_rejected: self.total_orders_rejected,
            hot_swap_count: self.hot_swap_count,
        }
    }
}
