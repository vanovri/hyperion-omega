package agents_test

import (
	"math"
	"testing"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

func TestStrategiesPathologicalInputsAndSolvencyInvariants(t *testing.T) {
	params := agents.DefaultStrategyParams()
	params.HawkesAlpha = math.NaN()
	params.HawkesBeta = 0.0
	params.BurstMultiplier = math.Inf(1)

	mm := agents.NewBotAccount(12345, 1, "mm", model.ArchetypeAvellanedaStoikov, 50_000*fixed.Scale, 2*fixed.Scale)
	mm.RiskAversion = math.NaN()
	mm.OrderIntensityK = -10.0

	mom := agents.NewBotAccount(12345, 2, "mom", model.ArchetypeMomentumQuant, 50_000*fixed.Scale, 2*fixed.Scale)
	mr := agents.NewBotAccount(12345, 3, "mr", model.ArchetypeMeanRevQuant, 50_000*fixed.Scale, 2*fixed.Scale)
	noise := agents.NewBotAccount(12345, 4, "noise", model.ArchetypeHawkesNoise, 100, 50) // Near-zero dust balance!

	// 1. Nil / zero market snapshots must never panic or emit invalid orders
	var out []model.OrderIntent
	out = agents.StepAvellanedaStoikov(mm, nil, 1_000, params, out)
	out = agents.StepQuant(mom, &model.MarketSnapshot{}, 1_000, params, out)
	out = agents.StepHawkesNoise(noise, &model.MarketSnapshot{MidPrice: -5}, 1_000, math.NaN(), params, out)
	if len(out) != 0 {
		t.Fatalf("expected 0 orders on degenerate snapshots, got %d", len(out))
	}

	// 2. Valid extreme volatility snapshot with 0 depth (tests OBI divide-by-zero protection)
	spec := model.LookupInstrument("BTC-USDT")
	snap := &model.MarketSnapshot{
		Symbol:         spec.Symbol,
		MidPrice:       spec.DefaultMidFP,
		BestBid:        spec.DefaultMidFP - 10*spec.TickSizeFP,
		BestAsk:        spec.DefaultMidFP + 10*spec.TickSizeFP,
		BidDepth5:      0,
		AskDepth5:      0,
		VWAP:           0,
		ShortEMA:       spec.DefaultMidFP + 500*spec.TickSizeFP,
		LongEMA:        spec.DefaultMidFP - 500*spec.TickSizeFP,
		RealizedVolBps: 99_999,
		TickSize:       spec.TickSizeFP,
		LotSize:        spec.LotSizeFP,
		Regime:         model.RegimeFlashCrash,
	}

	for step := 0; step < 200; step++ {
		out = out[:0]
		out = agents.StepAvellanedaStoikov(mm, snap, int64(step)*10_000_000, agents.DefaultStrategyParams(), out)
		out = agents.StepQuant(mom, snap, int64(step)*10_000_000, agents.DefaultStrategyParams(), out)
		out = agents.StepQuant(mr, snap, int64(step)*10_000_000, agents.DefaultStrategyParams(), out)
		out = agents.StepHawkesNoise(noise, snap, int64(step)*10_000_000, 0.01, agents.DefaultStrategyParams(), out)

		for _, it := range out {
			if it.Kind == model.ActionSubmit {
				if it.Price <= 0 || it.Price%spec.TickSizeFP != 0 {
					t.Fatalf("invalid price %d on archetype %s", it.Price, it.Archetype)
				}
				if it.Quantity < spec.LotSizeFP || it.Quantity%spec.LotSizeFP != 0 {
					t.Fatalf("invalid quantity %d on archetype %s", it.Quantity, it.Archetype)
				}
				if it.AccountID == noise.AccountID {
					t.Fatalf("dust-balance bot emitted insolvent order: %+v", it)
				}
			}
		}
	}

	// 3. Verify Syndicate Wash Ring (4 accounts), Layering Spoofer (3-phase), and Flash Sweep
	ring := make([]*agents.BotAccount, 4)
	for i := 0; i < 4; i++ {
		ring[i] = agents.NewBotAccount(99, uint32(10+i), "wash", model.ArchetypeSyndicateWash, 500_000*fixed.Scale, 10*fixed.Scale)
	}
	var washIntents []model.OrderIntent
	for leg := uint64(0); leg < 4; leg++ {
		washIntents = agents.StepCircularWashRing(ring, leg, snap, 1_000_000, 5, 15, washIntents)
	}
	if len(washIntents) != 8 {
		t.Fatalf("expected 8 wash intents for full 4-account cycle, got %d", len(washIntents))
	}

	spoofer := agents.NewBotAccount(99, 50, "spoof", model.ArchetypeSyndicateSpoofer, 500_000*fixed.Scale, 10*fixed.Scale)
	var spoofOut []model.OrderIntent
	spoofOut = agents.StepLayeringSpoofer(spoofer, snap, 2_000_000, 10, 15, spoofOut) // Phase 0: 5 layers
	if len(spoofOut) != 5 {
		t.Fatalf("expected 5 spoof layers, got %d", len(spoofOut))
	}
	for _, it := range spoofOut {
		spoofer.ReserveOrder(it, 15)
	}
	spoofOut = spoofOut[:0]
	spoofOut = agents.StepLayeringSpoofer(spoofer, snap, 3_000_000, 10, 15, spoofOut) // Phase 1: opposite IOC buy
	if len(spoofOut) != 1 || spoofOut[0].Side != model.SideBuy {
		t.Fatalf("expected 1 opposite-side buy order in phase 1, got %+v", spoofOut)
	}
	spoofOut = spoofOut[:0]
	spoofOut = agents.StepLayeringSpoofer(spoofer, snap, 4_000_000, 10, 15, spoofOut) // Phase 2: 5 cancels
	if len(spoofOut) != 5 || spoofOut[0].Kind != model.ActionCancel {
		t.Fatalf("expected 5 cancel intents in phase 2, got %d", len(spoofOut))
	}
}
