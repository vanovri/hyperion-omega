package instruments

import "fmt"

// Spec defines deterministic fixed-point tick/lot constraints for an exchange instrument.
// Scale invariant: 1.00000000 = 100_000_000 atomic units (FP_SCALE = 1e8).
type Spec struct {
	ID        uint16
	Symbol    string
	TickSize  uint64 // Minimum price increment in 1e8 fixed-point units
	LotSize   uint64 // Minimum base quantity increment in 1e8 fixed-point units
}

var (
	bySymbol = map[string]Spec{
		"BTC-USDT":  {ID: 1, Symbol: "BTC-USDT", TickSize: 1_000_000, LotSize: 1_000},
		"ETH-USDT":  {ID: 2, Symbol: "ETH-USDT", TickSize: 1_000_000, LotSize: 10_000},
		"SOL-USDT":  {ID: 3, Symbol: "SOL-USDT", TickSize: 100_000, LotSize: 100_000},
		"HYPE-USDT": {ID: 4, Symbol: "HYPE-USDT", TickSize: 10_000, LotSize: 1_000_000},
	}

	byID = [5]Spec{
		{}, // 0 = global/system
		{ID: 1, Symbol: "BTC-USDT", TickSize: 1_000_000, LotSize: 1_000},
		{ID: 2, Symbol: "ETH-USDT", TickSize: 1_000_000, LotSize: 10_000},
		{ID: 3, Symbol: "SOL-USDT", TickSize: 100_000, LotSize: 100_000},
		{ID: 4, Symbol: "HYPE-USDT", TickSize: 10_000, LotSize: 1_000_000},
	}
)

// MaxInstrumentID is the highest canonical instrument ID in v1.
const MaxInstrumentID = 4

// LookupSymbol resolves a canonical symbol string (e.g., "BTC-USDT") in O(1).
func LookupSymbol(symbol string) (Spec, bool) {
	s, ok := bySymbol[symbol]
	return s, ok
}

// LookupID resolves a canonical instrument ID (1..4) in O(1) without map lookup.
func LookupID(id uint16) (Spec, bool) {
	if id == 0 || int(id) >= len(byID) {
		return Spec{}, false
	}
	return byID[id], true
}

// All returns the canonical slice of supported instruments in deterministic ID order.
func All() []Spec {
	out := make([]Spec, MaxInstrumentID)
	copy(out, byID[1:])
	return out
}

// ValidateOrderAligned verifies fixed-point quantity (>0 and lot-aligned) and price (tick-aligned unless market order).
func ValidateOrderAligned(symbol string, isMarket bool, priceFP, qtyFP uint64) (Spec, error) {
	spec, ok := LookupSymbol(symbol)
	if !ok {
		return Spec{}, fmt.Errorf("unsupported symbol %q", symbol)
	}
	if qtyFP == 0 {
		return Spec{}, fmt.Errorf("qty_fp must be > 0")
	}
	if spec.LotSize > 0 && qtyFP%spec.LotSize != 0 {
		return Spec{}, fmt.Errorf("qty_fp %d not aligned to lot size %d for %s", qtyFP, spec.LotSize, symbol)
	}
	if !isMarket {
		if priceFP == 0 {
			return Spec{}, fmt.Errorf("price_fp must be > 0 for non-market orders")
		}
		if spec.TickSize > 0 && priceFP%spec.TickSize != 0 {
			return Spec{}, fmt.Errorf("price_fp %d not aligned to tick size %d for %s", priceFP, spec.TickSize, symbol)
		}
	}
	return spec, nil
}
