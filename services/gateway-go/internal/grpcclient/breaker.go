package grpcclient

import (
	"errors"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

// ErrCircuitOpen is returned immediately when the adaptive circuit breaker rejects an upstream call.
var ErrCircuitOpen = errors.New("upstream gRPC circuit breaker open")

// BreakerState represents the finite state of an AdaptiveCircuitBreaker.
type BreakerState int32

const (
	StateClosed BreakerState = iota
	StateOpen
	StateHalfOpen
)

func (s BreakerState) String() string {
	switch s {
	case StateClosed:
		return "CLOSED"
	case StateOpen:
		return "OPEN"
	case StateHalfOpen:
		return "HALF_OPEN"
	default:
		return "UNKNOWN"
	}
}

type rollingBucket struct {
	epochID   atomic.Int64
	requests  atomic.Uint64
	failures  atomic.Uint64
	successes atomic.Uint64
	_         [32]byte
}

// BreakerConfig configures the real-time error-ratio AdaptiveCircuitBreaker.
// All ratios use integer parts-per-million (ppm, scale 1_000_000) — zero floats.
type BreakerConfig struct {
	Name           string
	NumBuckets     int
	BucketDuration time.Duration
	MinRequests    uint64
	ErrorTripPPM   int64 // e.g., 500_000 = 50% error ratio
	OpenCooldown   time.Duration
	HalfOpenProbes uint32
	Metrics        *telemetry.Registry
}

// AdaptiveCircuitBreaker combines a lock-free rolling error-ratio window (Closed -> Open -> Half-Open)
// with Google SRE client-side adaptive probabilistic shedding before complete collapse.
type AdaptiveCircuitBreaker struct {
	name             string
	buckets          []rollingBucket
	bucketDurNS      int64
	minRequests      uint64
	errorTripPPM     int64
	cooldownNS       int64
	halfOpenProbes   uint32
	state            atomic.Int32
	openedAtNS       atomic.Int64
	halfOpenInFlight atomic.Uint32
	halfOpenSuccess  atomic.Uint32
	adaptiveSeq      atomic.Uint64
	metrics          *telemetry.Registry
	nowFunc          func() time.Time
}

// NewAdaptiveCircuitBreaker constructs a lock-free rolling-window circuit breaker.
func NewAdaptiveCircuitBreaker(cfg BreakerConfig) *AdaptiveCircuitBreaker {
	if cfg.NumBuckets <= 0 {
		cfg.NumBuckets = 10
	}
	if cfg.BucketDuration <= 0 {
		cfg.BucketDuration = time.Second
	}
	if cfg.MinRequests == 0 {
		cfg.MinRequests = 20
	}
	if cfg.ErrorTripPPM <= 0 {
		cfg.ErrorTripPPM = 500_000
	}
	if cfg.OpenCooldown <= 0 {
		cfg.OpenCooldown = 2 * time.Second
	}
	if cfg.HalfOpenProbes == 0 {
		cfg.HalfOpenProbes = 5
	}
	if cfg.Metrics == nil {
		cfg.Metrics = telemetry.Global
	}
	return &AdaptiveCircuitBreaker{
		name:           cfg.Name,
		buckets:        make([]rollingBucket, cfg.NumBuckets),
		bucketDurNS:    cfg.BucketDuration.Nanoseconds(),
		minRequests:    cfg.MinRequests,
		errorTripPPM:   cfg.ErrorTripPPM,
		cooldownNS:     cfg.OpenCooldown.Nanoseconds(),
		halfOpenProbes: cfg.HalfOpenProbes,
		metrics:        cfg.Metrics,
		nowFunc:        time.Now,
	}
}

// SetClock overrides wall clock for deterministic unit/torture tests.
func (cb *AdaptiveCircuitBreaker) SetClock(fn func() time.Time) {
	if fn != nil {
		cb.nowFunc = fn
	}
}

// State returns the current breaker state.
func (cb *AdaptiveCircuitBreaker) State() BreakerState {
	return BreakerState(cb.state.Load())
}

// ErrorRatioPPM returns the real-time rolling error ratio in integer parts-per-million [0, 1_000_000].
func (cb *AdaptiveCircuitBreaker) ErrorRatioPPM() (errorPPM int64, totalReqs uint64) {
	nowNS := cb.nowFunc().UnixNano()
	reqs, fails, _ := cb.windowTotals(nowNS)
	if reqs == 0 {
		return 0, 0
	}
	return int64((fails * 1_000_000) / reqs), reqs
}

func (cb *AdaptiveCircuitBreaker) currentBucket(nowNS int64) *rollingBucket {
	epoch := nowNS / cb.bucketDurNS
	idx := int(uint64(epoch) % uint64(len(cb.buckets)))
	b := &cb.buckets[idx]
	if old := b.epochID.Load(); old != epoch {
		if b.epochID.CompareAndSwap(old, epoch) {
			b.requests.Store(0)
			b.failures.Store(0)
			b.successes.Store(0)
		}
	}
	return b
}

func (cb *AdaptiveCircuitBreaker) windowTotals(nowNS int64) (reqs, fails, succs uint64) {
	curEpoch := nowNS / cb.bucketDurNS
	minEpoch := curEpoch - int64(len(cb.buckets)) + 1
	for i := range cb.buckets {
		b := &cb.buckets[i]
		if b.epochID.Load() >= minEpoch {
			reqs += b.requests.Load()
			fails += b.failures.Load()
			succs += b.successes.Load()
		}
	}
	return reqs, fails, succs
}

func (cb *AdaptiveCircuitBreaker) resetWindow() {
	for i := range cb.buckets {
		cb.buckets[i].epochID.Store(-1)
		cb.buckets[i].requests.Store(0)
		cb.buckets[i].failures.Store(0)
		cb.buckets[i].successes.Store(0)
	}
}

// Allow checks whether an outgoing RPC is permitted. Returns (releaseProbeFlag, error).
func (cb *AdaptiveCircuitBreaker) Allow() (bool, error) {
	nowNS := cb.nowFunc().UnixNano()
	st := BreakerState(cb.state.Load())

	switch st {
	case StateOpen:
		if nowNS-cb.openedAtNS.Load() < cb.cooldownNS {
			return false, ErrCircuitOpen
		}
		if cb.state.CompareAndSwap(int32(StateOpen), int32(StateHalfOpen)) {
			cb.halfOpenInFlight.Store(0)
			cb.halfOpenSuccess.Store(0)
		}
		return cb.allowHalfOpen()

	case StateHalfOpen:
		return cb.allowHalfOpen()

	default: // StateClosed
		// Adaptive SRE pre-trip load shedding when error ratio approaches trip threshold.
		reqs, fails, _ := cb.windowTotals(nowNS)
		if reqs >= cb.minRequests {
			errPPM := int64((fails * 1_000_000) / reqs)
			if errPPM >= cb.errorTripPPM {
				cb.tripOpen(nowNS)
				return false, ErrCircuitOpen
			}
			softKneePPM := (cb.errorTripPPM * 80) / 100
			if softKneePPM > 0 && errPPM > softKneePPM {
				// Linear shed probability between softKneePPM and errorTripPPM in integer ppm.
				span := cb.errorTripPPM - softKneePPM
				dropPPM := ((errPPM - softKneePPM) * 500_000) / span
				slot := int64((cb.adaptiveSeq.Add(1) * 0x9e3779b97f4a7c15) % 1_000_000)
				if slot < dropPPM {
					return false, ErrCircuitOpen
				}
			}
		}
		return false, nil
	}
}

func (cb *AdaptiveCircuitBreaker) allowHalfOpen() (bool, error) {
	for {
		cur := cb.halfOpenInFlight.Load()
		if cur >= cb.halfOpenProbes {
			return false, ErrCircuitOpen
		}
		if cb.halfOpenInFlight.CompareAndSwap(cur, cur+1) {
			return true, nil
		}
	}
}

// Record reports the outcome of a permitted RPC call and drives state transitions.
func (cb *AdaptiveCircuitBreaker) Record(isHalfOpenProbe bool, success bool) {
	nowNS := cb.nowFunc().UnixNano()

	if isHalfOpenProbe {
		if success {
			succ := cb.halfOpenSuccess.Add(1)
			if succ >= cb.halfOpenProbes {
				if cb.state.CompareAndSwap(int32(StateHalfOpen), int32(StateClosed)) {
					cb.resetWindow()
				}
			}
		} else {
			cb.tripOpenFromAny(nowNS)
		}
		return
	}

	b := cb.currentBucket(nowNS)
	b.requests.Add(1)
	if success {
		b.successes.Add(1)
		return
	}
	b.failures.Add(1)

	reqs, fails, _ := cb.windowTotals(nowNS)
	if reqs >= cb.minRequests {
		errPPM := int64((fails * 1_000_000) / reqs)
		if errPPM >= cb.errorTripPPM {
			cb.tripOpen(nowNS)
		}
	}
}

func (cb *AdaptiveCircuitBreaker) tripOpen(nowNS int64) {
	if cb.state.CompareAndSwap(int32(StateClosed), int32(StateOpen)) {
		cb.openedAtNS.Store(nowNS)
		cb.metrics.CircuitBreakerTripsTotal.Add(1)
	}
}

func (cb *AdaptiveCircuitBreaker) tripOpenFromAny(nowNS int64) {
	prev := BreakerState(cb.state.Swap(int32(StateOpen)))
	cb.openedAtNS.Store(nowNS)
	if prev != StateOpen {
		cb.metrics.CircuitBreakerTripsTotal.Add(1)
	}
}
