package outbox

import (
	"testing"

	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/feebump"
)

func makeHash(seed byte) [32]byte {
	var h [32]byte
	for i := range h {
		h[i] = seed + byte(i)
	}
	return h
}

func TestStore_StagePromoteAndReorgRevokeDeposits(t *testing.T) {
	store := NewStore(256)

	// Stage deposit A at block 10 (will finalize) and deposit B at block 18 (will be orphaned by 6-block reorg)
	depA := StagedDeposit{
		DepositID:   "dep-A",
		ChainID:     31337,
		AccountID:   101,
		AssetSymbol: "ETH",
		AmountFpLo:  10_00000000,
		BlockNumber: 10,
		BlockHash:   makeHash(0x10),
	}
	depB := StagedDeposit{
		DepositID:   "dep-B",
		ChainID:     31337,
		AccountID:   102,
		AssetSymbol: "USDT",
		AmountFpLo:  50_00000000,
		BlockNumber: 18,
		BlockHash:   makeHash(0x18),
	}

	if !store.StageDeposit(depA) {
		t.Fatal("expected depA first insert true")
	}
	if store.StageDeposit(depA) {
		t.Fatal("expected duplicate depA insert false")
	}
	if !store.StageDeposit(depB) {
		t.Fatal("expected depB first insert true")
	}

	// Promote with finalizedHeight=12 (depA <= 12 promotes, depB > 12 stays soft-confirmed)
	promoted := store.PromoteFinalizedDeposits(
		12,
		24,
		1_795_000_100,
		"hyperion.omega.settlement.deposits.v1",
		func(num uint64, h [32]byte) (bool, uint32) {
			return true, uint32(24 - num + 1)
		},
		func(d StagedDeposit) any { return d.DepositID },
	)
	if len(promoted) != 1 || promoted[0].DepositID != "dep-A" {
		t.Fatalf("expected only dep-A promoted, got %+v", promoted)
	}

	// Apply 6-block reorg with common ancestor 14 -> dep-B (block 18) must be revoked and alert queued!
	orphaned := map[uint64][32]byte{18: makeHash(0x18)}
	revoked, requeued := store.ApplyChainReorg(
		14,
		orphaned,
		"hyperion.omega.settlement.reorgs.v1",
		31337,
		func(revokedDeps []string, requeuedTxs []string) any {
			return map[string]any{"revoked": revokedDeps, "requeued": requeuedTxs}
		},
	)
	if len(revoked) != 1 || revoked[0] != "dep-B" {
		t.Fatalf("expected dep-B revoked, got %+v", revoked)
	}
	if len(requeued) != 0 {
		t.Fatalf("expected 0 requeued txs, got %d", len(requeued))
	}

	gotB, ok := store.GetDeposit("dep-B")
	if !ok || !gotB.ReorgOrphanedRevoked || gotB.FinalizedCredited {
		t.Fatalf("unexpected dep-B state after reorg: %+v", gotB)
	}

	// Re-mining dep-B on new canonical branch must allow re-staging cleanly
	depBNew := depB
	depBNew.BlockNumber = 19
	depBNew.BlockHash = makeHash(0x99)
	if !store.StageDeposit(depBNew) {
		t.Fatal("expected previously revoked deposit to re-stage cleanly on new canonical branch")
	}

	// Drain outbox events: 1 DepositFinalized + 1 ChainReorgAlert
	evs := store.DrainUnpublishedEvents(10)
	if len(evs) != 2 {
		t.Fatalf("expected 2 outbox events, got %d", len(evs))
	}
	if evs[0].EventType != "hyperion.omega.v1.DepositFinalized" || evs[1].EventType != "hyperion.omega.v1.ChainReorgAlert" {
		t.Fatalf("unexpected event order/types: %+v", evs)
	}

	// Verify transient publish failure requeue preserves order
	store.RequeueFailedEvents(evs)
	redrained := store.DrainUnpublishedEvents(10)
	if len(redrained) != 2 || redrained[0].Seq != evs[0].Seq {
		t.Fatalf("expected exact event order preserved after RequeueFailedEvents")
	}
}

func TestStore_TxNullifierDedupReorgRequeueAndStuckDetection(t *testing.T) {
	store := NewStore(128)
	nullifier := makeHash(0xAA)

	tx1 := OutboxTx{
		TxID:          "w-1",
		Kind:          TxKindWithdrawalEIP712,
		ChainID:       31337,
		AccountID:     101,
		NullifierHash: nullifier,
		Calldata:      []byte{0x01, 0x02},
	}
	if err := store.EnqueueTx(tx1); err != nil {
		t.Fatalf("EnqueueTx failed: %v", err)
	}
	if err := store.EnqueueTx(tx1); err == nil {
		t.Fatal("expected duplicate tx error")
	}

	// Different txID with same nullifier must be rejected atomically
	txReplay := tx1
	txReplay.TxID = "w-replay"
	if err := store.EnqueueTx(txReplay); err == nil {
		t.Fatal("expected duplicate nullifier rejection")
	}

	// Broadcast at block 5 -> stuck at tip 7 with threshold 2
	q := feebump.Quote{MaxFeePerGasWei: 2_000_000_000, MaxPriorityFeePerGasWei: 500_000_000}
	if err := store.MarkBroadcast("w-1", 0, q, 5, makeHash(0x55)); err != nil {
		t.Fatalf("MarkBroadcast failed: %v", err)
	}
	pending, stuck := store.ActionableTxs(6, 2)
	if len(pending) != 0 || len(stuck) != 0 {
		t.Fatalf("expected neither pending nor stuck at tip 6, got pending=%d stuck=%d", len(pending), len(stuck))
	}
	_, stuck = store.ActionableTxs(7, 2)
	if len(stuck) != 1 || stuck[0].TxID != "w-1" {
		t.Fatalf("expected w-1 stuck at tip 7, got %+v", stuck)
	}

	// Mark mined on block 8, then reorg ancestor 6 -> must requeue to Pending with RequeueCount == 1
	if err := store.MarkMined("w-1", 8, makeHash(0x08), 95_000); err != nil {
		t.Fatalf("MarkMined failed: %v", err)
	}
	_, requeued := store.ApplyChainReorg(
		6,
		map[uint64][32]byte{8: makeHash(0x08)},
		"hyperion.omega.settlement.reorgs.v1",
		31337,
		func(r, q []string) any { return q },
	)
	if len(requeued) != 1 || requeued[0].TxID != "w-1" || requeued[0].RequeueCount != 1 {
		t.Fatalf("expected w-1 requeued with count=1, got %+v", requeued)
	}
	if store.PendingCount() != 1 {
		t.Fatalf("expected PendingCount 1, got %d", store.PendingCount())
	}
}
