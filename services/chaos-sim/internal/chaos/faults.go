package chaos

import (
	"sync/atomic"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
)

// CampaignProfile defines all active deterministic chaos & fault-injection knobs.
// Read lock-free by worker goroutines via atomic.Pointer[CampaignProfile].
type CampaignProfile struct {
	Name                   string             `json:"name"`
	Regime                 model.MarketRegime `json:"regime"`
	BurstMultiplier        float64            `json:"burst_multiplier"`          // e.g. 20.0 for 20x traffic surge
	ReorderProb            float64            `json:"reorder_prob"`              // [0, 1] probability of out-of-order packet swap
	ReorderWindow          int                `json:"reorder_window"`            // Window size (2..32)
	DuplicateProb          float64            `json:"duplicate_prob"`            // [0, 1] idempotent duplicate injection rate
	DuplicateCopies        uint8              `json:"duplicate_copies"`          // Total transmissions when duplicated (2..4)
	ClockSkewProb          float64            `json:"clock_skew_prob"`           // [0, 1] expired/future timestamp injection rate
	ClockSkewAmplitudeNs   int64              `json:"clock_skew_amplitude_ns"`   // e.g. 120_000_000_000 (120s)
	CorruptSignatureProb   float64            `json:"corrupt_signature_prob"`    // [0, 1] invalid/replayed signature injection rate
	NetworkPartitionActive bool               `json:"network_partition_active"`  // Simulates inter-service partition
	SlowLorisConns         int                `json:"slow_loris_conns"`          // Active slow WebSocket consumers
	SlowLorisStallMs       int                `json:"slow_loris_stall_ms"`       // Per-read stall in ms
	WashRingActive         bool               `json:"wash_ring_active"`          // Circular wash-trading syndicate toggle
	WashRingSize           int                `json:"wash_ring_size"`            // 3..5 accounts
	SpooferActive          bool               `json:"spoofer_active"`            // Layering/spoofing syndicate toggle
	FlashSweepActive       bool               `json:"flash_sweep_active"`        // Aggressive multi-level liquidity sweep toggle
}

func DefaultCalmProfile() CampaignProfile {
	return CampaignProfile{
		Name:                 "calm-baseline",
		Regime:               model.RegimeCalm,
		BurstMultiplier:      1.0,
		ReorderWindow:        8,
		DuplicateCopies:      2,
		ClockSkewAmplitudeNs: 120_000_000_000, // 120s
		SlowLorisStallMs:     250,
		WashRingSize:         4,
	}
}

// PresetForRegime returns a canonical pre-configured CampaignProfile for live UI/E2E toggling.
func PresetForRegime(regime model.MarketRegime) CampaignProfile {
	p := DefaultCalmProfile()
	p.Regime = regime
	switch regime {
	case model.RegimeCalm:
		p.Name = "calm-baseline"
	case model.RegimeHighVolatility:
		p.Name = "high-volatility"
		p.BurstMultiplier = 3.0
		p.ReorderProb = 0.05
		p.DuplicateProb = 0.02
	case model.RegimeFlashCrash:
		p.Name = "flash-crash-cascade"
		p.BurstMultiplier = 6.0
		p.FlashSweepActive = true
		p.ReorderProb = 0.10
		p.DuplicateProb = 0.05
	case model.RegimeSpoofingAttack:
		p.Name = "syndicate-spoofing-and-wash"
		p.BurstMultiplier = 2.0
		p.SpooferActive = true
		p.WashRingActive = true
		p.WashRingSize = 4
	case model.RegimeWashTrading:
		p.Name = "circular-wash-ring"
		p.WashRingActive = true
		p.WashRingSize = 5
	case model.RegimeBurstTsunami:
		p.Name = "burst-tsunami-20x"
		p.BurstMultiplier = 20.0
		p.ReorderProb = 0.15
		p.DuplicateProb = 0.10
		p.ClockSkewProb = 0.03
	case model.RegimeNetworkPartition:
		p.Name = "network-partition-chaos"
		p.NetworkPartitionActive = true
		p.ReorderProb = 0.25
		p.DuplicateProb = 0.15
		p.ClockSkewProb = 0.05
	}
	return p
}

// LocalFaultCounters holds non-atomic shard-private fault counters to eliminate
// cross-core CPU cache-line bouncing (false sharing) on high-throughput parallel workers.
type LocalFaultCounters struct {
	ReorderedPackets    uint64
	DuplicatedRequests  uint64
	ClockSkewedRequests uint64
	CorruptedSignatures uint64
}

// FaultCounters tracks exact atomic counts of injected chaos faults for telemetry & E2E proofs.
// Each counter is cache-line padded to prevent false sharing across concurrent callers.
type FaultCounters struct {
	ReorderedPackets    atomic.Uint64
	_pad0               [56]byte
	DuplicatedRequests  atomic.Uint64
	_pad1               [56]byte
	ClockSkewedRequests atomic.Uint64
	_pad2               [56]byte
	CorruptedSignatures atomic.Uint64
	_pad3               [56]byte
	PartitionDrops      atomic.Uint64
	_pad4               [56]byte
}

type FaultStatsSnapshot struct {
	ReorderedPackets    uint64 `json:"reordered_packets"`
	DuplicatedRequests  uint64 `json:"duplicated_requests"`
	ClockSkewedRequests uint64 `json:"clock_skewed_requests"`
	CorruptedSignatures uint64 `json:"corrupted_signatures"`
	PartitionDrops      uint64 `json:"partition_drops"`
}

func (c *FaultCounters) Snapshot() FaultStatsSnapshot {
	return FaultStatsSnapshot{
		ReorderedPackets:    c.ReorderedPackets.Load(),
		DuplicatedRequests:  c.DuplicatedRequests.Load(),
		ClockSkewedRequests: c.ClockSkewedRequests.Load(),
		CorruptedSignatures: c.CorruptedSignatures.Load(),
		PartitionDrops:      c.PartitionDrops.Load(),
	}
}

// InjectBatchFaults mutates a batch of OrderIntents in-place using a deterministic PRNG stream
// to inject packet reordering, idempotent request duplication, clock skew, and signature corruption.
func InjectBatchFaults(
	batch []model.OrderIntent,
	prof *CampaignProfile,
	stream *rng.Stream,
	counters *FaultCounters,
) {
	var local LocalFaultCounters
	InjectBatchFaultsLocal(batch, prof, stream, &local)
	if counters != nil {
		if local.ReorderedPackets > 0 {
			counters.ReorderedPackets.Add(local.ReorderedPackets)
		}
		if local.DuplicatedRequests > 0 {
			counters.DuplicatedRequests.Add(local.DuplicatedRequests)
		}
		if local.ClockSkewedRequests > 0 {
			counters.ClockSkewedRequests.Add(local.ClockSkewedRequests)
		}
		if local.CorruptedSignatures > 0 {
			counters.CorruptedSignatures.Add(local.CorruptedSignatures)
		}
	}
}

// InjectBatchFaultsLocal mutates batch in-place and accumulates fault counts into shard-private
// LocalFaultCounters with zero atomic RMW instructions and zero cross-core false sharing.
func InjectBatchFaultsLocal(
	batch []model.OrderIntent,
	prof *CampaignProfile,
	stream *rng.Stream,
	counters *LocalFaultCounters,
) {
	if prof == nil || len(batch) == 0 || stream == nil {
		return
	}

	dupProb := prof.DuplicateProb
	skewProb := prof.ClockSkewProb
	corruptProb := prof.CorruptSignatureProb
	reorderProb := prof.ReorderProb

	// Fast exit when calm regime has zero network faults configured
	if dupProb <= 0 && skewProb <= 0 && corruptProb <= 0 && reorderProb <= 0 {
		for i := range batch {
			if batch[i].DuplicateCount == 0 {
				batch[i].DuplicateCount = 1
			}
		}
		return
	}

	dupCopies := prof.DuplicateCopies
	if dupCopies < 2 {
		dupCopies = 2
	} else if dupCopies > 5 {
		dupCopies = 5
	}

	skewAmp := prof.ClockSkewAmplitudeNs
	if skewAmp <= 0 {
		skewAmp = 120_000_000_000
	}

	for i := range batch {
		if dupProb > 0 && stream.Float64() < dupProb {
			batch[i].DuplicateCount = dupCopies
			if counters != nil {
				counters.DuplicatedRequests++
			}
		} else if batch[i].DuplicateCount == 0 {
			batch[i].DuplicateCount = 1
		}

		if skewProb > 0 && stream.Float64() < skewProb {
			if stream.Uint64()&1 == 0 {
				batch[i].ClockSkewNs = -skewAmp
			} else {
				batch[i].ClockSkewNs = skewAmp
			}
			if counters != nil {
				counters.ClockSkewedRequests++
			}
		}

		if corruptProb > 0 && stream.Float64() < corruptProb {
			batch[i].CorruptSignature = true
			if counters != nil {
				counters.CorruptedSignatures++
			}
		}
	}

	// Deterministic bounded-window packet reordering
	if reorderProb > 0 && len(batch) >= 2 {
		win := prof.ReorderWindow
		if win < 2 {
			win = 4
		} else if win > 32 {
			win = 32
		}
		for i := 0; i < len(batch)-1; i++ {
			if stream.Float64() < reorderProb {
				maxSpan := win
				if i+maxSpan >= len(batch) {
					maxSpan = len(batch) - i - 1
				}
				if maxSpan >= 1 {
					j := i + 1 + int(stream.Int63n(int64(maxSpan)))
					batch[i], batch[j] = batch[j], batch[i]
					if counters != nil {
						counters.ReorderedPackets++
					}
				}
			}
		}
	}
}
