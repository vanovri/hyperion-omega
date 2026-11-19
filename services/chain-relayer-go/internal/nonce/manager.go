package nonce

import (
	"container/heap"
	"errors"
	"fmt"
	"sync"
)

var (
	ErrEmptyTxID         = errors.New("nonce: empty outbox txID")
	ErrUnknownLease      = errors.New("nonce: no active lease for txID")
	ErrNonceMismatch     = errors.New("nonce: lease nonce mismatch")
	ErrAlreadyFinalized  = errors.New("nonce: lease already finalized")
)

type uint64MinHeap []uint64

func (h uint64MinHeap) Len() int           { return len(h) }
func (h uint64MinHeap) Less(i, j int) bool { return h[i] < h[j] }
func (h uint64MinHeap) Swap(i, j int)      { h[i], h[j] = h[j], h[i] }
func (h *uint64MinHeap) Push(x any)        { *h = append(*h, x.(uint64)) }
func (h *uint64MinHeap) Pop() any {
	old := *h
	n := len(old)
	v := old[n-1]
	*h = old[:n-1]
	return v
}

// LeaseState tracks the lifecycle of an allocated EOA transaction nonce.
type LeaseState uint8

const (
	LeaseActive LeaseState = iota + 1
	LeaseFinalized
)

type leaseEntry struct {
	txID  string
	nonce uint64
	state LeaseState
}

// Manager provides collision-free, gap-healing EOA nonce allocation across concurrent relayer workers,
// supporting idempotent EIP-1559 fee-bump replacement on the same nonce and reorg rollback recovery.
type Manager struct {
	mu             sync.Mutex
	nextContiguous uint64
	recycled       uint64MinHeap
	inRecycled     map[uint64]struct{}
	byTxID         map[string]*leaseEntry
	byNonce        map[uint64]string
	maxFinalized   int
	finalizedFIFO  []string
}

// NewManager initializes a collision-free nonce allocator anchored at initialOnChainNonce.
func NewManager(initialOnChainNonce uint64) *Manager {
	m := &Manager{
		nextContiguous: initialOnChainNonce,
		recycled:       make(uint64MinHeap, 0, 32),
		inRecycled:     make(map[uint64]struct{}, 32),
		byTxID:         make(map[string]*leaseEntry, 256),
		byNonce:        make(map[uint64]string, 256),
		maxFinalized:   4096,
		finalizedFIFO:  make([]string, 0, 4096),
	}
	heap.Init(&m.recycled)
	return m
}

// AcquireOrReuse returns the existing leased nonce if txID is already in-flight (e.g., EIP-1559 fee bump
// or post-reorg re-broadcast), or atomically allocates the lowest available gap-free nonce.
func (m *Manager) AcquireOrReuse(txID string) (nonce uint64, reused bool, err error) {
	if txID == "" {
		return 0, false, ErrEmptyTxID
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	if existing, ok := m.byTxID[txID]; ok {
		if existing.state == LeaseFinalized {
			return existing.nonce, true, ErrAlreadyFinalized
		}
		return existing.nonce, true, nil
	}

	var assigned uint64
	if m.recycled.Len() > 0 {
		assigned = heap.Pop(&m.recycled).(uint64)
		delete(m.inRecycled, assigned)
	} else {
		assigned = m.nextContiguous
		m.nextContiguous++
	}

	entry := &leaseEntry{
		txID:  txID,
		nonce: assigned,
		state: LeaseActive,
	}
	m.byTxID[txID] = entry
	m.byNonce[assigned] = txID
	return assigned, false, nil
}

// AbortPreBroadcast releases an unbroadcast nonce back into the min-heap so zero nonce gaps form.
func (m *Manager) AbortPreBroadcast(txID string, expectedNonce uint64) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	entry, ok := m.byTxID[txID]
	if !ok {
		return fmt.Errorf("%w: %s", ErrUnknownLease, txID)
	}
	if entry.state == LeaseFinalized {
		return ErrAlreadyFinalized
	}
	if entry.nonce != expectedNonce {
		return fmt.Errorf("%w: got %d want %d", ErrNonceMismatch, expectedNonce, entry.nonce)
	}

	delete(m.byTxID, txID)
	delete(m.byNonce, expectedNonce)

	// Collapse contiguous tail when possible; otherwise push to min-heap for immediate gap healing.
	if expectedNonce+1 == m.nextContiguous {
		m.nextContiguous--
		for m.nextContiguous > 0 {
			prev := m.nextContiguous - 1
			if _, inHeap := m.inRecycled[prev]; !inHeap {
				break
			}
			delete(m.inRecycled, prev)
			m.nextContiguous--
		}
		if len(m.inRecycled) == 0 && m.recycled.Len() > 0 {
			m.recycled = m.recycled[:0]
		} else if m.recycled.Len() > len(m.inRecycled) {
			m.rebuildHeapLocked()
		}
		return nil
	}

	if _, exists := m.inRecycled[expectedNonce]; !exists {
		m.inRecycled[expectedNonce] = struct{}{}
		heap.Push(&m.recycled, expectedNonce)
	}
	return nil
}

// MarkFinalized marks a leased nonce as irreversibly finalized on-chain and evicts bounded history in O(1) amortized memory.
func (m *Manager) MarkFinalized(txID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	entry, ok := m.byTxID[txID]
	if !ok {
		return fmt.Errorf("%w: %s", ErrUnknownLease, txID)
	}
	if entry.state == LeaseFinalized {
		return nil
	}
	entry.state = LeaseFinalized
	delete(m.byNonce, entry.nonce)

	m.finalizedFIFO = append(m.finalizedFIFO, txID)
	if len(m.finalizedFIFO) > m.maxFinalized {
		evictID := m.finalizedFIFO[0]
		copy(m.finalizedFIFO, m.finalizedFIFO[1:])
		m.finalizedFIFO = m.finalizedFIFO[:len(m.finalizedFIFO)-1]
		if old, exists := m.byTxID[evictID]; exists && old.state == LeaseFinalized {
			delete(m.byTxID, evictID)
		}
	}
	return nil
}

// ReopenAfterReorg transitions a previously mined (unfinalized) txID back to active in-flight lease
// preserving its exact nonce so re-broadcast cannot collide or skip a nonce.
func (m *Manager) ReopenAfterReorg(txID string, nonce uint64) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if entry, ok := m.byTxID[txID]; ok {
		entry.state = LeaseActive
		entry.nonce = nonce
		m.byNonce[nonce] = txID
		return
	}
	m.byTxID[txID] = &leaseEntry{
		txID:  txID,
		nonce: nonce,
		state: LeaseActive,
	}
	m.byNonce[nonce] = txID
	if nonce >= m.nextContiguous {
		m.nextContiguous = nonce + 1
	}
}

// ReconcileOnChainFloor advances nextContiguous if external transactions advanced the EOA nonce on-chain,
// purging any stale recycled nonces < onChainPendingFloor.
func (m *Manager) ReconcileOnChainFloor(onChainConfirmedNonce uint64) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if onChainConfirmedNonce > m.nextContiguous {
		m.nextContiguous = onChainConfirmedNonce
	}
	changed := false
	for n := range m.inRecycled {
		if n < onChainConfirmedNonce {
			if _, active := m.byNonce[n]; !active {
				delete(m.inRecycled, n)
				changed = true
			}
		}
	}
	if changed {
		m.rebuildHeapLocked()
	}
}

// Snapshot returns current allocator telemetry.
func (m *Manager) Snapshot() (nextContiguous uint64, activeLeases int, recycledGaps int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.nextContiguous, len(m.byNonce), len(m.inRecycled)
}

func (m *Manager) rebuildHeapLocked() {
	m.recycled = m.recycled[:0]
	for n := range m.inRecycled {
		m.recycled = append(m.recycled, n)
	}
	heap.Init(&m.recycled)
}
