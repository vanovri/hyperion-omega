package agents

import (
	"math"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

// StrategyParams configures global baseline microstructure parameters for the agent population.
type StrategyParams struct {
	MaxFeeBps          int64
	BaseMakerLots      int64 // In multiples of LotSize (e.g., 5 lots)
	BaseQuantLots      int64
	BaseNoiseLots      int64
	HawkesBaseMu       float64 // Baseline arrivals/sec per noise bot
	HawkesAlpha        float64 // Self-excitation jump
	HawkesBeta         float64 // Exponential decay rate (enforced > HawkesAlpha / 0.85)
	BurstMultiplier    float64 // 1.0 normal, 20.0 during 20x burst tsunami
}

func DefaultStrategyParams() StrategyParams {
	return StrategyParams{
		MaxFeeBps:       15, // 15 bps conservative solvency cushion
		BaseMakerLots:   6,
		BaseQuantLots:   4,
		BaseNoiseLots:   2,
		HawkesBaseMu:    2.5,
		HawkesAlpha:     4.0,
		HawkesBeta:      6.5,
		BurstMultiplier: 1.0,
	}
}

func regimeVolMultiplier(r model.MarketRegime) float64 {
	switch r {
	case model.RegimeHighVolatility:
		return 3.5
	case model.RegimeFlashCrash:
		return 7.0
	case model.RegimeSpoofingAttack:
		return 2.0
	default:
		return 1.0
	}
}

func regimeArrivalMultiplier(r model.MarketRegime, burstMult float64) float64 {
	m := 1.0
	switch r {
	case model.RegimeHighVolatility:
		m = 2.5
	case model.RegimeFlashCrash:
		m = 5.0
	case model.RegimeBurstTsunami:
		m = 20.0
	}
	if burstMult > m {
		m = burstMult
	}
	if math.IsNaN(m) || math.IsInf(m, 0) || m <= 0 {
		return 1.0
	}
	if m > 100.0 {
		return 100.0
	}
	return m
}

// StepAvellanedaStoikov computes dynamic two-sided quotes (plus stale quote cancellations)
// using the continuous-time Avellaneda-Stoikov (2008) inventory-risk market-making model.
// Appends up to 4 OrderIntents into out without heap allocations.
func StepAvellanedaStoikov(
	bot *BotAccount,
	snap *model.MarketSnapshot,
	nowNs int64,
	params StrategyParams,
	out []model.OrderIntent,
) []model.OrderIntent {
	if snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}

	mid := snap.MidPrice
	tick := snap.TickSize
	lot := snap.LotSize

	// 1. Normalized inventory q = (TotalBase - InitialBase) / invScale, clamped [-15, 15].
	invScale := bot.InitialBase / 10
	if invScale < lot {
		invScale = lot
	}
	q := float64(bot.TotalBase-bot.InitialBase) / float64(invScale)
	q = clampFloat(q, -15.0, 15.0)

	// 2. Guarded volatility sigma in fractional return units [0.0005, 0.50].
	volBps := snap.RealizedVolBps
	if volBps < 5 {
		volBps = 5
	} else if volBps > 5000 {
		volBps = 5000
	}
	sigma := clampFloat((float64(volBps)/10000.0)*regimeVolMultiplier(snap.Regime), 0.0005, 0.50)

	gamma := clampFloat(bot.RiskAversion, 0.02, 2.0)
	// Scale kappa to fractional price units [500, 8000] so 10 bps = 0.001 decays arrival exp(-k*d).
	kappaFrac := clampFloat(bot.OrderIntensityK*350.0, 400.0, 8000.0)
	tau := 0.50

	// Reservation fractional offset: dr = -q * gamma * sigma^2 * tau
	sigma2Tau := sigma * sigma * tau
	drFrac := clampFloat(-q*gamma*sigma2Tau, -0.15, 0.15)

	// Optimal total spread fraction: delta = gamma*sigma^2*tau + (2/gamma)*ln(1 + gamma/kappaFrac)
	logTerm := math.Log1p(gamma / kappaFrac)
	spreadFrac := clampFloat(gamma*sigma2Tau+(2.0/gamma)*logTerm, 0.0002, 0.20)
	halfSpreadFrac := 0.5 * spreadFrac

	// Convert fractional offsets into exact 1e8 integer price deltas via guarded quantization.
	bidRatioFixed := fixed.QuantizeSignalFloat(1.0+drFrac-halfSpreadFrac, fixed.Scale/2, fixed.Scale*2)
	askRatioFixed := fixed.QuantizeSignalFloat(1.0+drFrac+halfSpreadFrac, fixed.Scale/2, fixed.Scale*2)

	rawBid, err1 := fixed.MulDiv128(mid, bidRatioFixed, fixed.Scale)
	rawAsk, err2 := fixed.MulDiv128(mid, askRatioFixed, fixed.Scale)
	if err1 != nil || err2 != nil {
		return out
	}

	bidPrice := fixed.RoundToStep(rawBid, tick)
	askPrice := fixed.RoundUpToStep(rawAsk, tick)

	// Enforce strict non-crossing POST_ONLY safety against current top-of-book.
	if snap.BestAsk > tick && bidPrice >= snap.BestAsk {
		bidPrice = fixed.RoundToStep(snap.BestAsk-tick, tick)
	}
	if snap.BestBid > 0 && askPrice <= snap.BestBid {
		askPrice = fixed.RoundUpToStep(snap.BestBid+tick, tick)
	}
	if bidPrice <= 0 {
		bidPrice = tick
	}
	if askPrice <= bidPrice {
		askPrice = bidPrice + tick
	}

	// Cancel stale resting quotes whose price drifted > 3 ticks from target reservation quotes.
	staleDist := 3 * tick
	hasNearBid := false
	hasNearAsk := false
	instID := snap.InstrumentID
	if instID == 0 {
		instID = model.LookupInstrument(snap.Symbol).InstrumentID
	}
	for i := 0; i < MaxOpenSlots; i++ {
		slot := &bot.OpenOrders[i]
		if !slot.Active {
			continue
		}
		target := askPrice
		if slot.Side == model.SideBuy {
			target = bidPrice
		}
		if fixed.Abs(slot.Price-target) > staleDist || (nowNs-slot.PlacedAtNs) > 5_000_000_000 {
			_, idem, nonce := bot.NextClientIDs()
			out = append(out, model.OrderIntent{
				ClientOrderID:  slot.ClientOrderID,
				TargetOrderID:  slot.ClientOrderID,
				TargetServerID: slot.ServerOrderID,
				IdempotencyKey: idem,
				AccountNum:     bot.AccountNum,
				InstrumentID:   instID,
				BotID:          bot.BotID,
				Kind:           model.ActionCancel,
				Side:           slot.Side,
				AccountID:      bot.AccountID,
				Symbol:         snap.Symbol,
				Archetype:      bot.Archetype,
				TimestampNs:    nowNs,
				Nonce:          nonce,
			})
		} else {
			if slot.Side == model.SideBuy {
				hasNearBid = true
			} else {
				hasNearAsk = true
			}
		}
	}

	// Inventory-skewed quote size: reduce buy size when long, reduce sell size when short.
	baseLots := params.BaseMakerLots
	if baseLots < 1 {
		baseLots = 4
	}
	buySkew := clampFloat(1.0-0.05*q, 0.25, 1.75)
	sellSkew := clampFloat(1.0+0.05*q, 0.25, 1.75)

	if !hasNearBid && bot.HasFreeSlot() {
		desiredBidQty := fixed.RoundToStep(int64(math.Round(float64(baseLots*lot)*buySkew)), lot)
		bidQty := bot.ClampOrderToSolvency(model.SideBuy, model.OrderTypePostOnly, bidPrice, snap.BestAsk, desiredBidQty, lot, params.MaxFeeBps)
		if bidQty >= lot {
			cid, idem, nonce := bot.NextClientIDs()
			out = append(out, model.OrderIntent{
				ClientOrderID:  cid,
				IdempotencyKey: idem,
				AccountNum:     bot.AccountNum,
				InstrumentID:   instID,
				BotID:          bot.BotID,
				Kind:           model.ActionSubmit,
				Side:           model.SideBuy,
				OrderType:      model.OrderTypePostOnly,
				AccountID:      bot.AccountID,
				Symbol:         snap.Symbol,
				Archetype:      bot.Archetype,
				Price:          bidPrice,
				Quantity:       bidQty,
				TimestampNs:    nowNs,
				Nonce:          nonce,
			})
		}
	}

	if !hasNearAsk && bot.HasFreeSlot() {
		desiredAskQty := fixed.RoundToStep(int64(math.Round(float64(baseLots*lot)*sellSkew)), lot)
		askQty := bot.ClampOrderToSolvency(model.SideSell, model.OrderTypePostOnly, askPrice, snap.BestAsk, desiredAskQty, lot, params.MaxFeeBps)
		if askQty >= lot {
			cid, idem, nonce := bot.NextClientIDs()
			out = append(out, model.OrderIntent{
				ClientOrderID:  cid,
				IdempotencyKey: idem,
				AccountNum:     bot.AccountNum,
				InstrumentID:   instID,
				BotID:          bot.BotID,
				Kind:           model.ActionSubmit,
				Side:           model.SideSell,
				OrderType:      model.OrderTypePostOnly,
				AccountID:      bot.AccountID,
				Symbol:         snap.Symbol,
				Archetype:      bot.Archetype,
				Price:          askPrice,
				Quantity:       askQty,
				TimestampNs:    nowNs,
				Nonce:          nonce,
			})
		}
	}

	return out
}

// StepQuant evaluates Momentum or Mean-Reversion signals from rolling VWAP, Short/Long EMA,
// and L2 Order Book Imbalance (OBI). Zero NaN/divide-by-zero risk.
func StepQuant(
	bot *BotAccount,
	snap *model.MarketSnapshot,
	nowNs int64,
	params StrategyParams,
	out []model.OrderIntent,
) []model.OrderIntent {
	if snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}

	mid := snap.MidPrice
	vwap := snap.VWAP
	if vwap <= 0 {
		vwap = mid
	}
	shortEMA := snap.ShortEMA
	if shortEMA <= 0 {
		shortEMA = mid
	}
	longEMA := snap.LongEMA
	if longEMA <= 0 {
		longEMA = mid
	}

	// Order Book Imbalance in [-1, 1] with strict positive denominator guard.
	totalDepth := snap.BidDepth5 + snap.AskDepth5
	var obi float64
	if totalDepth > 0 {
		obi = clampFloat(float64(snap.BidDepth5-snap.AskDepth5)/float64(totalDepth), -1.0, 1.0)
	}

	vwapDev := clampFloat(float64(mid-vwap)/float64(vwap), -0.50, 0.50)
	emaTrend := clampFloat(float64(shortEMA-longEMA)/float64(longEMA), -0.50, 0.50)
	thresh := clampFloat(bot.SignalThreshold, 0.0003, 0.05)

	var side model.Side
	var conviction float64

	if bot.Archetype == model.ArchetypeMomentumQuant {
		// Momentum signal combines EMA breakout + Order Book Imbalance pressure.
		score := 0.65*emaTrend + 0.35*(obi*0.004)
		if snap.Regime == model.RegimeFlashCrash {
			score -= 0.006 // Momentum cascades during flash crash
		}
		if score > thresh {
			side = model.SideBuy
			conviction = clampFloat(score/thresh, 1.0, 5.0)
		} else if score < -thresh {
			side = model.SideSell
			conviction = clampFloat((-score)/thresh, 1.0, 5.0)
		}
	} else {
		// Mean-Reversion Quant fades overextensions away from rolling VWAP when book pressure Exhausts.
		revThresh := thresh * 1.5
		if vwapDev > revThresh && obi < 0.35 {
			side = model.SideSell
			conviction = clampFloat(vwapDev/revThresh, 1.0, 4.0)
		} else if vwapDev < -revThresh && obi > -0.35 {
			side = model.SideBuy
			conviction = clampFloat((-vwapDev)/revThresh, 1.0, 4.0)
		}
	}

	if side == model.SideUnspecified {
		return out
	}

	// Probabilistic participation gate so 300 quants stagger naturally instead of lockstep herding.
	if bot.RNG.Float64() > 0.35 {
		return out
	}

	lot := snap.LotSize
	tick := snap.TickSize
	baseLots := params.BaseQuantLots
	if baseLots < 1 {
		baseLots = 3
	}
	desiredQty := fixed.RoundToStep(int64(math.Round(float64(baseLots*lot)*conviction)), lot)

	var price int64
	if side == model.SideBuy {
		ask := snap.BestAsk
		if ask <= 0 {
			ask = mid + tick
		}
		price = fixed.RoundUpToStep(ask+tick, tick)
	} else {
		bid := snap.BestBid
		if bid <= tick {
			bid = mid
		}
		price = fixed.RoundToStep(bid-tick, tick)
		if price <= 0 {
			price = tick
		}
	}

	qty := bot.ClampOrderToSolvency(side, model.OrderTypeIOC, price, snap.BestAsk, desiredQty, lot, params.MaxFeeBps)
	if qty < lot {
		return out
	}

	instID := snap.InstrumentID
	if instID == 0 {
		instID = model.LookupInstrument(snap.Symbol).InstrumentID
	}
	cid, idem, nonce := bot.NextClientIDs()
	out = append(out, model.OrderIntent{
		ClientOrderID:  cid,
		IdempotencyKey: idem,
		AccountNum:     bot.AccountNum,
		InstrumentID:   instID,
		BotID:          bot.BotID,
		Kind:           model.ActionSubmit,
		Side:           side,
		OrderType:      model.OrderTypeIOC,
		AccountID:      bot.AccountID,
		Symbol:         snap.Symbol,
		Archetype:      bot.Archetype,
		Price:          price,
		Quantity:       qty,
		TimestampNs:    nowNs,
		Nonce:          nonce,
	})
	return out
}

// StepHawkesNoise advances an exact O(1) Markovian self-exciting Hawkes process
// lambda(t) = mu*M + A(t), dA = -beta*A*dt + alpha*dN(t) with stability invariant alpha/beta <= 0.85,
// producing clustered retail/noise order arrivals and natural OHLCV candlesticks.
func StepHawkesNoise(
	bot *BotAccount,
	snap *model.MarketSnapshot,
	nowNs int64,
	dtSec float64,
	params StrategyParams,
	out []model.OrderIntent,
) []model.OrderIntent {
	if snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}
	if math.IsNaN(dtSec) || dtSec <= 0 {
		dtSec = 0.010 // Default 10ms simulation step
	} else if dtSec > 1.0 {
		dtSec = 1.0
	}

	alpha := clampFloat(params.HawkesAlpha, 0.0, 50.0)
	beta := clampFloat(params.HawkesBeta, 0.5, 200.0)
	// Enforce stationary Hawkes branching ratio alpha/beta <= 0.80 < 1.0.
	if alpha > 0.80*beta {
		beta = alpha / 0.80
	}

	// Exponential decay of self-excitation memory A_k = exp(-beta * dt) * A_{k-1}
	decay := math.Exp(-clampFloat(beta*dtSec, 0.0, 50.0))
	bot.HawkesIntensity = clampFloat(bot.HawkesIntensity*decay, 0.0, 500.0)

	muEff := clampFloat(params.HawkesBaseMu, 0.05, 200.0) * regimeArrivalMultiplier(snap.Regime, params.BurstMultiplier)
	lambda := clampFloat(muEff+bot.HawkesIntensity, 0.05, 5000.0)

	// Arrival probability in step dtSec: p = 1 - exp(-lambda * dtSec)
	pArrive := -math.Expm1(-clampFloat(lambda*dtSec, 0.0, 20.0))
	if bot.RNG.Float64() >= pArrive {
		return out
	}

	// Self-excitation jump on arrival
	bot.HawkesIntensity = clampFloat(bot.HawkesIntensity+alpha, 0.0, 500.0)

	// Direction slightly biased by regime (flash crash tilts retail panic sell 75%)
	buyProb := 0.50
	if snap.Regime == model.RegimeFlashCrash {
		buyProb = 0.25
	}
	side := model.SideSell
	if bot.RNG.Float64() < buyProb {
		side = model.SideBuy
	}

	lot := snap.LotSize
	tick := snap.TickSize
	baseLots := float64(params.BaseNoiseLots)
	if baseLots < 1.0 {
		baseLots = 1.0
	}
	// Heavy-tailed Pareto retail order size capped at 25x base lots.
	paretoLots := bot.RNG.Pareto(baseLots, 1.85, baseLots*25.0)
	desiredQty := fixed.RoundToStep(int64(math.Round(paretoLots*float64(lot))), lot)

	// 65% resting/near-touch Limit orders (builds book depth & wicks), 35% crossing IOC orders (forms candles).
	uType := bot.RNG.Float64()
	var ordType model.OrderType
	var price int64
	offsetTicks := bot.RNG.Int63n(6) // 0..5 ticks

	instID := snap.InstrumentID
	if instID == 0 {
		instID = model.LookupInstrument(snap.Symbol).InstrumentID
	}
	if uType < 0.35 {
		ordType = model.OrderTypeIOC
		if side == model.SideBuy {
			ref := snap.BestAsk
			if ref <= 0 {
				ref = snap.MidPrice + tick
			}
			price = fixed.RoundUpToStep(ref+(offsetTicks+1)*tick, tick)
		} else {
			ref := snap.BestBid
			if ref <= tick {
				ref = snap.MidPrice
			}
			price = fixed.RoundToStep(ref-(offsetTicks+1)*tick, tick)
			if price <= 0 {
				price = tick
			}
		}
	} else {
		if !bot.HasFreeSlot() {
			// Cancel oldest open order first to keep retail order slots clean without leaking open orders.
			for i := 0; i < MaxOpenSlots; i++ {
				if bot.OpenOrders[i].Active {
					_, idem, nonce := bot.NextClientIDs()
					out = append(out, model.OrderIntent{
						ClientOrderID:  bot.OpenOrders[i].ClientOrderID,
						TargetOrderID:  bot.OpenOrders[i].ClientOrderID,
						TargetServerID: bot.OpenOrders[i].ServerOrderID,
						IdempotencyKey: idem,
						AccountNum:     bot.AccountNum,
						InstrumentID:   instID,
						BotID:          bot.BotID,
						Kind:           model.ActionCancel,
						Side:           bot.OpenOrders[i].Side,
						AccountID:      bot.AccountID,
						Symbol:         snap.Symbol,
						Archetype:      bot.Archetype,
						TimestampNs:    nowNs,
						Nonce:          nonce,
					})
					break
				}
			}
			return out
		}
		ordType = model.OrderTypeLimit
		if side == model.SideBuy {
			price = fixed.RoundToStep(snap.MidPrice-(offsetTicks+1)*tick, tick)
			if price <= 0 {
				price = tick
			}
		} else {
			price = fixed.RoundUpToStep(snap.MidPrice+(offsetTicks+1)*tick, tick)
		}
	}

	qty := bot.ClampOrderToSolvency(side, ordType, price, snap.BestAsk, desiredQty, lot, params.MaxFeeBps)
	if qty < lot {
		return out
	}

	cid, idem, nonce := bot.NextClientIDs()
	out = append(out, model.OrderIntent{
		ClientOrderID:  cid,
		IdempotencyKey: idem,
		AccountNum:     bot.AccountNum,
		InstrumentID:   instID,
		BotID:          bot.BotID,
		Kind:           model.ActionSubmit,
		Side:           side,
		OrderType:      ordType,
		AccountID:      bot.AccountID,
		Symbol:         snap.Symbol,
		Archetype:      bot.Archetype,
		Price:          price,
		Quantity:       qty,
		TimestampNs:    nowNs,
		Nonce:          nonce,
	})
	return out
}

// StepCircularWashRing generates a synchronized circular wash-trading cycle across 3 to 5 syndicate accounts:
// ring[0] -> ring[1] -> ... -> ring[K-1] -> ring[0] at matched mid-spread prices and identical quantities.
// Specifically designed to trigger Sentinel-ML temporal multigraph cycle detection (k <= 6).
func StepCircularWashRing(
	ring []*BotAccount,
	leg uint64,
	snap *model.MarketSnapshot,
	nowNs int64,
	washLots int64,
	maxFeeBps int64,
	out []model.OrderIntent,
) []model.OrderIntent {
	k := len(ring)
	if k < 3 || snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}
	if washLots < 1 {
		washLots = 8
	}

	seller := ring[int(leg%uint64(k))]
	buyer := ring[int((leg+1)%uint64(k))]

	tick := snap.TickSize
	lot := snap.LotSize

	// Choose exact midpoint inside spread so the two syndicate accounts match directly against each other.
	washPrice := fixed.RoundToStep(snap.MidPrice, tick)
	if snap.BestBid > 0 && snap.BestAsk > snap.BestBid+tick {
		washPrice = fixed.RoundToStep((snap.BestBid+snap.BestAsk)/2, tick)
		if washPrice <= snap.BestBid {
			washPrice = snap.BestBid + tick
		}
		if washPrice >= snap.BestAsk {
			washPrice = snap.BestAsk - tick
		}
	}
	if washPrice <= 0 {
		washPrice = tick
	}

	desiredQty := washLots * lot
	sellQty := seller.ClampOrderToSolvency(model.SideSell, model.OrderTypeLimit, washPrice, snap.BestAsk, desiredQty, lot, maxFeeBps)
	buyQty := buyer.ClampOrderToSolvency(model.SideBuy, model.OrderTypeIOC, washPrice, washPrice, desiredQty, lot, maxFeeBps)
	matchedQty := sellQty
	if buyQty < matchedQty {
		matchedQty = buyQty
	}
	matchedQty = fixed.RoundToStep(matchedQty, lot)
	if matchedQty < lot || !seller.HasFreeSlot() {
		return out
	}

	instID := model.LookupInstrument(snap.Symbol).InstrumentID
	sCID, sIdem, sNonce := seller.NextClientIDs()
	bCID, bIdem, bNonce := buyer.NextClientIDs()

	// Leg 1: Maker resting limit order from Seller
	out = append(out, model.OrderIntent{
		ClientOrderID:  sCID,
		IdempotencyKey: sIdem,
		AccountNum:     seller.AccountNum,
		InstrumentID:   instID,
		BotID:          seller.BotID,
		Kind:           model.ActionSubmit,
		Side:           model.SideSell,
		OrderType:      model.OrderTypeLimit,
		AccountID:      seller.AccountID,
		Symbol:         snap.Symbol,
		Archetype:      model.ArchetypeSyndicateWash,
		Price:          washPrice,
		Quantity:       matchedQty,
		TimestampNs:    nowNs,
		Nonce:          sNonce,
	})
	// Leg 2: Immediate Taker IOC cross from next ring member Buyer
	out = append(out, model.OrderIntent{
		ClientOrderID:  bCID,
		IdempotencyKey: bIdem,
		AccountNum:     buyer.AccountNum,
		InstrumentID:   instID,
		BotID:          buyer.BotID,
		Kind:           model.ActionSubmit,
		Side:           model.SideBuy,
		OrderType:      model.OrderTypeIOC,
		AccountID:      buyer.AccountID,
		Symbol:         snap.Symbol,
		Archetype:      model.ArchetypeSyndicateWash,
		Price:          washPrice,
		Quantity:       matchedQty,
		TimestampNs:    nowNs + 1_000, // +1 microsecond causal ordering
		Nonce:          bNonce,
	})
	return out
}

// StepLayeringSpoofer executes a 3-phase deceptive Layering/Spoofing cycle:
// Phase 0: Stack 5 large POST_ONLY deceptive sell layers near BestAsk to distort Order Book Imbalance.
// Phase 1: Execute aggressive IOC buy on the opposite side while book appears heavy on Ask.
// Phase 2: Rapidly pull (Cancel) all resting spoof layers within milliseconds (high OTR anomaly).
func StepLayeringSpoofer(
	bot *BotAccount,
	snap *model.MarketSnapshot,
	nowNs int64,
	spoofLayerLots int64,
	maxFeeBps int64,
	out []model.OrderIntent,
) []model.OrderIntent {
	if snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}
	tick := snap.TickSize
	lot := snap.LotSize
	if spoofLayerLots < 5 {
		spoofLayerLots = 20
	}

	instID := model.LookupInstrument(snap.Symbol).InstrumentID
	switch bot.SpoofPhase % 3 {
	case 0:
		// Place up to 5 stacked spoof layers on the Ask side (1..5 ticks above BestAsk)
		baseAsk := snap.BestAsk
		if baseAsk <= snap.BestBid {
			baseAsk = snap.MidPrice + tick
		}
		availBase := bot.AvailableBase()
		perLayerDesired := spoofLayerLots * lot
		if availBase/5 < perLayerDesired {
			perLayerDesired = fixed.RoundToStep(availBase/5, lot)
		}
		for lvl := int64(1); lvl <= 5; lvl++ {
			if !bot.HasFreeSlot() {
				break
			}
			px := fixed.RoundUpToStep(baseAsk+lvl*tick, tick)
			qty := bot.ClampOrderToSolvency(model.SideSell, model.OrderTypePostOnly, px, snap.BestAsk, perLayerDesired, lot, maxFeeBps)
			if qty < lot {
				break
			}
			cid, idem, nonce := bot.NextClientIDs()
			out = append(out, model.OrderIntent{
				ClientOrderID:  cid,
				IdempotencyKey: idem,
				AccountNum:     bot.AccountNum,
				InstrumentID:   instID,
				BotID:          bot.BotID,
				Kind:           model.ActionSubmit,
				Side:           model.SideSell,
				OrderType:      model.OrderTypePostOnly,
				AccountID:      bot.AccountID,
				Symbol:         snap.Symbol,
				Archetype:      model.ArchetypeSyndicateSpoofer,
				Price:          px,
				Quantity:       qty,
				TimestampNs:    nowNs + lvl*500,
				Nonce:          nonce,
			})
		}
		bot.SpoofPhase = 1

	case 1:
		// Opposite-side real execution (aggressive Buy IOC)
		ask := snap.BestAsk
		if ask <= 0 {
			ask = snap.MidPrice + tick
		}
		px := fixed.RoundUpToStep(ask+2*tick, tick)
		qty := bot.ClampOrderToSolvency(model.SideBuy, model.OrderTypeIOC, px, ask, 3*lot, lot, maxFeeBps)
		if qty >= lot {
			cid, idem, nonce := bot.NextClientIDs()
			out = append(out, model.OrderIntent{
				ClientOrderID:  cid,
				IdempotencyKey: idem,
				AccountNum:     bot.AccountNum,
				InstrumentID:   instID,
				BotID:          bot.BotID,
				Kind:           model.ActionSubmit,
				Side:           model.SideBuy,
				OrderType:      model.OrderTypeIOC,
				AccountID:      bot.AccountID,
				Symbol:         snap.Symbol,
				Archetype:      model.ArchetypeSyndicateSpoofer,
				Price:          px,
				Quantity:       qty,
				TimestampNs:    nowNs,
				Nonce:          nonce,
			})
		}
		bot.SpoofPhase = 2

	case 2:
		// Rapid mass cancel of all spoof layers
		for i := 0; i < MaxOpenSlots; i++ {
			if bot.OpenOrders[i].Active {
				_, idem, nonce := bot.NextClientIDs()
				out = append(out, model.OrderIntent{
					ClientOrderID:  bot.OpenOrders[i].ClientOrderID,
					TargetOrderID:  bot.OpenOrders[i].ClientOrderID,
					TargetServerID: bot.OpenOrders[i].ServerOrderID,
					IdempotencyKey: idem,
					AccountNum:     bot.AccountNum,
					InstrumentID:   instID,
					BotID:          bot.BotID,
					Kind:           model.ActionCancel,
					Side:           bot.OpenOrders[i].Side,
					AccountID:      bot.AccountID,
					Symbol:         snap.Symbol,
					Archetype:      model.ArchetypeSyndicateSpoofer,
					TimestampNs:    nowNs,
					Nonce:          nonce,
				})
			}
		}
		bot.SpoofPhase = 0
	}
	return out
}

// StepFlashCrashSweep fires an aggressive multi-level liquidity sweep slicing deep across Bids
// to trigger flash-crash dynamics, volatility circuit breakers, and Chronos slow-mo replay scenarios.
func StepFlashCrashSweep(
	sweepers []*BotAccount,
	snap *model.MarketSnapshot,
	nowNs int64,
	sweepLots int64,
	maxFeeBps int64,
	out []model.OrderIntent,
) []model.OrderIntent {
	if len(sweepers) == 0 || snap == nil || snap.MidPrice <= 0 || snap.TickSize <= 0 || snap.LotSize <= 0 {
		return out
	}
	tick := snap.TickSize
	lot := snap.LotSize
	if sweepLots < 10 {
		sweepLots = 40
	}

	// Price 5% below BestBid (deep sweep limit IOC) to wipe resting bid liquidity safely without market-order zero-price edge cases.
	floorPrice, err := fixed.MulDiv128(snap.MidPrice, 9_500, 10_000)
	if err != nil || floorPrice <= tick {
		floorPrice = tick
	}
	floorPrice = fixed.RoundToStep(floorPrice, tick)
	if floorPrice <= 0 {
		floorPrice = tick
	}

	instID := model.LookupInstrument(snap.Symbol).InstrumentID
	for idx, bot := range sweepers {
		qty := bot.ClampOrderToSolvency(model.SideSell, model.OrderTypeIOC, floorPrice, snap.BestAsk, sweepLots*lot, lot, maxFeeBps)
		if qty < lot {
			continue
		}
		cid, idem, nonce := bot.NextClientIDs()
		out = append(out, model.OrderIntent{
			ClientOrderID:  cid,
			IdempotencyKey: idem,
			AccountNum:     bot.AccountNum,
			InstrumentID:   instID,
			BotID:          bot.BotID,
			Kind:           model.ActionSubmit,
			Side:           model.SideSell,
			OrderType:      model.OrderTypeIOC,
			AccountID:      bot.AccountID,
			Symbol:         snap.Symbol,
			Archetype:      model.ArchetypeSyndicateSweep,
			Price:          floorPrice,
			Quantity:       qty,
			TimestampNs:    nowNs + int64(idx)*1_000,
			Nonce:          nonce,
		})
	}
	return out
}

func clampFloat(x, lo, hi float64) float64 {
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
