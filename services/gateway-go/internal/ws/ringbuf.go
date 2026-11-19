package ws

import "sync"

// MarketRingBuffer is a per-client bounded head-dropping circular queue for non-critical
// market data frames (MSG_L3_DELTA_BATCH, MSG_CANDLE_MICRO).
// When a slow consumer falls behind, Push overwrites the oldest unread frame, releases its
// PooledFrame refcount immediately (preventing pool starvation or OOM), and records the drop
// so the client's write pump sets FlagSlowConsumerDroppedPrior and triggers a snapshot resync!
type MarketRingBuffer struct {
	mu           sync.Mutex
	buf          []*PooledFrame
	head         int
	tail         int
	count        int
	droppedPrior uint64
	droppedInst  uint32 // bitmask of instrument_ids that experienced drops since last drain
}

// NewMarketRingBuffer allocates a ring buffer of capacity >= 4.
func NewMarketRingBuffer(capacity int) *MarketRingBuffer {
	if capacity < 4 {
		capacity = 4
	}
	return &MarketRingBuffer{
		buf: make([]*PooledFrame, capacity),
	}
}

// Push enqueues frame (caller must already have retained 1 ref for this client).
// Returns (droppedOldest bool). Never blocks!
func (rb *MarketRingBuffer) Push(f *PooledFrame) bool {
	rb.mu.Lock()
	var evicted *PooledFrame
	if rb.count == len(rb.buf) {
		evicted = rb.buf[rb.head]
		rb.buf[rb.head] = nil
		rb.head++
		if rb.head == len(rb.buf) {
			rb.head = 0
		}
		rb.count--
		rb.droppedPrior++
		if evicted != nil && evicted.InstrumentID < 32 {
			rb.droppedInst |= 1 << evicted.InstrumentID
		}
	}
	rb.buf[rb.tail] = f
	rb.tail++
	if rb.tail == len(rb.buf) {
		rb.tail = 0
	}
	rb.count++
	rb.mu.Unlock()

	if evicted != nil {
		evicted.Release()
		return true
	}
	return false
}

// DrainBatch pops up to len(dst) frames into dst and atomically returns any accumulated
// droppedPrior count + droppedInst bitmask since the previous drain.
func (rb *MarketRingBuffer) DrainBatch(dst []*PooledFrame) (n int, droppedCount uint64, droppedInstMask uint32) {
	rb.mu.Lock()
	limit := len(dst)
	if rb.count < limit {
		limit = rb.count
	}
	for i := 0; i < limit; i++ {
		dst[i] = rb.buf[rb.head]
		rb.buf[rb.head] = nil
		rb.head++
		if rb.head == len(rb.buf) {
			rb.head = 0
		}
	}
	rb.count -= limit
	droppedCount = rb.droppedPrior
	droppedInstMask = rb.droppedInst
	rb.droppedPrior = 0
	rb.droppedInst = 0
	rb.mu.Unlock()
	return limit, droppedCount, droppedInstMask
}

// Depth returns the current queued item count.
func (rb *MarketRingBuffer) Depth() int {
	rb.mu.Lock()
	c := rb.count
	rb.mu.Unlock()
	return c
}

// Clear releases all remaining PooledFrames in the ring buffer on connection teardown.
func (rb *MarketRingBuffer) Clear() {
	rb.mu.Lock()
	for i := 0; i < rb.count; i++ {
		idx := (rb.head + i) % len(rb.buf)
		if f := rb.buf[idx]; f != nil {
			rb.buf[idx] = nil
			f.Release()
		}
	}
	rb.head = 0
	rb.tail = 0
	rb.count = 0
	rb.droppedPrior = 0
	rb.droppedInst = 0
	rb.mu.Unlock()
}
