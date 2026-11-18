package omegav1_test

import (
	"testing"

	hyperionv1 "github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	omegav1 "github.com/vanovri/hyperion/proto/omega/gen/go/omegav1"
	"google.golang.org/protobuf/proto"
)

func TestOmegaSettlementAndConsensusRoundtrip(t *testing.T) {
	dep := &omegav1.DepositFinalized{
		DepositId:            "dep-0001",
		ChainId:              994201,
		VaultContractAddress: make([]byte, 20),
		OnchainDepositNonce:  7,
		AccountId:            10042,
		AssetSymbol:          "USDT",
		AmountFp:             &hyperionv1.U128{Hi: 0, Lo: 500_000_000_000},
		Confirmations:        12,
		FinalityStatus:       omegav1.DepositFinalityStatus_DEPOSIT_FINALITY_STATUS_FINALIZED_CREDITED,
	}
	raw, err := proto.Marshal(dep)
	if err != nil {
		t.Fatalf("marshal DepositFinalized: %v", err)
	}
	var out omegav1.DepositFinalized
	if err := proto.Unmarshal(raw, &out); err != nil {
		t.Fatalf("unmarshal DepositFinalized: %v", err)
	}
	if out.GetChainId() != 994201 || out.GetConfirmations() != 12 || out.GetAmountFp().GetLo() != 500_000_000_000 {
		t.Fatalf("unexpected decoded DepositFinalized: %+v", &out)
	}

	reorg := &omegav1.ChainReorgAlert{
		ReorgId:                   "reorg-6blk",
		ChainId:                   994201,
		CommonAncestorBlockNumber: 1000,
		OrphanedTipBlockNumber:    1006,
		NewCanonicalTipBlockNumber: 1007,
		ReorgDepth:                6,
		RevokedStagedDepositIds:   []string{"dep-premature-1"},
	}
	rawReorg, err := proto.Marshal(reorg)
	if err != nil {
		t.Fatalf("marshal ChainReorgAlert: %v", err)
	}
	var decReorg omegav1.ChainReorgAlert
	if err := proto.Unmarshal(rawReorg, &decReorg); err != nil {
		t.Fatalf("unmarshal ChainReorgAlert: %v", err)
	}
	if decReorg.GetReorgDepth() != 6 || len(decReorg.GetRevokedStagedDepositIds()) != 1 {
		t.Fatalf("unexpected ChainReorgAlert: %+v", &decReorg)
	}
}
