package feebump

import (
	"errors"
	"fmt"
)

const (
	// BpsDenominator = 10,000 (100.00%).
	BpsDenominator	uint64	= 10_000
	// MinMempoolReplacementBumpBps = 1,250 (+12.50%), the strict Ethereum/Arbitrum/OP-Stack mempool floor.
	MinMempoolReplacementBumpBps	uint64	= 1_250
	// DefaultReplacementBumpBps = 1,500 (+15.00%) institutional safety margin above 12.5%.
	DefaultReplacementBumpBps	uint64	= 1_500
)

var (
	ErrInvalidFeePolicy		= errors.New("feebump: invalid EIP-1559 fee policy")
	ErrFeeCeilingExceeded		= errors.New("feebump: required EIP-1559 maxFeePerGas exceeds configured ceiling")
	ErrMaxBumpAttemptsReached	= errors.New("feebump: maximum replacement fee-bump attempts reached")
	ErrArithmeticOverflow		= errors.New("feebump: uint64 fee arithmetic overflow")
)

// Policy defines deterministic integer-only EIP-1559 gas pricing & dynamic replacement rules.
type Policy struct {
	MinPriorityFeeWei	uint64
	MaxPriorityFeeCapWei	uint64
	MaxFeeHardCeilingWei	uint64
	ReplacementBumpBps	uint64
	MaxBumpAttempts		uint32
}

// DefaultPolicy returns institutional L2 defaults (wei units, zero floating-point).
func DefaultPolicy() Policy {
	return Policy{
		MinPriorityFeeWei:	100_000_000,		// 0.10 gwei
		MaxPriorityFeeCapWei:	25_000_000_000,		// 25 gwei
		MaxFeeHardCeilingWei:	500_000_000_000,	// 500 gwei hard safety ceiling
		ReplacementBumpBps:	DefaultReplacementBumpBps,
		MaxBumpAttempts:	8,
	}
}

func (p Policy) Validate() error {
	if p.MinPriorityFeeWei == 0 || p.MaxPriorityFeeCapWei < p.MinPriorityFeeWei {
		return fmt.Errorf("%w: priority bounds [%d, %d]", ErrInvalidFeePolicy, p.MinPriorityFeeWei, p.MaxPriorityFeeCapWei)
	}
	if p.MaxFeeHardCeilingWei < p.MaxPriorityFeeCapWei {
		return fmt.Errorf("%w: ceiling %d < maxPriority %d", ErrInvalidFeePolicy, p.MaxFeeHardCeilingWei, p.MaxPriorityFeeCapWei)
	}
	if p.ReplacementBumpBps < MinMempoolReplacementBumpBps {
		return fmt.Errorf("%w: bump %d bps < mempool minimum %d bps", ErrInvalidFeePolicy, p.ReplacementBumpBps, MinMempoolReplacementBumpBps)
	}
	if p.MaxBumpAttempts == 0 {
		return fmt.Errorf("%w: maxBumpAttempts must be > 0", ErrInvalidFeePolicy)
	}
	return nil
}

// Quote represents an immutable EIP-1559 (Type-2) fee tuple in exact wei.
type Quote struct {
	MaxFeePerGasWei		uint64
	MaxPriorityFeePerGasWei	uint64
	Attempt			uint32
}

// InitialQuote computes the initial EIP-1559 fee envelope:
//
//	tip = clamp(networkSuggestedTipWei, MinPriorityFeeWei, MaxPriorityFeeCapWei)
//	maxFee = 2 * latestBaseFeeWei + tip
func InitialQuote(policy Policy, latestBaseFeeWei uint64, networkSuggestedTipWei uint64) (Quote, error) {
	if err := policy.Validate(); err != nil {
		return Quote{}, err
	}
	tip := clampU64(networkSuggestedTipWei, policy.MinPriorityFeeWei, policy.MaxPriorityFeeCapWei)

	if latestBaseFeeWei > (^uint64(0)-tip)/2 {
		return Quote{}, ErrArithmeticOverflow
	}
	maxFee := (2 * latestBaseFeeWei) + tip
	if maxFee > policy.MaxFeeHardCeilingWei {
		return Quote{}, fmt.Errorf("%w: computed=%d ceiling=%d", ErrFeeCeilingExceeded, maxFee, policy.MaxFeeHardCeilingWei)
	}

	return Quote{
		MaxFeePerGasWei:		maxFee,
		MaxPriorityFeePerGasWei:	tip,
		Attempt:			0,
	}, nil
}

// BumpQuote computes a collision-free EIP-1559 replacement transaction fee tuple for the SAME nonce,
// guaranteeing strict >=(100% + ReplacementBumpBps) escalation on BOTH maxPriorityFeePerGas and maxFeePerGas
// while also covering `2 * latestBaseFeeWei + nextTip`.
func BumpQuote(policy Policy, prev Quote, latestBaseFeeWei uint64, networkSuggestedTipWei uint64) (Quote, error) {
	if err := policy.Validate(); err != nil {
		return Quote{}, err
	}
	nextAttempt := prev.Attempt + 1
	if nextAttempt > policy.MaxBumpAttempts {
		return Quote{}, fmt.Errorf("%w: attempt=%d max=%d", ErrMaxBumpAttemptsReached, nextAttempt, policy.MaxBumpAttempts)
	}

	factorBps := BpsDenominator + policy.ReplacementBumpBps

	bumpedTip, err := mulDivCeilU64(prev.MaxPriorityFeePerGasWei, factorBps, BpsDenominator)
	if err != nil {
		return Quote{}, err
	}
	if bumpedTip > policy.MaxPriorityFeeCapWei {
		return Quote{}, fmt.Errorf("%w: bumped tip %d > priority cap %d", ErrFeeCeilingExceeded, bumpedTip, policy.MaxPriorityFeeCapWei)
	}

	marketTip := clampU64(networkSuggestedTipWei, policy.MinPriorityFeeWei, policy.MaxPriorityFeeCapWei)
	nextTip := maxU64(bumpedTip, marketTip)

	bumpedMaxFee, err := mulDivCeilU64(prev.MaxFeePerGasWei, factorBps, BpsDenominator)
	if err != nil {
		return Quote{}, err
	}

	if latestBaseFeeWei > (^uint64(0)-nextTip)/2 {
		return Quote{}, ErrArithmeticOverflow
	}
	marketMaxFee := (2 * latestBaseFeeWei) + nextTip
	nextMaxFee := maxU64(bumpedMaxFee, marketMaxFee)

	if nextMaxFee > policy.MaxFeeHardCeilingWei {
		return Quote{}, fmt.Errorf("%w: nextMaxFee=%d ceiling=%d", ErrFeeCeilingExceeded, nextMaxFee, policy.MaxFeeHardCeilingWei)
	}

	return Quote{
		MaxFeePerGasWei:		nextMaxFee,
		MaxPriorityFeePerGasWei:	nextTip,
		Attempt:			nextAttempt,
	}, nil
}

func mulDivCeilU64(a, b, denom uint64) (uint64, error) {
	if a == 0 || b == 0 {
		return 0, nil
	}
	if a > ^uint64(0)/b {
		return 0, ErrArithmeticOverflow
	}
	prod := a * b
	q := prod / denom
	if prod%denom != 0 {
		q++
	}
	return q, nil
}

func clampU64(v, lo, hi uint64) uint64 {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

func maxU64(a, b uint64) uint64 {
	if a >= b {
		return a
	}
	return b
}
