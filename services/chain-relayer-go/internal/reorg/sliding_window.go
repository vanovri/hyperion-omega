package reorg

import (
	"context"
	"errors"
	"fmt"
	"sync"
)

const (
	// DefaultWindowCapacity must be a power of two >= 32 (256 covers up to 255-block forks; L2 target is 1..12).
	DefaultWindowCapacity = 256
	// DefaultMinFinalityBlocks is the canonical L2 finality threshold mandated in 01-hyperion-omega.md Section 6.3.
	DefaultMinFinalityBlocks = 12
	// DefaultMaxReorgDepth bounds automated rollback depth (1..12 blocks standard; configurable up to 64).
	DefaultMaxReorgDepth = 12
)

var (
	ErrInvalidWindowConfig   = errors.New("reorg: invalid sliding window configuration")
	ErrNonContiguousGap      = errors.New("reorg: block number exceeds tip + 1 without ancestor resolver")
	ErrCommonAncestorMissing = errors.New("reorg: common ancestor not found within sliding window")
	ErrDeepReorgExceededMax  = errors.New("reorg: detected fork depth exceeds max automatic reorg threshold")
	ErrZeroBlockHash         = errors.New("reorg: zero block hash rejected")
)

// BlockHeader represents an immutable L2 block header tracked in the parentHash sliding window.
type BlockHeader struct {
	Number     uint64
	Hash       [32]byte
	ParentHash [32]byte
	Timestamp  uint64
	BaseFeeWei uint64
}

// HeaderFetcher fetches canonical headers by height from the L2 RPC node when walking back a fork.
type HeaderFetcher interface {
	HeaderByNumber(ctx context.Context, number uint64) (BlockHeader, error)
}

// ReorgDetection captures exact fork geometry for constructing omegav1.ChainReorgAlert.
type ReorgDetection struct {
	Detected                  bool
	CommonAncestorBlockNumber uint64
	CommonAncestorBlockHash   [32]byte
	OrphanedTipBlockNumber    uint64
	OrphanedTipBlockHash      [32]byte
	NewCanonicalTipNumber     uint64
	NewCanonicalTipHash       [32]byte
	ReorgDepth                uint32
	OrphanedHeaders           []BlockHeader
	NewCanonicalBranch        []BlockHeader
}

// cacheAlignedSlot packs one block header entry into two 64-byte CPU cache lines (128 bytes total)
// to eliminate false sharing across concurrent reader/verifier goroutines.
type cacheAlignedSlot struct {
	number     uint64
	timestamp  uint64
	baseFeeWei uint64
	occupied   uint64
	hash       [32]byte
	parentHash [32]byte
	_pad       [32]byte
}

// SlidingWindow tracks canonical L2 block parentHash continuity across a power-of-two ring buffer.
type SlidingWindow struct {
	mu                sync.RWMutex
	mask              uint64
	capacity          uint64
	minFinalityBlocks uint64
	maxReorgDepth     uint32
	initialized       bool
	earliestNumber    uint64
	tipNumber         uint64
	reorgsDetected    uint64
	_padHead          [24]byte
	slots             []cacheAlignedSlot
}

// NewSlidingWindow constructs a cache-line aligned parentHash sliding window.
func NewSlidingWindow(capacity uint64, minFinalityBlocks uint64, maxReorgDepth uint32) (*SlidingWindow, error) {
	if capacity < 16 || (capacity&(capacity-1)) != 0 {
		return nil, fmt.Errorf("%w: capacity %d must be power of two >= 16", ErrInvalidWindowConfig, capacity)
	}
	if minFinalityBlocks == 0 || uint64(maxReorgDepth) >= capacity {
		return nil, fmt.Errorf("%w: finality=%d maxReorg=%d capacity=%d", ErrInvalidWindowConfig, minFinalityBlocks, maxReorgDepth, capacity)
	}
	if uint64(maxReorgDepth) > minFinalityBlocks {
		return nil, fmt.Errorf("%w: maxReorgDepth %d cannot exceed minFinalityBlocks %d", ErrInvalidWindowConfig, maxReorgDepth, minFinalityBlocks)
	}
	return &SlidingWindow{
		mask:              capacity - 1,
		capacity:          capacity,
		minFinalityBlocks: minFinalityBlocks,
		maxReorgDepth:     maxReorgDepth,
		slots:             make([]cacheAlignedSlot, capacity),
	}, nil
}

// Ingest evaluates a newly observed L2 block header, appending in O(1) on canonical progression
// or walking backward to resolve any 1..maxReorgDepth parentHash fork.
func (w *SlidingWindow) Ingest(ctx context.Context, hdr BlockHeader, fetcher HeaderFetcher) (ReorgDetection, error) {
	if hdr.Hash == ([32]byte{}) {
		return ReorgDetection{}, ErrZeroBlockHash
	}

	w.mu.Lock()
	defer w.mu.Unlock()

	if !w.initialized {
		w.writeSlotLocked(hdr)
		w.initialized = true
		w.earliestNumber = hdr.Number
		w.tipNumber = hdr.Number
		return ReorgDetection{}, nil
	}

	tipSlot := w.readSlotLocked(w.tipNumber)

	// 1. Idempotent duplicate delivery of current canonical tip
	if hdr.Number == w.tipNumber && hdr.Hash == tipSlot.Hash {
		return ReorgDetection{}, nil
	}

	// 2. Fast O(1) zero-allocation path: contiguous canonical child block
	if hdr.Number == w.tipNumber+1 && hdr.ParentHash == tipSlot.Hash {
		w.writeSlotLocked(hdr)
		w.tipNumber = hdr.Number
		if w.tipNumber >= w.earliestNumber+w.capacity {
			w.earliestNumber = w.tipNumber - w.capacity + 1
		}
		return ReorgDetection{}, nil
	}

	// 3. Gap ahead of tip + 1: backfill missing intermediate headers via fetcher first
	if hdr.Number > w.tipNumber+1 {
		if fetcher == nil {
			return ReorgDetection{}, fmt.Errorf("%w: tip=%d got=%d", ErrNonContiguousGap, w.tipNumber, hdr.Number)
		}
		return w.resolveBranchOrGapLocked(ctx, hdr, fetcher)
	}

	// 4. Same height with different hash, lower height with different hash, or tip+1 with mismatched ParentHash -> REORG!
	if hdr.Number <= w.tipNumber && hdr.Number >= w.earliestNumber {
		existing := w.readSlotLocked(hdr.Number)
		if existing.Hash == hdr.Hash {
			// Already canonical historical header
			return ReorgDetection{}, nil
		}
	}

	return w.resolveBranchOrGapLocked(ctx, hdr, fetcher)
}

func (w *SlidingWindow) resolveBranchOrGapLocked(
	ctx context.Context,
	newTip BlockHeader,
	fetcher HeaderFetcher,
) (ReorgDetection, error) {
	oldTip := w.readSlotLocked(w.tipNumber)

	// Walk backward from newTip along ParentHash until we hit a canonical block in [earliestNumber..tipNumber].
	branchRev := make([]BlockHeader, 0, int(w.maxReorgDepth)+4)
	cur := newTip
	branchRev = append(branchRev, cur)

	var (
		ancestorFound bool
		ancestorHdr   BlockHeader
	)

	for {
		parentNum := uint64(0)
		if cur.Number == 0 {
			break
		}
		parentNum = cur.Number - 1

		if parentNum >= w.earliestNumber && parentNum <= w.tipNumber {
			cand := w.readSlotLocked(parentNum)
			if cand.Hash == cur.ParentHash {
				ancestorFound = true
				ancestorHdr = cand
				break
			}
		}

		if parentNum < w.earliestNumber || fetcher == nil {
			break
		}

		parentHdr, err := fetcher.HeaderByNumber(ctx, parentNum)
		if err != nil {
			return ReorgDetection{}, fmt.Errorf("fetch fork parent %d: %w", parentNum, err)
		}
		if parentHdr.Hash != cur.ParentHash {
			return ReorgDetection{}, fmt.Errorf("inconsistent RPC parent chain at %d", parentNum)
		}
		cur = parentHdr
		branchRev = append(branchRev, cur)

		if uint64(len(branchRev)) > w.capacity {
			break
		}
	}

	if !ancestorFound {
		return ReorgDetection{}, ErrCommonAncestorMissing
	}

	// Pure forward gap (ancestor == oldTip -> no blocks were orphaned!)
	if ancestorHdr.Number == oldTip.Number && ancestorHdr.Hash == oldTip.Hash {
		for i := len(branchRev) - 1; i >= 0; i-- {
			b := branchRev[i]
			w.writeSlotLocked(b)
			w.tipNumber = b.Number
			if w.tipNumber >= w.earliestNumber+w.capacity {
				w.earliestNumber = w.tipNumber - w.capacity + 1
			}
		}
		return ReorgDetection{}, nil
	}

	depth64 := oldTip.Number - ancestorHdr.Number
	if depth64 > uint64(w.maxReorgDepth) {
		return ReorgDetection{}, fmt.Errorf(
			"%w: depth=%d max=%d (ancestor=%d oldTip=%d)",
			ErrDeepReorgExceededMax,
			depth64,
			w.maxReorgDepth,
			ancestorHdr.Number,
			oldTip.Number,
		)
	}

	orphaned := make([]BlockHeader, 0, depth64)
	for n := ancestorHdr.Number + 1; n <= oldTip.Number; n++ {
		orphaned = append(orphaned, w.readSlotLocked(n))
		w.clearSlotLocked(n)
	}

	newBranch := make([]BlockHeader, len(branchRev))
	for i := range branchRev {
		newBranch[i] = branchRev[len(branchRev)-1-i]
	}

	for _, b := range newBranch {
		w.writeSlotLocked(b)
	}
	w.tipNumber = newTip.Number
	if w.tipNumber >= w.earliestNumber+w.capacity {
		w.earliestNumber = w.tipNumber - w.capacity + 1
	}
	w.reorgsDetected++

	return ReorgDetection{
		Detected:                  true,
		CommonAncestorBlockNumber: ancestorHdr.Number,
		CommonAncestorBlockHash:   ancestorHdr.Hash,
		OrphanedTipBlockNumber:    oldTip.Number,
		OrphanedTipBlockHash:      oldTip.Hash,
		NewCanonicalTipNumber:     newTip.Number,
		NewCanonicalTipHash:       newTip.Hash,
		ReorgDepth:                uint32(depth64),
		OrphanedHeaders:           orphaned,
		NewCanonicalBranch:        newBranch,
	}, nil
}

// FastIngestContiguous is the zero-allocation hot-path helper when caller already verified no gap.
func (w *SlidingWindow) FastIngestContiguous(hdr BlockHeader) bool {
	w.mu.Lock()
	if !w.initialized {
		w.writeSlotLocked(hdr)
		w.initialized = true
		w.earliestNumber = hdr.Number
		w.tipNumber = hdr.Number
		w.mu.Unlock()
		return true
	}
	tipIdx := w.tipNumber & w.mask
	if hdr.Number == w.tipNumber+1 && hdr.ParentHash == w.slots[tipIdx].hash {
		w.writeSlotLocked(hdr)
		w.tipNumber = hdr.Number
		if w.tipNumber >= w.earliestNumber+w.capacity {
			w.earliestNumber = w.tipNumber - w.capacity + 1
		}
		w.mu.Unlock()
		return true
	}
	w.mu.Unlock()
	return false
}

// IsCanonicalAt returns true iff (blockNumber, blockHash) is currently on the active canonical chain
// and returns the current confirmation count (tip - blockNumber + 1).
func (w *SlidingWindow) IsCanonicalAt(blockNumber uint64, blockHash [32]byte) (canonical bool, confirmations uint32) {
	w.mu.RLock()
	defer w.mu.RUnlock()

	if !w.initialized || blockNumber < w.earliestNumber || blockNumber > w.tipNumber {
		return false, 0
	}
	slot := &w.slots[blockNumber&w.mask]
	if slot.occupied == 0 || slot.number != blockNumber || slot.hash != blockHash {
		return false, 0
	}
	conf64 := (w.tipNumber - blockNumber) + 1
	if conf64 > 0xFFFFFFFF {
		conf64 = 0xFFFFFFFF
	}
	return true, uint32(conf64)
}

// IsFinalized returns true iff the block is canonical AND confirmations >= minFinalityBlocks.
func (w *SlidingWindow) IsFinalized(blockNumber uint64, blockHash [32]byte) (bool, uint32) {
	ok, conf := w.IsCanonicalAt(blockNumber, blockHash)
	if !ok || uint64(conf) < w.minFinalityBlocks {
		return false, conf
	}
	return true, conf
}

// Tip returns the latest canonical tip, finalized watermark, and cumulative reorgs handled.
func (w *SlidingWindow) Status() (initialized bool, tipNumber uint64, finalizedNumber uint64, reorgsHandled uint64) {
	w.mu.RLock()
	defer w.mu.RUnlock()

	if !w.initialized {
		return false, 0, 0, w.reorgsDetected
	}
	var fin uint64
	if w.tipNumber+1 >= w.minFinalityBlocks {
		fin = (w.tipNumber + 1) - w.minFinalityBlocks
	}
	return true, w.tipNumber, fin, w.reorgsDetected
}

func (w *SlidingWindow) writeSlotLocked(hdr BlockHeader) {
	idx := hdr.Number & w.mask
	w.slots[idx] = cacheAlignedSlot{
		number:     hdr.Number,
		timestamp:  hdr.Timestamp,
		baseFeeWei: hdr.BaseFeeWei,
		occupied:   1,
		hash:       hdr.Hash,
		parentHash: hdr.ParentHash,
	}
}

func (w *SlidingWindow) clearSlotLocked(number uint64) {
	idx := number & w.mask
	if w.slots[idx].number == number {
		w.slots[idx] = cacheAlignedSlot{}
	}
}

func (w *SlidingWindow) readSlotLocked(number uint64) BlockHeader {
	s := &w.slots[number&w.mask]
	return BlockHeader{
		Number:     s.number,
		Hash:       s.hash,
		ParentHash: s.parentHash,
		Timestamp:  s.timestamp,
		BaseFeeWei: s.baseFeeWei,
	}
}
