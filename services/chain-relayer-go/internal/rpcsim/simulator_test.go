package rpcsim

import (
	"context"
	"errors"
	"testing"
)

func TestSimulatedL2Node_ReorgRPCFailuresAndReplacementPricing(t *testing.T) {
	ctx := context.Background()
	node := NewSimulatedL2Node(31337, 1_000_000_000, 200_000_000)

	// 1. Consecutive RPC failure injection & recovery
	node.InjectConsecutiveFailures(2, "injected outage")
	if _, err := node.LatestHeader(ctx); !errors.Is(err, ErrInjectedRPCFailure) {
		t.Fatalf("expected first RPC call to fail with ErrInjectedRPCFailure, got %v", err)
	}
	if _, err := node.LatestHeader(ctx); !errors.Is(err, ErrInjectedRPCFailure) {
		t.Fatalf("expected second RPC call to fail with ErrInjectedRPCFailure, got %v", err)
	}
	hdr0, err := node.LatestHeader(ctx)
	if err != nil || hdr0.Number != 0 {
		t.Fatalf("expected recovery on 3rd RPC call at genesis, got num=%d err=%v", hdr0.Number, err)
	}

	// 2. Strict EIP-1559 replacement pricing check (+12.5% minimum)
	node.SetHoldMempool(true)
	_, err = node.SendEIP1559Transaction(ctx, SubmittedTx{
		OutboxTxID:              "tx-0",
		Nonce:                   0,
		MaxFeePerGasWei:         2_000_000_000,
		MaxPriorityFeePerGasWei: 500_000_000,
		GasLimit:                150_000,
	})
	if err != nil {
		t.Fatalf("initial tx submit failed: %v", err)
	}

	// Attempt underpriced replacement (+5% only) -> must reject
	_, err = node.SendEIP1559Transaction(ctx, SubmittedTx{
		OutboxTxID:              "tx-0",
		Nonce:                   0,
		MaxFeePerGasWei:         2_100_000_000,
		MaxPriorityFeePerGasWei: 525_000_000,
		GasLimit:                150_000,
	})
	if !errors.Is(err, ErrReplacementUnderpriced) {
		t.Fatalf("expected ErrReplacementUnderpriced, got %v", err)
	}

	// Valid +15% replacement succeeds and mines cleanly once mempool unpaused
	txHash, err := node.SendEIP1559Transaction(ctx, SubmittedTx{
		OutboxTxID:              "tx-0",
		Nonce:                   0,
		MaxFeePerGasWei:         2_300_000_000,
		MaxPriorityFeePerGasWei: 575_000_000,
		GasLimit:                150_000,
	})
	if err != nil {
		t.Fatalf("valid +15%% replacement failed: %v", err)
	}
	node.SetHoldMempool(false)
	for i := 0; i < 6; i++ {
		node.MineBlock()
	}

	rcpt, found, err := node.TransactionReceipt(ctx, txHash)
	if err != nil || !found || rcpt.BlockNumber != 1 {
		t.Fatalf("expected tx-0 mined in block 1, found=%v rcpt=%+v err=%v", found, rcpt, err)
	}

	// 3. Sudden 6-block reorg evicts receipt and rolls back confirmed nonce to 0
	oldTip, _ := node.LatestHeader(ctx)
	anc, newTip, err := node.InjectSuddenReorg(6, 1, nil)
	if err != nil {
		t.Fatalf("InjectSuddenReorg(6) failed: %v", err)
	}
	if anc.Number != 0 || newTip.Number != 7 || newTip.Hash == oldTip.Hash {
		t.Fatalf("unexpected reorg geometry: anc=%d newTip=%d", anc.Number, newTip.Number)
	}
	if _, stillFound, _ := node.TransactionReceipt(ctx, txHash); stillFound {
		t.Fatal("expected receipt from orphaned block 1 to be evicted after 6-block reorg")
	}
	nonceAfter, _ := node.ConfirmedNonce(ctx)
	if nonceAfter != 0 {
		t.Fatalf("expected relayer confirmed nonce rolled back to 0, got %d", nonceAfter)
	}
}
