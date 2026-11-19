package httpapi

import (
	"encoding/hex"
	"fmt"
	"math/big"
	"strconv"
	"strings"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
)

// All DTO structs strictly follow contracts/gateway-openapi.yaml.
// ABSOLUTE INVARIANT: ZERO float32/float64. All monetary/quantity fields are decimal strings (scale 1e8).

type ErrorEnvelope struct {
	Code         string `json:"code"`
	Message      string `json:"message"`
	RejectReason string `json:"reject_reason,omitempty"`
	TraceID      string `json:"trace_id"`
	TsNS         string `json:"ts_ns"`
}

type HealthStatusResponse struct {
	Status          string            `json:"status"`
	Service         string            `json:"service"`
	Version         string            `json:"version"`
	EngineSeq       string            `json:"engine_seq"`
	TsNS            string            `json:"ts_ns"`
	CircuitBreakers map[string]string `json:"circuit_breakers,omitempty"`
}

type SubmitOrderPayload struct {
	ClientOrderID  string `json:"client_order_id"`
	AccountID      string `json:"account_id"`
	Symbol         string `json:"symbol"`
	Side           string `json:"side"`
	OrderType      string `json:"order_type"`
	STPMode        string `json:"stp_mode,omitempty"`
	PriceFP        string `json:"price_fp"`
	QtyFP          string `json:"qty_fp"`
	MaxSlippageBps uint32 `json:"max_slippage_bps,omitempty"`
}

type ModifyOrderPayload struct {
	ClientOrderID string `json:"client_order_id,omitempty"`
	AccountID     string `json:"account_id"`
	Symbol        string `json:"symbol"`
	NewPriceFP    string `json:"new_price_fp"`
	NewQtyFP      string `json:"new_qty_fp"`
}

type ExecutionFillDto struct {
	FillID         string `json:"fill_id"`
	EngineSeq      string `json:"engine_seq"`
	Symbol         string `json:"symbol"`
	MakerOrderID   string `json:"maker_order_id"`
	TakerOrderID   string `json:"taker_order_id"`
	MakerAccountID string `json:"maker_account_id"`
	TakerAccountID string `json:"taker_account_id"`
	AggressorSide  string `json:"aggressor_side"`
	PriceFP        string `json:"price_fp"`
	QtyFP          string `json:"qty_fp"`
	NotionalFP     string `json:"notional_fp"`
	MakerFeeFP     string `json:"maker_fee_fp"`
	TakerFeeFP     string `json:"taker_fee_fp"`
	ExecTsNS       string `json:"exec_ts_ns"`
	WalBlake3Hash  string `json:"wal_blake3_hash,omitempty"`
}

type OrderAckDto struct {
	EngineSeq      string             `json:"engine_seq"`
	OrderID        string             `json:"order_id"`
	ClientOrderID  string             `json:"client_order_id"`
	AccountID      string             `json:"account_id"`
	Symbol         string             `json:"symbol"`
	Status         string             `json:"status"`
	RejectReason   string             `json:"reject_reason,omitempty"`
	RejectDetail   string             `json:"reject_detail,omitempty"`
	FilledQtyFP    string             `json:"filled_qty_fp"`
	RemainingQtyFP string             `json:"remaining_qty_fp"`
	AvgFillPriceFP string             `json:"avg_fill_price_fp"`
	EngineTsNS     string             `json:"engine_ts_ns"`
	WalBlake3Hash  string             `json:"wal_blake3_hash,omitempty"`
	Fills          []ExecutionFillDto `json:"fills"`
}

type CreditDepositPayload struct {
	DepositID string `json:"deposit_id"`
	AccountID string `json:"account_id"`
	Asset     string `json:"asset"`
	AmountFP  string `json:"amount_fp"`
}

type PriceLevelDto struct {
	PriceFP    string `json:"price_fp"`
	TotalQtyFP string `json:"total_qty_fp"`
	OrderCount uint32 `json:"order_count"`
}

type L2BookSnapshotDto struct {
	EngineSeq       string          `json:"engine_seq"`
	Symbol          string          `json:"symbol"`
	Bids            []PriceLevelDto `json:"bids"`
	Asks            []PriceLevelDto `json:"asks"`
	BestBidFP       string          `json:"best_bid_fp"`
	BestAskFP       string          `json:"best_ask_fp"`
	MidPriceFP      string          `json:"mid_price_fp"`
	SpreadFP        string          `json:"spread_fp"`
	SnapshotTsNS    string          `json:"snapshot_ts_ns"`
	StateRootBlake3 string          `json:"state_root_blake3"`
}

type MerkleProofNodeDto struct {
	SiblingHash   string `json:"sibling_hash"`
	IsLeftSibling bool   `json:"is_left_sibling"`
	Depth         uint32 `json:"depth"`
}

type AccountAssetBalanceDto struct {
	Asset       string `json:"asset"`
	AvailableFP string `json:"available_fp"`
	LockedFP    string `json:"locked_fp"`
	TotalFP     string `json:"total_fp"`
}

type MerkleLedgerProofDto struct {
	EngineSeq             string                   `json:"engine_seq"`
	AccountID             string                   `json:"account_id"`
	Balances              []AccountAssetBalanceDto `json:"balances"`
	LeafBlake3Hash        string                   `json:"leaf_blake3_hash"`
	StateRootBlake3       string                   `json:"state_root_blake3"`
	ProofPath             []MerkleProofNodeDto     `json:"proof_path"`
	SystemTotalDepositsFP string                   `json:"system_total_deposits_fp"`
	SystemTotalUserEqFP   string                   `json:"system_total_user_equity_fp"`
	TreasuryFeePoolFP     string                   `json:"treasury_fee_pool_fp"`
	DoubleEntryBalanced   bool                     `json:"double_entry_balanced"`
	ProofTsNS             string                   `json:"proof_ts_ns"`
}

type OhlcvCandleDto struct {
	Symbol               string `json:"symbol"`
	Interval             string `json:"interval"`
	BucketOpenTsNS       string `json:"bucket_open_ts_ns"`
	BucketCloseTsNS      string `json:"bucket_close_ts_ns"`
	OpenFP               string `json:"open_fp"`
	HighFP               string `json:"high_fp"`
	LowFP                string `json:"low_fp"`
	CloseFP              string `json:"close_fp"`
	BaseVolumeFP         string `json:"base_volume_fp"`
	QuoteVolumeFP        string `json:"quote_volume_fp"`
	TakerBuyBaseVolFP    string `json:"taker_buy_base_volume_fp"`
	TakerSellBaseVolFP   string `json:"taker_sell_base_volume_fp"`
	VwapFP               string `json:"vwap_fp"`
	TwapFP               string `json:"twap_fp"`
	TradeCount           string `json:"trade_count"`
	IsClosed             bool   `json:"is_closed"`
}

type MicrostructureDto struct {
	Symbol              string `json:"symbol"`
	EngineSeq           string `json:"engine_seq"`
	TsNS                string `json:"ts_ns"`
	MidPriceFP          string `json:"mid_price_fp"`
	SpreadFP            string `json:"spread_fp"`
	SpreadBps           uint32 `json:"spread_bps"`
	RollingVwapFP       string `json:"rolling_vwap_fp"`
	RollingTwapFP       string `json:"rolling_twap_fp"`
	OfiFP               string `json:"ofi_fp"`
	BookImbalanceTop5   int64  `json:"book_imbalance_top5_ppm"`
	BookImbalanceTop10  int64  `json:"book_imbalance_top10_ppm"`
	BookImbalanceTop20  int64  `json:"book_imbalance_top20_ppm"`
	KylesLambdaFP       string `json:"kyles_lambda_fp"`
}

type SHAPAttributionDto struct {
	FeatureName         string `json:"feature_name"`
	FeatureValueFP      string `json:"feature_value_fp"`
	BaselineValueFP     string `json:"baseline_value_fp"`
	SHAPContributionPPM int64  `json:"shap_contribution_ppm"`
	ImportanceRank      uint32 `json:"importance_rank"`
}

type AnomalyAlertDto struct {
	AlertID            string               `json:"alert_id"`
	EngineSeq          string               `json:"engine_seq"`
	Symbol             string               `json:"symbol"`
	PatternType        string               `json:"pattern_type"`
	SeverityScore      uint32               `json:"severity_score"`
	ConfidencePPM      uint32               `json:"confidence_ppm"`
	OffenderAccountIDs []string             `json:"offender_account_ids"`
	SHAPAttributions   []SHAPAttributionDto `json:"shap_attributions"`
	DetectedTsNS       string               `json:"detected_ts_ns"`
	InferenceLatencyUS uint64               `json:"inference_latency_us"`
	ModelVersion       string               `json:"model_version,omitempty"`
}

type TimelineForkPayload struct {
	ForkName              string   `json:"fork_name"`
	Symbol                string   `json:"symbol"`
	StartEngineSeq        string   `json:"start_engine_seq"`
	EndEngineSeq          string   `json:"end_engine_seq"`
	MakerFeePPMOverride   *int64   `json:"maker_fee_ppm_override,omitempty"`
	TakerFeePPMOverride   *int64   `json:"taker_fee_ppm_override,omitempty"`
	MaxOrderQtyFPOverride *string  `json:"max_order_qty_fp_override,omitempty"`
	BlockedAccountIDs     []string `json:"blocked_account_ids,omitempty"`
}

type StateTreeDiffDto struct {
	ForkID                  string `json:"fork_id"`
	Symbol                  string `json:"symbol"`
	StartEngineSeq          string `json:"start_engine_seq"`
	EndEngineSeq            string `json:"end_engine_seq"`
	BaselineStateRootBlake3 string `json:"baseline_state_root_blake3"`
	ForkedStateRootBlake3   string `json:"forked_state_root_blake3"`
	StateIdentical          bool   `json:"state_identical"`
	BaselineClearingPriceFP string `json:"baseline_clearing_price_fp"`
	ForkedClearingPriceFP   string `json:"forked_clearing_price_fp"`
	ClearingPriceDeltaFP    string `json:"clearing_price_delta_fp"`
	PreventedWashVolumeFP   string `json:"prevented_wash_volume_fp"`
	PreventedFillsCount     string `json:"prevented_fills_count"`
	RejectedOrdersCount     string `json:"rejected_orders_count"`
	ReplayedEventsCount     string `json:"replayed_events_count"`
	ComputationLatencyUS    uint64 `json:"computation_latency_us"`
}

type RegisterKeyPayload struct {
	AccountID     string `json:"account_id"`
	KeyID         string `json:"key_id"`
	Scheme        string `json:"scheme"` // "ed25519" or "hmac-sha256"
	PublicKeyHex  string `json:"public_key_hex,omitempty"`
	HMACSecretHex string `json:"hmac_secret_hex,omitempty"`
}

// --- Pure Fixed-Point Conversion Helpers ---

func u64Str(v uint64) string { return strconv.FormatUint(v, 10) }
func i64Str(v int64) string  { return strconv.FormatInt(v, 10) }

func hex32OrZero(b []byte) string {
	if len(b) == 0 {
		return strings.Repeat("0", 64)
	}
	return hex.EncodeToString(b)
}

func parseUint64Strict(raw string, field string) (uint64, error) {
	v, err := strconv.ParseUint(strings.TrimSpace(raw), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid %s: %w", field, err)
	}
	return v, nil
}

func parseBigUint128(raw string, field string) (*hyperionv1.U128, error) {
	bi, ok := new(big.Int).SetString(strings.TrimSpace(raw), 10)
	if !ok || bi.Sign() <= 0 {
		return nil, fmt.Errorf("invalid positive fixed-point %s", field)
	}
	return fixedpoint.ToProtoU128(bi)
}

func parseOrderSide(s string) (hyperionv1.OrderSide, error) {
	switch strings.ToUpper(strings.TrimSpace(s)) {
	case "BUY":
		return hyperionv1.OrderSide_ORDER_SIDE_BUY, nil
	case "SELL":
		return hyperionv1.OrderSide_ORDER_SIDE_SELL, nil
	default:
		return hyperionv1.OrderSide_ORDER_SIDE_UNSPECIFIED, fmt.Errorf("invalid side %q", s)
	}
}

func formatOrderSide(s hyperionv1.OrderSide) string {
	if s == hyperionv1.OrderSide_ORDER_SIDE_SELL {
		return "SELL"
	}
	return "BUY"
}

func parseOrderType(s string) (hyperionv1.OrderType, error) {
	switch strings.ToUpper(strings.TrimSpace(s)) {
	case "LIMIT":
		return hyperionv1.OrderType_ORDER_TYPE_LIMIT, nil
	case "MARKET":
		return hyperionv1.OrderType_ORDER_TYPE_MARKET, nil
	case "IOC":
		return hyperionv1.OrderType_ORDER_TYPE_IOC, nil
	case "FOK":
		return hyperionv1.OrderType_ORDER_TYPE_FOK, nil
	case "POST_ONLY":
		return hyperionv1.OrderType_ORDER_TYPE_POST_ONLY, nil
	default:
		return hyperionv1.OrderType_ORDER_TYPE_UNSPECIFIED, fmt.Errorf("invalid order_type %q", s)
	}
}

func parseSTPMode(s string) hyperionv1.SelfTradePreventionMode {
	switch strings.ToUpper(strings.TrimSpace(s)) {
	case "NONE":
		return hyperionv1.SelfTradePreventionMode_SELF_TRADE_PREVENTION_MODE_NONE
	case "CANCEL_AGGRESSING":
		return hyperionv1.SelfTradePreventionMode_SELF_TRADE_PREVENTION_MODE_CANCEL_AGGRESSING
	case "CANCEL_BOTH":
		return hyperionv1.SelfTradePreventionMode_SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH
	default:
		return hyperionv1.SelfTradePreventionMode_SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING
	}
}

func formatOrderStatus(st hyperionv1.OrderStatus) string {
	return strings.TrimPrefix(st.String(), "ORDER_STATUS_")
}

func parseCandleInterval(s string) (hyperionv1.CandleInterval, error) {
	switch strings.ToLower(strings.TrimSpace(s)) {
	case "1s":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_1S, nil
	case "5s":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_5S, nil
	case "1m":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_1M, nil
	case "5m":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_5M, nil
	case "15m":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_15M, nil
	case "1h":
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_1H, nil
	default:
		return hyperionv1.CandleInterval_CANDLE_INTERVAL_UNSPECIFIED, fmt.Errorf("unsupported candle interval %q", s)
	}
}

func formatCandleInterval(iv hyperionv1.CandleInterval) string {
	switch iv {
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_1S:
		return "1s"
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_5S:
		return "5s"
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_1M:
		return "1m"
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_5M:
		return "5m"
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_15M:
		return "15m"
	case hyperionv1.CandleInterval_CANDLE_INTERVAL_1H:
		return "1h"
	default:
		return "1s"
	}
}

func mapOrderAckToDto(ack *hyperionv1.OrderAck) OrderAckDto {
	fills := make([]ExecutionFillDto, 0, len(ack.GetFills()))
	for _, f := range ack.GetFills() {
		fills = append(fills, ExecutionFillDto{
			FillID:         u64Str(f.GetFillId()),
			EngineSeq:      u64Str(f.GetEngineSeq()),
			Symbol:         f.GetSymbol(),
			MakerOrderID:   u64Str(f.GetMakerOrderId()),
			TakerOrderID:   u64Str(f.GetTakerOrderId()),
			MakerAccountID: u64Str(f.GetMakerAccountId()),
			TakerAccountID: u64Str(f.GetTakerAccountId()),
			AggressorSide:  formatOrderSide(f.GetAggressorSide()),
			PriceFP:        u64Str(f.GetPriceFp()),
			QtyFP:          u64Str(f.GetQtyFp()),
			NotionalFP:     fixedpoint.FromProtoU128(f.GetNotionalFp()).String(),
			MakerFeeFP:     i64Str(f.GetMakerFeeFp()),
			TakerFeeFP:     i64Str(f.GetTakerFeeFp()),
			ExecTsNS:       u64Str(f.GetExecTsNs()),
			WalBlake3Hash:  hex.EncodeToString(f.GetWalBlake3Hash()),
		})
	}
	var rej string
	if ack.GetRejectReason() != hyperionv1.RejectReason_REJECT_REASON_UNSPECIFIED &&
		ack.GetRejectReason() != hyperionv1.RejectReason_REJECT_REASON_NONE {
		rej = strings.TrimPrefix(ack.GetRejectReason().String(), "REJECT_REASON_")
	}
	return OrderAckDto{
		EngineSeq:      u64Str(ack.GetEngineSeq()),
		OrderID:        u64Str(ack.GetOrderId()),
		ClientOrderID:  ack.GetClientOrderId(),
		AccountID:      u64Str(ack.GetAccountId()),
		Symbol:         ack.GetSymbol(),
		Status:         formatOrderStatus(ack.GetStatus()),
		RejectReason:   rej,
		RejectDetail:   ack.GetRejectDetail(),
		FilledQtyFP:    u64Str(ack.GetFilledQtyFp()),
		RemainingQtyFP: u64Str(ack.GetRemainingQtyFp()),
		AvgFillPriceFP: u64Str(ack.GetAvgFillPriceFp()),
		EngineTsNS:     u64Str(ack.GetEngineTsNs()),
		WalBlake3Hash:  hex.EncodeToString(ack.GetWalBlake3Hash()),
		Fills:          fills,
	}
}

func mapL2SnapshotToDto(snap *hyperionv1.L2BookSnapshot) L2BookSnapshotDto {
	bids := make([]PriceLevelDto, 0, len(snap.GetBids()))
	for _, lv := range snap.GetBids() {
		bids = append(bids, PriceLevelDto{
			PriceFP:    u64Str(lv.GetPriceFp()),
			TotalQtyFP: u64Str(lv.GetTotalQtyFp()),
			OrderCount: lv.GetOrderCount(),
		})
	}
	asks := make([]PriceLevelDto, 0, len(snap.GetAsks()))
	for _, lv := range snap.GetAsks() {
		asks = append(asks, PriceLevelDto{
			PriceFP:    u64Str(lv.GetPriceFp()),
			TotalQtyFP: u64Str(lv.GetTotalQtyFp()),
			OrderCount: lv.GetOrderCount(),
		})
	}
	return L2BookSnapshotDto{
		EngineSeq:       u64Str(snap.GetEngineSeq()),
		Symbol:          snap.GetSymbol(),
		Bids:            bids,
		Asks:            asks,
		BestBidFP:       u64Str(snap.GetBestBidFp()),
		BestAskFP:       u64Str(snap.GetBestAskFp()),
		MidPriceFP:      u64Str(snap.GetMidPriceFp()),
		SpreadFP:        u64Str(snap.GetSpreadFp()),
		SnapshotTsNS:    u64Str(snap.GetSnapshotTsNs()),
		StateRootBlake3: hex32OrZero(snap.GetStateRootBlake3()),
	}
}
