package rng

import (
	"math"
)

// Stream is a zero-allocation, lock-free deterministic PRNG based on SplitMix64
// seeding + PCG-XSH-RR / Xoshiro256** style 64-bit state transitions.
// Each simulated agent owns an independent Stream derived from (MasterSeed, AgentID),
// guaranteeing 100% bit-for-bit reproducible trajectories without mutex contention.
type Stream struct {
	s0 uint64
	s1 uint64
	s2 uint64
	s3 uint64

	hasSpareNorm bool
	spareNorm    float64
}

// NewStream derives an independent deterministic PRNG stream from masterSeed and streamID.
func NewStream(masterSeed uint64, streamID uint64) Stream {
	z := masterSeed ^ (streamID * 0x9E3779B97F4A7C15)
	s0 := splitMix64(&z)
	s1 := splitMix64(&z)
	s2 := splitMix64(&z)
	s3 := splitMix64(&z)
	if s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 {
		s0 = 0x6A09E667F3BCC908
		s1 = 0xBB67AE8584CAA73B
		s2 = 0x3C6EF372FE94F82B
		s3 = 0xA54FF53A5F1D36F1
	}
	return Stream{s0: s0, s1: s1, s2: s2, s3: s3}
}

func splitMix64(x *uint64) uint64 {
	*x += 0x9E3779B97F4A7C15
	z := *x
	z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
	z = (z ^ (z >> 27)) * 0x94D049BB133111EB
	return z ^ (z >> 31)
}

func rotl64(x uint64, k int) uint64 {
	return (x << k) | (x >> (64 - k))
}

// Uint64 advances xoshiro256** and returns a 64-bit pseudo-random integer.
func (r *Stream) Uint64() uint64 {
	result := rotl64(r.s1*5, 7) * 9
	t := r.s1 << 17

	r.s2 ^= r.s0
	r.s3 ^= r.s1
	r.s1 ^= r.s2
	r.s0 ^= r.s3

	r.s2 ^= t
	r.s3 = rotl64(r.s3, 45)
	return result
}

// Int63n returns a uniform integer in [0, n). Returns 0 if n <= 0.
func (r *Stream) Int63n(n int64) int64 {
	if n <= 0 {
		return 0
	}
	un := uint64(n)
	if un&(un-1) == 0 {
		return int64(r.Uint64() & (un - 1))
	}
	max := ^uint64(0) - (^uint64(0) % un)
	v := r.Uint64()
	for v >= max {
		v = r.Uint64()
	}
	return int64(v % un)
}

// Float64Open returns a uniform float64 strictly in the open interval (0, 1),
// bounded in [2^-53, 1 - 2^-53] so math.Log(u) and 1/u NEVER divide by zero or return +/-Inf/NaN.
func (r *Stream) Float64Open() float64 {
	const inv53 = 1.0 / (1 << 53)
	u := (r.Uint64() >> 11)
	if u == 0 {
		u = 1
	} else if u >= (1<<53)-1 {
		u = (1 << 53) - 2
	}
	return float64(u) * inv53
}

// Float64 returns a uniform float64 in [0, 1).
func (r *Stream) Float64() float64 {
	const inv53 = 1.0 / (1 << 53)
	return float64(r.Uint64()>>11) * inv53
}

// NormFloat64 returns a standard normal N(0, 1) variate via Marsaglia polar method,
// strictly finite and clamped to [-8.0, 8.0] to prevent catastrophic tail overflow.
func (r *Stream) NormFloat64() float64 {
	if r.hasSpareNorm {
		r.hasSpareNorm = false
		return clampFinite(r.spareNorm, -8.0, 8.0)
	}
	for i := 0; i < 64; i++ {
		u := 2.0*r.Float64Open() - 1.0
		v := 2.0*r.Float64Open() - 1.0
		s := u*u + v*v
		if s > 1e-12 && s < 1.0 {
			mul := math.Sqrt(-2.0 * math.Log(s) / s)
			r.spareNorm = v * mul
			r.hasSpareNorm = true
			return clampFinite(u*mul, -8.0, 8.0)
		}
	}
	return 0.0
}

// ExpFloat64 returns an exponential Exp(1) variate using -ln(U) on strictly open U in (0, 1).
func (r *Stream) ExpFloat64() float64 {
	val := -math.Log(r.Float64Open())
	return clampFinite(val, 1e-9, 36.0)
}

// ExpRate returns an inter-arrival time ~ Exp(rate), guarded against rate <= 0.
func (r *Stream) ExpRate(rate float64) float64 {
	if math.IsNaN(rate) || math.IsInf(rate, 0) || rate <= 1e-9 {
		rate = 1e-9
	}
	return clampFinite(r.ExpFloat64()/rate, 1e-9, 3600.0)
}

// Pareto draws from a Type-I Pareto distribution X = xm / U^(1/alpha) with finite upper cap.
// Used for realistic heavy-tailed order sizes and Pareto network jitter injection.
func (r *Stream) Pareto(xm, alpha, maxCap float64) float64 {
	if math.IsNaN(xm) || xm <= 0 {
		xm = 1.0
	}
	if math.IsNaN(alpha) || alpha <= 0.1 {
		alpha = 1.5
	}
	if math.IsNaN(maxCap) || maxCap < xm {
		maxCap = xm * 100.0
	}
	u := r.Float64Open()
	denom := math.Pow(u, 1.0/alpha)
	if math.IsNaN(denom) || denom <= 1e-12 {
		return maxCap
	}
	val := xm / denom
	return clampFinite(val, xm, maxCap)
}

func clampFinite(x, lo, hi float64) float64 {
	if math.IsNaN(x) {
		return 0.5 * (lo + hi)
	}
	if math.IsInf(x, 1) || x > hi {
		return hi
	}
	if math.IsInf(x, -1) || x < lo {
		return lo
	}
	return x
}
