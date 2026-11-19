package auditor

import (
	"fmt"
	"math/big"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

type ViolationType string

const (
	ViolationPhantomFill       ViolationType = "PHANTOM_FILL"
	ViolationLostOrder         ViolationType = "LOST_ORDER"
	ViolationDoubleExecution   ViolationType = "IDEMPOTENCY_DOUBLE_EXECUTION"
	ViolationBalanceMismatch   ViolationType = "ACCOUNT_BALANCE_MISMATCH"
	ViolationNegativeBalance   ViolationType = "NEGATIVE_BALANCE_BANKRUPTCY"
	ViolationGlobalConservation ViolationType = "GLOBAL_DOUBLE_ENTRY_DRIFT"
)

type Violation struct {
	TimestampNs int64         `json:"timestamp_ns"`
	Type        ViolationType `json:"type"`
	AccountID   string        `json:"account_id,omitempty"`
	ClientOrder uint64        `json:"client_order_id,omitempty"`
	DeltaUnits  int64         `json:"delta_units"`       // In 1e8 units (1 cent = 1,000,000 units)
	ExceedsCent bool          `json:"exceeds_one_cent"`  // True when |DeltaUnits| >= 1,000,000
	Detail      string        `json:"detail"`
}

type AuditReport struct {
	AuditSeq              uint64      `json:"audit_seq"`
	TimestampNs           int64       `json:"timestamp_ns"`
	Passed                bool        `json:"passed"`
	AccountsAudited       int         `json:"accounts_audited"`
	OrdersTracked         int         `json:"orders_tracked"`
	FillsVerified         uint64      `json:"fills_verified"`
	DuplicateDedupes      uint64      `json:"duplicate_dedupes"`
	MaxAbsQuoteDriftUnits int64       `json:"max_abs_quote_drift_units"`
	MaxAbsBaseDriftUnits  int64       `json:"max_abs_base_drift_units"`
	GlobalQuoteConservationDiff string `json:"global_quote_conservation_diff"`
	GlobalBaseConservationDiff  string `json:"global_base_conservation_diff"`
	Violations            []Violation `json:"violations"`
}

type trackedOrder struct {
	ClientOrderID  uint64
	ServerOrderID  uint64
	IdempotencyKey uint64
	AccountID      string
	BotID          uint32
	Side           model.Side
	SubmittedQty   int64
	FilledQty      int64
	Acknowledged   bool
	Terminal       bool // Filled, Canceled, or Rejected
}

// ServerStateProvider is implemented by both MockExchange and remote Gateway/Ledger adapters
// so InvariantVerifier can continuously reconcile local bot accounting against server truth.
type ServerStateProvider interface {
	FetchBalances() (accounts map[string]model.AccountBalance, treasuryFeesQuote int64, err error)
	HasOrderRecord(clientOrderID uint64) (exists bool, terminal bool)
}

// InvariantVerifier maintains an independent double-entry shadow ledger of all simulated
// bots and continuously audits server fills, idempotency deduplication, order existence,
// and exact 1e8 integer balances (detecting discrepancies down to 1e-8 = 1/1,000,000th of 1 cent).
type InvariantVerifier struct {
	mu sync.RWMutex

	initialQuoteByAcc map[string]int64
	initialBaseByAcc  map[string]int64
	shadowQuoteByAcc  map[string]int64
	shadowBaseByAcc   map[string]int64

	orders         map[uint64]*trackedOrder
	seenTradeIDs   map[uint64]struct{}
	idemServerIDs  map[uint64]uint64 // IdempotencyKey -> first ServerOrderID

	totalFillsVerified   atomic.Uint64
	totalDuplicateDedupe atomic.Uint64
	auditSeq             atomic.Uint64

	eventViolations []Violation // Sticky event violations (phantom fills, double execution, negative balance)
	lastReport      AuditReport
}

func NewInvariantVerifier() *InvariantVerifier {
	return &InvariantVerifier{
		initialQuoteByAcc: make(map[string]int64, 1200),
		initialBaseByAcc:  make(map[string]int64, 1200),
		shadowQuoteByAcc:  make(map[string]int64, 1200),
		shadowBaseByAcc:   make(map[string]int64, 1200),
		orders:            make(map[uint64]*trackedOrder, 16384),
		seenTradeIDs:      make(map[uint64]struct{}, 16384),
		idemServerIDs:     make(map[uint64]uint64, 16384),
		lastReport: AuditReport{
			Passed:                      true,
			GlobalQuoteConservationDiff: "0",
			GlobalBaseConservationDiff:  "0",
		},
	}
}

// RegisterAccount seeds the shadow ledger with an account's exact initial deposit.
func (v *InvariantVerifier) RegisterAccount(accountID string, initQuote, initBase int64) {
	v.mu.Lock()
	defer v.mu.Unlock()
	v.initialQuoteByAcc[accountID] = initQuote
	v.initialBaseByAcc[accountID] = initBase
	v.shadowQuoteByAcc[accountID] = initQuote
	v.shadowBaseByAcc[accountID] = initBase
}

// RecordIntent registers an outgoing order submission before or upon network transmission.
func (v *InvariantVerifier) RecordIntent(intent model.OrderIntent) {
	if intent.Kind != model.ActionSubmit {
		return
	}
	v.mu.Lock()
	defer v.mu.Unlock()
	if _, exists := v.orders[intent.ClientOrderID]; !exists {
		v.orders[intent.ClientOrderID] = &trackedOrder{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			AccountID:      intent.AccountID,
			BotID:          intent.BotID,
			Side:           intent.Side,
			SubmittedQty:   intent.Quantity,
		}
	}
}

// RecordSubmitResult reconciles server acknowledgement, idempotency deduplication, and synchronous fills.
func (v *InvariantVerifier) RecordSubmitResult(intent model.OrderIntent, res model.SubmitResult) {
	if res.TransportDrop {
		return
	}
	nowNs := time.Now().UnixNano()

	v.mu.Lock()
	defer v.mu.Unlock()

	if intent.Kind == model.ActionCancel {
		if ord, ok := v.orders[intent.TargetOrderID]; ok && res.Status == model.StatusCanceled {
			ord.Terminal = true
		}
		return
	}

	if res.Status == model.StatusDuplicateDeduped {
		v.totalDuplicateDedupe.Add(1)
		if firstServerID, seen := v.idemServerIDs[intent.IdempotencyKey]; seen && res.ServerOrderID != 0 && res.ServerOrderID != firstServerID {
			v.appendViolationLocked(Violation{
				TimestampNs: nowNs,
				Type:        ViolationDoubleExecution,
				AccountID:   intent.AccountID,
				ClientOrder: intent.ClientOrderID,
				Detail:      fmt.Sprintf("duplicate idempotency key %d assigned distinct server order IDs (%d vs %d)", intent.IdempotencyKey, firstServerID, res.ServerOrderID),
			})
		}
		return
	}

	ord, exists := v.orders[intent.ClientOrderID]
	if !exists {
		ord = &trackedOrder{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			AccountID:      intent.AccountID,
			BotID:          intent.BotID,
			Side:           intent.Side,
			SubmittedQty:   intent.Quantity,
		}
		v.orders[intent.ClientOrderID] = ord
	}

	if res.Status == model.StatusRejected {
		ord.Terminal = true
		return
	}

	// Idempotency invariant: a single IdempotencyKey must NEVER create two distinct server orders.
	if res.ServerOrderID != 0 && intent.IdempotencyKey != 0 {
		if prevID, ok := v.idemServerIDs[intent.IdempotencyKey]; ok && prevID != res.ServerOrderID {
			v.appendViolationLocked(Violation{
				TimestampNs: nowNs,
				Type:        ViolationDoubleExecution,
				AccountID:   intent.AccountID,
				ClientOrder: intent.ClientOrderID,
				Detail:      fmt.Sprintf("idempotency key %d executed twice (%d and %d)", intent.IdempotencyKey, prevID, res.ServerOrderID),
			})
		} else {
			v.idemServerIDs[intent.IdempotencyKey] = res.ServerOrderID
		}
	}

	ord.Acknowledged = true
	ord.ServerOrderID = res.ServerOrderID
	if res.Status == model.StatusFilled || res.Status == model.StatusCanceled {
		ord.Terminal = true
	}

	for _, f := range res.Fills {
		v.applyFillLocked(f, nowNs)
	}
}

// RecordAsyncFill reconciles maker/asynchronous fills against the shadow ledger.
func (v *InvariantVerifier) RecordAsyncFill(f model.FillEvent) {
	nowNs := time.Now().UnixNano()
	v.mu.Lock()
	defer v.mu.Unlock()
	v.applyFillLocked(f, nowNs)
}

// InjectSyntheticPhantomFill allows unit tests to verify that the auditor immediately catches phantom fills.
func (v *InvariantVerifier) InjectSyntheticPhantomFill(f model.FillEvent) {
	v.RecordAsyncFill(f)
}

func (v *InvariantVerifier) applyFillLocked(f model.FillEvent, nowNs int64) {
	// Deduplicate by unique TradeID + Side/Account leg key
	legKey := (f.TradeID << 1)
	if f.Side == model.SideSell {
		legKey |= 1
	}
	if f.TradeID != 0 {
		if _, dup := v.seenTradeIDs[legKey]; dup {
			return
		}
		v.seenTradeIDs[legKey] = struct{}{}
	}

	ord, exists := v.orders[f.ClientOrderID]
	if !exists {
		v.appendViolationLocked(Violation{
			TimestampNs: nowNs,
			Type:        ViolationPhantomFill,
			AccountID:   f.AccountID,
			ClientOrder: f.ClientOrderID,
			DeltaUnits:  f.Quantity,
			ExceedsCent: fixed.Abs(f.Quantity) >= fixed.CentUnits,
			Detail:      fmt.Sprintf("phantom fill on unknown client_order_id=%d qty=%d", f.ClientOrderID, f.Quantity),
		})
		return
	}

	if ord.FilledQty+f.Quantity > ord.SubmittedQty {
		overfill := (ord.FilledQty + f.Quantity) - ord.SubmittedQty
		v.appendViolationLocked(Violation{
			TimestampNs: nowNs,
			Type:        ViolationPhantomFill,
			AccountID:   f.AccountID,
			ClientOrder: f.ClientOrderID,
			DeltaUnits:  overfill,
			ExceedsCent: overfill >= fixed.CentUnits,
			Detail:      fmt.Sprintf("overfill on order %d: cumulative=%d > submitted=%d", f.ClientOrderID, ord.FilledQty+f.Quantity, ord.SubmittedQty),
		})
	}
	ord.FilledQty += f.Quantity
	if ord.FilledQty >= ord.SubmittedQty {
		ord.Terminal = true
	}

	notional, err := fixed.QuoteNotional(f.Price, f.Quantity)
	if err != nil {
		return
	}
	if f.Side == model.SideBuy {
		v.shadowBaseByAcc[f.AccountID] += f.Quantity
		v.shadowQuoteByAcc[f.AccountID] -= (notional + f.FeeQuote)
	} else if f.Side == model.SideSell {
		v.shadowBaseByAcc[f.AccountID] -= f.Quantity
		v.shadowQuoteByAcc[f.AccountID] += (notional - f.FeeQuote)
	}

	if v.shadowQuoteByAcc[f.AccountID] < 0 {
		neg := v.shadowQuoteByAcc[f.AccountID]
		v.appendViolationLocked(Violation{
			TimestampNs: nowNs,
			Type:        ViolationNegativeBalance,
			AccountID:   f.AccountID,
			ClientOrder: f.ClientOrderID,
			DeltaUnits:  neg,
			ExceedsCent: fixed.Abs(neg) >= fixed.CentUnits,
			Detail:      fmt.Sprintf("negative shadow quote balance %d on account %s", neg, f.AccountID),
		})
	}
	if v.shadowBaseByAcc[f.AccountID] < 0 {
		neg := v.shadowBaseByAcc[f.AccountID]
		v.appendViolationLocked(Violation{
			TimestampNs: nowNs,
			Type:        ViolationNegativeBalance,
			AccountID:   f.AccountID,
			ClientOrder: f.ClientOrderID,
			DeltaUnits:  neg,
			ExceedsCent: fixed.Abs(neg) >= fixed.CentUnits,
			Detail:      fmt.Sprintf("negative shadow base balance %d on account %s", neg, f.AccountID),
		})
	}

	v.totalFillsVerified.Add(1)
}

// VerifyAgainstServer performs a complete account-by-account, order-by-order, and global
// conservation reconciliation against server state. Detects even a 1 base-unit (1e-8) drift!
func (v *InvariantVerifier) VerifyAgainstServer(provider ServerStateProvider) (AuditReport, error) {
	if provider == nil {
		return v.LastReport(), nil
	}
	serverBalances, treasuryFeesQuote, err := provider.FetchBalances()
	if err != nil {
		return v.LastReport(), err
	}

	nowNs := time.Now().UnixNano()
	seq := v.auditSeq.Add(1)

	v.mu.Lock()
	defer v.mu.Unlock()

	var maxQuoteDrift int64
	var maxBaseDrift int64

	expectedGlobalQuote := new(big.Int)
	expectedGlobalBase := new(big.Int)
	actualGlobalQuote := big.NewInt(treasuryFeesQuote)
	actualGlobalBase := new(big.Int)

	snapViolations := make([]Violation, 0)
	addSnap := func(vio Violation) {
		if len(snapViolations) < 256 {
			snapViolations = append(snapViolations, vio)
		}
	}

	for accID, initQ := range v.initialQuoteByAcc {
		expectedGlobalQuote.Add(expectedGlobalQuote, big.NewInt(initQ))
		expectedGlobalBase.Add(expectedGlobalBase, big.NewInt(v.initialBaseByAcc[accID]))

		shadowQ := v.shadowQuoteByAcc[accID]
		shadowB := v.shadowBaseByAcc[accID]

		srvBal, ok := serverBalances[accID]
		if !ok {
			addSnap(Violation{
				TimestampNs: nowNs,
				Type:        ViolationBalanceMismatch,
				AccountID:   accID,
				DeltaUnits:  shadowQ,
				ExceedsCent: fixed.Abs(shadowQ) >= fixed.CentUnits,
				Detail:      fmt.Sprintf("account %s missing from server ledger", accID),
			})
			continue
		}

		actualGlobalQuote.Add(actualGlobalQuote, big.NewInt(srvBal.TotalQuote))
		actualGlobalBase.Add(actualGlobalBase, big.NewInt(srvBal.TotalBase))

		if srvBal.TotalQuote < 0 || srvBal.AvailableQuote < 0 || srvBal.TotalBase < 0 || srvBal.AvailableBase < 0 {
			addSnap(Violation{
				TimestampNs: nowNs,
				Type:        ViolationNegativeBalance,
				AccountID:   accID,
				DeltaUnits:  srvBal.TotalQuote,
				ExceedsCent: true,
				Detail:      fmt.Sprintf("server reported negative balance for %s: quote=%d base=%d", accID, srvBal.TotalQuote, srvBal.TotalBase),
			})
		}

		qDrift := srvBal.TotalQuote - shadowQ
		bDrift := srvBal.TotalBase - shadowB
		if fixed.Abs(qDrift) > maxQuoteDrift {
			maxQuoteDrift = fixed.Abs(qDrift)
		}
		if fixed.Abs(bDrift) > maxBaseDrift {
			maxBaseDrift = fixed.Abs(bDrift)
		}

		if qDrift != 0 {
			addSnap(Violation{
				TimestampNs: nowNs,
				Type:        ViolationBalanceMismatch,
				AccountID:   accID,
				DeltaUnits:  qDrift,
				ExceedsCent: fixed.Abs(qDrift) >= fixed.CentUnits,
				Detail:      fmt.Sprintf("quote drift on %s: server=%d shadow=%d diff=%d", accID, srvBal.TotalQuote, shadowQ, qDrift),
			})
		}
		if bDrift != 0 {
			addSnap(Violation{
				TimestampNs: nowNs,
				Type:        ViolationBalanceMismatch,
				AccountID:   accID,
				DeltaUnits:  bDrift,
				ExceedsCent: fixed.Abs(bDrift) >= fixed.CentUnits,
				Detail:      fmt.Sprintf("base drift on %s: server=%d shadow=%d diff=%d", accID, srvBal.TotalBase, shadowB, bDrift),
			})
		}
	}

	// Verify acknowledged orders were never silently lost by the server
	for cid, ord := range v.orders {
		if ord.Acknowledged {
			exists, _ := provider.HasOrderRecord(cid)
			if !exists {
				addSnap(Violation{
					TimestampNs: nowNs,
					Type:        ViolationLostOrder,
					AccountID:   ord.AccountID,
					ClientOrder: cid,
					DeltaUnits:  ord.SubmittedQty - ord.FilledQty,
					ExceedsCent: (ord.SubmittedQty - ord.FilledQty) >= fixed.CentUnits,
					Detail:      fmt.Sprintf("acknowledged order client_id=%d server_id=%d missing on server", cid, ord.ServerOrderID),
				})
			}
		}
	}

	globalQuoteDiff := new(big.Int).Sub(actualGlobalQuote, expectedGlobalQuote)
	globalBaseDiff := new(big.Int).Sub(actualGlobalBase, expectedGlobalBase)

	if globalQuoteDiff.Sign() != 0 {
		delta := int64(0)
		if globalQuoteDiff.IsInt64() {
			delta = globalQuoteDiff.Int64()
		}
		addSnap(Violation{
			TimestampNs: nowNs,
			Type:        ViolationGlobalConservation,
			DeltaUnits:  delta,
			ExceedsCent: new(big.Int).Abs(globalQuoteDiff).Cmp(big.NewInt(fixed.CentUnits)) >= 0,
			Detail:      fmt.Sprintf("global quote double-entry conservation drift: diff=%s", globalQuoteDiff.String()),
		})
	}
	if globalBaseDiff.Sign() != 0 {
		delta := int64(0)
		if globalBaseDiff.IsInt64() {
			delta = globalBaseDiff.Int64()
		}
		addSnap(Violation{
			TimestampNs: nowNs,
			Type:        ViolationGlobalConservation,
			DeltaUnits:  delta,
			ExceedsCent: new(big.Int).Abs(globalBaseDiff).Cmp(big.NewInt(fixed.CentUnits)) >= 0,
			Detail:      fmt.Sprintf("global base double-entry conservation drift: diff=%s", globalBaseDiff.String()),
		})
	}

	vioCopy := make([]Violation, 0, len(v.eventViolations)+len(snapViolations))
	vioCopy = append(vioCopy, v.eventViolations...)
	vioCopy = append(vioCopy, snapViolations...)

	rep := AuditReport{
		AuditSeq:                    seq,
		TimestampNs:                 nowNs,
		Passed:                      len(vioCopy) == 0,
		AccountsAudited:             len(v.initialQuoteByAcc),
		OrdersTracked:               len(v.orders),
		FillsVerified:               v.totalFillsVerified.Load(),
		DuplicateDedupes:            v.totalDuplicateDedupe.Load(),
		MaxAbsQuoteDriftUnits:       maxQuoteDrift,
		MaxAbsBaseDriftUnits:        maxBaseDrift,
		GlobalQuoteConservationDiff: globalQuoteDiff.String(),
		GlobalBaseConservationDiff:  globalBaseDiff.String(),
		Violations:                  vioCopy,
	}
	v.lastReport = rep
	return rep, nil
}

func (v *InvariantVerifier) LastReport() AuditReport {
	v.mu.RLock()
	defer v.mu.RUnlock()
	rep := v.lastReport
	if len(v.eventViolations) > 0 && len(rep.Violations) == 0 {
		rep.Passed = false
		rep.Violations = make([]Violation, len(v.eventViolations))
		copy(rep.Violations, v.eventViolations)
	}
	return rep
}

func (v *InvariantVerifier) appendViolationLocked(vio Violation) {
	if len(v.eventViolations) < 256 {
		v.eventViolations = append(v.eventViolations, vio)
	}
}
