package model

import (
	"strconv"
)

type Side uint8

const (
	SideUnspecified Side = 0
	SideBuy         Side = 1
	SideSell        Side = 2
)

func (s Side) String() string {
	switch s {
	case SideBuy:
		return "BUY"
	case SideSell:
		return "SELL"
	default:
		return "UNSPECIFIED"
	}
}

type OrderType uint8

const (
	OrderTypeUnspecified OrderType = 0
	OrderTypeLimit       OrderType = 1
	OrderTypeMarket      OrderType = 2
	OrderTypeIOC         OrderType = 3
	OrderTypeFOK         OrderType = 4
	OrderTypePostOnly    OrderType = 5
)

func (t OrderType) String() string {
	switch t {
	case OrderTypeLimit:
		return "LIMIT"
	case OrderTypeMarket:
		return "MARKET"
	case OrderTypeIOC:
		return "IOC"
	case OrderTypeFOK:
		return "FOK"
	case OrderTypePostOnly:
		return "POST_ONLY"
	default:
		return "LIMIT"
	}
}

type OrderStatus uint8

const (
	StatusPending          OrderStatus = 0
	StatusAccepted         OrderStatus = 1
	StatusPartiallyFilled  OrderStatus = 2
	StatusFilled           OrderStatus = 3
	StatusCanceled         OrderStatus = 4
	StatusRejected         OrderStatus = 5
	StatusDuplicateDeduped OrderStatus = 6
)

func (st OrderStatus) String() string {
	switch st {
	case StatusAccepted:
		return "ACCEPTED"
	case StatusPartiallyFilled:
		return "PARTIALLY_FILLED"
	case StatusFilled:
		return "FILLED"
	case StatusCanceled:
		return "CANCELED"
	case StatusRejected:
		return "REJECTED"
	case StatusDuplicateDeduped:
		return "DUPLICATE_DEDUPED"
	default:
		return "PENDING"
	}
}

type BotArchetype string

const (
	ArchetypeAvellanedaStoikov BotArchetype = "AVELLANEDA_STOIKOV"
	ArchetypeMomentumQuant     BotArchetype = "MOMENTUM_QUANT"
	ArchetypeMeanRevQuant      BotArchetype = "MEAN_REVERSION_QUANT"
	ArchetypeHawkesNoise       BotArchetype = "HAWKES_NOISE"
	ArchetypeSyndicateWash     BotArchetype = "SYNDICATE_WASH"
	ArchetypeSyndicateSpoofer  BotArchetype = "SYNDICATE_SPOOFER"
	ArchetypeSyndicateSweep    BotArchetype = "SYNDICATE_FLASH_SWEEP"
)

type MarketRegime string

const (
	RegimeCalm             MarketRegime = "CALM"
	RegimeHighVolatility   MarketRegime = "HIGH_VOLATILITY"
	RegimeFlashCrash       MarketRegime = "FLASH_CRASH"
	RegimeSpoofingAttack   MarketRegime = "SPOOFING_ATTACK"
	RegimeWashTrading      MarketRegime = "WASH_TRADING_RING"
	RegimeNetworkPartition MarketRegime = "NETWORK_PARTITION"
	RegimeBurstTsunami     MarketRegime = "BURST_TSUNAMI_20X"
)

// NormalizeRegime maps ergonomic aliases to canonical OpenAPI ChaosCampaignPayload enum values.
func NormalizeRegime(raw string) (MarketRegime, bool) {
	switch raw {
	case "CALM", "NORMAL":
		return RegimeCalm, true
	case "HIGH_VOLATILITY":
		return RegimeHighVolatility, true
	case "FLASH_CRASH":
		return RegimeFlashCrash, true
	case "SPOOFING_ATTACK", "SPOOFING":
		return RegimeSpoofingAttack, true
	case "WASH_TRADING_RING", "WASH_TRADING":
		return RegimeWashTrading, true
	case "NETWORK_PARTITION", "PARTITION":
		return RegimeNetworkPartition, true
	case "BURST_TSUNAMI_20X", "BURST_TSUNAMI":
		return RegimeBurstTsunami, true
	default:
		return "", false
	}
}

func ValidRegime(r MarketRegime) bool {
	_, ok := NormalizeRegime(string(r))
	return ok
}

// InstrumentSpec defines canonical symbol <-> instrument_id mappings per 00-hyperion-contract.md §4.3.
type InstrumentSpec struct {
	InstrumentID uint32
	Symbol       string
	BaseAsset    string
	QuoteAsset   string
	TickSizeFP   int64
	LotSizeFP    int64
	DefaultMidFP int64
}

func LookupInstrument(symbol string) InstrumentSpec {
	switch symbol {
	case "ETH-USDT":
		return InstrumentSpec{2, "ETH-USDT", "ETH", "USDT", 1_000_000, 10_000, 3_500_00000000}
	case "SOL-USDT":
		return InstrumentSpec{3, "SOL-USDT", "SOL", "USDT", 100_000, 100_000, 180_00000000}
	case "HYPE-USDT":
		return InstrumentSpec{4, "HYPE-USDT", "HYPE", "USDT", 10_000, 1_000_000, 25_00000000}
	default:
		return InstrumentSpec{1, "BTC-USDT", "BTC", "USDT", 1_000_000, 1_000, 68_425_00000000}
	}
}

type ActionKind uint8

const (
	ActionNone   ActionKind = 0
	ActionSubmit ActionKind = 1
	ActionCancel ActionKind = 2
)

// OrderIntent is a cache-friendly, value-typed order or cancellation instruction
// emitted by an autonomous trading agent or adversarial syndicate bot.
// All prices and quantities are fixed-point int64 scaled by 1e8.
type OrderIntent struct {
	SeqID            uint64
	ClientOrderID    uint64
	TargetOrderID    uint64 // Populated when Kind == ActionCancel
	TargetServerID   uint64 // Server order_id when canceling via REST/gRPC
	IdempotencyKey   uint64
	AccountNum       uint64 // Canonical uint64 account_id (e.g. 10001)
	InstrumentID     uint32 // Canonical instrument_id (1=BTC-USDT, 2=ETH-USDT, ...)
	BotID            uint32
	Kind             ActionKind
	Side             Side
	OrderType        OrderType
	DuplicateCount   uint8 // >= 2 when idempotent duplication chaos fault is injected
	CorruptSignature bool  // true when invalid/expired signature chaos fault is injected
	AccountID        string
	Symbol           string
	Archetype        BotArchetype
	Price            int64 // 1e8 fixed-point
	Quantity         int64 // 1e8 fixed-point
	TimestampNs      int64
	ClockSkewNs      int64 // Injected drift for expired/future timestamp testing
	Nonce            uint64
}

// FormatClientOrderID appends a deterministic string ID without fmt.Sprintf reflection overhead.
func (o OrderIntent) FormatClientOrderID(dst []byte) []byte {
	dst = append(dst, "csim-"...)
	dst = strconv.AppendUint(dst, uint64(o.BotID), 10)
	dst = append(dst, '-')
	dst = strconv.AppendUint(dst, o.ClientOrderID, 10)
	return dst
}

// FormatIdempotencyKey appends a deterministic idempotency key string without heap churn.
func (o OrderIntent) FormatIdempotencyKey(dst []byte) []byte {
	dst = append(dst, "idem-"...)
	dst = strconv.AppendUint(dst, uint64(o.BotID), 10)
	dst = append(dst, '-')
	dst = strconv.AppendUint(dst, o.IdempotencyKey, 10)
	return dst
}

// MarketSnapshot represents an immutable atomic point-in-time view of L2 microstructure
// shared lock-free across all 1,000+ simulated bots.
// All monetary/quantity fields are fixed-point int64 scaled by 1e8.
type MarketSnapshot struct {
	Symbol         string       `json:"symbol"`
	InstrumentID   uint32       `json:"instrument_id"`
	Sequence       uint64       `json:"sequence"`
	TimestampNs    int64        `json:"timestamp_ns"`
	MidPrice       int64        `json:"mid_price"`
	BestBid        int64        `json:"best_bid"`
	BestAsk        int64        `json:"best_ask"`
	BidDepth5      int64        `json:"bid_depth_5"`
	AskDepth5      int64        `json:"ask_depth_5"`
	VWAP           int64        `json:"vwap"`
	ShortEMA       int64        `json:"short_ema"`
	LongEMA        int64        `json:"long_ema"`
	RealizedVolBps int64        `json:"realized_vol_bps"` // Bounded [5, 25000] bps
	TickSize       int64        `json:"tick_size"`
	LotSize        int64        `json:"lot_size"`
	Regime         MarketRegime `json:"regime"`
}

// FillEvent represents a single matched trade execution in 1e8 fixed-point units.
type FillEvent struct {
	TradeID        uint64 `json:"trade_id"`
	ClientOrderID  uint64 `json:"client_order_id"`
	ServerOrderID  uint64 `json:"server_order_id"`
	IdempotencyKey uint64 `json:"idempotency_key"`
	BotID          uint32 `json:"bot_id"`
	AccountID      string `json:"account_id"`
	CounterpartyID string `json:"counterparty_id"`
	Symbol         string `json:"symbol"`
	Side           Side   `json:"side"`
	Price          int64  `json:"price"`     // 1e8
	Quantity       int64  `json:"quantity"`  // 1e8
	FeeQuote       int64  `json:"fee_quote"` // 1e8 quote units
	IsMaker        bool   `json:"is_maker"`
	TimestampNs    int64  `json:"timestamp_ns"`
}

// SubmitResult captures the synchronous or asynchronous response from Gateway/gRPC/MockExchange.
type SubmitResult struct {
	ClientOrderID  uint64
	ServerOrderID  uint64
	IdempotencyKey uint64
	BotID          uint32
	Status         OrderStatus
	AcceptedQty    int64
	FilledQty      int64
	RemainingQty   int64
	Fills          []FillEvent
	RejectReason   string
	LatencyNs      int64
	TransportDrop  bool // True when intentionally dropped by ToxicProxy / NetworkPartition
}

// AccountBalance represents an account's exact 1e8 fixed-point balances on either shadow or server side.
type AccountBalance struct {
	AccountID      string `json:"account_id"`
	BotID          uint32 `json:"bot_id"`
	AvailableQuote int64  `json:"available_quote"`
	LockedQuote    int64  `json:"locked_quote"`
	TotalQuote     int64  `json:"total_quote"`
	AvailableBase  int64  `json:"available_base"`
	LockedBase     int64  `json:"locked_base"`
	TotalBase      int64  `json:"total_base"`
}
