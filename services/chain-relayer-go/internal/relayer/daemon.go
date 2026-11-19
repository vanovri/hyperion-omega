package relayer

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	hyperionv1 "github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	omegav1 "github.com/vanovri/hyperion/proto/omega/gen/go/omegav1"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/feebump"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/nonce"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/outbox"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/reorg"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/rpcsim"
	"google.golang.org/protobuf/proto"
)

const (
	TopicSettlementDeposits    = "hyperion.omega.settlement.deposits.v1"
	TopicSettlementWithdrawals = "hyperion.omega.settlement.withdrawals.v1"
	TopicSettlementBatches     = "hyperion.omega.settlement.batches.v1"
	TopicSettlementReorgs      = "hyperion.omega.settlement.reorgs.v1"
	TopicZKRoots               = "hyperion.omega.zk.roots.v1"
	TopicZKReceipts            = "hyperion.omega.zk.receipts.v1"
)

// EventSink abstracts Redpanda / Kafka publication and live gRPC subscribers.
type EventSink interface {
	Publish(ctx context.Context, ev outbox.OutboxEvent) error
}

// MemoryEventBus is a thread-safe Redpanda topic capture & live subscriber sink.
type MemoryEventBus struct {
	mu          sync.RWMutex
	byTopic     map[string][]outbox.OutboxEvent
	subscribers map[uint64]chan *omegav1.SettlementEventEnvelope
	nextSubID   uint64
}

func NewMemoryEventBus() *MemoryEventBus {
	return &MemoryEventBus{
		byTopic:     make(map[string][]outbox.OutboxEvent, 8),
		subscribers: make(map[uint64]chan *omegav1.SettlementEventEnvelope, 8),
	}
}

func (b *MemoryEventBus) Publish(_ context.Context, ev outbox.OutboxEvent) error {
	b.mu.Lock()
	b.byTopic[ev.Topic] = append(b.byTopic[ev.Topic], ev)
	// Bound per-topic retained buffer at 4096 messages to guarantee 0 memory growth in soak mode
	if len(b.byTopic[ev.Topic]) > 4096 {
		s := b.byTopic[ev.Topic]
		copy(s, s[len(s)-2048:])
		b.byTopic[ev.Topic] = s[:2048]
	}
	subs := make([]chan *omegav1.SettlementEventEnvelope, 0, len(b.subscribers))
	for _, ch := range b.subscribers {
		subs = append(subs, ch)
	}
	b.mu.Unlock()

	if len(subs) > 0 {
		env := toSettlementEnvelope(ev)
		if env != nil {
			for _, ch := range subs {
				select {
				case ch <- env:
				default:
				}
			}
		}
	}
	return nil
}

func (b *MemoryEventBus) Subscribe() (uint64, <-chan *omegav1.SettlementEventEnvelope, func()) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.nextSubID++
	id := b.nextSubID
	ch := make(chan *omegav1.SettlementEventEnvelope, 128)
	b.subscribers[id] = ch
	cancel := func() {
		b.mu.Lock()
		if c, ok := b.subscribers[id]; ok {
			delete(b.subscribers, id)
			close(c)
		}
		b.mu.Unlock()
	}
	return id, ch, cancel
}

func (b *MemoryEventBus) EventsForTopic(topic string) []outbox.OutboxEvent {
	b.mu.RLock()
	defer b.mu.RUnlock()
	src := b.byTopic[topic]
	out := make([]outbox.OutboxEvent, len(src))
	copy(out, src)
	return out
}

func toSettlementEnvelope(ev outbox.OutboxEvent) *omegav1.SettlementEventEnvelope {
	nowNs := uint64(time.Now().UnixNano())
	switch p := ev.Payload.(type) {
	case *omegav1.DepositFinalized:
		return &omegav1.SettlementEventEnvelope{
			ChainId: p.ChainId,
			TsNs:    nowNs,
			Event:   &omegav1.SettlementEventEnvelope_DepositFinalized{DepositFinalized: p},
		}
	case *omegav1.WithdrawalEIP712Intent:
		return &omegav1.SettlementEventEnvelope{
			ChainId: p.ChainId,
			TsNs:    nowNs,
			Event:   &omegav1.SettlementEventEnvelope_WithdrawalIntent{WithdrawalIntent: p},
		}
	case *omegav1.StateRootBatchCommit:
		return &omegav1.SettlementEventEnvelope{
			ChainId: p.ChainId,
			TsNs:    nowNs,
			Event:   &omegav1.SettlementEventEnvelope_BatchCommit{BatchCommit: p},
		}
	case *omegav1.ChainReorgAlert:
		return &omegav1.SettlementEventEnvelope{
			ChainId: p.ChainId,
			TsNs:    nowNs,
			Event:   &omegav1.SettlementEventEnvelope_ReorgAlert{ReorgAlert: p},
		}
	default:
		return nil
	}
}

// Config parameterizes the Reorg-Resilient Chain Relayer daemon.
type Config struct {
	ChainID                 uint64
	WindowCapacity          uint64
	MinFinalityBlocks       uint64
	MaxReorgDepth           uint32
	StuckTxBlocksThreshold  uint64
	PollInterval            time.Duration
	MaxConsecutiveBackoff   time.Duration
	EscapeHatchInactivity   uint64
	FeePolicy               feebump.Policy
	MaxRetainedOutboxItems  int
}

func DefaultConfig(chainID uint64) Config {
	return Config{
		ChainID:                chainID,
		WindowCapacity:         reorg.DefaultWindowCapacity,
		MinFinalityBlocks:      reorg.DefaultMinFinalityBlocks,
		MaxReorgDepth:          reorg.DefaultMaxReorgDepth,
		StuckTxBlocksThreshold: 2,
		PollInterval:           25 * time.Millisecond,
		MaxConsecutiveBackoff:  200 * time.Millisecond,
		EscapeHatchInactivity:  50,
		FeePolicy:              feebump.DefaultPolicy(),
		MaxRetainedOutboxItems: 1024,
	}
}

// Daemon orchestrates block confirmation polling, 1..12-block parentHash reorg recovery,
// Transactional Outbox dispatch, EIP-1559 fee replacement, and collision-free nonce management.
type Daemon struct {
	cfg       Config
	rpc       rpcsim.L2ChainClient
	window    *reorg.SlidingWindow
	nonces    *nonce.Manager
	outbox    *outbox.Store
	sink      EventSink

	stepMu                  sync.Mutex
	latestAnchoredEpoch     atomic.Uint64
	lastAnchorBlock         atomic.Uint64
	escapeHatchFrozen       atomic.Bool
	rpcErrorsTotal          atomic.Uint64
	feeBumpsTotal           atomic.Uint64
	depositsFinalizedTotal  atomic.Uint64
	depositsRevokedTotal    atomic.Uint64
	batchesAnchoredTotal    atomic.Uint64
	reorgSeq                atomic.Uint64
}

func NewDaemon(cfg Config, rpcClient rpcsim.L2ChainClient, sink EventSink) (*Daemon, error) {
	win, err := reorg.NewSlidingWindow(cfg.WindowCapacity, cfg.MinFinalityBlocks, cfg.MaxReorgDepth)
	if err != nil {
		return nil, err
	}
	if err := cfg.FeePolicy.Validate(); err != nil {
		return nil, err
	}
	if sink == nil {
		sink = NewMemoryEventBus()
	}
	return &Daemon{
		cfg:    cfg,
		rpc:    rpcClient,
		window: win,
		nonces: nonce.NewManager(0),
		outbox: outbox.NewStore(cfg.MaxRetainedOutboxItems),
		sink:   sink,
	}, nil
}

// Run starts the background reconciliation loop until ctx is canceled.
func (d *Daemon) Run(ctx context.Context) error {
	ticker := time.NewTicker(d.cfg.PollInterval)
	defer ticker.Stop()

	consecutiveErrs := 0
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
			if err := d.StepOnce(ctx); err != nil {
				consecutiveErrs++
				d.rpcErrorsTotal.Add(1)
				// Bounded exponential backoff without blocking context cancellation
				backoff := time.Duration(1<<minInt(consecutiveErrs, 5)) * 2 * time.Millisecond
				if backoff > d.cfg.MaxConsecutiveBackoff {
					backoff = d.cfg.MaxConsecutiveBackoff
				}
				select {
				case <-ctx.Done():
					return nil
				case <-time.After(backoff):
				}
			} else {
				consecutiveErrs = 0
			}
		}
	}
}

// StepOnce executes one deterministic, reorg-resilient cycle:
//  1. Sync canonical headers & detect any 1..12 block parentHash reorg.
//  2. On reorg: atomically revoke premature staged deposits, reopen orphaned tx nonces, & emit ChainReorgAlert.
//  3. Stage newly observed deposits on canonical blocks & promote finalized deposits (>= minFinalityBlocks).
//  4. Reconcile receipts, broadcast pending outbox txs, & dynamically bump fees on stuck EIP-1559 txs.
//  5. Flush all unpublished Transactional Outbox events to Redpanda / EventSink.
func (d *Daemon) StepOnce(ctx context.Context) error {
	d.stepMu.Lock()
	defer d.stepMu.Unlock()

	latestHdr, err := d.rpc.LatestHeader(ctx)
	if err != nil {
		return fmt.Errorf("fetch latest header: %w", err)
	}

	initBefore, prevTip, _, _ := d.window.Status()
	var scanStart uint64
	if !initBefore {
		// Initialize from genesis block 0 up to latestHdr.Number
		for h := uint64(0); h <= latestHdr.Number; h++ {
			hdr, err := d.rpc.HeaderByNumber(ctx, h)
			if err != nil {
				return fmt.Errorf("bootstrap header %d: %w", h, err)
			}
			if _, err := d.window.Ingest(ctx, hdr, d.rpc); err != nil {
				return err
			}
		}
		scanStart = 0
	} else {
		det, err := d.window.Ingest(ctx, latestHdr, d.rpc)
		if err != nil {
			return fmt.Errorf("ingest header %d: %w", latestHdr.Number, err)
		}
		if det.Detected {
			if err := d.handleReorgDetected(ctx, det); err != nil {
				return err
			}
			scanStart = det.CommonAncestorBlockNumber + 1
		} else if latestHdr.Number > prevTip {
			scanStart = prevTip + 1
		} else {
			scanStart = latestHdr.Number + 1 // no new blocks
		}
	}

	_, currentTip, finalizedHeight, _ := d.window.Status()

	// Stage deposits for all newly ingested canonical blocks [scanStart .. currentTip]
	for h := scanStart; h <= currentTip; h++ {
		hdr, err := d.rpc.HeaderByNumber(ctx, h)
		if err != nil {
			return fmt.Errorf("read canonical header %d: %w", h, err)
		}
		okCanon, _ := d.window.IsCanonicalAt(hdr.Number, hdr.Hash)
		if !okCanon {
			// A concurrent fork happened mid-scan; next StepOnce will resolve via parentHash walk!
			break
		}
		logs, err := d.rpc.DepositsInBlock(ctx, hdr.Number, hdr.Hash)
		if err != nil {
			return fmt.Errorf("fetch deposits at %d: %w", hdr.Number, err)
		}
		for _, lg := range logs {
			d.outbox.StageDeposit(outbox.StagedDeposit{
				DepositID:           lg.DepositID,
				ChainID:             d.cfg.ChainID,
				VaultAddress:        lg.VaultAddress,
				OnChainDepositNonce: lg.OnChainDepositNonce,
				AccountID:           lg.AccountID,
				DepositorAddress:    lg.DepositorAddress,
				TokenAddress:        lg.TokenAddress,
				AssetSymbol:         lg.AssetSymbol,
				AmountFpHi:          lg.AmountFpHi,
				AmountFpLo:          lg.AmountFpLo,
				TxHash:              lg.TxHash,
				LogIndex:            lg.LogIndex,
				BlockNumber:         lg.BlockNumber,
				BlockHash:           lg.BlockHash,
				Confirmations:       uint32((currentTip - lg.BlockNumber) + 1),
			})
		}
	}

	// Promote deposits that reached >= minFinalityBlocks on the canonical chain
	if initBefore || currentTip >= d.cfg.MinFinalityBlocks-1 {
		nowNs := uint64(time.Now().UnixNano())
		promoted := d.outbox.PromoteFinalizedDeposits(
			finalizedHeight,
			currentTip,
			nowNs,
			TopicSettlementDeposits,
			d.window.IsCanonicalAt,
			func(sd outbox.StagedDeposit) any {
				return &omegav1.DepositFinalized{
					DepositId:            sd.DepositID,
					ChainId:              sd.ChainID,
					VaultContractAddress: sd.VaultAddress[:],
					OnchainDepositNonce:  sd.OnChainDepositNonce,
					AccountId:            sd.AccountID,
					DepositorEvmAddress:  sd.DepositorAddress[:],
					TokenContractAddress: sd.TokenAddress[:],
					AssetSymbol:          sd.AssetSymbol,
					AmountFp:             &hyperionv1.U128{Hi: sd.AmountFpHi, Lo: sd.AmountFpLo},
					L2TxHash:             sd.TxHash[:],
					LogIndex:             sd.LogIndex,
					BlockNumber:          sd.BlockNumber,
					BlockHash:            sd.BlockHash[:],
					Confirmations:        sd.Confirmations,
					FinalityStatus:       omegav1.DepositFinalityStatus_DEPOSIT_FINALITY_STATUS_FINALIZED_CREDITED,
					FinalizedTsNs:        sd.FinalizedTsNs,
					Trace: &hyperionv1.TraceMetadata{
						CorrelationId: sd.DepositID,
						SourceService: "chain-relayer-go",
						IngestTsNs:    nowNs,
					},
				}
			},
		)
		if len(promoted) > 0 {
			d.depositsFinalizedTotal.Add(uint64(len(promoted)))
		}
	}

	// Reconcile outbound transactions, broadcast pending txs, & dynamically bump stuck txs
	if err := d.processOutboxTransactions(ctx, latestHdr, currentTip, finalizedHeight); err != nil {
		return err
	}

	// Check sequencer inactivity vs Escape Hatch threshold
	lastAnc := d.lastAnchorBlock.Load()
	if lastAnc > 0 && currentTip > lastAnc+d.cfg.EscapeHatchInactivity {
		d.escapeHatchFrozen.Store(true)
	}

	// Drain & publish Transactional Outbox events with retry rollback on transient broker failure
	return d.flushOutboxEvents(ctx)
}

func (d *Daemon) handleReorgDetected(ctx context.Context, det reorg.ReorgDetection) error {
	orphanMap := make(map[uint64][32]byte, len(det.OrphanedHeaders))
	for _, oh := range det.OrphanedHeaders {
		orphanMap[oh.Number] = oh.Hash
	}

	reorgIdx := d.reorgSeq.Add(1)
	nowNs := uint64(time.Now().UnixNano())
	reorgID := fmt.Sprintf("reorg-%d-b%d-d%d", reorgIdx, det.CommonAncestorBlockNumber, det.ReorgDepth)

	revokedDeps, requeuedTxs := d.outbox.ApplyChainReorg(
		det.CommonAncestorBlockNumber,
		orphanMap,
		TopicSettlementReorgs,
		d.cfg.ChainID,
		func(revokedIDs []string, requeuedIDs []string) any {
			return &omegav1.ChainReorgAlert{
				ReorgId:                   reorgID,
				ChainId:                   d.cfg.ChainID,
				CommonAncestorBlockNumber: det.CommonAncestorBlockNumber,
				CommonAncestorBlockHash:   append([]byte(nil), det.CommonAncestorBlockHash[:]...),
				OrphanedTipBlockNumber:    det.OrphanedTipBlockNumber,
				OrphanedTipBlockHash:      append([]byte(nil), det.OrphanedTipBlockHash[:]...),
				NewCanonicalTipBlockNumber: det.NewCanonicalTipNumber,
				NewCanonicalTipBlockHash:   append([]byte(nil), det.NewCanonicalTipHash[:]...),
				ReorgDepth:                det.ReorgDepth,
				RevokedStagedDepositIds:   revokedIDs,
				RequeuedOutboxTxIds:       requeuedIDs,
				DetectedTsNs:              nowNs,
				Trace: &hyperionv1.TraceMetadata{
					CorrelationId: reorgID,
					SourceService: "chain-relayer-go",
					IngestTsNs:    nowNs,
				},
			}
		},
	)

	if len(revokedDeps) > 0 {
		d.depositsRevokedTotal.Add(uint64(len(revokedDeps)))
	}

	// Restore nonce leases for any transactions that were mined on the orphaned fork!
	for _, rtx := range requeuedTxs {
		if rtx.HasNonce {
			d.nonces.ReopenAfterReorg(rtx.TxID, rtx.Nonce)
		}
	}

	// Reconcile on-chain confirmed floor if available
	if onChainNonce, err := d.rpc.ConfirmedNonce(ctx); err == nil {
		d.nonces.ReconcileOnChainFloor(onChainNonce)
	}
	return nil
}

func (d *Daemon) processOutboxTransactions(
	ctx context.Context,
	latestHdr reorg.BlockHeader,
	currentTip uint64,
	finalizedHeight uint64,
) error {
	suggestedTip, err := d.rpc.SuggestPriorityFee(ctx)
	if err != nil {
		return fmt.Errorf("suggest priority fee: %w", err)
	}

	// 1. Check receipts for Submitted transactions before deciding whether to fee-bump
	_, maybeSubmitted := d.outbox.ActionableTxs(currentTip, 0)
	for _, stx := range maybeSubmitted {
		if stx.LastTxHash == ([32]byte{}) {
			continue
		}
		rcpt, found, err := d.rpc.TransactionReceipt(ctx, stx.LastTxHash)
		if err != nil {
			return fmt.Errorf("query receipt %s: %w", stx.TxID, err)
		}
		if found && rcpt.StatusOK {
			okCanon, _ := d.window.IsCanonicalAt(rcpt.BlockNumber, rcpt.BlockHash)
			if okCanon {
				_ = d.outbox.MarkMined(stx.TxID, rcpt.BlockNumber, rcpt.BlockHash, rcpt.GasUsed)
				if stx.Kind == outbox.TxKindStateRootAnchor {
					d.lastAnchorBlock.Store(rcpt.BlockNumber)
				}
			}
		}
	}

	// 2. Broadcast Pending txs and dynamically bump Stuck txs
	pending, stuck := d.outbox.ActionableTxs(currentTip, d.cfg.StuckTxBlocksThreshold)

	for _, ptx := range pending {
		leasedNonce, _, err := d.nonces.AcquireOrReuse(ptx.TxID)
		if err != nil {
			return err
		}
		quote, err := feebump.InitialQuote(d.cfg.FeePolicy, latestHdr.BaseFeeWei, suggestedTip)
		if err != nil {
			_ = d.nonces.AbortPreBroadcast(ptx.TxID, leasedNonce)
			return err
		}
		// If this tx was previously submitted & orphaned by a reorg, ensure fee >= previous quote if still in mempool
		if ptx.RequeueCount > 0 && ptx.CurrentFee.MaxFeePerGasWei > quote.MaxFeePerGasWei {
			quote = ptx.CurrentFee
		}

		txHash, err := d.rpc.SendEIP1559Transaction(ctx, rpcsim.SubmittedTx{
			OutboxTxID:              ptx.TxID,
			Nonce:                   leasedNonce,
			MaxFeePerGasWei:         quote.MaxFeePerGasWei,
			MaxPriorityFeePerGasWei: quote.MaxPriorityFeePerGasWei,
			GasLimit:                350_000,
			Calldata:                ptx.Calldata,
		})
		if err != nil {
			if errors.Is(err, rpcsim.ErrReplacementUnderpriced) {
				bumped, bErr := feebump.BumpQuote(d.cfg.FeePolicy, ptx.CurrentFee, latestHdr.BaseFeeWei, suggestedTip)
				if bErr == nil {
					txHash, err = d.rpc.SendEIP1559Transaction(ctx, rpcsim.SubmittedTx{
						OutboxTxID:              ptx.TxID,
						Nonce:                   leasedNonce,
						MaxFeePerGasWei:         bumped.MaxFeePerGasWei,
						MaxPriorityFeePerGasWei: bumped.MaxPriorityFeePerGasWei,
						GasLimit:                350_000,
						Calldata:                ptx.Calldata,
					})
					if err == nil {
						quote = bumped
						d.feeBumpsTotal.Add(1)
					}
				}
			}
			if err != nil {
				if ptx.RequeueCount == 0 {
					_ = d.nonces.AbortPreBroadcast(ptx.TxID, leasedNonce)
				}
				return fmt.Errorf("broadcast pending tx %s: %w", ptx.TxID, err)
			}
		}
		if err := d.outbox.MarkBroadcast(ptx.TxID, leasedNonce, quote, currentTip, txHash); err != nil {
			return err
		}
	}

	for _, stx := range stuck {
		// Re-check receipt once more right before bumping
		if rcpt, found, err := d.rpc.TransactionReceipt(ctx, stx.LastTxHash); err == nil && found && rcpt.StatusOK {
			if okCanon, _ := d.window.IsCanonicalAt(rcpt.BlockNumber, rcpt.BlockHash); okCanon {
				_ = d.outbox.MarkMined(stx.TxID, rcpt.BlockNumber, rcpt.BlockHash, rcpt.GasUsed)
				continue
			}
		}

		bumped, err := feebump.BumpQuote(d.cfg.FeePolicy, stx.CurrentFee, latestHdr.BaseFeeWei, suggestedTip)
		if err != nil {
			return fmt.Errorf("bump quote for %s: %w", stx.TxID, err)
		}
		txHash, err := d.rpc.SendEIP1559Transaction(ctx, rpcsim.SubmittedTx{
			OutboxTxID:              stx.TxID,
			Nonce:                   stx.Nonce,
			MaxFeePerGasWei:         bumped.MaxFeePerGasWei,
			MaxPriorityFeePerGasWei: bumped.MaxPriorityFeePerGasWei,
			GasLimit:                350_000,
			Calldata:                stx.Calldata,
		})
		if err != nil {
			return fmt.Errorf("send replacement tx %s: %w", stx.TxID, err)
		}
		d.feeBumpsTotal.Add(1)
		if err := d.outbox.MarkBroadcast(stx.TxID, stx.Nonce, bumped, currentTip, txHash); err != nil {
			return err
		}
	}

	// 3. Finalize mined transactions that reached >= minFinalityBlocks
	finalizedTxs := d.outbox.PromoteFinalizedTxs(
		finalizedHeight,
		d.window.IsCanonicalAt,
		func(ftx outbox.OutboxTx) (string, string, string, any, bool) {
			_ = d.nonces.MarkFinalized(ftx.TxID)
			nowNs := uint64(time.Now().UnixNano())
			switch ftx.Kind {
			case outbox.TxKindStateRootAnchor:
				d.batchesAnchoredTotal.Add(1)
				if ftx.EpochID > d.latestAnchoredEpoch.Load() {
					d.latestAnchoredEpoch.Store(ftx.EpochID)
				}
				if batch, ok := ftx.PayloadRef.(*omegav1.StateRootBatchCommit); ok && batch != nil {
					cp := proto.Clone(batch).(*omegav1.StateRootBatchCommit)
					cp.L2CommitTxHash = append([]byte(nil), ftx.LastTxHash[:]...)
					cp.L2BlockNumber = ftx.MinedBlockNumber
					cp.L2BlockHash = append([]byte(nil), ftx.MinedBlockHash[:]...)
					cp.MaxFeePerGasWei = ftx.CurrentFee.MaxFeePerGasWei
					cp.MaxPriorityFeePerGasWei = ftx.CurrentFee.MaxPriorityFeePerGasWei
					cp.GasUsed = ftx.GasUsed
					cp.CommittedTsNs = nowNs
					return TopicSettlementBatches, fmt.Sprintf("epoch:%d", cp.EpochId), "hyperion.omega.v1.StateRootBatchCommit", cp, true
				}
			case outbox.TxKindWithdrawalEIP712:
				if w, ok := ftx.PayloadRef.(*omegav1.WithdrawalEIP712Intent); ok && w != nil {
					cp := proto.Clone(w).(*omegav1.WithdrawalEIP712Intent)
					cp.Status = omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_FINALIZED_SETTLED
					return TopicSettlementWithdrawals, fmt.Sprintf("account:%d", cp.AccountId), "hyperion.omega.v1.WithdrawalEIP712Intent", cp, true
				}
			}
			return "", "", "", nil, false
		},
	)
	_ = finalizedTxs

	return nil
}

func (d *Daemon) flushOutboxEvents(ctx context.Context) error {
	events := d.outbox.DrainUnpublishedEvents(256)
	for i, ev := range events {
		if err := d.sink.Publish(ctx, ev); err != nil {
			d.outbox.RequeueFailedEvents(events[i:])
			return fmt.Errorf("publish outbox event seq=%d: %w", ev.Seq, err)
		}
	}
	return nil
}

// SubmitWithdrawal validates and enqueues an EIP-712 structured withdrawal intent into the Transactional Outbox.
func (d *Daemon) SubmitWithdrawal(intent *omegav1.WithdrawalEIP712Intent) (*omegav1.SubmitWithdrawalIntentResponse, error) {
	if intent == nil || intent.WithdrawalId == "" {
		return &omegav1.SubmitWithdrawalIntentResponse{
			Status:       omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED,
			RejectReason: "empty withdrawal_id",
		}, nil
	}
	if intent.ChainId != d.cfg.ChainID {
		return &omegav1.SubmitWithdrawalIntentResponse{
			WithdrawalId: intent.WithdrawalId,
			Status:       omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED,
			RejectReason: fmt.Sprintf("cross-chain replay rejected: got chain_id=%d want=%d", intent.ChainId, d.cfg.ChainID),
		}, nil
	}
	nowSec := uint64(time.Now().Unix())
	if intent.DeadlineUnixSec > 0 && nowSec > intent.DeadlineUnixSec {
		return &omegav1.SubmitWithdrawalIntentResponse{
			WithdrawalId: intent.WithdrawalId,
			Status:       omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED,
			RejectReason: "withdrawal deadline expired",
		}, nil
	}
	if len(intent.Secp256K1Signature) != 65 {
		return &omegav1.SubmitWithdrawalIntentResponse{
			WithdrawalId: intent.WithdrawalId,
			Status:       omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED,
			RejectReason: "invalid 65-byte secp256k1 signature length",
		}, nil
	}

	nullifier := computeNullifierHash(intent.ChainId, intent.VerifyingVaultAddress, intent.AccountId, intent.WithdrawalNonce, intent.EpochId)
	cp := proto.Clone(intent).(*omegav1.WithdrawalEIP712Intent)
	cp.NullifierHash = append([]byte(nil), nullifier[:]...)
	cp.Status = omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX

	err := d.outbox.EnqueueTx(outbox.OutboxTx{
		TxID:          "withdraw:" + intent.WithdrawalId,
		Kind:          outbox.TxKindWithdrawalEIP712,
		ChainID:       intent.ChainId,
		EpochID:       intent.EpochId,
		AccountID:     intent.AccountId,
		NullifierHash: nullifier,
		Calldata:      encodeWithdrawalCalldata(cp),
		CreatedTsNs:   uint64(time.Now().UnixNano()),
		PayloadRef:    cp,
	})
	if err != nil {
		return &omegav1.SubmitWithdrawalIntentResponse{
			WithdrawalId:  intent.WithdrawalId,
			NullifierHash: nullifier[:],
			Status:        omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED,
			RejectReason:  err.Error(),
		}, nil
	}

	return &omegav1.SubmitWithdrawalIntentResponse{
		WithdrawalId:  intent.WithdrawalId,
		NullifierHash: nullifier[:],
		Status:        omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX,
	}, nil
}

// EnqueueStateRootBatch enqueues a Channel 13 Merkle-Sum + ZK-Solvency batch commit into the Transactional Outbox.
func (d *Daemon) EnqueueStateRootBatch(batch *omegav1.StateRootBatchCommit) error {
	if batch == nil || batch.EpochId == 0 {
		return errors.New("relayer: invalid epoch_id=0 in StateRootBatchCommit")
	}
	if batch.ChainId != d.cfg.ChainID {
		return fmt.Errorf("relayer: chain_id mismatch got=%d want=%d", batch.ChainId, d.cfg.ChainID)
	}
	if len(batch.ZkSolvencyProofBytes) == 0 {
		return errors.New("relayer: missing zk_solvency_proof_bytes")
	}

	cp := proto.Clone(batch).(*omegav1.StateRootBatchCommit)
	txID := fmt.Sprintf("anchor:epoch:%d:batch:%d", batch.EpochId, batch.BatchId)
	return d.outbox.EnqueueTx(outbox.OutboxTx{
		TxID:        txID,
		Kind:        outbox.TxKindStateRootAnchor,
		ChainID:     batch.ChainId,
		EpochID:     batch.EpochId,
		BatchID:     batch.BatchId,
		Calldata:    encodeAnchorCalldata(cp),
		CreatedTsNs: uint64(time.Now().UnixNano()),
		PayloadRef:  cp,
	})
}

// Status returns a live snapshot matching omegav1.RelayerStatusResponse.
func (d *Daemon) Status() *omegav1.RelayerStatusResponse {
	_, tip, fin, reorgs := d.window.Status()
	return &omegav1.RelayerStatusResponse{
		ChainId:               d.cfg.ChainID,
		LatestObservedBlock:   tip,
		LatestFinalizedBlock:  fin,
		LatestAnchoredEpochId: d.latestAnchoredEpoch.Load(),
		PendingOutboxCount:    d.outbox.PendingCount(),
		TotalReorgsHandled:    reorgs,
		EscapeHatchFrozenMode: d.escapeHatchFrozen.Load(),
	}
}

// MetricsSnapshot exposes operational counters for Prometheus /metrics and tests.
type MetricsSnapshot struct {
	LatestObservedBlock    uint64 `json:"latest_observed_block"`
	LatestFinalizedBlock   uint64 `json:"latest_finalized_block"`
	LatestAnchoredEpoch    uint64 `json:"latest_anchored_epoch"`
	TotalReorgsHandled     uint64 `json:"total_reorgs_handled"`
	DepositsFinalizedTotal uint64 `json:"deposits_finalized_total"`
	DepositsRevokedTotal   uint64 `json:"deposits_revoked_total"`
	BatchesAnchoredTotal   uint64 `json:"batches_anchored_total"`
	FeeBumpsTotal          uint64 `json:"fee_bumps_total"`
	RPCErrorsTotal         uint64 `json:"rpc_errors_total"`
	PendingOutboxCount     uint32 `json:"pending_outbox_count"`
}

func (d *Daemon) Metrics() MetricsSnapshot {
	_, tip, fin, reorgs := d.window.Status()
	return MetricsSnapshot{
		LatestObservedBlock:    tip,
		LatestFinalizedBlock:   fin,
		LatestAnchoredEpoch:    d.latestAnchoredEpoch.Load(),
		TotalReorgsHandled:     reorgs,
		DepositsFinalizedTotal: d.depositsFinalizedTotal.Load(),
		DepositsRevokedTotal:   d.depositsRevokedTotal.Load(),
		BatchesAnchoredTotal:   d.batchesAnchoredTotal.Load(),
		FeeBumpsTotal:          d.feeBumpsTotal.Load(),
		RPCErrorsTotal:         d.rpcErrorsTotal.Load(),
		PendingOutboxCount:     d.outbox.PendingCount(),
	}
}

func (d *Daemon) OutboxStore() *outbox.Store {
	return d.outbox
}

func computeNullifierHash(chainID uint64, vaultAddr []byte, accountID uint64, withdrawalNonce uint64, epochID uint64) [32]byte {
	h := sha256.New()
	var buf [32]byte
	binary.BigEndian.PutUint64(buf[0:8], chainID)
	binary.BigEndian.PutUint64(buf[8:16], accountID)
	binary.BigEndian.PutUint64(buf[16:24], withdrawalNonce)
	binary.BigEndian.PutUint64(buf[24:32], epochID)
	h.Write(buf[:])
	h.Write(vaultAddr)
	var out [32]byte
	copy(out[:], h.Sum(nil))
	return out
}

func encodeWithdrawalCalldata(w *omegav1.WithdrawalEIP712Intent) []byte {
	// Selector 0x9c4d535b + compact deterministic ABI payload
	buf := make([]byte, 36+len(w.Secp256K1Signature))
	buf[0], buf[1], buf[2], buf[3] = 0x9c, 0x4d, 0x53, 0x5b
	binary.BigEndian.PutUint64(buf[4:12], w.AccountId)
	binary.BigEndian.PutUint64(buf[12:20], w.WithdrawalNonce)
	binary.BigEndian.PutUint64(buf[20:28], w.EpochId)
	binary.BigEndian.PutUint64(buf[28:36], w.DeadlineUnixSec)
	copy(buf[36:], w.Secp256K1Signature)
	return buf
}

func encodeAnchorCalldata(b *omegav1.StateRootBatchCommit) []byte {
	buf := make([]byte, 28+len(b.NewMerkleSumRoot)+len(b.ZkSolvencyProofBytes))
	buf[0], buf[1], buf[2], buf[3] = 0x61, 0x8b, 0x19, 0x04
	binary.BigEndian.PutUint64(buf[4:12], b.EpochId)
	binary.BigEndian.PutUint64(buf[12:20], b.StartEngineSeq)
	binary.BigEndian.PutUint64(buf[20:28], b.EndEngineSeq)
	copy(buf[28:], b.NewMerkleSumRoot)
	copy(buf[28+len(b.NewMerkleSumRoot):], b.ZkSolvencyProofBytes)
	return buf
}

func minInt(a, b int) int {
	if a < b {
		return a
	}
	return b
}
