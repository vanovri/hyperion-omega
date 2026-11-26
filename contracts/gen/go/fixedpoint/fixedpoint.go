package fixedpoint

import (
	"fmt"
	"math"
	"math/big"
	"math/bits"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
)

const (
	// FPScale is the global 1e8 fixed-point scale factor.
	FPScale uint64 = 100_000_000
	// PPMScale is the 1e6 parts-per-million scale factor.
	PPMScale int64 = 1_000_000
	// WSFrameMagic is the 16-bit little-endian binary WebSocket frame magic (0x4859).
	WSFrameMagic uint16 = 0x4859
	// WSFrameVersion is the binary WebSocket wire protocol version.
	WSFrameVersion uint8 = 1
	// WSHeaderLen is the universal 32-byte binary WebSocket header size.
	WSHeaderLen int = 32
)

// Uint128 is a stack-allocated, zero-heap exact unsigned 128-bit integer.
type Uint128 struct {
	Hi uint64
	Lo uint64
}

// Int128 is a stack-allocated, zero-heap two's-complement signed 128-bit integer.
type Int128 struct {
	Hi int64
	Lo uint64
}

// AlignedBatch8 packs 8 order prices and 8 quantities onto two exact 64-byte cache lines.
type AlignedBatch8 struct {
	Prices [8]uint64 // 64 bytes
	Qtys   [8]uint64 // 64 bytes
}

// AlignedOut8 holds 8 zero-allocation 128-bit notional results across two 64-byte cache lines.
type AlignedOut8 struct {
	Hi [8]uint64 // 64 bytes
	Lo [8]uint64 // 64 bytes
}

var (
	twoPow64  = new(big.Int).Lsh(big.NewInt(1), 64)
	twoPow127 = new(big.Int).Lsh(big.NewInt(1), 127)
	twoPow128 = new(big.Int).Lsh(big.NewInt(1), 128)
	minI128   = new(big.Int).Neg(twoPow127)
	mask64    = new(big.Int).Sub(twoPow64, big.NewInt(1))
)

// ToProtoU128 converts an exact non-negative *big.Int (< 2^128) to hyperionv1.U128.
func ToProtoU128(v *big.Int) (*hyperionv1.U128, error) {
	if v == nil || v.Sign() < 0 || v.Cmp(twoPow128) >= 0 {
		return nil, fmt.Errorf("u128 out of bounds: %v", v)
	}
	lo := new(big.Int).And(v, mask64).Uint64()
	hi := new(big.Int).Rsh(v, 64).Uint64()
	return &hyperionv1.U128{Hi: hi, Lo: lo}, nil
}

// FromProtoU128 converts hyperionv1.U128 to an exact *big.Int.
func FromProtoU128(m *hyperionv1.U128) *big.Int {
	if m == nil {
		return big.NewInt(0)
	}
	hi := new(big.Int).SetUint64(m.Hi)
	lo := new(big.Int).SetUint64(m.Lo)
	return hi.Lsh(hi, 64).Or(hi, lo)
}

// ToProtoI128 converts a signed *big.Int in [-2^127, 2^127-1] to two's-complement hyperionv1.I128.
func ToProtoI128(v *big.Int) (*hyperionv1.I128, error) {
	if v == nil || v.Cmp(minI128) < 0 || v.Cmp(twoPow127) >= 0 {
		return nil, fmt.Errorf("i128 out of bounds: %v", v)
	}
	twos := new(big.Int).Set(v)
	if twos.Sign() < 0 {
		twos.Add(twos, twoPow128)
	}
	lo := new(big.Int).And(twos, mask64).Uint64()
	hiUnsigned := new(big.Int).Rsh(twos, 64).Uint64()
	return &hyperionv1.I128{Hi: int64(hiUnsigned), Lo: lo}, nil
}

// FromProtoI128 converts two's-complement hyperionv1.I128 back to a signed *big.Int.
func FromProtoI128(m *hyperionv1.I128) *big.Int {
	if m == nil {
		return big.NewInt(0)
	}
	hi := new(big.Int).SetInt64(m.Hi)
	lo := new(big.Int).SetUint64(m.Lo)
	return hi.Lsh(hi, 64).Add(hi, lo)
}

// Int128FromInt64 converts a signed int64 into a stack-allocated two's-complement Int128.
func Int128FromInt64(v int64) Int128 {
	hi := int64(0)
	if v < 0 {
		hi = -1
	}
	return Int128{Hi: hi, Lo: uint64(v)}
}

// MulDivU64ToU128 computes exact floor((priceFP * qtyFP) / 1e8) with 0 heap allocations
// using hardware 64x64->128 multiply and 128/64 divide instructions.
func MulDivU64ToU128(priceFP, qtyFP uint64) Uint128 {
	hi, lo := bits.Mul64(priceFP, qtyFP)
	if hi < FPScale {
		qLo, _ := bits.Div64(hi, lo, FPScale)
		return Uint128{Hi: 0, Lo: qLo}
	}
	qHi, rHi := bits.Div64(0, hi, FPScale)
	qLo, _ := bits.Div64(rHi, lo, FPScale)
	return Uint128{Hi: qHi, Lo: qLo}
}

// CheckedFeeFP128 computes exact round-toward-zero (notional * feePPM) / 1_000_000
// with 0 heap allocations and strict int64 overflow checking.
func CheckedFeeFP128(notional Uint128, feePPM int64) (int64, bool) {
	if feePPM == 0 {
		return 0, true
	}
	neg := feePPM < 0
	var absPPM uint64
	if neg {
		if feePPM == math.MinInt64 {
			absPPM = 1 << 63
		} else {
			absPPM = uint64(-feePPM)
		}
	} else {
		absPPM = uint64(feePPM)
	}

	// Multiply 128-bit notional by 64-bit absPPM -> 192-bit (p2, p1, p0)
	loHi, p0 := bits.Mul64(notional.Lo, absPPM)
	hiHi, hiLo := bits.Mul64(notional.Hi, absPPM)
	p1, carry := bits.Add64(loHi, hiLo, 0)
	p2 := hiHi + carry

	// If top limb >= PPMScale, quotient exceeds 128 bits and definitely overflows int64
	ppmU64 := uint64(PPMScale)
	if p2 >= ppmU64 {
		return 0, false
	}
	qHi, rem1 := bits.Div64(p2, p1, ppmU64)
	if qHi > 0 {
		return 0, false
	}
	qLo, _ := bits.Div64(rem1, p0, ppmU64)
	if neg {
		if qLo > (uint64(1) << 63) {
			return 0, false
		}
		if qLo == (uint64(1) << 63) {
			return math.MinInt64, true
		}
		return -int64(qLo), true
	}
	if qLo > math.MaxInt64 {
		return 0, false
	}
	return int64(qLo), true
}

// ComputeNotionalBatch8Aligned executes an 8-lane unrolled zero-allocation notional calculation
// over 64-byte cache-line aligned SoA blocks.
func ComputeNotionalBatch8Aligned(in *AlignedBatch8, out *AlignedOut8) {
	for i := 0; i < 8; i++ {
		res := MulDivU64ToU128(in.Prices[i], in.Qtys[i])
		out.Hi[i] = res.Hi
		out.Lo[i] = res.Lo
	}
}

// ComputeNotionalFP computes exact floor((priceFP * qtyFP) / 1e8) without uint64 overflow.
// Internally accelerated via hardware 128-bit MulDivU64ToU128 before wrapping in *big.Int.
func ComputeNotionalFP(priceFP, qtyFP uint64) *big.Int {
	u := MulDivU64ToU128(priceFP, qtyFP)
	if u.Hi == 0 {
		return new(big.Int).SetUint64(u.Lo)
	}
	hi := new(big.Int).SetUint64(u.Hi)
	lo := new(big.Int).SetUint64(u.Lo)
	return hi.Lsh(hi, 64).Or(hi, lo)
}
