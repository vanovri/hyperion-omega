package perf

import (
	"math/big"
	"sync"
)

// =============================================================================
// 3-PASS MICRO-OPTIMIZATION BENCHMARK TARGETS (02-extreme-overclock-protocol.md)
// =============================================================================
// Pass 1 (Baseline): Heap-allocated map[uint64]*BaselineHeader + math/big EIP-1559 fee math
// Pass 2 (ZeroCopyRing): Pre-allocated power-of-two ring slice + fixed [32]byte + uint64 integer math
// Pass 3 (CacheAlignedFinal): 64-byte cache-line padded fixed array + branchless integer EIP-1559 bump

type BaselineHeader struct {
	Number     uint64
	Hash       []byte
	ParentHash []byte
}

// Pass1BaselineTracker is the naive map + slice-clone + math/big reference implementation.
type Pass1BaselineTracker struct {
	mu      sync.Mutex
	headers map[uint64]*BaselineHeader
	tip     uint64
}

func NewPass1BaselineTracker() *Pass1BaselineTracker {
	return &Pass1BaselineTracker{
		headers: make(map[uint64]*BaselineHeader, 256),
	}
}

func (b *Pass1BaselineTracker) StepAndQuote(num uint64, hash [32]byte, parent [32]byte, prevMaxFee uint64, prevTip uint64, baseFee uint64) (uint64, uint64) {
	b.mu.Lock()
	hCopy := make([]byte, 32)
	pCopy := make([]byte, 32)
	copy(hCopy, hash[:])
	copy(pCopy, parent[:])
	b.headers[num] = &BaselineHeader{
		Number:     num,
		Hash:       hCopy,
		ParentHash: pCopy,
	}
	if num >= 256 {
		delete(b.headers, num-256)
	}
	b.tip = num
	b.mu.Unlock()

	// math/big fee bump (+15%)
	bf := new(big.Int).SetUint64(baseFee)
	pm := new(big.Int).SetUint64(prevMaxFee)
	pt := new(big.Int).SetUint64(prevTip)
	numFactor := big.NewInt(11_500)
	denFactor := big.NewInt(10_000)
	nineNineNineNine := big.NewInt(9_999)

	nextTip := new(big.Int).Mul(pt, numFactor)
	nextTip.Add(nextTip, nineNineNineNine)
	nextTip.Div(nextTip, denFactor)

	nextMax := new(big.Int).Mul(pm, numFactor)
	nextMax.Add(nextMax, nineNineNineNine)
	nextMax.Div(nextMax, denFactor)

	marketMax := new(big.Int).Mul(bf, big.NewInt(2))
	marketMax.Add(marketMax, nextTip)
	if marketMax.Cmp(nextMax) > 0 {
		nextMax = marketMax
	}
	return nextMax.Uint64(), nextTip.Uint64()
}

type zeroCopyEntry struct {
	number     uint64
	hash       [32]byte
	parentHash [32]byte
}

// Pass2ZeroCopyRing eliminates heap allocations using a flat circular ring slice and uint64 math.
type Pass2ZeroCopyRing struct {
	mu   sync.Mutex
	mask uint64
	tip  uint64
	ring []zeroCopyEntry
}

func NewPass2ZeroCopyRing() *Pass2ZeroCopyRing {
	return &Pass2ZeroCopyRing{
		mask: 255,
		ring: make([]zeroCopyEntry, 256),
	}
}

func (z *Pass2ZeroCopyRing) StepAndQuote(num uint64, hash [32]byte, parent [32]byte, prevMaxFee uint64, prevTip uint64, baseFee uint64) (uint64, uint64) {
	z.mu.Lock()
	z.ring[num&z.mask] = zeroCopyEntry{
		number:     num,
		hash:       hash,
		parentHash: parent,
	}
	z.tip = num
	z.mu.Unlock()

	nextTip := (prevTip*11_500 + 9_999) / 10_000
	if nextTip <= prevTip {
		nextTip = prevTip + 1
	}
	nextMax := (prevMaxFee*11_500 + 9_999) / 10_000
	marketMax := 2*baseFee + nextTip
	if marketMax > nextMax {
		nextMax = marketMax
	}
	return nextMax, nextTip
}

// alignedRingSlot occupies exactly 128 bytes (two 64-byte cache lines) with zero false sharing.
type alignedRingSlot struct {
	number     uint64
	hash       [32]byte
	parentHash [32]byte
	_pad       [56]byte
}

// Pass3CacheLineAligned uses a contiguous inline [256]alignedRingSlot array, 64-byte padded head,
// and single-writer/atomic-friendly flat indexing with branch-minimized integer arithmetic.
type Pass3CacheLineAligned struct {
	tip   uint64
	_pad0 [56]byte
	slots [256]alignedRingSlot
}

func NewPass3CacheLineAligned() *Pass3CacheLineAligned {
	return &Pass3CacheLineAligned{}
}

//go:nosplit
func (c *Pass3CacheLineAligned) StepAndQuote(num uint64, hash [32]byte, parent [32]byte, prevMaxFee uint64, prevTip uint64, baseFee uint64) (uint64, uint64) {
	slot := &c.slots[num&255]
	slot.number = num
	slot.hash = hash
	slot.parentHash = parent
	c.tip = num

	nextTip := (prevTip*11_500 + 9_999) / 10_000
	if nextTip <= prevTip {
		nextTip = prevTip + 1
	}
	nextMax := (prevMaxFee*11_500 + 9_999) / 10_000
	marketMax := (baseFee << 1) + nextTip
	if marketMax > nextMax {
		nextMax = marketMax
	}
	return nextMax, nextTip
}
