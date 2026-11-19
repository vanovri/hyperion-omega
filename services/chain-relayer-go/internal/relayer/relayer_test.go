package relayer_test

import (
	"context"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	hyperionv1 "github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	omegav1 "github.com/vanovri/hyperion/proto/omega/gen/go/omegav1"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/relayer"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/rpcsim"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/server"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func TestSuddenSixBlockReorg_RevokesPrematureDepositsRequeuesBatchesAndEmitsProtoAlert(t *testing.T) {
	const chainID uint64 = 31337
	sim := rpcsim.NewSimulatedL2Node(chainID, 1_000_000_000, 100_000_000)
	bus := relayer.NewMemoryEventBus()
	cfg := relayer.DefaultConfig(chainID)
	d, err := relayer.NewDaemon(cfg, sim, bus)
	if err != nil {
		t.Fatalf("NewDaemon failed: %v", err)
	}
	ctx := context.Background()

	// 1. Mine block 1..2 with Deposit D_SAFE at block 2
	sim.MineBlock() // block 1
	sim.MineBlock(rpcsim.DepositLog{
		DepositID:           "dep-safe-b2",
		OnChainDepositNonce: 1,
		AccountID:           1001,
		AssetSymbol:         "USDT",
		AmountFpLo:          500_00000000,
	}) // block 2

	// Mine blocks 3..9 so block 2 approaches finality
	for b := 3; b <= 9; b++ {
		sim.MineBlock()
	}
	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("StepOnce at block 9 failed: %v", err)
	}

	// 2. Mine blocks 10..15 (this will later be orphaned by a sudden 6-block reorg back to ancestor block 9!):
	//    - At block 11: premature deposit `dep-orphan-b11`
	//    - At block 13: block 2 reaches 12 confirmations (13 - 2 + 1 = 12 -> `dep-safe-b2` FINALIZES!)
	//    - Also enqueue Epoch 1 StateRootBatchCommit so it gets mined on orphaned block 14!
	//    - At block 15: premature deposit `dep-orphan-b15`
	sim.MineBlock() // block 10
	sim.MineBlock(rpcsim.DepositLog{
		DepositID:           "dep-orphan-b11",
		OnChainDepositNonce: 2,
		AccountID:           1002,
		AssetSymbol:         "ETH",
		AmountFpLo:          10_00000000,
	}) // block 11
	sim.MineBlock() // block 12
	sim.MineBlock() // block 13 -> finalizes dep-safe-b2!

	if err := d.EnqueueStateRootBatch(&omegav1.StateRootBatchCommit{
		BatchId:                 1,
		EpochId:                 1,
		ChainId:                 chainID,
		StartEngineSeq:          1,
		EndEngineSeq:            500,
		NewMerkleSumRoot:        make([]byte, 32),
		DeclaredLiabilitiesFp:   &hyperionv1.U128{Lo: 500_00000000},
		VerifiedVaultReservesFp: &hyperionv1.U128{Lo: 500_00000000},
		ZkSolvencyProofBytes:    make([]byte, 256),
	}); err != nil {
		t.Fatalf("EnqueueStateRootBatch failed: %v", err)
	}
	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("StepOnce at block 13 failed: %v", err)
	}

	// Block 14 mines the Epoch 1 state root tx; Block 15 adds premature deposit `dep-orphan-b15`
	sim.MineBlock() // block 14 (mines anchor:epoch:1:batch:1!)
	sim.MineBlock(rpcsim.DepositLog{
		DepositID:           "dep-orphan-b15",
		OnChainDepositNonce: 3,
		AccountID:           1003,
		AssetSymbol:         "WBTC",
		AmountFpLo:          2_00000000,
	}) // block 15

	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("StepOnce at block 15 failed: %v", err)
	}

	// Verify `dep-safe-b2` is finalized, while `dep-orphan-b11` and `dep-orphan-b15` are ONLY staged (0 premature credit!)
	depEventsBefore := bus.EventsForTopic(relayer.TopicSettlementDeposits)
	if len(depEventsBefore) != 1 {
		t.Fatalf("expected exactly 1 finalized deposit before reorg, got %d", len(depEventsBefore))
	}
	if got := depEventsBefore[0].Payload.(*omegav1.DepositFinalized).DepositId; got != "dep-safe-b2" {
		t.Fatalf("unexpected finalized deposit: %s", got)
	}

	// 3. INJECT SUDDEN 6-BLOCK REORG (orphaning blocks 10..15 back to common ancestor 9, mining fork 10'..17')!
	forkDeposits := map[uint64][]rpcsim.DepositLog{
		12: {
			{
				DepositID:           "dep-canonical-fork-b12",
				OnChainDepositNonce: 2,
				AccountID:           1004,
				AssetSymbol:         "USDT",
				AmountFpLo:          750_00000000,
			},
		},
	}
	ancestorHdr, newTipHdr, err := sim.InjectSuddenReorg(6, 2, forkDeposits)
	if err != nil {
		t.Fatalf("InjectSuddenReorg(6) failed: %v", err)
	}
	if ancestorHdr.Number != 9 || newTipHdr.Number != 17 {
		t.Fatalf("unexpected fork geometry: ancestor=%d newTip=%d", ancestorHdr.Number, newTipHdr.Number)
	}

	// Step relayer -> detects 6-block reorg, revokes premature deposits, requeues orphaned Epoch 1 batch, emits ChainReorgAlert!
	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("StepOnce after 6-block reorg failed: %v", err)
	}

	reorgEvents := bus.EventsForTopic(relayer.TopicSettlementReorgs)
	if len(reorgEvents) != 1 {
		t.Fatalf("expected 1 ChainReorgAlert, got %d", len(reorgEvents))
	}
	alert := reorgEvents[0].Payload.(*omegav1.ChainReorgAlert)
	if alert.ReorgDepth != 6 || alert.CommonAncestorBlockNumber != 9 || alert.OrphanedTipBlockNumber != 15 || alert.NewCanonicalTipBlockNumber != 17 {
		t.Fatalf("ChainReorgAlert mismatch: %+v", alert)
	}
	if len(alert.RevokedStagedDepositIds) != 2 {
		t.Fatalf("expected 2 revoked premature deposits, got %v", alert.RevokedStagedDepositIds)
	}
	if len(alert.RequeuedOutboxTxIds) != 1 || alert.RequeuedOutboxTxIds[0] != "anchor:epoch:1:batch:1" {
		t.Fatalf("expected orphaned Epoch 1 anchor to be requeued, got %v", alert.RequeuedOutboxTxIds)
	}

	// Verify revoked status on both orphaned deposits
	for _, orphanID := range []string{"dep-orphan-b11", "dep-orphan-b15"} {
		sd, ok := d.OutboxStore().GetDeposit(orphanID)
		if !ok || !sd.ReorgOrphanedRevoked || sd.FinalizedCredited {
			t.Fatalf("orphaned deposit %s was not cleanly revoked: %+v", orphanID, sd)
		}
	}

	// 4. Mine 15 more canonical blocks so re-broadcasted Epoch 1 batch & `dep-canonical-fork-b12` finalize!
	for i := 0; i < 15; i++ {
		sim.MineBlock()
		if err := d.StepOnce(ctx); err != nil {
			t.Fatalf("post-reorg StepOnce %d failed: %v", i, err)
		}
	}

	// Verify `dep-orphan-b11` and `dep-orphan-b15` NEVER leaked into finalized deposits,
	// while `dep-safe-b2` and `dep-canonical-fork-b12` finalized cleanly!
	depEventsAfter := bus.EventsForTopic(relayer.TopicSettlementDeposits)
	if len(depEventsAfter) != 2 {
		t.Fatalf("expected 2 total finalized deposits (safe + canonical fork), got %d", len(depEventsAfter))
	}
	finalIDs := map[string]bool{
		depEventsAfter[0].Payload.(*omegav1.DepositFinalized).DepositId: true,
		depEventsAfter[1].Payload.(*omegav1.DepositFinalized).DepositId: true,
	}
	if !finalIDs["dep-safe-b2"] || !finalIDs["dep-canonical-fork-b12"] {
		t.Fatalf("unexpected finalized deposit set: %v", finalIDs)
	}
	if finalIDs["dep-orphan-b11"] || finalIDs["dep-orphan-b15"] {
		t.Fatalf("CRITICAL DOUBLE-SPEND: orphaned deposit finalized!")
	}

	// Verify re-queued Epoch 1 StateRootBatchCommit finalized on the new canonical fork!
	batchEvents := bus.EventsForTopic(relayer.TopicSettlementBatches)
	if len(batchEvents) != 1 {
		t.Fatalf("expected 1 finalized StateRootBatchCommit after reorg recovery, got %d", len(batchEvents))
	}
	if d.Metrics().LatestAnchoredEpoch != 1 || d.Metrics().TotalReorgsHandled != 1 {
		t.Fatalf("unexpected metrics after reorg recovery: %+v", d.Metrics())
	}
}

func TestConsecutiveRPCFailures_AndDynamicEIP1559FeeBumping(t *testing.T) {
	const chainID uint64 = 31337
	sim := rpcsim.NewSimulatedL2Node(chainID, 1_000_000_000, 200_000_000)
	bus := relayer.NewMemoryEventBus()
	cfg := relayer.DefaultConfig(chainID)
	cfg.StuckTxBlocksThreshold = 2
	d, err := relayer.NewDaemon(cfg, sim, bus)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()

	sim.MineBlock()
	if err := d.StepOnce(ctx); err != nil {
		t.Fatal(err)
	}

	// Submit EIP-712 Withdrawal Intent
	sig := make([]byte, 65)
	sig[64] = 27
	resp, err := d.SubmitWithdrawal(&omegav1.WithdrawalEIP712Intent{
		WithdrawalId:       "wd-eip1559-1",
		ChainId:            chainID,
		AccountId:          2001,
		AssetSymbol:        "ETH",
		AmountFp:           &hyperionv1.U128{Lo: 5_00000000},
		WithdrawalNonce:    1,
		EpochId:            1,
		DeadlineUnixSec:    uint64(time.Now().Add(1 * time.Hour).Unix()),
		Secp256K1Signature: sig,
	})
	if err != nil || resp.Status != omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX {
		t.Fatalf("SubmitWithdrawal failed: %+v err=%v", resp, err)
	}

	// Duplicate replay of same withdrawal nonce/nullifier must be rejected immediately!
	dupResp, _ := d.SubmitWithdrawal(&omegav1.WithdrawalEIP712Intent{
		WithdrawalId:       "wd-eip1559-replay",
		ChainId:            chainID,
		AccountId:          2001,
		AssetSymbol:        "ETH",
		AmountFp:           &hyperionv1.U128{Lo: 5_00000000},
		WithdrawalNonce:    1,
		EpochId:            1,
		DeadlineUnixSec:    uint64(time.Now().Add(1 * time.Hour).Unix()),
		Secp256K1Signature: sig,
	})
	if dupResp.Status != omegav1.WithdrawalSettlementStatus_WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED {
		t.Fatalf("duplicate nullifier replay must be rejected, got %+v", dupResp)
	}

	// Hold mempool inclusion & spike baseFee 3x to force dynamic EIP-1559 replacement fee bumping!
	sim.SetHoldMempool(true)
	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("initial broadcast StepOnce failed: %v", err)
	}
	txInitial, _ := d.OutboxStore().GetTx("withdraw:wd-eip1559-1")

	sim.SetBaseFeeAndTip(3_000_000_000, 400_000_000)
	sim.MineBlock()
	sim.MineBlock()

	// Inject 5 consecutive RPC outages right before the fee-bump recovery!
	sim.InjectConsecutiveFailures(5, "HTTP 502 Bad Gateway L2 Sequencer")
	for k := 0; k < 5; k++ {
		if err := d.StepOnce(ctx); err == nil {
			t.Fatalf("expected injected RPC error on attempt %d", k+1)
		}
	}

	// Next StepOnce succeeds and performs EIP-1559 replacement bump on the SAME nonce (0)!
	if err := d.StepOnce(ctx); err != nil {
		t.Fatalf("recovery StepOnce after consecutive RPC failures failed: %v", err)
	}
	txBumped, _ := d.OutboxStore().GetTx("withdraw:wd-eip1559-1")
	if txBumped.Nonce != txInitial.Nonce {
		t.Fatalf("replacement tx changed nonce! want=%d got=%d", txInitial.Nonce, txBumped.Nonce)
	}
	if txBumped.CurrentFee.MaxFeePerGasWei <= txInitial.CurrentFee.MaxFeePerGasWei ||
		txBumped.CurrentFee.MaxPriorityFeePerGasWei <= txInitial.CurrentFee.MaxPriorityFeePerGasWei {
		t.Fatalf("EIP-1559 replacement did not bump fees: initial=%+v bumped=%+v", txInitial.CurrentFee, txBumped.CurrentFee)
	}

	// Unhold mempool and mine to finality (13 blocks)
	sim.SetHoldMempool(false)
	for i := 0; i < 14; i++ {
		sim.MineBlock()
		if err := d.StepOnce(ctx); err != nil {
			t.Fatalf("finalization step %d failed: %v", i, err)
		}
	}

	wdEvents := bus.EventsForTopic(relayer.TopicSettlementWithdrawals)
	if len(wdEvents) != 1 {
		t.Fatalf("expected finalized withdrawal event after RPC outage + fee bump, got %d", len(wdEvents))
	}
	if d.Metrics().FeeBumpsTotal < 1 {
		t.Fatalf("expected >=1 fee bump recorded, got %d", d.Metrics().FeeBumpsTotal)
	}
}

func TestGRPCAndHTTPSettlementEndpoints_LiveIntegration(t *testing.T) {
	const chainID uint64 = 31337
	sim := rpcsim.NewSimulatedL2Node(chainID, 1_000_000_000, 100_000_000)
	bus := relayer.NewMemoryEventBus()
	d, err := relayer.NewDaemon(relayer.DefaultConfig(chainID), sim, bus)
	if err != nil {
		t.Fatal(err)
	}
	sim.MineBlock()
	_ = d.StepOnce(context.Background())

	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	grpcSrv := grpc.NewServer()
	omegav1.RegisterL2SettlementServiceServer(grpcSrv, server.NewGRPCServer(d, bus))
	go func() { _ = grpcSrv.Serve(lis) }()
	defer grpcSrv.Stop()

	conn, err := grpc.NewClient(lis.Addr().String(), grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	client := omegav1.NewL2SettlementServiceClient(conn)
	st, err := client.GetRelayerStatus(context.Background(), &omegav1.GetRelayerStatusRequest{ChainId: chainID})
	if err != nil || st.LatestObservedBlock != 1 {
		t.Fatalf("GetRelayerStatus failed: %+v err=%v", st, err)
	}

	httpTs := httptest.NewServer(server.NewHTTPMux(d, bus))
	defer httpTs.Close()

	res, err := http.Get(httpTs.URL + "/metrics")
	if err != nil || res.StatusCode != http.StatusOK {
		t.Fatalf("GET /metrics failed: %v", err)
	}
	res.Body.Close()

	resH, err := http.Get(httpTs.URL + "/healthz")
	if err != nil || resH.StatusCode != http.StatusOK {
		t.Fatalf("GET /healthz failed: %v", err)
	}
	buf := make([]byte, 256)
	n, _ := resH.Body.Read(buf)
	resH.Body.Close()
	if !strings.Contains(string(buf[:n]), `"status":"ok"`) {
		t.Fatalf("unexpected /healthz body: %s", string(buf[:n]))
	}
}
