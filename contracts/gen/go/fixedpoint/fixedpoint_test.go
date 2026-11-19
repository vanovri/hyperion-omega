package fixedpoint_test

import (
	"bytes"
	"math"
	"math/big"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"testing"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"google.golang.org/protobuf/proto"
)

func baselineBigIntNotionalFP(priceFP, qtyFP uint64) *big.Int {
	p := new(big.Int).SetUint64(priceFP)
	q := new(big.Int).SetUint64(qtyFP)
	prod := new(big.Int).Mul(p, q)
	return prod.Quo(prod, new(big.Int).SetUint64(fixedpoint.FPScale))
}

func TestFixedPoint128RoundtripAndMutationBoundaries(t *testing.T) {
	twoPow127 := new(big.Int).Lsh(big.NewInt(1), 127)
	twoPow128 := new(big.Int).Lsh(big.NewInt(1), 128)
	maxI128 := new(big.Int).Sub(twoPow127, big.NewInt(1))
	minI128 := new(big.Int).Neg(twoPow127)
	maxU128 := new(big.Int).Sub(twoPow128, big.NewInt(1))

	// Signed I128 valid boundaries
	samples := []*big.Int{
		big.NewInt(0),
		big.NewInt(1),
		big.NewInt(-1),
		new(big.Int).Lsh(big.NewInt(1), 96),
		new(big.Int).Neg(new(big.Int).Lsh(big.NewInt(1), 105)),
		maxI128,
		minI128,
	}
	for _, s := range samples {
		pb, err := fixedpoint.ToProtoI128(s)
		if err != nil {
			t.Fatalf("ToProtoI128(%v) failed: %v", s, err)
		}
		got := fixedpoint.FromProtoI128(pb)
		if got.Cmp(s) != 0 {
			t.Fatalf("i128 roundtrip mismatch: got %v want %v", got, s)
		}
	}

	// Signed I128 out-of-bounds & nil
	for _, bad := range []*big.Int{
		nil,
		new(big.Int).Sub(minI128, big.NewInt(1)),
		twoPow127,
	} {
		if _, err := fixedpoint.ToProtoI128(bad); err == nil {
			t.Fatalf("expected ToProtoI128(%v) error", bad)
		}
	}
	if fixedpoint.FromProtoI128(nil).Sign() != 0 {
		t.Fatal("FromProtoI128(nil) must return 0")
	}

	// Unsigned U128 valid boundaries
	for _, u := range []*big.Int{
		big.NewInt(0),
		big.NewInt(1),
		new(big.Int).SetUint64(math.MaxUint64),
		maxU128,
	} {
		pb, err := fixedpoint.ToProtoU128(u)
		if err != nil {
			t.Fatalf("ToProtoU128(%v) failed: %v", u, err)
		}
		got := fixedpoint.FromProtoU128(pb)
		if got.Cmp(u) != 0 {
			t.Fatalf("u128 roundtrip mismatch: got %v want %v", got, u)
		}
	}

	// Unsigned U128 out-of-bounds & nil
	for _, bad := range []*big.Int{nil, big.NewInt(-1), twoPow128} {
		if _, err := fixedpoint.ToProtoU128(bad); err == nil {
			t.Fatalf("expected ToProtoU128(%v) error", bad)
		}
	}
	if fixedpoint.FromProtoU128(nil).Sign() != 0 {
		t.Fatal("FromProtoU128(nil) must return 0")
	}

	// Int128FromInt64 boundaries
	if x := fixedpoint.Int128FromInt64(0); x.Hi != 0 || x.Lo != 0 {
		t.Fatalf("unexpected Int128FromInt64(0): %+v", x)
	}
	if x := fixedpoint.Int128FromInt64(42); x.Hi != 0 || x.Lo != 42 {
		t.Fatalf("unexpected Int128FromInt64(42): %+v", x)
	}
	if x := fixedpoint.Int128FromInt64(-1); x.Hi != -1 || x.Lo != math.MaxUint64 {
		t.Fatalf("unexpected Int128FromInt64(-1): %+v", x)
	}

	// Notional math: exact, floor truncation, hi == FPScale boundary, and MaxUint64 * MaxUint64
	pairs := [][2]uint64{
		{0, 0},
		{6_842_550_000_000, 125_000_000},
		{3, 33_333_333},
		{3, 33_333_334},
		{math.MaxUint64, fixedpoint.FPScale},     // hi == FPScale - 1
		{math.MaxUint64, fixedpoint.FPScale + 1}, // hi == FPScale (exact boundary!)
		{math.MaxUint64, fixedpoint.FPScale + 2}, // hi == FPScale + 1
		{math.MaxUint64, math.MaxUint64},         // full 128-bit two-limb division
	}
	for _, pq := range pairs {
		want := baselineBigIntNotionalFP(pq[0], pq[1])
		gotBI := fixedpoint.ComputeNotionalFP(pq[0], pq[1])
		if gotBI.Cmp(want) != 0 {
			t.Fatalf("ComputeNotionalFP(%d,%d)=%v want %v", pq[0], pq[1], gotBI, want)
		}
		gotU := fixedpoint.MulDivU64ToU128(pq[0], pq[1])
		rebuilt := new(big.Int).Lsh(new(big.Int).SetUint64(gotU.Hi), 64)
		rebuilt.Or(rebuilt, new(big.Int).SetUint64(gotU.Lo))
		if rebuilt.Cmp(want) != 0 {
			t.Fatalf("MulDivU64ToU128(%d,%d)=%+v (%v) want %v", pq[0], pq[1], gotU, rebuilt, want)
		}
	}

	// CheckedFeeFP128 full boundary coverage (kills every sign, carry, overflow & MinInt64 mutant)
	notional := fixedpoint.MulDivU64ToU128(6_842_550_000_000, 125_000_000)
	if f, ok := fixedpoint.CheckedFeeFP128(notional, 0); !ok || f != 0 {
		t.Fatalf("fee(0) got %d %v", f, ok)
	}
	if f, ok := fixedpoint.CheckedFeeFP128(notional, -20); !ok || f != -171_063_750 {
		t.Fatalf("maker fee got %d %v", f, ok)
	}
	if f, ok := fixedpoint.CheckedFeeFP128(notional, 100); !ok || f != 855_318_750 {
		t.Fatalf("taker fee got %d %v", f, ok)
	}
	// Exact truncation toward zero
	if f, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 0, Lo: 999_999}, -1); !ok || f != 0 {
		t.Fatalf("trunc neg got %d %v", f, ok)
	}
	// Carry propagation across Lo->Hi and Hi!=0 exact computation:
	// (2^64 + 1_000_000) * 1 / 1_000_000 = 18_446_744_073_709 + 1 = 18_446_744_073_710 (exact!)
	hiNotional := fixedpoint.Uint128{Hi: 1, Lo: 1_000_000}
	if f, ok := fixedpoint.CheckedFeeFP128(hiNotional, 1); !ok || f != 18_446_744_073_710 {
		t.Fatalf("hi carry fee got %d %v", f, ok)
	}
	// Exact MinInt64 feePPM with notional = 1_000_000 -> fee = MinInt64
	if f, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 0, Lo: 1_000_000}, math.MinInt64); !ok || f != math.MinInt64 {
		t.Fatalf("MinInt64 boundary got %d %v", f, ok)
	}
	// Negative overflow just past MinInt64 (notional = 1_000_001, feePPM = MinInt64)
	if _, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 0, Lo: 1_000_001}, math.MinInt64); ok {
		t.Fatal("expected negative int64 overflow")
	}
	// Positive MaxInt64 exact boundary (notional = 1_000_000, feePPM = MaxInt64)
	if f, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 0, Lo: 1_000_000}, math.MaxInt64); !ok || f != math.MaxInt64 {
		t.Fatalf("MaxInt64 boundary got %d %v", f, ok)
	}
	// Positive overflow past MaxInt64 (qLo == 1<<63)
	if _, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 0, Lo: 1 << 63}, 1_000_000); ok {
		t.Fatal("expected positive int64 overflow at 2^63")
	}
	// qHi > 0 overflow (p2 < PPMScale but qHi >= 1)
	if _, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: 1_000_000, Lo: 0}, 1); ok {
		t.Fatal("expected qHi > 0 overflow")
	}
	// p2 >= PPMScale overflow (hi = MaxUint64, feePPM = 1_000_000 -> p2 == 999_999, feePPM = 1_000_001 -> p2 == 1_000_000)
	if _, ok := fixedpoint.CheckedFeeFP128(fixedpoint.Uint128{Hi: math.MaxUint64, Lo: math.MaxUint64}, 1_000_001); ok {
		t.Fatal("expected p2 >= PPMScale overflow")
	}

	// Batch8 aligned equivalence
	batch := fixedpoint.AlignedBatch8{
		Prices: [8]uint64{6_842_550_000_000, 350_120_000_000, 3, 3, math.MaxUint64, math.MaxUint64, 1, 0},
		Qtys:   [8]uint64{125_000_000, 500_000_000, 33_333_333, 33_333_334, fixedpoint.FPScale + 1, math.MaxUint64, fixedpoint.FPScale, 123},
	}
	var out fixedpoint.AlignedOut8
	fixedpoint.ComputeNotionalBatch8Aligned(&batch, &out)
	for i := 0; i < 8; i++ {
		want := fixedpoint.MulDivU64ToU128(batch.Prices[i], batch.Qtys[i])
		if out.Hi[i] != want.Hi || out.Lo[i] != want.Lo {
			t.Fatalf("batch[%d] mismatch: got (%d,%d) want (%d,%d)", i, out.Hi[i], out.Lo[i], want.Hi, want.Lo)
		}
	}
}

func TestGeneratedProtobufWireSerialization(t *testing.T) {
	u128Val, err := fixedpoint.ToProtoU128(big.NewInt(8_553_187_500_000))
	if err != nil {
		t.Fatal(err)
	}
	alert := &hyperionv1.AnomalyAlert{
		AlertId:            "alert-wash-001",
		EngineSeq:          4200,
		Symbol:             "BTC-USDT",
		PatternType:        hyperionv1.AnomalyPatternType_ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE,
		SeverityScore:      96,
		ConfidencePpm:      984_000,
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

func TestSoak500kZeroLeakAndPprofProfile(t *testing.T) {
	const warmup = 10_000
	const soakCycles = 500_000

	var sink uint64
	for i := uint64(0); i < warmup; i++ {
		u := fixedpoint.MulDivU64ToU128(6_842_550_000_000+(i&0xFF), 125_000_000+(i&0x7F))
		fee, ok := fixedpoint.CheckedFeeFP128(u, -20)
		if !ok {
			t.Fatal("unexpected warmup overflow")
		}
		sink ^= u.Lo ^ uint64(fee)
	}

	runtime.GC()
	runtime.GC()
	var mBefore runtime.MemStats
	runtime.ReadMemStats(&mBefore)

	batch := fixedpoint.AlignedBatch8{
		Prices: [8]uint64{6_842_550_000_000, 350_120_000_000, 18_450_000_000, 2_450_000_000, 6_843_100_000_000, 350_990_000_000, 18_500_000_000, 2_490_000_000},
		Qtys:   [8]uint64{125_000_000, 500_000_000, 1_250_000_000, 10_000_000_000, 75_000_000, 250_000_000, 3_000_000_000, 5_000_000_000},
	}
	var out fixedpoint.AlignedOut8

	for i := uint64(0); i < soakCycles; i++ {
		u := fixedpoint.MulDivU64ToU128(6_842_550_000_000+(i&0xFFFF), 125_000_000+(i&0x3FFF))
		fee, ok := fixedpoint.CheckedFeeFP128(u, 100)
		if !ok {
			t.Fatal("unexpected soak overflow")
		}
		i128Val := fixedpoint.Int128FromInt64(fee)
		fixedpoint.ComputeNotionalBatch8Aligned(&batch, &out)
		sink ^= u.Lo ^ u.Hi ^ i128Val.Lo ^ out.Lo[i&7]
	}

	runtime.GC()
	runtime.GC()
	var mAfter runtime.MemStats
	runtime.ReadMemStats(&mAfter)

	var profBuf bytes.Buffer
	if err := pprof.WriteHeapProfile(&profBuf); err != nil {
		t.Fatalf("pprof.WriteHeapProfile failed: %v", err)
	}
	if profBuf.Len() == 0 {
		t.Fatal("expected non-empty pprof heap profile")
	}
	if outDir := os.Getenv("HYPERION_PPROF_OUT"); outDir != "" {
		_ = os.MkdirAll(outDir, 0o755)
		_ = os.WriteFile(filepath.Join(outDir, "ch01_fixedpoint_heap.pprof"), profBuf.Bytes(), 0o644)
	}

	var netHeapGrowth int64
	if mAfter.HeapAlloc > mBefore.HeapAlloc {
		netHeapGrowth = int64(mAfter.HeapAlloc - mBefore.HeapAlloc)
	}
	allocDelta := mAfter.Mallocs - mBefore.Mallocs
	t.Logf("[GO 500K SOAK] cycles=%d | sink=%d | MallocsDelta=%d | HeapBefore=%d B | HeapAfter=%d B | NetGrowth=%d B | pprofBytes=%d",
		soakCycles, sink, allocDelta, mBefore.HeapAlloc, mAfter.HeapAlloc, netHeapGrowth, profBuf.Len())
	if netHeapGrowth > 0 && allocDelta > 64 {
		t.Fatalf("memory leak detected across 500,000 cycles: growth=%d B allocs=%d", netHeapGrowth, allocDelta)
	}
}

var benchBatchGlobal = fixedpoint.AlignedBatch8{
	Prices: [8]uint64{
		6_842_550_000_000, 350_120_000_000, 18_450_000_000, 2_450_000_000,
		6_843_100_000_000, 350_990_000_000, 18_500_000_000, 2_490_000_000,
	},
	Qtys: [8]uint64{
		125_000_000, 500_000_000, 1_250_000_000, 10_000_000_000,
		75_000_000, 250_000_000, 3_000_000_000, 5_000_000_000,
	},
}

var benchSinkGlobal uint64

func BenchmarkNotional3PassComparison(b *testing.B) {
	// Pass 1 (Baseline): AoS slice + big.Int heap allocations
	type aosRow struct{ p, q uint64 }
	rows := make([]aosRow, 8)
	for k := 0; k < 8; k++ {
		rows[k] = aosRow{p: benchBatchGlobal.Prices[k], q: benchBatchGlobal.Qtys[k]}
	}

	b.Run("Pass1_Baseline_BigIntHeap_8Orders", func(b *testing.B) {
		b.ReportAllocs()
		var sink uint64
		for i := 0; i < b.N; i++ {
			for k := 0; k < 8; k++ {
				res := baselineBigIntNotionalFP(rows[k].p, rows[k].q)
				sink ^= res.Uint64()
			}
		}
		benchSinkGlobal ^= sink
	})

	b.Run("Pass2_ZeroAlloc_HardwareMul64Div64_8Orders", func(b *testing.B) {
		b.ReportAllocs()
		var sink uint64
		for i := 0; i < b.N; i++ {
			for k := 0; k < 8; k++ {
				res := fixedpoint.MulDivU64ToU128(rows[k].p, rows[k].q)
				sink ^= res.Lo ^ res.Hi
			}
		}
		benchSinkGlobal ^= sink
	})

	b.Run("Pass3_CacheAligned64_Batch8Unrolled_8Orders", func(b *testing.B) {
		b.ReportAllocs()
		var out fixedpoint.AlignedOut8
		b.ResetTimer()
		var sink uint64
		for i := 0; i < b.N; i++ {
			fixedpoint.ComputeNotionalBatch8Aligned(&benchBatchGlobal, &out)
			sink ^= out.Lo[0] ^ out.Lo[7]
		}
		benchSinkGlobal ^= sink
	})
}
