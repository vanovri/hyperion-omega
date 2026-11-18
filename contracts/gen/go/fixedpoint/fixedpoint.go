package fixedpoint

import (
	"fmt"
	"math/big"

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

var (
	twoPow64  = new(big.Int).Lsh(big.NewInt(1), 64)
	twoPow127 = new(big.Int).Lsh(big.NewInt(1), 127)
	twoPow128 = new(big.Int).Lsh(big.NewInt(1), 128)
	mask64    = new(big.Int).Sub(twoPow64, big.NewInt(1))
	fpScaleBI = new(big.Int).SetUint64(FPScale)
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
	minI128 := new(big.Int).Neg(twoPow127)
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

// ComputeNotionalFP computes exact floor((priceFP * qtyFP) / 1e8) without uint64 overflow.
func ComputeNotionalFP(priceFP, qtyFP uint64) *big.Int {
	p := new(big.Int).SetUint64(priceFP)
	q := new(big.Int).SetUint64(qtyFP)
	prod := new(big.Int).Mul(p, q)
	return prod.Quo(prod, fpScaleBI)
}
