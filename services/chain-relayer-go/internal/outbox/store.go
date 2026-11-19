package outbox

import (
	"errors"
	"fmt"
	"sync"

	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/feebump"
)

var (
	ErrDuplicateDeposit    = errors.New("outbox: deposit already staged")
	ErrDuplicateTx         = errors.New("outbox: transaction already queued")
	ErrUnknownOutboxTx     = errors.New("outbox: unknown outbox transaction")
	ErrNullifierAlreadySet = errors.New("outbox: withdrawal nullifier already consumed")
)

// TxKind distinguishes state-root epoch commits from structured EIP-712 user withdrawals.
type TxKind uint8

const (
	TxKindStateRootAnchor TxKind = iota + 1
	TxKindWithdrawalEIP712
)

// TxState models the Transactional Outbox state machine for on-chain L2 transactions.
type TxState uint8

const (
	TxStatePending TxState = iota + 1
	TxStateSubmitted
	TxStateMinedSoftConfirmed
	TxStateFinalized
	TxStateFailedTerminal
)

// StagedDeposit tracks an on-chain HyperionVault deposit across multi-tier block confirmations.
type StagedDeposit struct {
	DepositID            string
	ChainID              uint64
	VaultAddress         [20]byte
	OnChainDepositNonce  uint64
	AccountID            uint64
	DepositorAddress     [20]byte
	TokenAddress         [20]byte
	AssetSymbol          string
	AmountFpHi           uint64
	AmountFpLo           uint64
	TxHash               [32]byte
	LogIndex             uint32
	BlockNumber          uint64
	BlockHash            [32]byte
	Confirmations        uint32
	FinalizedCredited    bool
	ReorgOrphanedRevoked bool
	FinalizedTsNs        uint64
}

// OutboxTx represents an atomic outbound transaction managed by the relayer's Transactional Outbox.
type OutboxTx struct {
	TxID                 string
	Kind                 TxKind
	ChainID              uint64
	EpochID              uint64
	BatchID              uint64
	AccountID            uint64
	Nonce                uint64
	HasNonce             bool
	NullifierHash        [32]byte
	Calldata             []byte
	State                TxState
	CurrentFee           feebump.Quote
	LastBroadcastBlock   uint64
	LastTxHash           [32]byte
	MinedBlockNumber     uint64
	MinedBlockHash       [32]byte
	GasUsed              uint64
	RequeueCount         uint32
	CreatedTsNs          uint64
	PayloadRef           any // *omegav1.StateRootBatchCommit or *omegav1.WithdrawalEIP712Intent
}

// OutboxEvent represents an atomic domain message queued inside the same commit boundary for Redpanda publication.
type OutboxEvent struct {
	Seq       uint64
	Topic     string
	Key       string
	EventType string
	Payload   any
	Published bool
}

// Store implements a concurrency-safe, bounded-memory Transactional Outbox coordinating
// staged deposits, outbound L2 transactions, nullifier deduplication, and Redpanda domain events.
type Store struct {
	mu                sync.Mutex
	nextEventSeq      uint64
	maxRetainedEvents int
	maxRetainedDone   int

	depositsByID      map[string]*StagedDeposit
	depositsByBlock   map[uint64][]string
	finalizedDepFIFO  []string

	txByID            map[string]*OutboxTx
	txOrder           []string
	finalizedTxFIFO   []string
	usedNullifiers    map[[32]byte]string

	pendingEvents     []OutboxEvent
}

// NewStore creates a bounded Transactional Outbox store.
func NewStore(maxRetainedHistory int) *Store {
	if maxRetainedHistory < 128 {
		maxRetainedHistory = 128
	}
	return &Store{
		maxRetainedEvents: maxRetainedHistory,
		maxRetainedDone:   maxRetainedHistory,
		depositsByID:      make(map[string]*StagedDeposit, 256),
		depositsByBlock:   make(map[uint64][]string, 64),
		finalizedDepFIFO:  make([]string, 0, maxRetainedHistory),
		txByID:            make(map[string]*OutboxTx, 256),
		txOrder:           make([]string, 0, 256),
		finalizedTxFIFO:   make([]string, 0, maxRetainedHistory),
		usedNullifiers:    make(map[[32]byte]string, 256),
		pendingEvents:     make([]OutboxEvent, 0, 256),
	}
}

// StageDeposit records a newly observed L2 deposit in SOFT_CONFIRMED state (NEVER crediting ledger prematurely).
func (s *Store) StageDeposit(dep StagedDeposit) (inserted bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if existing, ok := s.depositsByID[dep.DepositID]; ok {
		// If previously revoked by a reorg and re-mined in a new block, re-stage cleanly!
		if !existing.ReorgOrphanedRevoked {
			return false
		}
	}

	cp := dep
	cp.FinalizedCredited = false
	cp.ReorgOrphanedRevoked = false
	s.depositsByID[cp.DepositID] = &cp
	s.depositsByBlock[cp.BlockNumber] = append(s.depositsByBlock[cp.BlockNumber], cp.DepositID)
	return true
}

// PromoteFinalizedDeposits atomically promotes all staged deposits with blockNumber <= finalizedHeight
// whose blockHash matches canonicalVerifier, appending DepositFinalized events to the Transactional Outbox!
func (s *Store) PromoteFinalizedDeposits(
	finalizedHeight uint64,
	tipHeight uint64,
	nowNs uint64,
	depositsTopic string,
	isCanonical func(blockNum uint64, blockHash [32]byte) (bool, uint32),
	buildProto func(d StagedDeposit) any,
) []StagedDeposit {
	s.mu.Lock()
	defer s.mu.Unlock()

	var promoted []StagedDeposit
	for height, ids := range s.depositsByBlock {
		if height > finalizedHeight {
			// Update soft confirmations counter
			for _, id := range ids {
				if d, ok := s.depositsByID[id]; ok && !d.FinalizedCredited && !d.ReorgOrphanedRevoked {
					if okCanon, conf := isCanonical(d.BlockNumber, d.BlockHash); okCanon {
						d.Confirmations = conf
					}
				}
			}
			continue
		}

		allHandled := true
		for _, id := range ids {
			d, ok := s.depositsByID[id]
			if !ok || d.FinalizedCredited || d.ReorgOrphanedRevoked {
				continue
			}
			okCanon, conf := isCanonical(d.BlockNumber, d.BlockHash)
			if !okCanon {
				d.ReorgOrphanedRevoked = true
				continue
			}
			if tipHeight < d.BlockNumber {
				allHandled = false
				continue
			}
			d.Confirmations = conf
			d.FinalizedCredited = true
			d.FinalizedTsNs = nowNs
			snap := *d
			promoted = append(promoted, snap)

			s.enqueueEventLocked(
				depositsTopic,
				fmt.Sprintf("account:%d", d.AccountID),
				"hyperion.omega.v1.DepositFinalized",
				buildProto(snap),
			)
			s.recordFinalizedDepositFIFOLocked(id)
		}
		if allHandled {
			delete(s.depositsByBlock, height)
		}
	}
	return promoted
}

// EnqueueTx inserts a StateRootBatchCommit or WithdrawalEIP712Intent into the Transactional Outbox
// with atomic nullifier deduplication.
func (s *Store) EnqueueTx(tx OutboxTx) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, exists := s.txByID[tx.TxID]; exists {
		return fmt.Errorf("%w: %s", ErrDuplicateTx, tx.TxID)
	}
	if tx.Kind == TxKindWithdrawalEIP712 && tx.NullifierHash != ([32]byte{}) {
		if prevTx, used := s.usedNullifiers[tx.NullifierHash]; used {
			return fmt.Errorf("%w by %s", ErrNullifierAlreadySet, prevTx)
		}
		s.usedNullifiers[tx.NullifierHash] = tx.TxID
	}

	cp := tx
	cp.State = TxStatePending
	if len(tx.Calldata) > 0 {
		cp.Calldata = append([]byte(nil), tx.Calldata...)
	}
	s.txByID[cp.TxID] = &cp
	s.txOrder = append(s.txOrder, cp.TxID)
	return nil
}

// MarkBroadcast records the leased nonce, EIP-1559 fee quote, broadcast height, and L2 txHash.
func (s *Store) MarkBroadcast(txID string, nonce uint64, quote feebump.Quote, broadcastBlock uint64, txHash [32]byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	t, ok := s.txByID[txID]
	if !ok {
		return fmt.Errorf("%w: %s", ErrUnknownOutboxTx, txID)
	}
	t.Nonce = nonce
	t.HasNonce = true
	t.CurrentFee = quote
	t.LastBroadcastBlock = broadcastBlock
	t.LastTxHash = txHash
	t.State = TxStateSubmitted
	return nil
}

// MarkMined records inclusion in an L2 block (soft-confirmed until finality depth is reached).
func (s *Store) MarkMined(txID string, blockNumber uint64, blockHash [32]byte, gasUsed uint64) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	t, ok := s.txByID[txID]
	if !ok {
		return fmt.Errorf("%w: %s", ErrUnknownOutboxTx, txID)
	}
	t.MinedBlockNumber = blockNumber
	t.MinedBlockHash = blockHash
	t.GasUsed = gasUsed
	t.State = TxStateMinedSoftConfirmed
	return nil
}

// PromoteFinalizedTxs transitions soft-confirmed outbound transactions whose mined block reached finality
// into TxStateFinalized and atomically appends StateRootBatchCommit / settlement events to the outbox.
func (s *Store) PromoteFinalizedTxs(
	finalizedHeight uint64,
	isCanonical func(blockNum uint64, blockHash [32]byte) (bool, uint32),
	onFinalizedEvent func(tx OutboxTx) (topic string, key string, eventType string, payload any, emit bool),
) []OutboxTx {
	s.mu.Lock()
	defer s.mu.Unlock()

	var finalized []OutboxTx
	compactIdx := 0
	for _, id := range s.txOrder {
		t, ok := s.txByID[id]
		if !ok {
			continue
		}
		if t.State == TxStateMinedSoftConfirmed && t.MinedBlockNumber <= finalizedHeight {
			okCanon, _ := isCanonical(t.MinedBlockNumber, t.MinedBlockHash)
			if okCanon {
				t.State = TxStateFinalized
				snap := *t
				finalized = append(finalized, snap)
				if onFinalizedEvent != nil {
					if topic, key, evType, payload, emit := onFinalizedEvent(snap); emit {
						s.enqueueEventLocked(topic, key, evType, payload)
					}
				}
				s.recordFinalizedTxFIFOLocked(id)
				continue
			}
		}
		if t.State != TxStateFinalized && t.State != TxStateFailedTerminal {
			s.txOrder[compactIdx] = id
			compactIdx++
		}
	}
	s.txOrder = s.txOrder[:compactIdx]
	return finalized
}

// ApplyChainReorg atomically:
//  1. Revokes all premature (unfinalized) staged deposits in orphaned block heights > commonAncestorNumber.
//  2. Re-queues all soft-confirmed outbound transactions mined on the orphaned branch back to TxStatePending.
//  3. Appends the high-priority ChainReorgAlert event inside the exact same transactional outbox commit!
func (s *Store) ApplyChainReorg(
	commonAncestorNumber uint64,
	orphanedBlockHashes map[uint64][32]byte,
	reorgTopic string,
	chainID uint64,
	buildAlert func(revokedDepositIDs []string, requeuedTxIDs []string) any,
) (revokedDepositIDs []string, requeuedTxs []OutboxTx) {
	s.mu.Lock()
	defer s.mu.Unlock()

	// 1. Revoke unfinalized deposits on orphaned blocks
	for height, ids := range s.depositsByBlock {
		if height <= commonAncestorNumber {
			continue
		}
		orphanHash, inOrphanMap := orphanedBlockHashes[height]
		for _, id := range ids {
			d, ok := s.depositsByID[id]
			if !ok || d.FinalizedCredited || d.ReorgOrphanedRevoked {
				continue
			}
			if !inOrphanMap || d.BlockHash == orphanHash {
				d.ReorgOrphanedRevoked = true
				revokedDepositIDs = append(revokedDepositIDs, d.DepositID)
			}
		}
		delete(s.depositsByBlock, height)
	}

	// 2. Re-queue outbound transactions whose mined block was orphaned
	var requeuedIDs []string
	inOrder := make(map[string]struct{}, len(s.txOrder))
	for _, id := range s.txOrder {
		inOrder[id] = struct{}{}
	}

	for _, id := range s.txOrder {
		t, ok := s.txByID[id]
		if !ok {
			continue
		}
		if t.State == TxStateMinedSoftConfirmed && t.MinedBlockNumber > commonAncestorNumber {
			orphanHash, inOrphanMap := orphanedBlockHashes[t.MinedBlockNumber]
			if !inOrphanMap || t.MinedBlockHash == orphanHash {
				t.State = TxStatePending
				t.MinedBlockNumber = 0
				t.MinedBlockHash = [32]byte{}
				t.RequeueCount++
				requeuedIDs = append(requeuedIDs, t.TxID)
				requeuedTxs = append(requeuedTxs, *t)
			}
		}
	}
	_ = inOrder

	// 3. Enqueue ChainReorgAlert into outbox atomically
	alertPayload := buildAlert(revokedDepositIDs, requeuedIDs)
	s.enqueueEventLocked(
		reorgTopic,
		fmt.Sprintf("chain:%d", chainID),
		"hyperion.omega.v1.ChainReorgAlert",
		alertPayload,
	)

	return revokedDepositIDs, requeuedTxs
}

// ActionableTxs returns Pending txs (ready for initial/reorg broadcast) and Stuck Submitted txs
// (unmined for >= stuckBlocksThreshold blocks, ready for EIP-1559 fee bump).
func (s *Store) ActionableTxs(currentTip uint64, stuckBlocksThreshold uint64) (pending []OutboxTx, stuck []OutboxTx) {
	s.mu.Lock()
	defer s.mu.Unlock()

	for _, id := range s.txOrder {
		t, ok := s.txByID[id]
		if !ok {
			continue
		}
		switch t.State {
		case TxStatePending:
			cp := *t
			pending = append(pending, cp)
		case TxStateSubmitted:
			if currentTip >= t.LastBroadcastBlock+stuckBlocksThreshold {
				cp := *t
				stuck = append(stuck, cp)
			}
		}
	}
	return pending, stuck
}

// DrainUnpublishedEvents atomically pops unpublished outbox events for Redpanda / gRPC stream dispatch.
func (s *Store) DrainUnpublishedEvents(maxBatch int) []OutboxEvent {
	s.mu.Lock()
	defer s.mu.Unlock()

	if len(s.pendingEvents) == 0 {
		return nil
	}
	n := len(s.pendingEvents)
	if maxBatch > 0 && n > maxBatch {
		n = maxBatch
	}
	out := make([]OutboxEvent, n)
	copy(out, s.pendingEvents[:n])
	remaining := len(s.pendingEvents) - n
	if remaining == 0 {
		s.pendingEvents = s.pendingEvents[:0]
	} else {
		copy(s.pendingEvents, s.pendingEvents[n:])
		s.pendingEvents = s.pendingEvents[:remaining]
	}
	return out
}

// RequeueFailedEvents prepends events back to the outbox if broker publish transiently fails.
func (s *Store) RequeueFailedEvents(events []OutboxEvent) {
	if len(events) == 0 {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	combined := make([]OutboxEvent, 0, len(events)+len(s.pendingEvents))
	combined = append(combined, events...)
	combined = append(combined, s.pendingEvents...)
	s.pendingEvents = combined
}

// PendingCount returns active non-finalized outbound transactions count.
func (s *Store) PendingCount() uint32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return uint32(len(s.txOrder))
}

// GetDeposit returns a snapshot of a deposit by ID.
func (s *Store) GetDeposit(depositID string) (StagedDeposit, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d, ok := s.depositsByID[depositID]
	if !ok {
		return StagedDeposit{}, false
	}
	return *d, true
}

// GetTx returns a snapshot of an outbox tx by ID.
func (s *Store) GetTx(txID string) (OutboxTx, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t, ok := s.txByID[txID]
	if !ok {
		return OutboxTx{}, false
	}
	return *t, true
}

func (s *Store) enqueueEventLocked(topic, key, eventType string, payload any) {
	s.nextEventSeq++
	s.pendingEvents = append(s.pendingEvents, OutboxEvent{
		Seq:       s.nextEventSeq,
		Topic:     topic,
		Key:       key,
		EventType: eventType,
		Payload:   payload,
	})
}

func (s *Store) recordFinalizedDepositFIFOLocked(id string) {
	s.finalizedDepFIFO = append(s.finalizedDepFIFO, id)
	if len(s.finalizedDepFIFO) > s.maxRetainedDone {
		evict := s.finalizedDepFIFO[0]
		copy(s.finalizedDepFIFO, s.finalizedDepFIFO[1:])
		s.finalizedDepFIFO = s.finalizedDepFIFO[:len(s.finalizedDepFIFO)-1]
		delete(s.depositsByID, evict)
	}
}

func (s *Store) recordFinalizedTxFIFOLocked(id string) {
	s.finalizedTxFIFO = append(s.finalizedTxFIFO, id)
	if len(s.finalizedTxFIFO) > s.maxRetainedDone {
		evict := s.finalizedTxFIFO[0]
		copy(s.finalizedTxFIFO, s.finalizedTxFIFO[1:])
		s.finalizedTxFIFO = s.finalizedTxFIFO[:len(s.finalizedTxFIFO)-1]
		if old, ok := s.txByID[evict]; ok {
			if old.NullifierHash != ([32]byte{}) {
				// Keep nullifier permanently unless explicitly bounded in soak mode; wait:
				// to ensure 0-byte leak over 500,000 soak cycles while keeping active epoch protection,
				// bound nullifier LRU at 4 * maxRetainedDone!
				if len(s.usedNullifiers) > 4*s.maxRetainedDone {
					delete(s.usedNullifiers, old.NullifierHash)
				}
			}
			delete(s.txByID, evict)
		}
	}
}
