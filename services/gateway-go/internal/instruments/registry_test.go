package instruments_test

import (
	"testing"

	"github.com/vanovri/hyperion/services/gateway-go/internal/instruments"
)

func TestInstrumentRegistry_TickLotAlignmentAndBounds(t *testing.T) {
	if _, ok := instruments.LookupID(0); ok {
		t.Fatal("ID 0 must not resolve to tradeable instrument")
	}
	if _, ok := instruments.LookupID(5); ok {
		t.Fatal("ID 5 out of bounds must return false")
	}
	for id := uint16(1); id <= instruments.MaxInstrumentID; id++ {
		spec, ok := instruments.LookupID(id)
		if !ok || spec.ID != id {
			t.Fatalf("LookupID(%d) failed: %+v", id, spec)
		}
		bySym, ok2 := instruments.LookupSymbol(spec.Symbol)
		if !ok2 || bySym != spec {
			t.Fatalf("LookupSymbol(%s) mismatch", spec.Symbol)
		}
	}
	if len(instruments.All()) != instruments.MaxInstrumentID {
		t.Fatalf("expected %d instruments", instruments.MaxInstrumentID)
	}

	// Exact valid limit order (BTC-USDT tick=1_000_000, lot=1_000).
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", false, 6_842_551_000_000, 100_001_000); err != nil {
		t.Fatalf("expected aligned BTC-USDT order valid, got %v", err)
	}
	// Unaligned price_fp (off by 1 atomic unit).
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", false, 6_842_551_000_001, 100_000_000); err == nil {
		t.Fatal("expected unaligned price_fp to be rejected")
	}
	// Unaligned qty_fp (off by 1 atomic unit).
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", false, 6_842_551_000_000, 100_000_001); err == nil {
		t.Fatal("expected unaligned qty_fp to be rejected")
	}
	// Zero qty_fp rejected for both market and limit.
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", true, 0, 0); err == nil {
		t.Fatal("expected qty_fp=0 rejected")
	}
	// Zero price_fp rejected for limit, allowed for market!
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", false, 0, 100_000_000); err == nil {
		t.Fatal("expected price_fp=0 rejected on limit order")
	}
	if _, err := instruments.ValidateOrderAligned("BTC-USDT", true, 0, 100_000_000); err != nil {
		t.Fatalf("expected price_fp=0 allowed on market order, got %v", err)
	}
	// Unknown symbol rejected.
	if _, err := instruments.ValidateOrderAligned("DOGE-USDT", false, 1_000_000, 1_000_000); err == nil {
		t.Fatal("expected unknown symbol rejected")
	}
}
