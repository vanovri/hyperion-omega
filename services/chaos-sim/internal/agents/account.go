package agents

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/binary"
	"strconv"
	"sync"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
)

const MaxOpenSlots = 8

type OpenOrderSlot struct {
	Active        bool
	ClientOrderID uint64
	ServerOrderID uint64
	Side          model.Side
	Price         int64 // 1e8
	RemainingQty  int64 // 1e8
	LockedAmount  int64 // 1e8 (Quote for BUY, Base for SELL)
	PlacedAtNs    int64
}

// BotAccount holds per-bot deterministic strategy state, solvency-enforced fixed-point (1e8)
// balances, and cryptographic identity. Hot per-tick fields are packed contiguously at the
// head of the struct for L1 cache locality; cold signing keys are placed at the tail.
type BotAccount struct {
	Mu              sync.Mutex
	BotID           uint32
	SpoofPhase      uint8
	RingIndex       uint8
	RingSize        uint8
	AccountNum      uint64 // Canonical uint64 account_id (10001 + BotID)
	TotalQuote      int64
	TotalBase       int64
	LockedQuote     int64
	LockedBase      int64
	InitialQuote    int64
	InitialBase     int64
	NextOrderSeq    uint64
	NextNonce       uint64
	HawkesIntensity float64
	LastStepNs      int64
	RiskAversion    float64 // Avellaneda-Stoikov gamma
	OrderIntensityK float64 // Avellaneda-Stoikov kappa
	SignalThreshold float64 // Quant z-score / momentum threshold
	RNG             rng.Stream
	Archetype       model.BotArchetype
	AccountID       string // Decimal string of AccountNum ("10001") matching OpenAPI Uint64String

	OpenOrders [MaxOpenSlots]OpenOrderSlot

	// Cold cryptographic & metadata fields (rarely touched during inner simulation math)
	Label      string // Human-readable archetype label ("bot-mm-0")
	KeyID      string // Cryptographic key identifier ("key-10001")
	PrivKey    ed25519.PrivateKey
	PubKey     ed25519.PublicKey
	HMACSecret [32]byte
	_pad       [64]byte // Prevents L1 cache-line false sharing across neighboring heap bots
}

// NewBotAccount constructs a deterministic bot account with reproducible Ed25519 & HMAC keys.
func NewBotAccount(masterSeed uint64, botID uint32, prefix string, archetype model.BotArchetype, initQuote, initBase int64) *BotAccount {
	accNum := 10_001 + uint64(botID)
	accID := strconv.FormatUint(accNum, 10)
	label := prefix + "-" + strconv.FormatUint(uint64(botID), 10)
	keyID := "key-" + accID
	stream := rng.NewStream(masterSeed, uint64(botID)+1)

	// Deterministic 32-byte seed for Ed25519 and HMAC secret (zero OS entropy dependency).
	var material [48]byte
	binary.LittleEndian.PutUint64(material[0:8], masterSeed)
	binary.LittleEndian.PutUint32(material[8:12], botID)
	copy(material[12:], []byte("HYPERION-CH07-BOT-KEY-V1"))
	edSeed := sha256.Sum256(material[:])
	priv := ed25519.NewKeyFromSeed(edSeed[:])
	pub := priv.Public().(ed25519.PublicKey)

	binary.LittleEndian.PutUint64(material[0:8], ^masterSeed)
	hmacSec := sha256.Sum256(material[:])

	// Heterogeneous per-bot strategy parameters drawn deterministically once at init.
	gamma := 0.05 + 0.45*stream.Float64() // [0.05, 0.50]
	kappa := 1.5 + 8.5*stream.Float64()   // [1.5, 10.0]
	thresh := 0.0008 + 0.0032*stream.Float64()

	return &BotAccount{
		BotID:           botID,
		AccountNum:      accNum,
		AccountID:       accID,
		Label:           label,
		KeyID:           keyID,
		Archetype:       archetype,
		RNG:             stream,
		PrivKey:         priv,
		PubKey:          pub,
		HMACSecret:      hmacSec,
		InitialQuote:    initQuote,
		InitialBase:     initBase,
		TotalQuote:      initQuote,
		TotalBase:       initBase,
		RiskAversion:    gamma,
		OrderIntensityK: kappa,
		SignalThreshold: thresh,
	}
}

func (b *BotAccount) AvailableQuote() int64 {
	avail := b.TotalQuote - b.LockedQuote
	if avail < 0 {
		return 0
	}
	return avail
}

func (b *BotAccount) AvailableBase() int64 {
	avail := b.TotalBase - b.LockedBase
	if avail < 0 {
		return 0
	}
	return avail
}

// ClampOrderToSolvency ensures the proposed order can NEVER cause negative cash or
// negative inventory bankruptcy, even under flash-crash slippage or maximum taker fees.
// Returns 0 if the account cannot afford at least 1 LotSize.
func (b *BotAccount) ClampOrderToSolvency(
	side model.Side,
	orderType model.OrderType,
	limitPrice int64,
	bestAsk int64,
	desiredQty int64,
	lotSize int64,
	maxFeeBps int64,
) int64 {
	if desiredQty <= 0 || lotSize <= 0 {
		return 0
	}
	if maxFeeBps < 0 {
		maxFeeBps = 0
	}

	if side == model.SideSell {
		avail := b.AvailableBase()
		if avail < lotSize {
			return 0
		}
		if desiredQty > avail {
			desiredQty = avail
		}
		return fixed.RoundToStep(desiredQty, lotSize)
	}

	if side != model.SideBuy {
		return 0
	}

	effPrice := limitPrice
	if orderType == model.OrderTypeMarket {
		// Use 5% collar above BestAsk for market buy solvency protection.
		baseAsk := bestAsk
		if baseAsk <= 0 {
			baseAsk = limitPrice
		}
		if baseAsk <= 0 {
			return 0
		}
		collar, err := fixed.MulDiv128(baseAsk, 10_500, 10_000)
		if err != nil || collar <= 0 {
			return 0
		}
		effPrice = collar
	}
	if effPrice <= 0 {
		return 0
	}

	availQuote := b.AvailableQuote()
	// Leave small rounding safety cushion so integer ceil fees never dip below 0.
	const safetyCushion int64 = 1_000 // 0.00001000 quote units
	if availQuote <= safetyCushion {
		return 0
	}
	usableQuote := availQuote - safetyCushion

	// Total cost per unit of qty: effPrice * (10000 + maxFeeBps) / 10000
	costPerBase, err := fixed.MulDiv128(effPrice, 10_000+maxFeeBps, 10_000)
	if err != nil || costPerBase <= 0 {
		return 0
	}

	maxQty, err := fixed.MulDiv128(usableQuote, fixed.Scale, costPerBase)
	if err != nil || maxQty < lotSize {
		return 0
	}
	if desiredQty > maxQty {
		desiredQty = maxQty
	}
	qty := fixed.RoundToStep(desiredQty, lotSize)
	if qty < lotSize {
		return 0
	}

	// Final exact verification of notional + fee <= usableQuote
	notional, err := fixed.QuoteNotional(effPrice, qty)
	if err != nil {
		return 0
	}
	fee, err := fixed.FeeFromBps(notional, maxFeeBps)
	if err != nil {
		return 0
	}
	totalReq, err := fixed.CheckedAdd(notional, fee+2)
	if err != nil || totalReq > usableQuote {
		qty -= lotSize
		if qty < lotSize {
			return 0
		}
	}
	return qty
}

// ReserveOrder locks funds locally when a resting order is submitted/accepted.
func (b *BotAccount) ReserveOrder(intent model.OrderIntent, maxFeeBps int64) bool {
	slotIdx := -1
	for i := 0; i < MaxOpenSlots; i++ {
		if !b.OpenOrders[i].Active {
			slotIdx = i
			break
		}
	}
	if slotIdx < 0 {
		return false
	}

	var lockAmt int64
	if intent.Side == model.SideBuy {
		notional, err := fixed.QuoteNotional(intent.Price, intent.Quantity)
		if err != nil {
			return false
		}
		fee, err := fixed.FeeFromBps(notional, maxFeeBps)
		if err != nil {
			return false
		}
		lockAmt = notional + fee
		if b.LockedQuote+lockAmt > b.TotalQuote {
			return false
		}
		b.LockedQuote += lockAmt
	} else {
		lockAmt = intent.Quantity
		if b.LockedBase+lockAmt > b.TotalBase {
			return false
		}
		b.LockedBase += lockAmt
	}

	b.OpenOrders[slotIdx] = OpenOrderSlot{
		Active:        true,
		ClientOrderID: intent.ClientOrderID,
		Side:          intent.Side,
		Price:         intent.Price,
		RemainingQty:  intent.Quantity,
		LockedAmount:  lockAmt,
		PlacedAtNs:    intent.TimestampNs,
	}
	return true
}

// HasFreeSlot returns true if the bot has room for another resting open order.
func (b *BotAccount) HasFreeSlot() bool {
	for i := 0; i < MaxOpenSlots; i++ {
		if !b.OpenOrders[i].Active {
			return true
		}
	}
	return false
}

// ApplySubmitResult reconciles synchronous fills, resting locks, or rejections on the bot's state.
func (b *BotAccount) ApplySubmitResult(intent model.OrderIntent, res model.SubmitResult, maxFeeBps int64) {
	if res.TransportDrop || res.Status == model.StatusDuplicateDeduped {
		return
	}

	// First release any pre-reservation for this ClientOrderID so we compute exact state cleanly.
	b.releaseSlotByClientID(intent.ClientOrderID)

	if res.Status == model.StatusRejected || res.Status == model.StatusCanceled && len(res.Fills) == 0 {
		return
	}

	for _, f := range res.Fills {
		notional, err := fixed.QuoteNotional(f.Price, f.Quantity)
		if err != nil {
			continue
		}
		if f.Side == model.SideBuy {
			b.TotalBase += f.Quantity
			b.TotalQuote -= (notional + f.FeeQuote)
		} else if f.Side == model.SideSell {
			b.TotalBase -= f.Quantity
			b.TotalQuote += (notional - f.FeeQuote)
		}
	}

	// If resting quantity remains on book, lock exact remaining requirement.
	if (res.Status == model.StatusAccepted || res.Status == model.StatusPartiallyFilled) && res.RemainingQty > 0 {
		resting := intent
		resting.Quantity = res.RemainingQty
		if b.ReserveOrder(resting, maxFeeBps) {
			for i := 0; i < MaxOpenSlots; i++ {
				if b.OpenOrders[i].Active && b.OpenOrders[i].ClientOrderID == intent.ClientOrderID {
					b.OpenOrders[i].ServerOrderID = res.ServerOrderID
					break
				}
			}
		}
	}
}

// ApplyAsyncFill updates local bot balances when a resting maker order is matched asynchronously.
func (b *BotAccount) ApplyAsyncFill(f model.FillEvent) {
	notional, err := fixed.QuoteNotional(f.Price, f.Quantity)
	if err != nil {
		return
	}
	if f.Side == model.SideBuy {
		b.TotalBase += f.Quantity
		b.TotalQuote -= (notional + f.FeeQuote)
	} else if f.Side == model.SideSell {
		b.TotalBase -= f.Quantity
		b.TotalQuote += (notional - f.FeeQuote)
	}

	for i := 0; i < MaxOpenSlots; i++ {
		slot := &b.OpenOrders[i]
		if slot.Active && slot.ClientOrderID == f.ClientOrderID {
			if f.Quantity >= slot.RemainingQty {
				b.releaseSlotIndex(i)
			} else {
				// Release proportional lock
				relLock, _ := fixed.MulDiv128(slot.LockedAmount, f.Quantity, slot.RemainingQty)
				if slot.Side == model.SideBuy {
					b.LockedQuote -= relLock
					if b.LockedQuote < 0 {
						b.LockedQuote = 0
					}
				} else {
					b.LockedBase -= relLock
					if b.LockedBase < 0 {
						b.LockedBase = 0
					}
				}
				slot.LockedAmount -= relLock
				slot.RemainingQty -= f.Quantity
			}
			break
		}
	}
}

// ApplyCanceled releases any remaining lock for a canceled resting order.
func (b *BotAccount) ApplyCanceled(clientOrderID uint64) {
	b.releaseSlotByClientID(clientOrderID)
}

func (b *BotAccount) releaseSlotByClientID(clientOrderID uint64) {
	for i := 0; i < MaxOpenSlots; i++ {
		if b.OpenOrders[i].Active && b.OpenOrders[i].ClientOrderID == clientOrderID {
			b.releaseSlotIndex(i)
			return
		}
	}
}

func (b *BotAccount) releaseSlotIndex(i int) {
	slot := &b.OpenOrders[i]
	if !slot.Active {
		return
	}
	if slot.Side == model.SideBuy {
		b.LockedQuote -= slot.LockedAmount
		if b.LockedQuote < 0 {
			b.LockedQuote = 0
		}
	} else {
		b.LockedBase -= slot.LockedAmount
		if b.LockedBase < 0 {
			b.LockedBase = 0
		}
	}
	*slot = OpenOrderSlot{}
}

func (b *BotAccount) NextClientIDs() (clientOrderID uint64, idempotencyKey uint64, nonce uint64) {
	b.NextOrderSeq++
	b.NextNonce++
	// Encode BotID in upper 20 bits and monotonic sequence in lower 44 bits for globally unique uint64 IDs.
	uid := (uint64(b.BotID) << 44) | (b.NextOrderSeq & 0xFFFFFFFFFFF)
	return uid, uid ^ 0x5A5A5A5A5A5A5A5A, b.NextNonce
}

func (b *BotAccount) SnapshotBalance() model.AccountBalance {
	return model.AccountBalance{
		AccountID:      b.AccountID,
		BotID:          b.BotID,
		AvailableQuote: b.AvailableQuote(),
		LockedQuote:    b.LockedQuote,
		TotalQuote:     b.TotalQuote,
		AvailableBase:  b.AvailableBase(),
		LockedBase:     b.LockedBase,
		TotalBase:      b.TotalBase,
	}
}
