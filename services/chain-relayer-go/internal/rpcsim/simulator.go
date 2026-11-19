package rpcsim

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"sync"

	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/reorg"
)

var (
	ErrInjectedRPCFailure        = errors.New("rpcsim: injected consecutive RPC failure")
	ErrBlockNotFound             = errors.New("rpcsim: canonical block not found")
	ErrNonceTooLow               = errors.New("rpcsim: nonce too low")
	ErrReplacementUnderpriced    = errors.New("rpcsim: replacement transaction underpriced (< +12.5% bump)")
	ErrMaxFeeBelowBaseFee        = errors.New("rpcsim: maxFeePerGas below block baseFee")
	ErrReorgDepthExceedsHistory  = errors.New("rpcsim: requested reorg depth exceeds chain height")
)

// DepositLog models an emitted HyperionVault.DepositReceived event on a specific L2 block.
type DepositLog struct {
	DepositID           string
	VaultAddress        [20]byte
	OnChainDepositNonce uint64
	AccountID           uint64
	DepositorAddress    [20]byte
	TokenAddress        [20]byte
	AssetSymbol         string
	AmountFpHi          uint64
	AmountFpLo          uint64
	TxHash              [32]byte
	LogIndex            uint32
	BlockNumber         uint64
	BlockHash           [32]byte
}

// SubmittedTx models an EIP-1559 Type-2 transaction submitted by chain-relayer-go.
type SubmittedTx struct {
	OutboxTxID              string
	Nonce                   uint64
	MaxFeePerGasWei         uint64
	MaxPriorityFeePerGasWei uint64
	GasLimit                uint64
	Calldata                []byte
	TxHash                  [32]byte
}

// TxReceipt models an L2 transaction inclusion receipt.
type TxReceipt struct {
	OutboxTxID  string
	TxHash      [32]byte
	Nonce       uint64
	BlockNumber uint64
	BlockHash   [32]byte
	GasUsed     uint64
	StatusOK    bool
}

// L2ChainClient defines the abstract RPC client interface consumed by the Chain Relayer daemon.
type L2ChainClient interface {
	ChainID(ctx context.Context) (uint64, error)
	LatestHeader(ctx context.Context) (reorg.BlockHeader, error)
	HeaderByNumber(ctx context.Context, number uint64) (reorg.BlockHeader, error)
	SuggestPriorityFee(ctx context.Context) (uint64, error)
	ConfirmedNonce(ctx context.Context) (uint64, error)
	DepositsInBlock(ctx context.Context, blockNumber uint64, blockHash [32]byte) ([]DepositLog, error)
	SendEIP1559Transaction(ctx context.Context, tx SubmittedTx) ([32]byte, error)
	TransactionReceipt(ctx context.Context, txHash [32]byte) (TxReceipt, bool, error)
}

type simBlock struct {
	header   reorg.BlockHeader
	deposits []DepositLog
	receipts []TxReceipt
}

// SimulatedL2Node is a thread-safe deterministic L2 EVM RPC simulator supporting:
//   - Sudden multi-block chain reorganizations (1..12+ blocks) with fork generation.
//   - Consecutive RPC outage / HTTP-503 / rate-limit fault injection.
//   - Strict EIP-1559 mempool nonce ordering & >= +12.5% replacement fee enforcement.
//   - BaseFee spikes & mempool holdback controls.
type SimulatedL2Node struct {
	mu                     sync.Mutex
	chainID                uint64
	forkSalt               uint64
	currentBaseFeeWei      uint64
	suggestedTipWei        uint64
	relayerConfirmedNonce  uint64
	holdMempool            bool
	remainingRPCFailures   int
	injectedFailureMessage string

	canonical []simBlock
	mempool   map[uint64]SubmittedTx
	receipts  map[[32]byte]TxReceipt
}

// NewSimulatedL2Node creates a genesis-initialized deterministic L2 node simulator.
func NewSimulatedL2Node(chainID uint64, initialBaseFeeWei uint64, initialTipWei uint64) *SimulatedL2Node {
	genesisHash := computeDeterministicHash(chainID, 0, [32]byte{}, 0)
	genesis := simBlock{
		header: reorg.BlockHeader{
			Number:     0,
			Hash:       genesisHash,
			ParentHash: [32]byte{},
			Timestamp:  1_795_000_000,
			BaseFeeWei: initialBaseFeeWei,
		},
	}
	return &SimulatedL2Node{
		chainID:           chainID,
		currentBaseFeeWei: initialBaseFeeWei,
		suggestedTipWei:   initialTipWei,
		canonical:         []simBlock{genesis},
		mempool:           make(map[uint64]SubmittedTx, 64),
		receipts:          make(map[[32]byte]TxReceipt, 256),
	}
}

// InjectConsecutiveFailures causes the next count RPC calls to fail with an injected transport error.
func (n *SimulatedL2Node) InjectConsecutiveFailures(count int, reason string) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.remainingRPCFailures = count
	if reason == "" {
		reason = "HTTP 503 upstream L2 sequencer RPC unavailable"
	}
	n.injectedFailureMessage = reason
}

// SetBaseFeeAndTip updates network gas conditions (e.g., simulating sudden congestion spikes).
func (n *SimulatedL2Node) SetBaseFeeAndTip(baseFeeWei uint64, tipWei uint64) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.currentBaseFeeWei = baseFeeWei
	n.suggestedTipWei = tipWei
}

// SetHoldMempool pauses/resumes inclusion of mempool txs so stuck-tx EIP-1559 fee bumping can be tested deterministically.
func (n *SimulatedL2Node) SetHoldMempool(hold bool) {
	n.mu.Lock()
	defer n.mu.Unlock()
	n.holdMempool = hold
}

// MineBlock appends a new canonical block including eligible contiguous-nonce mempool transactions
// and optional user deposits.
func (n *SimulatedL2Node) MineBlock(deposits ...DepositLog) reorg.BlockHeader {
	n.mu.Lock()
	defer n.mu.Unlock()
	return n.mineBlockLocked(deposits)
}

func (n *SimulatedL2Node) mineBlockLocked(deposits []DepositLog) reorg.BlockHeader {
	prev := n.canonical[len(n.canonical)-1].header
	nextNum := prev.Number + 1
	blkHash := computeDeterministicHash(n.chainID, nextNum, prev.Hash, n.forkSalt)

	hdr := reorg.BlockHeader{
		Number:     nextNum,
		Hash:       blkHash,
		ParentHash: prev.Hash,
		Timestamp:  prev.Timestamp + 2,
		BaseFeeWei: n.currentBaseFeeWei,
	}

	stampedDeposits := make([]DepositLog, len(deposits))
	for i, d := range deposits {
		d.BlockNumber = nextNum
		d.BlockHash = blkHash
		d.LogIndex = uint32(i)
		if d.TxHash == ([32]byte{}) {
			d.TxHash = computeTxDigest("deposit", d.DepositID, nextNum, n.forkSalt)
		}
		stampedDeposits[i] = d
	}

	var minedReceipts []TxReceipt
	if !n.holdMempool {
		for {
			tx, exists := n.mempool[n.relayerConfirmedNonce]
			if !exists {
				break
			}
			// EIP-1559 inclusion check: maxFeePerGas must cover block baseFeePerGas
			if tx.MaxFeePerGasWei < hdr.BaseFeeWei {
				break
			}
			delete(n.mempool, n.relayerConfirmedNonce)
			n.relayerConfirmedNonce++

			rcpt := TxReceipt{
				OutboxTxID:  tx.OutboxTxID,
				TxHash:      tx.TxHash,
				Nonce:       tx.Nonce,
				BlockNumber: nextNum,
				BlockHash:   blkHash,
				GasUsed:     142_500,
				StatusOK:    true,
			}
			n.receipts[tx.TxHash] = rcpt
			minedReceipts = append(minedReceipts, rcpt)
		}
	}

	n.canonical = append(n.canonical, simBlock{
		header:   hdr,
		deposits: stampedDeposits,
		receipts: minedReceipts,
	})
	return hdr
}

// InjectSuddenReorg orphans the top `depth` canonical blocks (`oldTip - depth + 1 .. oldTip`),
// rolls back relayerConfirmedNonce and receipts from orphaned blocks, and mines `depth + extraBlocks`
// blocks on a brand-new fork branch with distinct block hashes!
func (n *SimulatedL2Node) InjectSuddenReorg(depth uint64, extraBlocks uint64, forkDeposits map[uint64][]DepositLog) (ancestor reorg.BlockHeader, newTip reorg.BlockHeader, err error) {
	n.mu.Lock()
	defer n.mu.Unlock()

	tipIdx := uint64(len(n.canonical) - 1)
	if depth == 0 || depth > tipIdx {
		return reorg.BlockHeader{}, reorg.BlockHeader{}, fmt.Errorf("%w: depth=%d tip=%d", ErrReorgDepthExceedsHistory, depth, tipIdx)
	}

	ancestorIdx := tipIdx - depth
	ancestor = n.canonical[ancestorIdx].header

	// Evict receipts & roll back confirmed EOA nonces from orphaned blocks so relayer must re-submit / reconcile!
	minRolledBackNonce := n.relayerConfirmedNonce
	for idx := ancestorIdx + 1; idx <= tipIdx; idx++ {
		for _, rcpt := range n.canonical[idx].receipts {
			delete(n.receipts, rcpt.TxHash)
			if rcpt.Nonce < minRolledBackNonce {
				minRolledBackNonce = rcpt.Nonce
			}
		}
	}
	n.relayerConfirmedNonce = minRolledBackNonce
	n.canonical = n.canonical[:ancestorIdx+1]

	// Switch forkSalt so every block on the new canonical branch has a distinct cryptographic hash
	n.forkSalt++

	totalNewBlocks := depth + extraBlocks
	for i := uint64(0); i < totalNewBlocks; i++ {
		nextHeight := ancestor.Number + 1 + i
		var deps []DepositLog
		if forkDeposits != nil {
			deps = forkDeposits[nextHeight]
		}
		newTip = n.mineBlockLocked(deps)
	}
	return ancestor, newTip, nil
}

// =============================================================================
// L2ChainClient RPC METHODS (WITH FAULT INJECTION)
// =============================================================================

func (n *SimulatedL2Node) checkFaultLocked() error {
	if n.remainingRPCFailures > 0 {
		n.remainingRPCFailures--
		return fmt.Errorf("%w: %s", ErrInjectedRPCFailure, n.injectedFailureMessage)
	}
	return nil
}

func (n *SimulatedL2Node) ChainID(_ context.Context) (uint64, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return 0, err
	}
	return n.chainID, nil
}

func (n *SimulatedL2Node) LatestHeader(_ context.Context) (reorg.BlockHeader, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return reorg.BlockHeader{}, err
	}
	return n.canonical[len(n.canonical)-1].header, nil
}

func (n *SimulatedL2Node) HeaderByNumber(_ context.Context, number uint64) (reorg.BlockHeader, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return reorg.BlockHeader{}, err
	}
	if number >= uint64(len(n.canonical)) {
		return reorg.BlockHeader{}, fmt.Errorf("%w: %d", ErrBlockNotFound, number)
	}
	return n.canonical[number].header, nil
}

func (n *SimulatedL2Node) SuggestPriorityFee(_ context.Context) (uint64, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return 0, err
	}
	return n.suggestedTipWei, nil
}

func (n *SimulatedL2Node) ConfirmedNonce(_ context.Context) (uint64, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return 0, err
	}
	return n.relayerConfirmedNonce, nil
}

func (n *SimulatedL2Node) DepositsInBlock(_ context.Context, blockNumber uint64, blockHash [32]byte) ([]DepositLog, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return nil, err
	}
	if blockNumber >= uint64(len(n.canonical)) {
		return nil, fmt.Errorf("%w: %d", ErrBlockNotFound, blockNumber)
	}
	blk := n.canonical[blockNumber]
	if blk.header.Hash != blockHash {
		return nil, fmt.Errorf("%w: block hash mismatch at %d", ErrBlockNotFound, blockNumber)
	}
	out := make([]DepositLog, len(blk.deposits))
	copy(out, blk.deposits)
	return out, nil
}

func (n *SimulatedL2Node) SendEIP1559Transaction(_ context.Context, tx SubmittedTx) ([32]byte, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return [32]byte{}, err
	}

	if tx.Nonce < n.relayerConfirmedNonce {
		return [32]byte{}, fmt.Errorf("%w: txNonce=%d confirmed=%d", ErrNonceTooLow, tx.Nonce, n.relayerConfirmedNonce)
	}

	// Enforce Ethereum mempool >= +12.5% (11250 / 10000) replacement rule on same-nonce replacement!
	if existing, inMempool := n.mempool[tx.Nonce]; inMempool {
		minReqFee := (existing.MaxFeePerGasWei*11_250 + 9_999) / 10_000
		minReqTip := (existing.MaxPriorityFeePerGasWei*11_250 + 9_999) / 10_000
		if tx.MaxFeePerGasWei < minReqFee || tx.MaxPriorityFeePerGasWei < minReqTip {
			return [32]byte{}, fmt.Errorf(
				"%w: prev=(%d,%d) got=(%d,%d)",
				ErrReplacementUnderpriced,
				existing.MaxFeePerGasWei,
				existing.MaxPriorityFeePerGasWei,
				tx.MaxFeePerGasWei,
				tx.MaxPriorityFeePerGasWei,
			)
		}
	}

	txHash := computeTxDigest(tx.OutboxTxID, fmt.Sprintf("%d:%d:%d", tx.Nonce, tx.MaxFeePerGasWei, tx.MaxPriorityFeePerGasWei), tx.Nonce, n.forkSalt)
	tx.TxHash = txHash
	cp := tx
	if len(tx.Calldata) > 0 {
		cp.Calldata = append([]byte(nil), tx.Calldata...)
	}
	n.mempool[tx.Nonce] = cp
	return txHash, nil
}

func (n *SimulatedL2Node) TransactionReceipt(_ context.Context, txHash [32]byte) (TxReceipt, bool, error) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if err := n.checkFaultLocked(); err != nil {
		return TxReceipt{}, false, err
	}
	rcpt, ok := n.receipts[txHash]
	return rcpt, ok, nil
}

func computeDeterministicHash(chainID uint64, blockNum uint64, parent [32]byte, forkSalt uint64) [32]byte {
	var buf [64]byte
	binary.LittleEndian.PutUint64(buf[0:8], chainID)
	binary.LittleEndian.PutUint64(buf[8:16], blockNum)
	binary.LittleEndian.PutUint64(buf[16:24], forkSalt)
	copy(buf[24:56], parent[:])
	return sha256.Sum256(buf[:])
}

func computeTxDigest(tag string, id string, seq uint64, salt uint64) [32]byte {
	h := sha256.New()
	h.Write([]byte(tag))
	h.Write([]byte{0})
	h.Write([]byte(id))
	var b [16]byte
	binary.LittleEndian.PutUint64(b[0:8], seq)
	binary.LittleEndian.PutUint64(b[8:16], salt)
	h.Write(b[:])
	var out [32]byte
	copy(out[:], h.Sum(nil))
	return out
}
