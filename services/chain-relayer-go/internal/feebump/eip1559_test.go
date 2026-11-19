package feebump

import (
	"errors"
	"testing"
)

func TestPolicyValidate_ExactBoundaries(t *testing.T) {
	p := DefaultPolicy()
	if err := p.Validate(); err != nil {
		t.Fatalf("default policy invalid: %v", err)
	}

	// Zero min priority
	bad := p
	bad.MinPriorityFeeWei = 0
	if !errors.Is(bad.Validate(), ErrInvalidFeePolicy) {
		t.Fatalf("expected invalid policy for 0 min priority")
	}

	// MaxPriority == MinPriority is valid; MaxPriority < MinPriority is invalid
	eq := p
	eq.MaxPriorityFeeCapWei = eq.MinPriorityFeeWei
	if err := eq.Validate(); err != nil {
		t.Fatalf("equal min/max priority must be valid: %v", err)
	}
	lt := p
	lt.MaxPriorityFeeCapWei = lt.MinPriorityFeeWei - 1
	if !errors.Is(lt.Validate(), ErrInvalidFeePolicy) {
		t.Fatalf("maxPriority < minPriority must fail")
	}

	// MaxFee == MaxPriority is valid; MaxFee < MaxPriority is invalid
	eqCeil := p
	eqCeil.MaxFeeHardCeilingWei = eqCeil.MaxPriorityFeeCapWei
	if err := eqCeil.Validate(); err != nil {
		t.Fatalf("ceiling == maxPriority must be valid: %v", err)
	}
	ltCeil := p
	ltCeil.MaxFeeHardCeilingWei = ltCeil.MaxPriorityFeeCapWei - 1
	if !errors.Is(ltCeil.Validate(), ErrInvalidFeePolicy) {
		t.Fatalf("ceiling < maxPriority must fail")
	}

	// ReplacementBumpBps exact 1250 floor
	floorOk := p
	floorOk.ReplacementBumpBps = MinMempoolReplacementBumpBps
	if err := floorOk.Validate(); err != nil {
		t.Fatalf("1250 bps floor must be valid: %v", err)
	}
	belowFloor := p
	belowFloor.ReplacementBumpBps = MinMempoolReplacementBumpBps - 1
	if !errors.Is(belowFloor.Validate(), ErrInvalidFeePolicy) {
		t.Fatalf("1249 bps must fail")
	}

	zeroAttempts := p
	zeroAttempts.MaxBumpAttempts = 0
	if !errors.Is(zeroAttempts.Validate(), ErrInvalidFeePolicy) {
		t.Fatalf("0 max attempts must fail")
	}
}

func TestInitialQuoteAndBumpQuote_ExactIntegerSemantics(t *testing.T) {
	p := Policy{
		MinPriorityFeeWei:    100,
		MaxPriorityFeeCapWei: 1_000,
		MaxFeeHardCeilingWei: 10_000,
		ReplacementBumpBps:   1_500, // +15.00%
		MaxBumpAttempts:      3,
	}

	// Clamp low tip -> 100; maxFee = 2*500 + 100 = 1100
	q0, err := InitialQuote(p, 500, 10)
	if err != nil {
		t.Fatalf("InitialQuote failed: %v", err)
	}
	if q0.MaxPriorityFeePerGasWei != 100 || q0.MaxFeePerGasWei != 1_100 || q0.Attempt != 0 {
		t.Fatalf("unexpected q0: %+v", q0)
	}

	// Exact ceiling boundary: 2*4500 + 1000 = 10000 == ceiling (must pass!)
	qExact, err := InitialQuote(p, 4_500, 2_000)
	if err != nil || qExact.MaxFeePerGasWei != 10_000 || qExact.MaxPriorityFeePerGasWei != 1_000 {
		t.Fatalf("exact ceiling failed: %+v err=%v", qExact, err)
	}

	// 1 wei over ceiling: 2*4500 + 1000 with ceiling=9999 -> must fail!
	pTight := p
	pTight.MaxFeeHardCeilingWei = 9_999
	if _, err := InitialQuote(pTight, 4_500, 2_000); !errors.Is(err, ErrFeeCeilingExceeded) {
		t.Fatalf("expected ceiling exceeded, got %v", err)
	}

	// Bump 1: 100 * 1.15 = 115; 1100 * 1.15 = 1265; market = 2*500 + 115 = 1115 -> max=1265
	q1, err := BumpQuote(p, q0, 500, 100)
	if err != nil {
		t.Fatalf("BumpQuote 1 failed: %v", err)
	}
	if q1.Attempt != 1 || q1.MaxPriorityFeePerGasWei != 115 || q1.MaxFeePerGasWei != 1_265 {
		t.Fatalf("unexpected q1: %+v", q1)
	}

	// Bump 2 with baseFee spike (baseFee=2000 -> market = 4000 + ceil(115*1.15=132.25 -> 133) = 4133)
	q2, err := BumpQuote(p, q1, 2_000, 100)
	if err != nil {
		t.Fatalf("BumpQuote 2 failed: %v", err)
	}
	if q2.Attempt != 2 || q2.MaxPriorityFeePerGasWei != 133 || q2.MaxFeePerGasWei != 4_133 {
		t.Fatalf("unexpected q2 (ceil rounding check!): %+v", q2)
	}

	// Bump 3 (Attempt == MaxBumpAttempts == 3 -> must still succeed!)
	q3, err := BumpQuote(p, q2, 2_000, 100)
	if err != nil || q3.Attempt != 3 {
		t.Fatalf("attempt == MaxBumpAttempts must succeed: %+v err=%v", q3, err)
	}

	// Bump 4 (Attempt 4 > MaxBumpAttempts 3 -> must fail!)
	if _, err := BumpQuote(p, q3, 2_000, 100); !errors.Is(err, ErrMaxBumpAttemptsReached) {
		t.Fatalf("attempt > MaxBumpAttempts must fail, got %v", err)
	}

	// Small 1-wei bump strictly increases above prev even if bps rounds to 1
	pTiny := Policy{
		MinPriorityFeeWei:    1,
		MaxPriorityFeeCapWei: 100,
		MaxFeeHardCeilingWei: 1_000,
		ReplacementBumpBps:   1_250,
		MaxBumpAttempts:      2,
	}
	qTiny := Quote{MaxFeePerGasWei: 5, MaxPriorityFeePerGasWei: 1, Attempt: 0}
	qTinyBumped, err := BumpQuote(pTiny, qTiny, 1, 1)
	if err != nil || qTinyBumped.MaxPriorityFeePerGasWei <= qTiny.MaxPriorityFeePerGasWei || qTinyBumped.MaxFeePerGasWei <= qTiny.MaxFeePerGasWei {
		t.Fatalf("tiny quote failed strict monotonic increase: %+v err=%v", qTinyBumped, err)
	}
}

func TestFeebump_OverflowAndPriorityCapBoundaries(t *testing.T) {
	// 1. Exact uint64 overflow boundary in InitialQuote & BumpQuote
	pMax := Policy{
		MinPriorityFeeWei:    1,
		MaxPriorityFeeCapWei: 1_000,
		MaxFeeHardCeilingWei: ^uint64(0),
		ReplacementBumpBps:   1_500,
		MaxBumpAttempts:      4,
	}
	const tip uint64 = 100
	exactMaxBase := (^uint64(0) - tip) / 2
	qExact, err := InitialQuote(pMax, exactMaxBase, tip)
	if err != nil || qExact.MaxFeePerGasWei != 2*exactMaxBase+tip {
		t.Fatalf("exact non-overflow baseFee must succeed: q=%+v err=%v", qExact, err)
	}
	if _, err := InitialQuote(pMax, exactMaxBase+1, tip); !errors.Is(err, ErrArithmeticOverflow) {
		t.Fatalf("baseFee = exactMaxBase+1 must return ErrArithmeticOverflow, got %v", err)
	}

	// 2. BumpQuote exact uint64 overflow boundary on baseFee
	qLow := Quote{MaxFeePerGasWei: 1_000, MaxPriorityFeePerGasWei: 100, Attempt: 0}
	// nextTip will be ceil(100 * 1.15) = 115
	exactBumpBase := (^uint64(0) - 115) / 2
	qBumpOk, err := BumpQuote(pMax, qLow, exactBumpBase, 100)
	if err != nil || qBumpOk.MaxFeePerGasWei != 2*exactBumpBase+115 {
		t.Fatalf("BumpQuote exact non-overflow baseFee must succeed: %+v err=%v", qBumpOk, err)
	}
	if _, err := BumpQuote(pMax, qLow, exactBumpBase+1, 100); !errors.Is(err, ErrArithmeticOverflow) {
		t.Fatalf("BumpQuote baseFee+1 must return ErrArithmeticOverflow, got %v", err)
	}

	// 3. Priority cap exact boundary: 800 * 1.25 = 1000 == MaxPriorityFeeCapWei succeeds; 801 * 1.25 = 1002 > 1000 fails
	pCap := Policy{
		MinPriorityFeeWei:    100,
		MaxPriorityFeeCapWei: 1_000,
		MaxFeeHardCeilingWei: 100_000,
		ReplacementBumpBps:   2_500, // +25%
		MaxBumpAttempts:      4,
	}
	qExactCap := Quote{MaxFeePerGasWei: 5_000, MaxPriorityFeePerGasWei: 800, Attempt: 0} // 800*1.25 = 1000 == cap
	qExactCapRes, err := BumpQuote(pCap, qExactCap, 500, 100)
	if err != nil || qExactCapRes.MaxPriorityFeePerGasWei != 1_000 {
		t.Fatalf("bumpedTip == MaxPriorityFeeCapWei must succeed: %+v err=%v", qExactCapRes, err)
	}
	qOverCap := Quote{MaxFeePerGasWei: 5_000, MaxPriorityFeePerGasWei: 801, Attempt: 0} // ceil(801*1.25) = 1002 > 1000
	if _, err := BumpQuote(pCap, qOverCap, 500, 100); !errors.Is(err, ErrFeeCeilingExceeded) {
		t.Fatalf("bumpedTip > MaxPriorityFeeCapWei must fail with ErrFeeCeilingExceeded, got %v", err)
	}

	// 4. mulDivCeilU64 overflow boundary
	if _, err := mulDivCeilU64(^uint64(0)/10+1, 10, 10); !errors.Is(err, ErrArithmeticOverflow) {
		t.Fatalf("expected mulDivCeilU64 overflow, got %v", err)
	}
	val, err := mulDivCeilU64(^uint64(0)/10, 10, 10)
	if err != nil || val != ^uint64(0)/10 {
		t.Fatalf("exact non-overflow mulDivCeilU64 failed: val=%d err=%v", val, err)
	}
}
