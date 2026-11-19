//! Deterministic `SimulatedClock` supporting per-node clock rate skew (ppm) and monotonic step jumps.

use crate::traits::Clock;
use crate::types::NodeId;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default)]
struct NodeClockSkew {
    /// Signed rate drift in parts-per-million (`+10_000` = +1.0% faster clock).
    rate_skew_ppm: i64,
    /// Signed offset in nanoseconds.
    offset_ns: i64,
    /// Monotonic floor ensuring `now_ns(node)` never goes backward on that node.
    monotonic_floor_ns: u64,
}

pub struct SimulatedClock {
    inner: Mutex<ClockState>,
}

#[derive(Debug, Default)]
struct ClockState {
    master_ns: u64,
    skews: BTreeMap<NodeId, NodeClockSkew>,
}

impl Default for SimulatedClock {
    fn default() -> Self {
        Self::new(1_000_000_000)
    }
}

impl SimulatedClock {
    pub fn new(initial_master_ns: u64) -> Self {
        Self {
            inner: Mutex::new(ClockState {
                master_ns: initial_master_ns,
                skews: BTreeMap::new(),
            }),
        }
    }

    pub fn master_ns(&self) -> u64 {
        self.inner.lock().expect("clock mutex").master_ns
    }

    pub fn advance_to(&self, next_master_ns: u64) {
        let mut g = self.inner.lock().expect("clock mutex");
        if next_master_ns > g.master_ns {
            g.master_ns = next_master_ns;
        }
    }

    pub fn advance_by(&self, delta_ns: u64) -> u64 {
        let mut g = self.inner.lock().expect("clock mutex");
        g.master_ns = g.master_ns.saturating_add(delta_ns);
        g.master_ns
    }

    pub fn set_node_skew(&self, node_id: NodeId, rate_skew_ppm: i64, offset_ns: i64) {
        let mut g = self.inner.lock().expect("clock mutex");
        let entry = g.skews.entry(node_id).or_default();
        entry.rate_skew_ppm = rate_skew_ppm;
        entry.offset_ns = offset_ns;
    }
}

impl Clock for SimulatedClock {
    fn now_ns(&self, node_id: NodeId) -> u64 {
        let mut g = self.inner.lock().expect("clock mutex");
        let master = g.master_ns;
        let entry = g.skews.entry(node_id).or_default();
        let factor_ppm = (1_000_000i128).saturating_add(i128::from(entry.rate_skew_ppm)).max(100_000);
        let scaled = (u128::from(master) * (factor_ppm as u128)) / 1_000_000u128;
        let with_offset = if entry.offset_ns >= 0 {
            (scaled as u64).saturating_add(entry.offset_ns as u64)
        } else {
            (scaled as u64).saturating_sub(entry.offset_ns.unsigned_abs())
        };
        let mono = with_offset.max(entry.monotonic_floor_ns);
        entry.monotonic_floor_ns = mono;
        mono
    }
}
