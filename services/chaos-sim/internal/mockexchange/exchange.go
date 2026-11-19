package mockexchange

import (
	"slices"
	"sync"
	"sync/atomic"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

type bookOrder struct {
	ServerOrderID  uint64
	ClientOrderID  uint64
	IdempotencyKey uint64
	BotID          uint32
	AccountID      string
	Side           model.Side
	Price          int64 // 1e8
	RemainingQty   int64 // 1e8
	LockedQuote    int64 // Exact quote locked for resting BUY
	TimestampNs    int64
}

type accountLedger struct {
	AccountID   string
	BotID       uint32
	TotalQuote  int64
	LockedQuote int64
	TotalBase   int64
	LockedBase  int64
}

type ExchangeConfig struct {
	Symbol        string
	InitialMid    int64 // 1e8 (e.g. 50_000 * 1e8)
	TickSize      int64 // 1e8 (e.g. 1_000_000 = $0.01)
	LotSize       int64 // 1e8 (e.g. 100_000 = 0.001)
	MakerFeeBps   int64 // e.g. 2 bps
	TakerFeeBps   int64 // e.g. 5 bps
	MaxClockSkewNs int64 // e.g. 30_000_000_000 (30s)
}

func DefaultExchangeConfig() ExchangeConfig {
	spec := model.LookupInstrument("BTC-USDT")
	return ExchangeConfig{
		Symbol:         spec.Symbol,
		InitialMid:     spec.DefaultMidFP,
		TickSize:       spec.TickSizeFP,
		LotSize:        spec.LotSizeFP,
		MakerFeeBps:    2,
		TakerFeeBps:    5,
		MaxClockSkewNs: 5_000_000_000, // Canonical 5s Gateway clock-skew window per 00-hyperion-contract.md §6.2
	}
}

// Exchange is a high-speed, deterministic, double-entry in-memory Limit Order Book
// used for chaos-sim standalone execution, >50k orders/s benchmarks, and invariant verification.
type Exchange struct {
	mu           sync.Mutex
	cfg          ExchangeConfig
	instrumentID uint32

	accounts          map[string]*accountLedger
	bids              []*bookOrder
	asks              []*bookOrder
	restingByClientID map[uint64]*bookOrder
	allKnownOrders    map[uint64]bool // clientOrderID -> terminal
	idempotencySeen   map[uint64]uint64

	// Pre-allocated slab free-list for zero-allocation resting bookOrder lifecycle
	freeOrders []*bookOrder

	nextServerOrderID uint64
	nextTradeID       uint64
	seq               uint64
	treasuryFeesQuote int64

	// Rolling microstructure state
	lastMid        int64
	shortEMA       int64
	longEMA        int64
	cumVWAPQuote   int64
	cumVWAPBase    int64
	realizedVolBps int64

	snapPtr     atomic.Pointer[model.MarketSnapshot]
	onMakerFill func(model.FillEvent)

	acceptedOrders  atomic.Uint64
	rejectedOrders  atomic.Uint64
	dedupedOrders   atomic.Uint64
	matchedTrades   atomic.Uint64
	canceledOrders  atomic.Uint64
}

func NewExchange(cfg ExchangeConfig, onMakerFill func(model.FillEvent)) *Exchange {
	if cfg.InitialMid <= 0 {
		cfg.InitialMid = 50_000 * fixed.Scale
	}
	if cfg.TickSize <= 0 {
		cfg.TickSize = 1_000_000
	}
	if cfg.LotSize <= 0 {
		cfg.LotSize = 100_000
	}
	if cfg.MaxClockSkewNs <= 0 {
		cfg.MaxClockSkewNs = 30_000_000_000
	}

	spec := model.LookupInstrument(cfg.Symbol)
	const initialSlab = 4096
	slab := make([]bookOrder, initialSlab)
	freeList := make([]*bookOrder, initialSlab)
	for i := 0; i < initialSlab; i++ {
		freeList[i] = &slab[i]
	}

	ex := &Exchange{
		cfg:               cfg,
		instrumentID:      spec.InstrumentID,
		accounts:          make(map[string]*accountLedger, 1200),
		bids:              make([]*bookOrder, 0, 2048),
		asks:              make([]*bookOrder, 0, 2048),
		restingByClientID: make(map[uint64]*bookOrder, 4096),
		allKnownOrders:    make(map[uint64]bool, 32768),
		idempotencySeen:   make(map[uint64]uint64, 32768),
		freeOrders:        freeList,
		lastMid:           cfg.InitialMid,
		shortEMA:          cfg.InitialMid,
		longEMA:           cfg.InitialMid,
		realizedVolBps:    25,
		onMakerFill:       onMakerFill,
	}
	initSnap := &model.MarketSnapshot{
		Symbol:         cfg.Symbol,
		InstrumentID:   spec.InstrumentID,
		Sequence:       1,
		MidPrice:       cfg.InitialMid,
		BestBid:        cfg.InitialMid - cfg.TickSize,
		BestAsk:        cfg.InitialMid + cfg.TickSize,
		BidDepth5:      10 * fixed.Scale,
		AskDepth5:      10 * fixed.Scale,
		VWAP:           cfg.InitialMid,
		ShortEMA:       cfg.InitialMid,
		LongEMA:        cfg.InitialMid,
		RealizedVolBps: 25,
		TickSize:       cfg.TickSize,
		LotSize:        cfg.LotSize,
		Regime:         model.RegimeCalm,
	}
	ex.snapPtr.Store(initSnap)
	return ex
}

func (ex *Exchange) allocOrderLocked() *bookOrder {
	n := len(ex.freeOrders)
	if n > 0 {
		o := ex.freeOrders[n-1]
		ex.freeOrders = ex.freeOrders[:n-1]
		*o = bookOrder{}
		return o
	}
	return &bookOrder{}
}

func (ex *Exchange) freeOrderLocked(o *bookOrder) {
	if o == nil || len(ex.freeOrders) >= 16384 {
		return
	}
	*o = bookOrder{}
	ex.freeOrders = append(ex.freeOrders, o)
}

func (ex *Exchange) SetRegime(r model.MarketRegime) {
	ex.mu.Lock()
	defer ex.mu.Unlock()
	cur := ex.snapPtr.Load()
	if cur == nil || cur.Regime == r {
		return
	}
	cp := *cur
	cp.Regime = r
	ex.snapPtr.Store(&cp)
}

func (ex *Exchange) SeedAccount(accountID string, botID uint32, initQuote, initBase int64) {
	ex.mu.Lock()
	defer ex.mu.Unlock()
	ex.accounts[accountID] = &accountLedger{
		AccountID:  accountID,
		BotID:      botID,
		TotalQuote: initQuote,
		TotalBase:  initBase,
	}
}

func (ex *Exchange) Snapshot() *model.MarketSnapshot {
	return ex.snapPtr.Load()
}

// Process executes a Submit or Cancel OrderIntent atomically under double-entry invariants.
func (ex *Exchange) Process(intent model.OrderIntent, serverNowNs int64, activeRegime model.MarketRegime) model.SubmitResult {
	// Fast-path signature & timestamp skew guard before acquiring book mutex if corrupt
	if intent.CorruptSignature {
		ex.rejectedOrders.Add(1)
		return model.SubmitResult{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			Status:         model.StatusRejected,
			RejectReason:   "INVALID_SIGNATURE",
		}
	}
	effTs := intent.TimestampNs + intent.ClockSkewNs
	if serverNowNs > 0 && effTs > 0 && fixed.Abs(serverNowNs-effTs) > ex.cfg.MaxClockSkewNs {
		ex.rejectedOrders.Add(1)
		return model.SubmitResult{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			Status:         model.StatusRejected,
			RejectReason:   "EXPIRED_OR_SKEWED_TIMESTAMP",
		}
	}

	var makerNotifications []model.FillEvent

	ex.mu.Lock()

	if intent.Kind == model.ActionCancel {
		res := ex.cancelLocked(intent)
		if res.Status == model.StatusCanceled {
			ex.publishSnapshotLocked(serverNowNs, activeRegime)
		}
		ex.mu.Unlock()
		return res
	}

	// Idempotency check
	if intent.IdempotencyKey != 0 {
		if prevSrvID, seen := ex.idempotencySeen[intent.IdempotencyKey]; seen {
			ex.dedupedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				ServerOrderID:  prevSrvID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusDuplicateDeduped,
			}
		}
	}

	acc, ok := ex.accounts[intent.AccountID]
	if !ok {
		ex.rejectedOrders.Add(1)
		ex.mu.Unlock()
		return model.SubmitResult{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			Status:         model.StatusRejected,
			RejectReason:   "UNKNOWN_ACCOUNT",
		}
	}

	if intent.Quantity <= 0 || (intent.OrderType != model.OrderTypeMarket && intent.Price <= 0) {
		ex.rejectedOrders.Add(1)
		ex.mu.Unlock()
		return model.SubmitResult{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			Status:         model.StatusRejected,
			RejectReason:   "INVALID_PRICE_OR_QTY",
		}
	}

	// POST_ONLY guard: reject if order would cross existing book
	if intent.OrderType == model.OrderTypePostOnly {
		if intent.Side == model.SideBuy && len(ex.asks) > 0 && intent.Price >= ex.asks[0].Price {
			ex.rejectedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   "POST_ONLY_WOULD_CROSS",
			}
		}
		if intent.Side == model.SideSell && len(ex.bids) > 0 && intent.Price <= ex.bids[0].Price {
			ex.rejectedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   "POST_ONLY_WOULD_CROSS",
			}
		}
	}

	// FOK pre-check: ensure sufficient crossable non-self liquidity
	if intent.OrderType == model.OrderTypeFOK {
		availCross := ex.availableCrossableLocked(intent)
		if availCross < intent.Quantity {
			ex.rejectedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   "FOK_INSUFFICIENT_LIQUIDITY",
			}
		}
	}

	// Solvency pre-check on server ledger
	if intent.Side == model.SideSell {
		if acc.TotalBase-acc.LockedBase < intent.Quantity {
			ex.rejectedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   "INSUFFICIENT_BASE_BALANCE",
			}
		}
	} else {
		checkPrice := intent.Price
		if intent.OrderType == model.OrderTypeMarket {
			if len(ex.asks) == 0 {
				ex.rejectedOrders.Add(1)
				ex.mu.Unlock()
				return model.SubmitResult{
					ClientOrderID:  intent.ClientOrderID,
					IdempotencyKey: intent.IdempotencyKey,
					BotID:          intent.BotID,
					Status:         model.StatusRejected,
					RejectReason:   "NO_MARKET_LIQUIDITY",
				}
			}
			checkPrice = ex.asks[len(ex.asks)-1].Price
		}
		reqNotional, err1 := fixed.QuoteNotional(checkPrice, intent.Quantity)
		reqFee, err2 := fixed.FeeFromBps(reqNotional, ex.cfg.TakerFeeBps)
		if err1 != nil || err2 != nil || acc.TotalQuote-acc.LockedQuote < reqNotional+reqFee {
			ex.rejectedOrders.Add(1)
			ex.mu.Unlock()
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   "INSUFFICIENT_QUOTE_BALANCE",
			}
		}
	}

	ex.nextServerOrderID++
	srvOrderID := ex.nextServerOrderID
	if intent.IdempotencyKey != 0 {
		ex.idempotencySeen[intent.IdempotencyKey] = srvOrderID
	}
	ex.acceptedOrders.Add(1)

	remaining := intent.Quantity
	var takerFills []model.FillEvent

	if intent.OrderType != model.OrderTypePostOnly {
		if intent.Side == model.SideBuy {
			remaining, takerFills, makerNotifications = ex.matchBuyLocked(intent, acc, srvOrderID, remaining, serverNowNs)
		} else {
			remaining, takerFills, makerNotifications = ex.matchSellLocked(intent, acc, srvOrderID, remaining, serverNowNs)
		}
	}

	filledQty := intent.Quantity - remaining
	var status model.OrderStatus

	if remaining == 0 {
		status = model.StatusFilled
		ex.allKnownOrders[intent.ClientOrderID] = true
	} else if intent.OrderType == model.OrderTypeIOC || intent.OrderType == model.OrderTypeMarket || intent.OrderType == model.OrderTypeFOK {
		if filledQty > 0 {
			status = model.StatusFilled // IOC partial execution completes lifecycle
		} else {
			status = model.StatusCanceled
		}
		remaining = 0
		ex.allKnownOrders[intent.ClientOrderID] = true
	} else {
		// Rest remaining on Limit Order Book
		var lockedQuote int64
		if intent.Side == model.SideBuy {
			notional, _ := fixed.QuoteNotional(intent.Price, remaining)
			fee, _ := fixed.FeeFromBps(notional, ex.cfg.MakerFeeBps)
			lockedQuote = notional + fee
			if acc.TotalQuote-acc.LockedQuote < lockedQuote {
				// Cannot rest remainder safely; cancel remainder cleanly
				if filledQty > 0 {
					status = model.StatusFilled
				} else {
					status = model.StatusCanceled
				}
				remaining = 0
				ex.allKnownOrders[intent.ClientOrderID] = true
			} else {
				acc.LockedQuote += lockedQuote
				rest := ex.allocOrderLocked()
				*rest = bookOrder{
					ServerOrderID:  srvOrderID,
					ClientOrderID:  intent.ClientOrderID,
					IdempotencyKey: intent.IdempotencyKey,
					BotID:          intent.BotID,
					AccountID:      intent.AccountID,
					Side:           model.SideBuy,
					Price:          intent.Price,
					RemainingQty:   remaining,
					LockedQuote:    lockedQuote,
					TimestampNs:    serverNowNs,
				}
				ex.insertBidLocked(rest)
				ex.restingByClientID[intent.ClientOrderID] = rest
				ex.allKnownOrders[intent.ClientOrderID] = false
				if filledQty > 0 {
					status = model.StatusPartiallyFilled
				} else {
					status = model.StatusAccepted
				}
			}
		} else {
			acc.LockedBase += remaining
			rest := ex.allocOrderLocked()
			*rest = bookOrder{
				ServerOrderID:  srvOrderID,
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				AccountID:      intent.AccountID,
				Side:           model.SideSell,
				Price:          intent.Price,
				RemainingQty:   remaining,
				TimestampNs:    serverNowNs,
			}
			ex.insertAskLocked(rest)
			ex.restingByClientID[intent.ClientOrderID] = rest
			ex.allKnownOrders[intent.ClientOrderID] = false
			if filledQty > 0 {
				status = model.StatusPartiallyFilled
			} else {
				status = model.StatusAccepted
			}
		}
	}

	ex.publishSnapshotLocked(serverNowNs, activeRegime)
	ex.mu.Unlock()

	if ex.onMakerFill != nil && len(makerNotifications) > 0 {
		for _, mf := range makerNotifications {
			ex.onMakerFill(mf)
		}
	}

	return model.SubmitResult{
		ClientOrderID:  intent.ClientOrderID,
		ServerOrderID:  srvOrderID,
		IdempotencyKey: intent.IdempotencyKey,
		BotID:          intent.BotID,
		Status:         status,
		AcceptedQty:    intent.Quantity,
		FilledQty:      filledQty,
		RemainingQty:   remaining,
		Fills:          takerFills,
	}
}

func (ex *Exchange) matchBuyLocked(
	intent model.OrderIntent,
	takerAcc *accountLedger,
	takerSrvID uint64,
	remaining int64,
	nowNs int64,
) (int64, []model.FillEvent, []model.FillEvent) {
	var takerFills []model.FillEvent
	var makerFills []model.FillEvent

	idx := 0
	for idx < len(ex.asks) && remaining > 0 {
		maker := ex.asks[idx]
		if intent.OrderType != model.OrderTypeMarket && maker.Price > intent.Price {
			break
		}
		// Self-Trade Prevention (Cancel Resting Maker)
		if maker.AccountID == intent.AccountID {
			makerAcc := ex.accounts[maker.AccountID]
			makerAcc.LockedBase -= maker.RemainingQty
			if makerAcc.LockedBase < 0 {
				makerAcc.LockedBase = 0
			}
			delete(ex.restingByClientID, maker.ClientOrderID)
			ex.allKnownOrders[maker.ClientOrderID] = true
			ex.asks = slices.Delete(ex.asks, idx, idx+1)
			ex.freeOrderLocked(maker)
			continue
		}

		execQty := remaining
		if maker.RemainingQty < execQty {
			execQty = maker.RemainingQty
		}
		execPrice := maker.Price
		notional, err := fixed.QuoteNotional(execPrice, execQty)
		if err != nil || notional < 0 {
			break
		}
		takerFee, _ := fixed.FeeFromBps(notional, ex.cfg.TakerFeeBps)
		makerFee, _ := fixed.FeeFromBps(notional, ex.cfg.MakerFeeBps)

		// Ensure taker still has unlocked quote for this fill
		if takerAcc.TotalQuote-takerAcc.LockedQuote < notional+takerFee {
			break
		}

		makerAcc := ex.accounts[maker.AccountID]

		// Double-entry settlement:
		// Base: Maker (-execQty) -> Taker (+execQty) [Exact 0 sum]
		// Quote: Taker (-(notional + takerFee)) -> Maker (+(notional - makerFee)) + Treasury (+(takerFee + makerFee)) [Exact 0 sum!]
		makerAcc.LockedBase -= execQty
		if makerAcc.LockedBase < 0 {
			makerAcc.LockedBase = 0
		}
		makerAcc.TotalBase -= execQty
		makerAcc.TotalQuote += (notional - makerFee)

		takerAcc.TotalBase += execQty
		takerAcc.TotalQuote -= (notional + takerFee)

		ex.treasuryFeesQuote += (takerFee + makerFee)
		ex.nextTradeID++
		tid := ex.nextTradeID
		ex.matchedTrades.Add(1)
		ex.updateTradeMicrostructureLocked(execPrice, execQty, notional)

		takerFills = append(takerFills, model.FillEvent{
			TradeID:        tid,
			ClientOrderID:  intent.ClientOrderID,
			ServerOrderID:  takerSrvID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			AccountID:      intent.AccountID,
			CounterpartyID: maker.AccountID,
			Symbol:         ex.cfg.Symbol,
			Side:           model.SideBuy,
			Price:          execPrice,
			Quantity:       execQty,
			FeeQuote:       takerFee,
			IsMaker:        false,
			TimestampNs:    nowNs,
		})
		makerFills = append(makerFills, model.FillEvent{
			TradeID:        tid,
			ClientOrderID:  maker.ClientOrderID,
			ServerOrderID:  maker.ServerOrderID,
			IdempotencyKey: maker.IdempotencyKey,
			BotID:          maker.BotID,
			AccountID:      maker.AccountID,
			CounterpartyID: intent.AccountID,
			Symbol:         ex.cfg.Symbol,
			Side:           model.SideSell,
			Price:          execPrice,
			Quantity:       execQty,
			FeeQuote:       makerFee,
			IsMaker:        true,
			TimestampNs:    nowNs,
		})

		remaining -= execQty
		maker.RemainingQty -= execQty
		if maker.RemainingQty <= 0 {
			delete(ex.restingByClientID, maker.ClientOrderID)
			ex.allKnownOrders[maker.ClientOrderID] = true
			ex.asks = slices.Delete(ex.asks, idx, idx+1)
			ex.freeOrderLocked(maker)
		} else {
			idx++
		}
	}
	return remaining, takerFills, makerFills
}

func (ex *Exchange) matchSellLocked(
	intent model.OrderIntent,
	takerAcc *accountLedger,
	takerSrvID uint64,
	remaining int64,
	nowNs int64,
) (int64, []model.FillEvent, []model.FillEvent) {
	var takerFills []model.FillEvent
	var makerFills []model.FillEvent

	idx := 0
	for idx < len(ex.bids) && remaining > 0 {
		maker := ex.bids[idx]
		if intent.OrderType != model.OrderTypeMarket && maker.Price < intent.Price {
			break
		}
		if maker.AccountID == intent.AccountID {
			makerAcc := ex.accounts[maker.AccountID]
			makerAcc.LockedQuote -= maker.LockedQuote
			if makerAcc.LockedQuote < 0 {
				makerAcc.LockedQuote = 0
			}
			delete(ex.restingByClientID, maker.ClientOrderID)
			ex.allKnownOrders[maker.ClientOrderID] = true
			ex.bids = slices.Delete(ex.bids, idx, idx+1)
			ex.freeOrderLocked(maker)
			continue
		}

		execQty := remaining
		if maker.RemainingQty < execQty {
			execQty = maker.RemainingQty
		}
		execPrice := maker.Price
		notional, err := fixed.QuoteNotional(execPrice, execQty)
		if err != nil || notional < 0 {
			break
		}
		takerFee, _ := fixed.FeeFromBps(notional, ex.cfg.TakerFeeBps)
		makerFee, _ := fixed.FeeFromBps(notional, ex.cfg.MakerFeeBps)

		makerAcc := ex.accounts[maker.AccountID]

		// Release proportional quote lock on resting Buy maker
		var releaseLock int64
		if execQty >= maker.RemainingQty {
			releaseLock = maker.LockedQuote
		} else {
			releaseLock, _ = fixed.MulDiv128(maker.LockedQuote, execQty, maker.RemainingQty)
		}
		makerAcc.LockedQuote -= releaseLock
		if makerAcc.LockedQuote < 0 {
			makerAcc.LockedQuote = 0
		}
		maker.LockedQuote -= releaseLock
		if maker.LockedQuote < 0 {
			maker.LockedQuote = 0
		}

		// Double-entry settlement:
		makerAcc.TotalBase += execQty
		makerAcc.TotalQuote -= (notional + makerFee)

		takerAcc.TotalBase -= execQty
		takerAcc.TotalQuote += (notional - takerFee)

		ex.treasuryFeesQuote += (takerFee + makerFee)
		ex.nextTradeID++
		tid := ex.nextTradeID
		ex.matchedTrades.Add(1)
		ex.updateTradeMicrostructureLocked(execPrice, execQty, notional)

		takerFills = append(takerFills, model.FillEvent{
			TradeID:        tid,
			ClientOrderID:  intent.ClientOrderID,
			ServerOrderID:  takerSrvID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			AccountID:      intent.AccountID,
			CounterpartyID: maker.AccountID,
			Symbol:         ex.cfg.Symbol,
			Side:           model.SideSell,
			Price:          execPrice,
			Quantity:       execQty,
			FeeQuote:       takerFee,
			IsMaker:        false,
			TimestampNs:    nowNs,
		})
		makerFills = append(makerFills, model.FillEvent{
			TradeID:        tid,
			ClientOrderID:  maker.ClientOrderID,
			ServerOrderID:  maker.ServerOrderID,
			IdempotencyKey: maker.IdempotencyKey,
			BotID:          maker.BotID,
			AccountID:      maker.AccountID,
			CounterpartyID: intent.AccountID,
			Symbol:         ex.cfg.Symbol,
			Side:           model.SideBuy,
			Price:          execPrice,
			Quantity:       execQty,
			FeeQuote:       makerFee,
			IsMaker:        true,
			TimestampNs:    nowNs,
		})

		remaining -= execQty
		maker.RemainingQty -= execQty
		if maker.RemainingQty <= 0 {
			delete(ex.restingByClientID, maker.ClientOrderID)
			ex.allKnownOrders[maker.ClientOrderID] = true
			ex.bids = slices.Delete(ex.bids, idx, idx+1)
			ex.freeOrderLocked(maker)
		} else {
			idx++
		}
	}
	return remaining, takerFills, makerFills
}

func (ex *Exchange) cancelLocked(intent model.OrderIntent) model.SubmitResult {
	targetID := intent.TargetOrderID
	if targetID == 0 {
		targetID = intent.ClientOrderID
	}
	ord, ok := ex.restingByClientID[targetID]
	if !ok {
		return model.SubmitResult{
			ClientOrderID: targetID,
			BotID:         intent.BotID,
			Status:        model.StatusRejected,
			RejectReason:  "ORDER_NOT_FOUND",
		}
	}
	delete(ex.restingByClientID, targetID)
	ex.allKnownOrders[targetID] = true
	ex.canceledOrders.Add(1)

	acc := ex.accounts[ord.AccountID]
	if ord.Side == model.SideBuy {
		acc.LockedQuote -= ord.LockedQuote
		if acc.LockedQuote < 0 {
			acc.LockedQuote = 0
		}
		for i, b := range ex.bids {
			if b.ClientOrderID == targetID {
				ex.bids = slices.Delete(ex.bids, i, i+1)
				break
			}
		}
	} else {
		acc.LockedBase -= ord.RemainingQty
		if acc.LockedBase < 0 {
			acc.LockedBase = 0
		}
		for i, a := range ex.asks {
			if a.ClientOrderID == targetID {
				ex.asks = slices.Delete(ex.asks, i, i+1)
				break
			}
		}
	}

	srvID := ord.ServerOrderID
	ex.freeOrderLocked(ord)

	return model.SubmitResult{
		ClientOrderID: targetID,
		ServerOrderID: srvID,
		BotID:         intent.BotID,
		Status:        model.StatusCanceled,
	}
}

func (ex *Exchange) availableCrossableLocked(intent model.OrderIntent) int64 {
	var sum int64
	if intent.Side == model.SideBuy {
		for _, a := range ex.asks {
			if a.Price > intent.Price {
				break
			}
			if a.AccountID != intent.AccountID {
				sum += a.RemainingQty
				if sum >= intent.Quantity {
					return sum
				}
			}
		}
	} else {
		for _, b := range ex.bids {
			if b.Price < intent.Price {
				break
			}
			if b.AccountID != intent.AccountID {
				sum += b.RemainingQty
				if sum >= intent.Quantity {
					return sum
				}
			}
		}
	}
	return sum
}

func (ex *Exchange) insertBidLocked(ord *bookOrder) {
	// Bids sorted descending by Price; FIFO (after existing equal-price orders).
	lo, hi := 0, len(ex.bids)
	for lo < hi {
		mid := int(uint(lo+hi) >> 1)
		if ex.bids[mid].Price >= ord.Price {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	ex.bids = slices.Insert(ex.bids, lo, ord)
}

func (ex *Exchange) insertAskLocked(ord *bookOrder) {
	// Asks sorted ascending by Price; FIFO (after existing equal-price orders).
	lo, hi := 0, len(ex.asks)
	for lo < hi {
		mid := int(uint(lo+hi) >> 1)
		if ex.asks[mid].Price <= ord.Price {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	ex.asks = slices.Insert(ex.asks, lo, ord)
}

func (ex *Exchange) updateTradeMicrostructureLocked(price, qty, notional int64) {
	prevMid := ex.lastMid
	ex.lastMid = price
	// Incremental integer EMA: short alpha = 1/8, long alpha = 1/32
	ex.shortEMA = ex.shortEMA + (price-ex.shortEMA)/8
	ex.longEMA = ex.longEMA + (price-ex.longEMA)/32

	// Rolling decayed VWAP accumulators preventing int64 overflow over millions of trades
	if ex.cumVWAPQuote > (1<<56) || ex.cumVWAPBase > (1<<50) {
		ex.cumVWAPQuote >>= 1
		ex.cumVWAPBase >>= 1
	}
	ex.cumVWAPQuote += notional
	ex.cumVWAPBase += qty

	if prevMid > 0 {
		retBps, err := fixed.MulDiv128(fixed.Abs(price-prevMid), 10_000, prevMid)
		if err == nil {
			scaledRet := retBps * 8
			if scaledRet < 5 {
				scaledRet = 5
			}
			ex.realizedVolBps = fixed.Clamp((ex.realizedVolBps*15+scaledRet)/16, 5, 10_000)
		}
	}
}

func (ex *Exchange) publishSnapshotLocked(nowNs int64, regime model.MarketRegime) {
	ex.seq++
	bestBid := ex.lastMid - ex.cfg.TickSize
	bestAsk := ex.lastMid + ex.cfg.TickSize
	if len(ex.bids) > 0 {
		bestBid = ex.bids[0].Price
	}
	if len(ex.asks) > 0 {
		bestAsk = ex.asks[0].Price
	}
	mid := ex.lastMid
	if len(ex.bids) > 0 && len(ex.asks) > 0 {
		mid = (bestBid + bestAsk) / 2
	}

	var bidD5, askD5 int64
	limitB := 5
	if len(ex.bids) < limitB {
		limitB = len(ex.bids)
	}
	for i := 0; i < limitB; i++ {
		bidD5 += ex.bids[i].RemainingQty
	}
	limitA := 5
	if len(ex.asks) < limitA {
		limitA = len(ex.asks)
	}
	for i := 0; i < limitA; i++ {
		askD5 += ex.asks[i].RemainingQty
	}

	vwap := mid
	if ex.cumVWAPBase > 0 {
		if v, err := fixed.MulDiv128(ex.cumVWAPQuote, fixed.Scale, ex.cumVWAPBase); err == nil && v > 0 {
			vwap = v
		}
	}

	// Snapshot coalescing: skip heap allocation when top-of-book & microstructure metrics are unchanged.
	if prev := ex.snapPtr.Load(); prev != nil &&
		prev.MidPrice == mid &&
		prev.BestBid == bestBid &&
		prev.BestAsk == bestAsk &&
		prev.BidDepth5 == bidD5 &&
		prev.AskDepth5 == askD5 &&
		prev.VWAP == vwap &&
		prev.ShortEMA == ex.shortEMA &&
		prev.LongEMA == ex.longEMA &&
		prev.RealizedVolBps == ex.realizedVolBps &&
		prev.Regime == regime {
		return
	}

	snap := &model.MarketSnapshot{
		Symbol:         ex.cfg.Symbol,
		InstrumentID:   ex.instrumentID,
		Sequence:       ex.seq,
		TimestampNs:    nowNs,
		MidPrice:       mid,
		BestBid:        bestBid,
		BestAsk:        bestAsk,
		BidDepth5:      bidD5,
		AskDepth5:      askD5,
		VWAP:           vwap,
		ShortEMA:       ex.shortEMA,
		LongEMA:        ex.longEMA,
		RealizedVolBps: ex.realizedVolBps,
		TickSize:       ex.cfg.TickSize,
		LotSize:        ex.cfg.LotSize,
		Regime:         regime,
	}
	ex.snapPtr.Store(snap)
}

// FetchBalances satisfies auditor.ServerStateProvider for continuous double-entry reconciliation.
func (ex *Exchange) FetchBalances() (map[string]model.AccountBalance, int64, error) {
	ex.mu.Lock()
	defer ex.mu.Unlock()
	res := make(map[string]model.AccountBalance, len(ex.accounts))
	for id, acc := range ex.accounts {
		res[id] = model.AccountBalance{
			AccountID:      acc.AccountID,
			BotID:          acc.BotID,
			AvailableQuote: acc.TotalQuote - acc.LockedQuote,
			LockedQuote:    acc.LockedQuote,
			TotalQuote:     acc.TotalQuote,
			AvailableBase:  acc.TotalBase - acc.LockedBase,
			LockedBase:     acc.LockedBase,
			TotalBase:      acc.TotalBase,
		}
	}
	return res, ex.treasuryFeesQuote, nil
}

// HasOrderRecord satisfies auditor.ServerStateProvider for lost-order detection.
func (ex *Exchange) HasOrderRecord(clientOrderID uint64) (bool, bool) {
	ex.mu.Lock()
	defer ex.mu.Unlock()
	term, exists := ex.allKnownOrders[clientOrderID]
	return exists, term
}

type ExchangeStats struct {
	AcceptedOrders    uint64 `json:"accepted_orders"`
	RejectedOrders    uint64 `json:"rejected_orders"`
	DedupedOrders     uint64 `json:"deduped_orders"`
	MatchedTrades     uint64 `json:"matched_trades"`
	CanceledOrders    uint64 `json:"canceled_orders"`
	RestingBids       int    `json:"resting_bids"`
	RestingAsks       int    `json:"resting_asks"`
	TreasuryFeesQuote int64  `json:"treasury_fees_quote"`
}

func (ex *Exchange) Stats() ExchangeStats {
	ex.mu.Lock()
	rb := len(ex.bids)
	ra := len(ex.asks)
	tf := ex.treasuryFeesQuote
	ex.mu.Unlock()
	return ExchangeStats{
		AcceptedOrders:    ex.acceptedOrders.Load(),
		RejectedOrders:    ex.rejectedOrders.Load(),
		DedupedOrders:     ex.dedupedOrders.Load(),
		MatchedTrades:     ex.matchedTrades.Load(),
		CanceledOrders:    ex.canceledOrders.Load(),
		RestingBids:       rb,
		RestingAsks:       ra,
		TreasuryFeesQuote: tf,
	}
}
