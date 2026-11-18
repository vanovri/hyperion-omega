package fixedpoint_test

import (
	"math/big"
	"testing"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"google.golang.org/protobuf/proto"
)

func TestFixedPoint128Roundtrip(t *testing.T) {
	samples := []*big.Int{
		big.NewInt(0),
		big.NewInt(1),
		big.NewInt(-1),
		new(big.Int).Lsh(big.NewInt(1), 96),
		new(big.Int).Neg(new(big.Int).Lsh(big.NewInt(1), 105)),
	}
	for _, s := range samples {
		pb, err := fixedpoint.ToProtoI128(s)
		if err != nil {
			t.Fatalf("ToProtoI128(%v) failed: %v", s, err)
		}
		got := fixedpoint.FromProtoI128(pb)
		if got.Cmp(s) != 0 {
			t.Fatalf("roundtrip mismatch: got %v want %v", got, s)
		}
	}

	notional := fixedpoint.ComputeNotionalFP(6_842_550_000_000, 125_000_000)
	if notional.Cmp(big.NewInt(8_553_187_500_000)) != 0 {
		t.Fatalf("unexpected notional: %v", notional)
	}
}

func TestGeneratedProtobufWireSerialization(t *testing.T) {
	u128Val, err := fixedpoint.ToProtoU128(big.NewInt(8_553_187_500_000))
	if err != nil {
		t.Fatal(err)
	}
	alert := &hyperionv1.AnomalyAlert{
		AlertId:        "alert-wash-001",
		EngineSeq:      4200,
		Symbol:         "BTC-USDT",
		PatternType:    hyperionv1.AnomalyPatternType_ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE,
		SeverityScore:  96,
		ConfidencePpm:  984_000,
		OffenderAccountIds: []uint64{9001, 9002, 9003},
		PatternDetail: &hyperionv1.AnomalyAlert_WashTradingCycle{
			WashTradingCycle: &hyperionv1.WashTradingCycle{
				CycleId:               "ring-01",
				ParticipantAccountIds: []uint64{9001, 9002, 9003, 9001},
				CycleLength:           3,
				CircularVolumeFp:      u128Val,
				VolumeSymmetryPpm:     995_000,
			},
		},
	}

	raw, err := proto.Marshal(alert)
	if err != nil {
		t.Fatalf("marshal AnomalyAlert: %v", err)
	}
	var decoded hyperionv1.AnomalyAlert
	if err := proto.Unmarshal(raw, &decoded); err != nil {
		t.Fatalf("unmarshal AnomalyAlert: %v", err)
	}
	if decoded.GetSeverityScore() != 96 || decoded.GetWashTradingCycle().GetCycleLength() != 3 {
		t.Fatalf("unexpected decoded alert: %+v", &decoded)
	}
}
