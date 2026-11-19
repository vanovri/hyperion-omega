package agents_test

import (
	"testing"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

func TestBotAccountSolvencyAndReservationBoundaries(t *testing.T) {
	// 1. AvailableQuote & AvailableBase exact subtraction & negative floor
	bot := agents.NewBotAccount(1, 0, "acc", model.ArchetypeAvellanedaStoikov, 1000*fixed.Scale, 10*fixed.Scale)
	bot.LockedQuote = 400 * fixed.Scale
	bot.LockedBase = 3 * fixed.Scale
	if got := bot.AvailableQuote(); got != 600*fixed.Scale {
		t.Fatalf("AvailableQuote got %d want %d", got, 600*fixed.Scale)
	}
	if got := bot.AvailableBase(); got != 7*fixed.Scale {
		t.Fatalf("AvailableBase got %d want %d", got, 7*fixed.Scale)
	}
	bot.LockedQuote = 1200 * fixed.Scale
	bot.LockedBase = 15 * fixed.Scale
	if got := bot.AvailableQuote(); got != 0 {
		t.Fatalf("AvailableQuote overlocked got %d want 0", got)
	}
	if got := bot.AvailableBase(); got != 0 {
		t.Fatalf("AvailableBase overlocked got %d want 0", got)
	}

	// 2. ClampOrderToSolvency exhaustive boundary matrix
	b := agents.NewBotAccount(2, 1, "solv", model.ArchetypeMomentumQuant, 10_000*fixed.Scale, 5*fixed.Scale)
	lot := int64(100_000) // 0.001

	// Invalid zero/negative inputs
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 0, lot, 10); q != 0 {
		t.Fatalf("zero desiredQty got %d", q)
	}
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, lot, 0, 10); q != 0 {
		t.Fatalf("zero lotSize got %d", q)
	}
	if q := b.ClampOrderToSolvency(model.SideUnspecified, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, lot, lot, 10); q != 0 {
		t.Fatalf("unspecified side got %d", q)
	}
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 0, 0, lot, lot, 10); q != 0 {
		t.Fatalf("zero price got %d", q)
	}

	// Exact SELL clamp at available base boundary & below lotSize
	if q := b.ClampOrderToSolvency(model.SideSell, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 3*fixed.Scale, lot, 10); q != 3*fixed.Scale {
		t.Fatalf("sell within avail got %d", q)
	}
	if q := b.ClampOrderToSolvency(model.SideSell, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 9*fixed.Scale, lot, 10); q != 5*fixed.Scale {
		t.Fatalf("sell clamped to avail got %d want %d", q, 5*fixed.Scale)
	}
	b.LockedBase = 5*fixed.Scale - lot + 1 // avail = lot - 1 < lot
	if q := b.ClampOrderToSolvency(model.SideSell, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, lot, lot, 10); q != 0 {
		t.Fatalf("sell below lot got %d", q)
	}
	b.LockedBase = 5*fixed.Scale - lot // avail == lot exact
	if q := b.ClampOrderToSolvency(model.SideSell, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, lot, lot, 10); q != lot {
		t.Fatalf("sell exact lot got %d want %d", q, lot)
	}
	b.LockedBase = 0

	// Exact BUY clamp with fees, negative fee clamping, market 5% collar, and safety cushion
	// AvailableQuote = 10,000 * 1e8, Price = 1,000 * 1e8, fee = 100 bps (1%) -> max affordable ~ 9.900... BTC
	qLimit := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 20*fixed.Scale, lot, 100)
	if qLimit != 9_90000000 { // 9.90000000 BTC (notional 9,900 + fee 99 = 9,999 <= 10,000 - cushion)
		t.Fatalf("buy limit clamped with 100bps fee got %d want 990000000", qLimit)
	}
	// Negative maxFeeBps must be clamped to 0 (not subsidizing affordability!)
	qNegFee := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 20*fixed.Scale, lot, -500)
	if qNegFee != 9_99900000 { // 9.999 BTC * 1000 = 9999 <= 10000 - cushion; 10.000 would violate cushion!
		t.Fatalf("buy limit negative fee clamp got %d want 999900000", qNegFee)
	}
	// Market buy uses 10,500/10,000 (5%) collar above bestAsk
	qMkt := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeMarket, 0, 1000*fixed.Scale, 20*fixed.Scale, lot, 0)
	// usableQuote ~ 10,000*1e8, effPrice = 1,050*1e8 -> floor(10000/1050) in lots = 9.52300000
	if qMkt != 9_52300000 {
		t.Fatalf("market buy collar clamp got %d want 952300000", qMkt)
	}
	// Market buy with bestAsk == 0 falls back to limitPrice
	qMktFallback := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeMarket, 1000*fixed.Scale, 0, 20*fixed.Scale, lot, 0)
	if qMktFallback != 9_52300000 {
		t.Fatalf("market buy bestAsk=0 fallback got %d want 952300000", qMktFallback)
	}
	// Market buy with both bestAsk == 0 and limitPrice == 0 -> 0
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeMarket, 0, 0, lot, lot, 0); q != 0 {
		t.Fatalf("market buy zero price got %d", q)
	}
	// DesiredQty smaller than maxQty stays untouched (rounded to lot)
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 2*fixed.Scale, lot, 10); q != 2*fixed.Scale {
		t.Fatalf("buy within budget got %d want %d", q, 2*fixed.Scale)
	}
	// Safety cushion exact boundary (availQuote <= 1000 -> 0) and exact 1-lot affordability boundaries
	b.LockedQuote = b.TotalQuote - 1_000
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1*fixed.Scale, 1*fixed.Scale, lot, lot, 0); q != 0 {
		t.Fatalf("buy at exact cushion boundary got %d want 0", q)
	}
	// Exact 1-lot boundary with final totalReq == usableQuote vs totalReq == usableQuote + 1:
	// lot = 100_000 (0.001), effPrice = 1000 * 1e8 -> notional(1 lot) = 1_00000000 (1.0 USDT = 1e8 units).
	// With fee = 0, totalReq = notional + 2 = 100_000_002.
	// Case A: usableQuote = 100_000_002 -> maxQty == lot (100_000), qty == lot, totalReq == usableQuote -> MUST return lot!
	b.LockedQuote = b.TotalQuote - (1_000 + 100_000_002)
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 5*lot, lot, 0); q != lot {
		t.Fatalf("exact 1-lot totalReq==usableQuote boundary got %d want %d", q, lot)
	}
	// Case B: usableQuote = 100_000_001 -> maxQty == lot, totalReq = 100_000_002 > usableQuote -> step down qty-=lot -> 0!
	b.LockedQuote = b.TotalQuote - (1_000 + 100_000_001)
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 5*lot, lot, 0); q != 0 {
		t.Fatalf("exact 1-lot totalReq>usableQuote stepdown got %d want 0", q)
	}
	// Case C: usableQuote = 200_000_001 -> maxQty == 2*lot, totalReq = 200_000_002 > usableQuote -> step down to exact 1*lot!
	b.LockedQuote = b.TotalQuote - (1_000 + 200_000_001)
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, 1000*fixed.Scale, 1000*fixed.Scale, 5*lot, lot, 0); q != lot {
		t.Fatalf("exact 2-lot totalReq>usableQuote stepdown got %d want %d", q, lot)
	}
	// Negative desiredQty, negative lotSize, negative limitPrice, and tiny price causing collar<=0
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeLimit, -100, 100, lot, lot, 0); q != 0 {
		t.Fatalf("negative limitPrice got %d want 0", q)
	}
	if q := b.ClampOrderToSolvency(model.SideBuy, model.OrderTypeMarket, -50, -50, lot, lot, 0); q != 0 {
		t.Fatalf("negative market price got %d want 0", q)
	}
	b.LockedQuote = 0

	// 3. ReserveOrder exact locks, capacity bounds, and over-reserve rejection
	buyIntent := model.OrderIntent{
		ClientOrderID: 101,
		Side:          model.SideBuy,
		Price:         1000 * fixed.Scale,
		Quantity:      2 * fixed.Scale,
	}
	if !b.ReserveOrder(buyIntent, 100) { // notional 2000 + fee 20 = 2020
		t.Fatal("expected buy ReserveOrder to succeed")
	}
	if b.LockedQuote != 2020*fixed.Scale {
		t.Fatalf("LockedQuote got %d want %d", b.LockedQuote, 2020*fixed.Scale)
	}
	// Exact-total boundary buy should succeed, 1 unit over must fail
	exactBuy := model.OrderIntent{
		ClientOrderID: 102,
		Side:          model.SideBuy,
		Price:         7980 * fixed.Scale,
		Quantity:      1 * fixed.Scale,
	}
	if !b.ReserveOrder(exactBuy, 0) {
		t.Fatal("expected exact remaining quote ReserveOrder to succeed")
	}
	overBuy := model.OrderIntent{
		ClientOrderID: 103,
		Side:          model.SideBuy,
		Price:         1 * fixed.Scale,
		Quantity:      1 * fixed.Scale,
	}
	if b.ReserveOrder(overBuy, 0) {
		t.Fatal("expected over-budget buy ReserveOrder to fail")
	}

	// Exact-total boundary sell and over-sell
	sellIntent := model.OrderIntent{
		ClientOrderID: 201,
		Side:          model.SideSell,
		Price:         1000 * fixed.Scale,
		Quantity:      5 * fixed.Scale,
	}
	if !b.ReserveOrder(sellIntent, 0) {
		t.Fatal("expected exact sell ReserveOrder to succeed")
	}
	if b.LockedBase != 5*fixed.Scale {
		t.Fatalf("LockedBase got %d want %d", b.LockedBase, 5*fixed.Scale)
	}
	if b.ReserveOrder(model.OrderIntent{ClientOrderID: 202, Side: model.SideSell, Price: 1000 * fixed.Scale, Quantity: 1}, 0) {
		t.Fatal("expected over-inventory sell ReserveOrder to fail")
	}

	// Exhaust all 8 open slots and ensure 9th fails
	slotBot := agents.NewBotAccount(3, 2, "slots", model.ArchetypeAvellanedaStoikov, 100_000*fixed.Scale, 100*fixed.Scale)
	for i := 0; i < agents.MaxOpenSlots; i++ {
		if !slotBot.ReserveOrder(model.OrderIntent{ClientOrderID: uint64(i + 1), Side: model.SideSell, Price: fixed.Scale, Quantity: fixed.Scale}, 0) {
			t.Fatalf("slot %d failed unexpectedly", i)
		}
	}
	if slotBot.ReserveOrder(model.OrderIntent{ClientOrderID: 99, Side: model.SideSell, Price: fixed.Scale, Quantity: fixed.Scale}, 0) {
		t.Fatal("expected 9th ReserveOrder to fail when MaxOpenSlots full")
	}
}
