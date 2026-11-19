use crossbeam_utils::CachePadded;
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Wait-free LMAX Disruptor-style Single-Producer Single-Consumer (SPSC) bounded ring buffer.
///
/// Uses shadow cursor caching on separate 64-byte cache lines (`CachePadded`) to eliminate
/// cross-core cache-coherency invalidations on the matching hot path until the local cached
/// window is exhausted.
pub struct SpscRingBuffer<T> {
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,
    mask: usize,
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    dropped_count: CachePadded<AtomicU64>,
}

// SAFETY: SPSC discipline enforces exclusive producer access to `tail` slot writes and
// exclusive consumer access to `head` slot reads, synchronized with Acquire/Release fences.
unsafe impl<T: Send> Send for SpscRingBuffer<T> {}
unsafe impl<T: Send> Sync for SpscRingBuffer<T> {}

pub struct SpscProducer<T> {
    ring: std::sync::Arc<SpscRingBuffer<T>>,
    cached_head: usize,
    local_tail: usize,
}

pub struct SpscConsumer<T> {
    ring: std::sync::Arc<SpscRingBuffer<T>>,
    cached_tail: usize,
    local_head: usize,
}

pub fn spsc_channel<T>(capacity_pow2: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let cap = capacity_pow2.max(2).next_power_of_two();
    let mut vec = Vec::with_capacity(cap);
    for _ in 0..cap {
        vec.push(UnsafeCell::new(MaybeUninit::uninit()));
    }
    let ring = std::sync::Arc::new(SpscRingBuffer {
        buffer: vec.into_boxed_slice(),
        mask: cap - 1,
        head: CachePadded::new(AtomicUsize::new(0)),
        tail: CachePadded::new(AtomicUsize::new(0)),
        dropped_count: CachePadded::new(AtomicU64::new(0)),
    });
    (
        SpscProducer {
            ring: std::sync::Arc::clone(&ring),
            cached_head: 0,
            local_tail: 0,
        },
        SpscConsumer {
            ring,
            cached_tail: 0,
            local_head: 0,
        },
    )
}

impl<T> SpscProducer<T> {
    /// Non-blocking wait-free push on the matching thread hot path (zero heap allocation).
    #[inline(always)]
    pub fn try_push(&mut self, value: T) -> Result<(), T> {
        let cap = self.ring.mask + 1;
        if self.local_tail.wrapping_sub(self.cached_head) == cap {
            self.cached_head = self.ring.head.load(Ordering::Acquire);
            if self.local_tail.wrapping_sub(self.cached_head) == cap {
                self.ring.dropped_count.fetch_add(1, Ordering::Relaxed);
                return Err(value);
            }
        }
        let slot_idx = self.local_tail & self.ring.mask;
        // SAFETY: Producer exclusively owns `slot_idx` while `local_tail - head < capacity`.
        unsafe {
            (*self.ring.buffer[slot_idx].get()).write(value);
        }
        self.local_tail = self.local_tail.wrapping_add(1);
        self.ring.tail.store(self.local_tail, Ordering::Release);
        Ok(())
    }

    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.ring.mask + 1
    }

    #[inline(always)]
    pub fn dropped_count(&self) -> u64 {
        self.ring.dropped_count.load(Ordering::Relaxed)
    }
}

impl<T> SpscConsumer<T> {
    /// Non-blocking wait-free pop on the async publisher thread.
    #[inline(always)]
    pub fn try_pop(&mut self) -> Option<T> {
        if self.local_head == self.cached_tail {
            self.cached_tail = self.ring.tail.load(Ordering::Acquire);
            if self.local_head == self.cached_tail {
                return None;
            }
        }
        let slot_idx = self.local_head & self.ring.mask;
        // SAFETY: Consumer exclusively owns `slot_idx` after observing `tail` via Acquire.
        let item = unsafe { (*self.ring.buffer[slot_idx].get()).assume_init_read() };
        self.local_head = self.local_head.wrapping_add(1);
        self.ring.head.store(self.local_head, Ordering::Release);
        Some(item)
    }

    /// Drains up to `max_batch` items in a single burst to amortize Redpanda network I/O.
    #[inline]
    pub fn drain_batch(&mut self, out: &mut Vec<T>, max_batch: usize) -> usize {
        if self.local_head == self.cached_tail {
            self.cached_tail = self.ring.tail.load(Ordering::Acquire);
            if self.local_head == self.cached_tail {
                return 0;
            }
        }
        let available = self.cached_tail.wrapping_sub(self.local_head);
        let to_read = available.min(max_batch);
        for _ in 0..to_read {
            let slot_idx = self.local_head & self.ring.mask;
            let item = unsafe { (*self.ring.buffer[slot_idx].get()).assume_init_read() };
            out.push(item);
            self.local_head = self.local_head.wrapping_add(1);
        }
        self.ring.head.store(self.local_head, Ordering::Release);
        to_read
    }

    #[inline(always)]
    pub fn pending_len(&self) -> usize {
        let tail = self.ring.tail.load(Ordering::Acquire);
        let head = self.ring.head.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }
}

impl<T> Drop for SpscRingBuffer<T> {
    fn drop(&mut self) {
        let head = *self.head.get_mut();
        let tail = *self.tail.get_mut();
        let mut cur = head;
        while cur != tail {
            let idx = cur & self.mask;
            unsafe {
                (*self.buffer[idx].get()).assume_init_drop();
            }
            cur = cur.wrapping_add(1);
        }
    }
}

/// Lock-free Multi-Producer Single-Consumer (MPSC) sequence-claimed Disruptor ring buffer
/// for routing concurrent gRPC worker commands into the pinned single-writer matching thread.
struct MpscSlot<T> {
    sequence: CachePadded<AtomicU64>,
    value: UnsafeCell<MaybeUninit<T>>,
}

pub struct MpscDisruptorRing<T> {
    slots: Box<[MpscSlot<T>]>,
    mask: u64,
    enqueue_pos: CachePadded<AtomicU64>,
    dequeue_pos: CachePadded<AtomicU64>,
}

unsafe impl<T: Send> Send for MpscDisruptorRing<T> {}
unsafe impl<T: Send> Sync for MpscDisruptorRing<T> {}

impl<T> MpscDisruptorRing<T> {
    pub fn new(capacity_pow2: usize) -> std::sync::Arc<Self> {
        let cap = capacity_pow2.max(2).next_power_of_two();
        let mut slots = Vec::with_capacity(cap);
        for i in 0..cap {
            slots.push(MpscSlot {
                sequence: CachePadded::new(AtomicU64::new(i as u64)),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            });
        }
        std::sync::Arc::new(Self {
            slots: slots.into_boxed_slice(),
            mask: (cap - 1) as u64,
            enqueue_pos: CachePadded::new(AtomicU64::new(0)),
            dequeue_pos: CachePadded::new(AtomicU64::new(0)),
        })
    }

    /// Lock-free CAS sequence claim by any producer thread.
    #[inline]
    pub fn try_send(&self, item: T) -> Result<(), T> {
        let mut pos = self.enqueue_pos.load(Ordering::Relaxed);
        loop {
            let slot = &self.slots[(pos & self.mask) as usize];
            let seq = slot.sequence.load(Ordering::Acquire);
            let diff = (seq as i64).wrapping_sub(pos as i64);
            if diff == 0 {
                match self.enqueue_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe {
                            (*slot.value.get()).write(item);
                        }
                        slot.sequence.store(pos.wrapping_add(1), Ordering::Release);
                        return Ok(());
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return Err(item);
            } else {
                pos = self.enqueue_pos.load(Ordering::Relaxed);
            }
        }
    }

    /// Single-consumer wait-free pop on the pinned Disruptor matching thread.
    #[inline(always)]
    pub fn try_recv_single_consumer(&self) -> Option<T> {
        let pos = self.dequeue_pos.load(Ordering::Relaxed);
        let slot = &self.slots[(pos & self.mask) as usize];
        let seq = slot.sequence.load(Ordering::Acquire);
        let expected = pos.wrapping_add(1);
        if seq != expected {
            return None;
        }
        let item = unsafe { (*slot.value.get()).assume_init_read() };
        self.dequeue_pos.store(expected, Ordering::Relaxed);
        slot.sequence
            .store(pos.wrapping_add(self.mask + 1), Ordering::Release);
        Some(item)
    }
}

impl<T> Drop for MpscDisruptorRing<T> {
    fn drop(&mut self) {
        while self.try_recv_single_consumer().is_some() {}
    }
}
