package fixed_test

import (
	"encoding/json"
	"math"
	"testing"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
)

func TestFixedPointArithmeticAndOverflowGuards(t *testing.T) {
	// Scale constants exact values
	if fixed.Scale != 100_000_000 {
		t.Fatalf("Scale mismatch: %d", fixed.Scale)
	}
	if fixed.CentUnits != 1_000_000 {
		t.Fatalf("CentUnits mismatch: %d", fixed.CentUnits)
	}
	if fixed.BasisPointScale != 10_000 {
		t.Fatalf("BasisPointScale mismatch: %d", fixed.BasisPointScale)
	}
	if fixed.PPMScale != 1_000_000 {
		t.Fatalf("PPMScale mismatch: %d", fixed.PPMScale)
	}

	// FromUnits & MustFromUnits
	u, err := fixed.FromUnits(5)
	if err != nil || u != 500_000_000 {
		t.Fatalf("FromUnits(5) = %d, %v", u, err)
	}
	if _, err := fixed.FromUnits(math.MaxInt64); err == nil {
		t.Fatal("expected FromUnits overflow")
	}

	px, err := fixed.ParseDecimal("68425.50")
	if err != nil || px != 68425_50000000 {
		t.Fatalf("ParseDecimal got %d err=%v", px, err)
	}
	negPx, err := fixed.ParseDecimal("-12.345")
	if err != nil || negPx != -12_34500000 {
		t.Fatalf("ParseDecimal neg got %d err=%v", negPx, err)
	}
	qty, err := fixed.ParseDecimal("1.25")
	if err != nil || qty != 1_25000000 {
		t.Fatalf("ParseDecimal qty got %d err=%v", qty, err)
	}
	notional, err := fixed.QuoteNotional(px, qty)
	if err != nil || notional != 85531_87500000 {
		t.Fatalf("QuoteNotional got %d err=%v", notional, err)
	}
	// QuoteNotional zero & negative guards
	if z, err := fixed.QuoteNotional(0, qty); err != nil || z != 0 {
		t.Fatalf("QuoteNotional(0, qty) = %d, %v", z, err)
	}
	if z, err := fixed.QuoteNotional(px, 0); err != nil || z != 0 {
		t.Fatalf("QuoteNotional(px, 0) = %d, %v", z, err)
	}
	if _, err := fixed.QuoteNotional(-1, qty); err == nil {
		t.Fatal("expected error for negative price")
	}
	if _, err := fixed.QuoteNotional(px, -1); err == nil {
		t.Fatal("expected error for negative qty")
	}

	// FeeFromBps & FeeFromPPM including negative maker rebates
	feeBps, err := fixed.FeeFromBps(notional, 5)
	if err != nil || feeBps != 42_76593750 {
		t.Fatalf("FeeFromBps got %d err=%v", feeBps, err)
	}
	rebateBps, err := fixed.FeeFromBps(notional, -2)
	if err != nil || rebateBps != -17_10637500 {
		t.Fatalf("FeeFromBps rebate got %d err=%v", rebateBps, err)
	}
	feePPM, err := fixed.FeeFromPPM(notional, 500) // 5 bps = 500 ppm
	if err != nil || feePPM != 42_76593750 {
		t.Fatalf("FeeFromPPM got %d err=%v", feePPM, err)
	}
	rebatePPM, err := fixed.FeeFromPPM(notional, -200)
	if err != nil || rebatePPM != -17_10637500 {
		t.Fatalf("FeeFromPPM rebate got %d err=%v", rebatePPM, err)
	}

	// CheckedAdd / CheckedSub / CheckedMul exact & boundary checks
	if v, err := fixed.CheckedAdd(10, 25); err != nil || v != 35 {
		t.Fatalf("CheckedAdd(10,25) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedAdd(math.MaxInt64, 0); err != nil || v != math.MaxInt64 {
		t.Fatalf("CheckedAdd(Max,0) = %d, %v", v, err)
	}
	if _, err := fixed.CheckedAdd(math.MaxInt64, 1); err == nil {
		t.Fatal("expected CheckedAdd overflow")
	}
	if _, err := fixed.CheckedAdd(math.MinInt64, -1); err == nil {
		t.Fatal("expected CheckedAdd underflow")
	}

	if v, err := fixed.CheckedSub(35, 10); err != nil || v != 25 {
		t.Fatalf("CheckedSub(35,10) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedSub(math.MinInt64, 0); err != nil || v != math.MinInt64 {
		t.Fatalf("CheckedSub(Min,0) = %d, %v", v, err)
	}
	if _, err := fixed.CheckedSub(math.MinInt64, 1); err == nil {
		t.Fatal("expected CheckedSub overflow")
	}
	if _, err := fixed.CheckedSub(math.MaxInt64, -1); err == nil {
		t.Fatal("expected CheckedSub pos overflow")
	}

	if v, err := fixed.CheckedMul(0, 999); err != nil || v != 0 {
		t.Fatalf("CheckedMul(0,999) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedMul(999, 0); err != nil || v != 0 {
		t.Fatalf("CheckedMul(999,0) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedMul(7, -9); err != nil || v != -63 {
		t.Fatalf("CheckedMul(7,-9) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedMul(-1, 5); err != nil || v != -5 {
		t.Fatalf("CheckedMul(-1,5) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedMul(5, -1); err != nil || v != -5 {
		t.Fatalf("CheckedMul(5,-1) = %d, %v", v, err)
	}
	if v, err := fixed.CheckedMul(math.MinInt64, 1); err != nil || v != math.MinInt64 {
		t.Fatalf("CheckedMul(MinInt64,1) = %d, %v", v, err)
	}
	if _, err := fixed.CheckedMul(-1, math.MinInt64); err == nil {
		t.Fatal("expected CheckedMul(-1, MinInt64) overflow")
	}
	if _, err := fixed.CheckedMul(math.MinInt64, -1); err == nil {
		t.Fatal("expected CheckedMul(MinInt64, -1) overflow")
	}
	if _, err := fixed.CheckedMul(math.MaxInt64/2+2, 2); err == nil {
		t.Fatal("expected CheckedMul overflow")
	}

	// MulDiv128 exhaustive edge & sign cases
	if _, err := fixed.MulDiv128(100, 200, 0); err == nil {
		t.Fatal("expected division by zero error")
	}
	if v, err := fixed.MulDiv128(0, 200, 10); err != nil || v != 0 {
		t.Fatalf("MulDiv128 zero a = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(200, 0, 10); err != nil || v != 0 {
		t.Fatalf("MulDiv128 zero b = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(-300, 400, 100); err != nil || v != -1200 {
		t.Fatalf("MulDiv128 neg a = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(300, -400, 100); err != nil || v != -1200 {
		t.Fatalf("MulDiv128 neg b = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(300, 400, -100); err != nil || v != -1200 {
		t.Fatalf("MulDiv128 neg denom = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(-300, -400, 100); err != nil || v != 1200 {
		t.Fatalf("MulDiv128 neg*neg = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(math.MinInt64, 1, 2); err != nil || v != math.MinInt64/2 {
		t.Fatalf("MulDiv128 MinInt64 a = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(2, math.MinInt64, 4); err != nil || v != math.MinInt64/2 {
		t.Fatalf("MulDiv128 MinInt64 b = %d, %v", v, err)
	}
	if v, err := fixed.MulDiv128(math.MinInt64, 1, math.MinInt64); err != nil || v != 1 {
		t.Fatalf("MulDiv128 MinInt64 denom = %d, %v", v, err)
	}
	if _, err := fixed.MulDiv128(math.MinInt64, -1, 1); err == nil {
		t.Fatal("expected MulDiv128 MinInt64*-1 overflow")
	}
	// Exact hi == ud == 1 boundary (1<<32 * 1<<32 = 2^64, hi=1, lo=0, ud=1)
	if _, err := fixed.MulDiv128(1<<32, 1<<32, 1); err == nil {
		t.Fatal("expected MulDiv128 exact hi==ud overflow")
	}
	if _, err := fixed.MulDiv128(math.MaxInt64, math.MaxInt64, 1); err == nil {
		t.Fatal("expected MulDiv128 hi>ud overflow")
	}
	// Exact quo == math.MaxInt64 boundary (must succeed!) vs MaxInt64+1 positive (must fail!)
	if v, err := fixed.MulDiv128(math.MaxInt64, 1, 1); err != nil || v != math.MaxInt64 {
		t.Fatalf("MulDiv128 exact MaxInt64 = %d, %v", v, err)
	}
	// Trigger quo == MaxInt64 + 1 (exact MinInt64) vs quo > MaxInt64 positive overflow
	if v, err := fixed.MulDiv128(-(1 << 62), 2, 1); err != nil || v != math.MinInt64 {
		t.Fatalf("MulDiv128 exact MinInt64 = %d, %v", v, err)
	}
	if _, err := fixed.MulDiv128(1<<62, 2, 1); err == nil {
		t.Fatal("expected MulDiv128 positive 2^63 overflow")
	}
	if _, err := fixed.MulDiv128(-(1<<62 + 1), 2, 1); err == nil {
		t.Fatal("expected MulDiv128 negative overflow beyond MinInt64")
	}

	// RoundToStep & RoundUpToStep boundary tests
	if got := fixed.RoundToStep(105, 10); got != 100 {
		t.Fatalf("RoundToStep(105,10) = %d", got)
	}
	if got := fixed.RoundToStep(100, 10); got != 100 {
		t.Fatalf("RoundToStep(100,10) = %d", got)
	}
	if got := fixed.RoundToStep(0, 10); got != 0 {
		t.Fatalf("RoundToStep(0,10) = %d", got)
	}
	if got := fixed.RoundToStep(-5, 10); got != 0 {
		t.Fatalf("RoundToStep(-5,10) = %d", got)
	}
	if got := fixed.RoundToStep(55, 0); got != 55 {
		t.Fatalf("RoundToStep(55,0) = %d", got)
	}

	if got := fixed.RoundUpToStep(101, 10); got != 110 {
		t.Fatalf("RoundUpToStep(101,10) = %d", got)
	}
	if got := fixed.RoundUpToStep(100, 10); got != 100 {
		t.Fatalf("RoundUpToStep(100,10) = %d", got)
	}
	if got := fixed.RoundUpToStep(0, 10); got != 0 {
		t.Fatalf("RoundUpToStep(0,10) = %d", got)
	}
	if got := fixed.RoundUpToStep(-5, 10); got != 0 {
		t.Fatalf("RoundUpToStep(-5,10) = %d", got)
	}
	if got := fixed.RoundUpToStep(55, 0); got != 55 {
		t.Fatalf("RoundUpToStep(55,0) = %d", got)
	}
	if got := fixed.RoundUpToStep(math.MaxInt64, 10); got != (math.MaxInt64/10)*10 {
		t.Fatalf("RoundUpToStep overflow cap got %d", got)
	}

	// Clamp & Abs exact boundary checks
	if got := fixed.Clamp(5, 10, 20); got != 10 {
		t.Fatalf("Clamp below: %d", got)
	}
	if got := fixed.Clamp(10, 10, 20); got != 10 {
		t.Fatalf("Clamp exact lo: %d", got)
	}
	if got := fixed.Clamp(15, 10, 20); got != 15 {
		t.Fatalf("Clamp inside: %d", got)
	}
	if got := fixed.Clamp(20, 10, 20); got != 20 {
		t.Fatalf("Clamp exact hi: %d", got)
	}
	if got := fixed.Clamp(25, 10, 20); got != 20 {
		t.Fatalf("Clamp above: %d", got)
	}
	if got := fixed.Clamp(15, 20, 10); got != 15 {
		t.Fatalf("Clamp swapped bounds: %d", got)
	}
	if got := fixed.Abs(0); got != 0 {
		t.Fatalf("Abs(0) = %d", got)
	}
	if got := fixed.Abs(42); got != 42 {
		t.Fatalf("Abs(42) = %d", got)
	}
	if got := fixed.Abs(-42); got != 42 {
		t.Fatalf("Abs(-42) = %d", got)
	}
	if got := fixed.Abs(math.MinInt64); got != math.MaxInt64 {
		t.Fatalf("Abs(MinInt64) = %d", got)
	}

	// JSON string serialization invariant
	v := fixed.Value(px)
	raw, err := json.Marshal(v)
	if err != nil || string(raw) != `"6842550000000"` {
		t.Fatalf("unexpected JSON: %s (%v)", string(raw), err)
	}
	var decoded fixed.Value
	if err := json.Unmarshal(raw, &decoded); err != nil || decoded.Int64() != px {
		t.Fatalf("unmarshal mismatch: %d (%v)", decoded, err)
	}

	// QuantizeSignalFloat boundary & NaN / Inf checks
	if got := fixed.QuantizeSignalFloat(math.NaN(), -100, 200); got != 50 {
		t.Fatalf("NaN quantization got %d want 50", got)
	}
	if got := fixed.QuantizeSignalFloat(math.NaN(), 200, -100); got != 50 {
		t.Fatalf("NaN swapped bounds got %d want 50", got)
	}
	if got := fixed.QuantizeSignalFloat(math.Inf(1), -100, 100); got != 100 {
		t.Fatalf("+Inf quantization got %d", got)
	}
	if got := fixed.QuantizeSignalFloat(math.Inf(-1), -100, 100); got != -100 {
		t.Fatalf("-Inf quantization got %d", got)
	}
	if got := fixed.QuantizeSignalFloat(1.5, 0, 2*fixed.Scale); got != 150_000_000 {
		t.Fatalf("QuantizeSignalFloat(1.5) got %d", got)
	}
	if got := fixed.QuantizeSignalFloat(5.0, 0, 2*fixed.Scale); got != 2*fixed.Scale {
		t.Fatalf("QuantizeSignalFloat clamp hi got %d", got)
	}
	if got := fixed.QuantizeSignalFloat(-5.0, -fixed.Scale, fixed.Scale); got != -fixed.Scale {
		t.Fatalf("QuantizeSignalFloat clamp lo got %d", got)
	}
}

func TestDeterministicRNGNoNaNOrInf(t *testing.T) {
	s1 := rng.NewStream(42, 7)
	s2 := rng.NewStream(42, 7)

	for i := 0; i < 10_000; i++ {
		u1 := s1.Uint64()
		u2 := s2.Uint64()
		if u1 != u2 {
			t.Fatalf("PRNG diverged at step %d: %d != %d", i, u1, u2)
		}
		fOpen := s1.Float64Open()
		if fOpen <= 0.0 || fOpen >= 1.0 || math.IsNaN(fOpen) || math.IsInf(fOpen, 0) {
			t.Fatalf("invalid Float64Open: %v", fOpen)
		}
		_ = s2.Float64Open()

		n := s1.NormFloat64()
		if math.IsNaN(n) || math.IsInf(n, 0) || n < -8.0 || n > 8.0 {
			t.Fatalf("invalid NormFloat64: %v", n)
		}
		_ = s2.NormFloat64()

		p := s1.Pareto(math.NaN(), -5.0, math.Inf(1))
		if math.IsNaN(p) || math.IsInf(p, 0) || p <= 0 {
			t.Fatalf("invalid Pareto under pathological args: %v", p)
		}
		_ = s2.Pareto(math.NaN(), -5.0, math.Inf(1))

		e := s1.ExpRate(0.0)
		if math.IsNaN(e) || math.IsInf(e, 0) || e <= 0 {
			t.Fatalf("invalid ExpRate(0): %v", e)
		}
		_ = s2.ExpRate(0.0)
	}
}
