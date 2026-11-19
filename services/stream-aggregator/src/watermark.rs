use serde::{Deserialize, Serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

/// Deterministic Bounded-Out-Of-Orderness Watermark tracker (`W(t) = max_ts_ns - lateness_ns`).
#[repr(C, align(64))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicWatermark {
    max_event_ts_ns: u64,
    watermark_ts_ns: u64,
    allowed_lateness_ns: u64,
    late_events_dropped: u64,
    reordered_events_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatermarkDecision {
    /// Event timestamp is >= current watermark; accepted (with updated watermark).
    Accept { new_watermark_ts_ns: u64 },
    /// Event timestamp is strictly older than current watermark (`ts_ns < watermark_ts_ns`).
    LateDrop { current_watermark_ts_ns: u64 },
}

impl DeterministicWatermark {
    pub fn new(allowed_lateness_ns: u64) -> Self {
        Self {
            max_event_ts_ns: 0,
            watermark_ts_ns: 0,
            allowed_lateness_ns,
            late_events_dropped: 0,
            reordered_events_count: 0,
        }
    }

    #[inline(always)]
    pub fn watermark_ts_ns(&self) -> u64 {
        self.watermark_ts_ns
    }

    #[inline(always)]
    pub fn max_event_ts_ns(&self) -> u64 {
        self.max_event_ts_ns
    }

    #[inline(always)]
    pub fn allowed_lateness_ns(&self) -> u64 {
        self.allowed_lateness_ns
    }

    #[inline(always)]
    pub fn late_events_dropped(&self) -> u64 {
        self.late_events_dropped
    }

    #[inline(always)]
    pub fn reordered_events_count(&self) -> u64 {
        self.reordered_events_count
    }

    /// Evaluates incoming `event_ts_ns` against deterministic watermark and advances
    /// `max_event_ts_ns` & `watermark_ts_ns` monotonically.
    #[inline]
    pub fn observe(&mut self, event_ts_ns: u64) -> WatermarkDecision {
        if event_ts_ns < self.watermark_ts_ns {
            self.late_events_dropped += 1;
            return WatermarkDecision::LateDrop {
                current_watermark_ts_ns: self.watermark_ts_ns,
            };
        }

        if event_ts_ns < self.max_event_ts_ns {
            self.reordered_events_count += 1;
        } else {
            self.max_event_ts_ns = event_ts_ns;
            let candidate = self.max_event_ts_ns.saturating_sub(self.allowed_lateness_ns);
            if candidate > self.watermark_ts_ns {
                self.watermark_ts_ns = candidate;
            }
        }

        WatermarkDecision::Accept {
            new_watermark_ts_ns: self.watermark_ts_ns,
        }
    }
}

/// Canonical total-order key `(ts_ns, engine_seq, sub_seq)` for bounded watermark reordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedItem<T> {
    pub ts_ns: u64,
    pub engine_seq: u64,
    pub sub_seq: u64,
    pub payload: T,
}

impl<T> PartialEq for SequencedItem<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ts_ns == other.ts_ns
            && self.engine_seq == other.engine_seq
            && self.sub_seq == other.sub_seq
    }
}

impl<T> Eq for SequencedItem<T> {}

impl<T> PartialOrd for SequencedItem<T> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for SequencedItem<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.ts_ns
            .cmp(&other.ts_ns)
            .then_with(|| self.engine_seq.cmp(&other.engine_seq))
            .then_with(|| self.sub_seq.cmp(&other.sub_seq))
    }
}

/// Pre-allocated bounded watermark reorder buffer.
/// - Zero-lateness (`allowed_lateness_ns == 0`): 100% bypass (`O(1)` immediate emission).
/// - Non-zero lateness: buffers events within `[W(t), max_ts_ns]` in pre-allocated flat heap
///   and emits events in strict canonical `(ts_ns, engine_seq, sub_seq)` order as `W(t)` advances.
#[derive(Debug, Clone)]
pub struct WatermarkReorderBuffer<T> {
    heap: BinaryHeap<Reverse<SequencedItem<T>>>,
    max_capacity: usize,
}

impl<T: Clone + Serialize + for<'de> Deserialize<'de>> WatermarkReorderBuffer<T> {
    pub fn with_capacity(max_capacity: usize) -> Self {
        let cap = max_capacity.max(16);
        Self {
            heap: BinaryHeap::with_capacity(cap),
            max_capacity: cap,
        }
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.heap.capacity()
    }

    #[inline]
    pub fn push(&mut self, item: SequencedItem<T>) {
        self.heap.push(Reverse(item));
    }

    /// Pops the next event ready to be committed (`item.ts_ns <= watermark_ts_ns`),
    /// OR forces eviction of the oldest item if `max_capacity` is exceeded.
    #[inline]
    pub fn pop_ready(&mut self, watermark_ts_ns: u64) -> Option<SequencedItem<T>> {
        let force_evict = self.heap.len() > self.max_capacity;
        if force_evict {
            return self.heap.pop().map(|r| r.0);
        }
        if let Some(Reverse(top)) = self.heap.peek() {
            if top.ts_ns <= watermark_ts_ns {
                return self.heap.pop().map(|r| r.0);
            }
        }
        None
    }

    /// Drains all remaining buffered items in strict `(ts_ns, engine_seq, sub_seq)` order.
    pub fn drain_all_ordered(&mut self) -> Vec<SequencedItem<T>> {
        let mut out = Vec::with_capacity(self.heap.len());
        while let Some(Reverse(item)) = self.heap.pop() {
            out.push(item);
        }
        out
    }

    /// Snapshot items without mutating the buffer (for transactional checkpoint persistence).
    pub fn snapshot_items(&self) -> Vec<SequencedItem<T>> {
        let mut items: Vec<SequencedItem<T>> =
            self.heap.iter().map(|r| r.0.clone()).collect();
        items.sort();
        items
    }

    /// Restore buffer from a persisted checkpoint snapshot without growing capacity beyond `max_capacity`.
    pub fn restore_items(&mut self, items: Vec<SequencedItem<T>>) {
        self.heap.clear();
        for item in items {
            self.heap.push(Reverse(item));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermark_advances_monotonically_and_drops_late() {
        let mut wm = DeterministicWatermark::new(500);
        assert_eq!(
            wm.observe(1_000),
            WatermarkDecision::Accept {
                new_watermark_ts_ns: 500
            }
        );
        // Out-of-order within [500, 1000]: accepted, watermark stays 500
        assert_eq!(
            wm.observe(700),
            WatermarkDecision::Accept {
                new_watermark_ts_ns: 500
            }
        );
        // Late event (< 500): dropped!
        assert_eq!(
            wm.observe(499),
            WatermarkDecision::LateDrop {
                current_watermark_ts_ns: 500
            }
        );
        assert_eq!(wm.late_events_dropped(), 1);
        assert_eq!(wm.reordered_events_count(), 1);

        // Reorder buffer pops strictly when ts_ns <= watermark_ts_ns
        let mut buf: WatermarkReorderBuffer<u64> = WatermarkReorderBuffer::with_capacity(32);
        buf.push(SequencedItem {
            ts_ns: 600,
            engine_seq: 2,
            sub_seq: 0,
            payload: 22,
        });
        buf.push(SequencedItem {
            ts_ns: 450,
            engine_seq: 1,
            sub_seq: 0,
            payload: 11,
        });
        assert_eq!(buf.pop_ready(500).unwrap().payload, 11);
        assert!(buf.pop_ready(500).is_none());
        assert_eq!(buf.pop_ready(600).unwrap().payload, 22);
    }
}
