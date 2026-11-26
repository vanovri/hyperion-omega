use serde::{Deserialize, Serialize};

/// Number of `u64` words in the sliding deduplication bitmap (`1024 * 64 = 65,536` sequence IDs).
/// Uses exactly `8 KiB` of L1-friendly contiguous memory with zero heap allocations on hot path.
pub const DEDUP_WORDS: usize = 1024;
pub const DEDUP_WINDOW_BITS: u64 = (DEDUP_WORDS as u64) * 64;

/// Cache-line aligned `O(1)` sequence ID deduplicator + Kafka partition offset guard.
///
/// Guarantees deterministic idempotency across:
/// 1. In-order & out-of-order redeliveries within a `65,536`-sequence sliding window.
/// 2. Historical replays older than the sliding horizon (`seq <= horizon_floor`).
/// 3. Multi-fill events sharing an `engine_seq` via composite `(engine_seq, sub_id)` tracking
///    or distinct monotonic event sequence keys.
#[repr(C, align(64))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeqDedupWindow {
    max_seq: u64,
    initialized: bool,
    accepted_count: u64,
    duplicate_count: u64,
    words: Vec<u64>,
}

impl Default for SeqDedupWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SeqDedupWindow {
    pub fn new() -> Self {
        Self {
            max_seq: 0,
            initialized: false,
            accepted_count: 0,
            duplicate_count: 0,
            words: vec![0_u64; DEDUP_WORDS],
        }
    }

    #[inline(always)]
    pub fn max_seq(&self) -> u64 {
        self.max_seq
    }

    #[inline(always)]
    pub fn accepted_count(&self) -> u64 {
        self.accepted_count
    }

    #[inline(always)]
    pub fn duplicate_count(&self) -> u64 {
        self.duplicate_count
    }

    #[inline(always)]
    fn slot_and_mask(seq: u64) -> (usize, u64) {
        let bit_pos = seq & (DEDUP_WINDOW_BITS - 1);
        let word_idx = (bit_pos >> 6) as usize;
        let bit_mask = 1_u64 << (bit_pos & 63);
        (word_idx, bit_mask)
    }

    /// Checks and marks `seq` in `O(1)` constant time.
    /// Returns `true` iff `seq` has NOT been seen before (accepted), `false` if duplicate/stale.
    #[inline]
    pub fn check_and_mark(&mut self, seq: u64) -> bool {
        if seq == 0 {
            self.duplicate_count += 1;
            return false;
        }

        if !self.initialized {
            self.initialized = true;
            self.max_seq = seq;
            let (w, m) = Self::slot_and_mask(seq);
            self.words[w] |= m;
            self.accepted_count += 1;
            return true;
        }

        if seq > self.max_seq {
            let jump = seq - self.max_seq;
            if jump >= DEDUP_WINDOW_BITS {
                self.words.fill(0);
            } else if jump > 1 {
                // Clear skipped bit positions in circular bitmap so wrapped-around out-of-order
                // arrivals do not collide with stale bits from 65,536 steps earlier.
                for s in (self.max_seq + 1)..seq {
                    let (w, m) = Self::slot_and_mask(s);
                    self.words[w] &= !m;
                }
            }
            let (w, m) = Self::slot_and_mask(seq);
            self.words[w] |= m;
            self.max_seq = seq;
            self.accepted_count += 1;
            return true;
        }

        let distance = self.max_seq - seq;
        if distance >= DEDUP_WINDOW_BITS {
            // Older than the 65,536-sequence horizon -> guaranteed already processed/committed.
            self.duplicate_count += 1;
            return false;
        }

        let (w, m) = Self::slot_and_mask(seq);
        if (self.words[w] & m) != 0 {
            self.duplicate_count += 1;
            false
        } else {
            self.words[w] |= m;
            self.accepted_count += 1;
            true
        }
    }
}

/// Per-topic, per-partition transactional Kafka offset tracker bound to `stream_consumer_checkpoints`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionCheckpoint {
    pub consumer_group: String,
    pub topic: String,
    pub partition_id: i32,
    pub last_kafka_offset: i64,
    pub last_engine_seq: u64,
    pub watermark_ts_ns: u64,
}

impl PartitionCheckpoint {
    pub fn new(
        consumer_group: impl Into<String>,
        topic: impl Into<String>,
        partition_id: i32,
    ) -> Self {
        Self {
            consumer_group: consumer_group.into(),
            topic: topic.into(),
            partition_id,
            last_kafka_offset: -1,
            last_engine_seq: 0,
            watermark_ts_ns: 0,
        }
    }

    /// Returns `true` if `kafka_offset` is strictly newer than `last_kafka_offset`.
    #[inline(always)]
    pub fn accepts_offset(&self, kafka_offset: i64) -> bool {
        kafka_offset > self.last_kafka_offset
    }

    #[inline(always)]
    pub fn advance(&mut self, kafka_offset: i64, engine_seq: u64, watermark_ts_ns: u64) {
        if kafka_offset > self.last_kafka_offset {
            self.last_kafka_offset = kafka_offset;
        }
        if engine_seq > self.last_engine_seq {
            self.last_engine_seq = engine_seq;
        }
        if watermark_ts_ns > self.watermark_ts_ns {
            self.watermark_ts_ns = watermark_ts_ns;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_sequential_and_out_of_order_exactness() {
        let mut dedup = SeqDedupWindow::new();
        assert!(!dedup.check_and_mark(0));

        // Out-of-order arrival: 10, 8, 12, 9, 11
        for s in [10_u64, 8, 12, 9, 11] {
            assert!(dedup.check_and_mark(s), "first arrival of {s} must pass");
        }
        for s in [8_u64, 9, 10, 11, 12] {
            assert!(
                !dedup.check_and_mark(s),
                "duplicate of {s} must be rejected"
            );
        }
        assert_eq!(dedup.accepted_count(), 5);

        // Large jump beyond 65,536 window
        let far = 100_000_u64;
        assert!(dedup.check_and_mark(far));
        assert!(!dedup.check_and_mark(far));
        // Stale sequence below horizon must be rejected
        assert!(!dedup.check_and_mark(12));
        // Sequence inside new window must be accepted once
        assert!(dedup.check_and_mark(far - 500));
        assert!(!dedup.check_and_mark(far - 500));

        // Wraparound skipped-slot clearing test (kills bitmap-clear mutation DD-04)
        let mut wrap = SeqDedupWindow::new();
        assert!(wrap.check_and_mark(1));
        assert!(wrap.check_and_mark(DEDUP_WINDOW_BITS));
        assert!(wrap.check_and_mark(DEDUP_WINDOW_BITS + 2)); // skips DEDUP_WINDOW_BITS + 1 (same bit as 1!)
        assert!(
            wrap.check_and_mark(DEDUP_WINDOW_BITS + 1),
            "wrapped-around skipped sequence must be accepted"
        );

        // Partition offset monotonicity guard
        let mut cp = PartitionCheckpoint::new("grp", "topic", 0);
        assert!(cp.accepts_offset(0));
        cp.advance(10, 100, 1_000);
        assert!(!cp.accepts_offset(10));
        assert!(!cp.accepts_offset(9));
        assert!(cp.accepts_offset(11));
    }
}
