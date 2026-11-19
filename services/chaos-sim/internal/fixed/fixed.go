package fixed

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"math/bits"
	"strconv"
	"strings"
)

// Scale is the canonical HYPERION fixed-point multiplier (1e8).
// All monetary prices, quantities, notional values, balances, and fees
// MUST be represented as exact integers scaled by 100,000,000.
const (
	Scale           int64 = 100_000_000
	CentUnits       int64 = 1_000_000 // 0.01 in 1e8 scale
	BasisPointScale int64 = 10_000
	PPMScale        int64 = 1_000_000 // Canonical HYPERION fee_ppm scale (100 ppm = 1 bps)
)

var (
	ErrOverflow        = errors.New("fixed: integer overflow")
	ErrDivisionByZero  = errors.New("fixed: division by zero")
	ErrNegativeNotAllowed = errors.New("fixed: negative value not allowed")
	ErrInvalidFormat   = errors.New("fixed: invalid fixed-point string")
)

// Value wraps an int64 scaled by 1e8 and serializes to/from JSON strings
// (matching OpenAPI x-hyperion-scale: 100000000 string representation) while also
// accepting JSON numbers for ergonomic control API inputs.
type Value int64

func (v Value) Int64() int64 { return int64(v) }

func (v Value) String() string {
	return strconv.FormatInt(int64(v), 10)
}

// DecimalString formats v as a human-readable fixed-point decimal ("123.45000000").
func (v Value) DecimalString() string {
	raw := int64(v)
	neg := raw < 0
	var mag uint64
	if neg {
		if raw == math.MinInt64 {
			mag = uint64(math.MaxInt64) + 1
		} else {
			mag = uint64(-raw)
		}
	} else {
		mag = uint64(raw)
	}
	whole := mag / uint64(Scale)
	frac := mag % uint64(Scale)
	if neg {
		return fmt.Sprintf("-%d.%08d", whole, frac)
	}
	return fmt.Sprintf("%d.%08d", whole, frac)
}

func (v Value) MarshalJSON() ([]byte, error) {
	return json.Marshal(strconv.FormatInt(int64(v), 10))
}

func (v *Value) UnmarshalJSON(data []byte) error {
	s := strings.TrimSpace(string(data))
	if len(s) >= 2 && s[0] == '"' && s[len(s)-1] == '"' {
		s = s[1 : len(s)-1]
	}
	if s == "" || s == "null" {
		*v = 0
		return nil
	}
	if strings.ContainsRune(s, '.') {
		parsed, err := ParseDecimal(s)
		if err != nil {
			return err
		}
		*v = Value(parsed)
		return nil
	}
	iv, err := strconv.ParseInt(s, 10, 64)
	if err != nil {
		return fmt.Errorf("%w: %v", ErrInvalidFormat, err)
	}
	*v = Value(iv)
	return nil
}

// ParseDecimal parses an exact decimal string like "50000.125" into 1e8 scaled int64
// without any floating-point conversion.
func ParseDecimal(s string) (int64, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0, ErrInvalidFormat
	}
	neg := false
	if s[0] == '-' {
		neg = true
		s = s[1:]
	} else if s[0] == '+' {
		s = s[1:]
	}
	if s == "" {
		return 0, ErrInvalidFormat
	}
	parts := strings.Split(s, ".")
	if len(parts) > 2 {
		return 0, ErrInvalidFormat
	}
	wholeStr := parts[0]
	if wholeStr == "" {
		wholeStr = "0"
	}
	whole, err := strconv.ParseUint(wholeStr, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("%w: %v", ErrInvalidFormat, err)
	}
	var frac uint64
	if len(parts) == 2 && parts[1] != "" {
		fStr := parts[1]
		if len(fStr) > 8 {
			fStr = fStr[:8]
		} else {
			fStr = fStr + strings.Repeat("0", 8-len(fStr))
		}
		frac, err = strconv.ParseUint(fStr, 10, 64)
		if err != nil {
			return 0, fmt.Errorf("%w: %v", ErrInvalidFormat, err)
		}
	}
	hi, lo := bits.Mul64(whole, uint64(Scale))
	if hi != 0 {
		return 0, ErrOverflow
	}
	sum, carry := bits.Add64(lo, frac, 0)
	if carry != 0 {
		return 0, ErrOverflow
	}
	if neg {
		if sum > uint64(math.MaxInt64)+1 {
			return 0, ErrOverflow
		}
		if sum == uint64(math.MaxInt64)+1 {
			return math.MinInt64, nil
		}
		return -int64(sum), nil
	}
	if sum > uint64(math.MaxInt64) {
		return 0, ErrOverflow
	}
	return int64(sum), nil
}

// FromUnits converts whole integer units into 1e8 fixed-point safely.
func FromUnits(u int64) (int64, error) {
	return CheckedMul(u, Scale)
}

// MustFromUnits converts whole units or panics on static init bug.
func MustFromUnits(u int64) int64 {
	v, err := FromUnits(u)
	if err != nil {
		panic(err)
	}
	return v
}

// CheckedAdd returns a + b or ErrOverflow.
func CheckedAdd(a, b int64) (int64, error) {
	res := a + b
	if (res^a)&(res^b) < 0 {
		return 0, ErrOverflow
	}
	return res, nil
}

// CheckedSub returns a - b or ErrOverflow.
func CheckedSub(a, b int64) (int64, error) {
	res := a - b
	if (res^a)&(a^b) < 0 {
		return 0, ErrOverflow
	}
	return res, nil
}

// CheckedMul returns a * b or ErrOverflow.
func CheckedMul(a, b int64) (int64, error) {
	if a == 0 || b == 0 {
		return 0, nil
	}
	if (a == -1 && b == math.MinInt64) || (b == -1 && a == math.MinInt64) {
		return 0, ErrOverflow
	}
	res := a * b
	if res/a != b {
		return 0, ErrOverflow
	}
	return res, nil
}

// MulDiv128 computes (a * b) / denom using exact 128-bit unsigned intermediate
// arithmetic with zero heap allocations across the entire int64 range including math.MinInt64.
func MulDiv128(a, b, denom int64) (int64, error) {
	if denom == 0 {
		return 0, ErrDivisionByZero
	}
	if a == 0 || b == 0 {
		return 0, nil
	}

	neg := (a < 0) != (b < 0)
	ua := absUint64(a)
	ub := absUint64(b)
	ud := absUint64(denom)
	if denom < 0 {
		neg = !neg
	}

	hi, lo := bits.Mul64(ua, ub)
	if hi >= ud {
		return 0, ErrOverflow
	}
	quo, _ := bits.Div64(hi, lo, ud)
	if neg {
		if quo > uint64(math.MaxInt64)+1 {
			return 0, ErrOverflow
		}
		// In Go two's complement: -int64(uint64(math.MaxInt64)+1) == math.MinInt64 exactly.
		return -int64(quo), nil
	}
	if quo > uint64(math.MaxInt64) {
		return 0, ErrOverflow
	}
	return int64(quo), nil
}

func absUint64(x int64) uint64 {
	if x < 0 {
		return uint64(-x)
	}
	return uint64(x)
}

// QuoteNotional computes (price * qty) / Scale in exact 128-bit integer math.
func QuoteNotional(price, qty int64) (int64, error) {
	if price < 0 || qty < 0 {
		return 0, ErrNegativeNotAllowed
	}
	return MulDiv128(price, qty, Scale)
}

// FeeFromBps computes (notional * bps) / 10_000 in exact 128-bit integer math.
// Supports negative bps (maker rebates) and positive bps (taker fees).
func FeeFromBps(notional, bps int64) (int64, error) {
	return MulDiv128(notional, bps, BasisPointScale)
}

// FeeFromPPM computes round_toward_zero((notional * fee_ppm) / 1_000_000) per 00-hyperion-contract.md §5.1.2.
func FeeFromPPM(notional, ppm int64) (int64, error) {
	return MulDiv128(notional, ppm, PPMScale)
}

// RoundToStep aligns val down to the nearest positive step multiple (tick size or lot size).
// Guarantees result >= 0 when val >= 0.
func RoundToStep(val, step int64) int64 {
	if step <= 0 || val <= 0 {
		if val < 0 {
			return 0
		}
		return val
	}
	return (val / step) * step
}

// RoundUpToStep aligns val up to the nearest positive step multiple.
func RoundUpToStep(val, step int64) int64 {
	if step <= 0 || val <= 0 {
		if val < 0 {
			return 0
		}
		return val
	}
	rem := val % step
	if rem == 0 {
		return val
	}
	res, err := CheckedAdd(val, step-rem)
	if err != nil {
		return (math.MaxInt64 / step) * step
	}
	return res
}

// QuantizeSignalFloat safely converts a dimensionless mathematical model signal
// (e.g., Avellaneda-Stoikov reservation offset or Hawkes intensity) into bounded
// fixed-point units, guarding strictly against NaN, +Inf, -Inf, and int64 overflow.
// Never use floats to store balances, order prices, or ledger states.
func QuantizeSignalFloat(signal float64, minBound, maxBound int64) int64 {
	if minBound > maxBound {
		minBound, maxBound = maxBound, minBound
	}
	if math.IsNaN(signal) {
		return (minBound / 2) + (maxBound / 2)
	}
	if math.IsInf(signal, 1) {
		return maxBound
	}
	if math.IsInf(signal, -1) {
		return minBound
	}
	scaled := signal * float64(Scale)
	if math.IsNaN(scaled) {
		return minBound
	}
	if scaled >= float64(maxBound) {
		return maxBound
	}
	if scaled <= float64(minBound) {
		return minBound
	}
	return int64(math.Round(scaled))
}

// Clamp bounds v into [lo, hi] using branch-friendly comparison order.
func Clamp(v, lo, hi int64) int64 {
	if lo > hi {
		lo, hi = hi, lo
	}
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

// Abs computes |v| branchlessly for all int64 values (saturating MinInt64 -> MaxInt64).
func Abs(v int64) int64 {
	if v == math.MinInt64 {
		return math.MaxInt64
	}
	mask := v >> 63
	return (v ^ mask) - mask
}
