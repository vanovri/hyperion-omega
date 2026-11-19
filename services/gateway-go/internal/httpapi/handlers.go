package httpapi

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/grpcclient"
	"github.com/vanovri/hyperion/services/gateway-go/internal/idempotency"
	"github.com/vanovri/hyperion/services/gateway-go/internal/instruments"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/redpanda"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
)

// Server wires all Channel 03 subsystems into high-throughput HTTP/REST + Binary WebSocket handlers.
type Server struct {
	cfg        config.Config
	keyStore   *auth.KeyStore
	jwtAuth    *auth.JWTVerifier
	sigAuth    *auth.SignatureAuthenticator
	limiter    *ratelimit.HybridLimiter
	idem       *idempotency.Store
	mesh       *grpcclient.MeshClients
	wsHub      *ws.Hub
	rpBridge   *redpanda.Bridge
	metrics    *telemetry.Registry
	httpClient *http.Client
}

// Dependencies bundles constructed subsystems for Server.
type Dependencies struct {
	Config     config.Config
	KeyStore   *auth.KeyStore
	JWTAuth    *auth.JWTVerifier
	SigAuth    *auth.SignatureAuthenticator
	Limiter    *ratelimit.HybridLimiter
	Idem       *idempotency.Store
	Mesh       *grpcclient.MeshClients
	WSHub      *ws.Hub
	Redpanda   *redpanda.Bridge
	Metrics    *telemetry.Registry
}

// NewServer constructs the Gateway HTTP/WS router assembly.
func NewServer(deps Dependencies) *Server {
	if deps.Metrics == nil {
		deps.Metrics = telemetry.Global
	}
	return &Server{
		cfg:      deps.Config,
		keyStore: deps.KeyStore,
		jwtAuth:  deps.JWTAuth,
		sigAuth:  deps.SigAuth,
		limiter:  deps.Limiter,
		idem:     deps.Idem,
		mesh:     deps.Mesh,
		wsHub:    deps.WSHub,
		rpBridge: deps.Redpanda,
		metrics:  deps.Metrics,
		httpClient: &http.Client{
			Timeout: 3 * time.Second,
		},
	}
}

// PublicHandler returns the HTTP handler mounted on :8080 (REST `/api/v1/*`, `/ws`, `/healthz`).
func (s *Server) PublicHandler() http.Handler {
	mux := http.NewServeMux()

	mux.HandleFunc("GET /healthz", s.handleHealthz)
	mux.HandleFunc("GET /metrics", s.metrics.ServeHTTP)
	mux.HandleFunc("GET /ws", s.wsHub.ServeWS)

	// Trading Mutations (Ed25519 / HMAC-SHA256 + Timestamp/Nonce Anti-Replay + Hybrid Limiter + Idempotency-Key).
	mux.Handle("POST /api/v1/orders", s.idem.Wrap(http.HandlerFunc(s.handleSubmitOrder), true))
	mux.Handle("PATCH /api/v1/orders/{order_id}", s.idem.Wrap(http.HandlerFunc(s.handleModifyOrder), true))
	mux.Handle("DELETE /api/v1/orders/{order_id}", s.idem.Wrap(http.HandlerFunc(s.handleCancelOrder), true))

	// Accounts & Cryptographic Audit (JWT + Idempotency on deposit).
	mux.Handle("POST /api/v1/accounts/deposits", s.requireJWT(s.idem.Wrap(http.HandlerFunc(s.handleCreditDeposit), true)))
	mux.Handle("GET /api/v1/accounts/{account_id}/merkle-proof", s.requireJWT(http.HandlerFunc(s.handleGetMerkleProof)))
	mux.Handle("POST /api/v1/auth/keys", s.requireJWT(http.HandlerFunc(s.handleRegisterKey)))

	// Public Market & Quantitative Microstructure Analytics.
	mux.HandleFunc("GET /api/v1/market/orderbook", s.handleGetOrderbook)
	mux.HandleFunc("GET /api/v1/analytics/candles", s.handleGetCandles)
	mux.HandleFunc("GET /api/v1/analytics/microstructure", s.handleGetMicrostructure)

	// Sentinel-ML Fraud Alerts, Chronos Time-Travel, & Chaos-Sim Control Plane (JWT Protected).
	mux.Handle("GET /api/v1/fraud/alerts", s.requireJWT(http.HandlerFunc(s.handleListFraudAlerts)))
	mux.Handle("GET /api/v1/chronos/seek", s.requireJWT(http.HandlerFunc(s.handleChronosSeek)))
	mux.Handle("POST /api/v1/chronos/fork", s.requireJWT(http.HandlerFunc(s.handleChronosFork)))
	mux.Handle("POST /api/v1/chaos/campaigns", s.requireJWT(http.HandlerFunc(s.handleChaosCampaign)))

	return s.withObservabilityAndCORS(mux)
}

// AdminHandler returns the HTTP handler mounted on :8081 (`/healthz` and Prometheus `/metrics`).
func (s *Server) AdminHandler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", s.handleHealthz)
	mux.HandleFunc("GET /metrics", s.metrics.ServeHTTP)
	return mux
}

func (s *Server) withObservabilityAndCORS(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.metrics.HTTPRequestsTotal.Add(1)
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, PATCH, DELETE, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Authorization, Content-Type, Idempotency-Key, X-Idempotency-Key, X-Hyperion-Account-Id, X-Hyperion-Key-Id, X-Hyperion-Timestamp, X-Hyperion-Nonce, X-Hyperion-Signature, X-Hyperion-Auth-Scheme, Traceparent, X-Correlation-Id, X-Request-Timeout-Ms")
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func (s *Server) requireJWT(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		claims, err := s.jwtAuth.ExtractRequestClaims(r, time.Now())
		if err != nil {
			s.metrics.HTTPAuthFailuresTotal.Add(1)
			s.writeError(w, r, http.StatusUnauthorized, "UNAUTHORIZED", err.Error(), "")
			return
		}
		if claims.AccountID > 0 && s.limiter != nil {
			dec := s.limiter.AllowAccount(claims.AccountID, "", false)
			if !dec.Allowed {
				s.writeError(w, r, http.StatusTooManyRequests, dec.Reason, "account rate limit exceeded", dec.Reason)
				return
			}
		}
		next.ServeHTTP(w, r.WithContext(auth.WithClaims(r.Context(), claims)))
	})
}

// propagateDeadline derives a request context honoring optional X-Request-Timeout-Ms header.
func (s *Server) propagateDeadline(r *http.Request) (context.Context, context.CancelFunc, *hyperionv1.TraceMetadata, string) {
	traceMeta, traceparent := s.metrics.ExtractOrMintTrace(r.Header, s.cfg.ServiceName)
	ctx := grpcclient.WithTraceHeaders(r.Context(), traceparent, traceMeta.CorrelationId)

	timeout := s.cfg.DefaultRPCTimeout
	if rawMS := strings.TrimSpace(r.Header.Get("X-Request-Timeout-Ms")); rawMS != "" {
		if ms, err := strconv.ParseInt(rawMS, 10, 64); err == nil && ms > 0 && ms < 30_000 {
			timeout = time.Duration(ms) * time.Millisecond
		}
	}
	cctx, cancel := context.WithTimeout(ctx, timeout)
	return cctx, cancel, traceMeta, traceparent
}

func (s *Server) authenticateSignedMutation(w http.ResponseWriter, r *http.Request) (auth.SignedIdentity, []byte, bool) {
	rawBody, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_BODY", "failed reading body", "")
		return auth.SignedIdentity{}, nil, false
	}
	id, aerr := s.sigAuth.Authenticate(r, rawBody)
	if aerr != nil {
		s.metrics.HTTPAuthFailuresTotal.Add(1)
		if errors.Is(aerr, auth.ErrNonceReplayed) || errors.Is(aerr, auth.ErrClockSkewExceeded) {
			s.metrics.ReplayRejectedTotal.Add(1)
		}
		s.writeError(w, r, http.StatusUnauthorized, "INVALID_SIGNATURE_OR_NONCE", aerr.Error(), "INVALID_SIGNATURE_OR_NONCE")
		return auth.SignedIdentity{}, nil, false
	}
	return id, rawBody, true
}

func (s *Server) handleHealthz(w http.ResponseWriter, _ *http.Request) {
	statusStr := "ok"
	breakers := make(map[string]string, 4)
	if s.mesh != nil {
		for name, pool := range map[string]*grpcclient.ConnPool{
			"ledger-rs":         s.mesh.LedgerPool,
			"stream-aggregator": s.mesh.AnalyticsPool,
			"sentinel-ml":       s.mesh.FraudPool,
			"chronos-replay":    s.mesh.ChronosPool,
		} {
			if pool != nil && pool.Breaker() != nil {
				st := pool.Breaker().State()
				breakers[name] = st.String()
				if st == grpcclient.StateOpen {
					statusStr = "degraded"
				}
			}
		}
	}
	writeJSON(w, http.StatusOK, HealthStatusResponse{
		Status:          statusStr,
		Service:         s.cfg.ServiceName,
		Version:         s.cfg.Version,
		EngineSeq:       u64Str(s.metrics.LatestEngineSeq.Load()),
		TsNS:            u64Str(uint64(time.Now().UnixNano())),
		CircuitBreakers: breakers,
	})
}

func (s *Server) handleSubmitOrder(w http.ResponseWriter, r *http.Request) {
	sigID, rawBody, ok := s.authenticateSignedMutation(w, r)
	if !ok {
		return
	}

	var payload SubmitOrderPayload
	if err := json.Unmarshal(rawBody, &payload); err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_JSON", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}

	bodyAcctID, err := parseUint64Strict(payload.AccountID, "account_id")
	if err != nil || bodyAcctID != sigID.AccountID {
		s.writeError(w, r, http.StatusUnauthorized, "ACCOUNT_MISMATCH", "body account_id must match signed X-Hyperion-Account-Id", "INVALID_SIGNATURE_OR_NONCE")
		return
	}

	// Enforce Hybrid L1/L2 Token-Bucket Rate Limiter + Spam Detector + Aegis Freeze/Throttle!
	if s.limiter != nil {
		dec := s.limiter.AllowAccount(sigID.AccountID, payload.Symbol, true)
		if !dec.Allowed {
			s.writeError(w, r, http.StatusTooManyRequests, dec.Reason, "rate limit or risk mitigation active", dec.Reason)
			return
		}
	}

	side, err := parseOrderSide(payload.Side)
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SIDE", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	ordType, err := parseOrderType(payload.OrderType)
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_ORDER_TYPE", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	priceFP, err := parseUint64Strict(payload.PriceFP, "price_fp")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	qtyFP, err := parseUint64Strict(payload.QtyFP, "qty_fp")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	instSpec, err := instruments.ValidateOrderAligned(payload.Symbol, ordType == hyperionv1.OrderType_ORDER_TYPE_MARKET, priceFP, qtyFP)
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}

	ctx, cancel, traceMeta, traceparent := s.propagateDeadline(r)
	defer cancel()

	idemKey := strings.TrimSpace(r.Header.Get("Idempotency-Key"))
	if idemKey == "" {
		idemKey = strings.TrimSpace(r.Header.Get("X-Idempotency-Key"))
	}

	pbCmd := &hyperionv1.SubmitOrder{
		ClientOrderId:  payload.ClientOrderID,
		AccountId:      sigID.AccountID,
		Symbol:         instSpec.Symbol,
		InstrumentId:   uint32(instSpec.ID),
		Side:           side,
		OrderType:      ordType,
		StpMode:        parseSTPMode(payload.STPMode),
		PriceFp:        priceFP,
		QtyFp:          qtyFP,
		MaxSlippageBps: payload.MaxSlippageBps,
		ClientTsNs:     sigID.TimestampNS,
		Nonce:          sigID.Nonce,
		Signature:      sigID.Signature,
		IdempotencyKey: idemKey,
		Trace:          traceMeta,
	}

	resp, gerr := s.mesh.SubmitOrder(ctx, &hyperionv1.SubmitOrderRequest{Order: pbCmd})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	ack := resp.GetAck()
	if ack == nil {
		s.writeError(w, r, http.StatusBadGateway, "EMPTY_UPSTREAM_ACK", "ledger-rs returned empty OrderAck", "")
		return
	}

	if s.wsHub != nil {
		s.wsHub.SendPrivateOrderAck(ack)
	}
	if s.rpBridge != nil {
		s.rpBridge.PublishOrderCommand(instSpec.Symbol, traceparent, "hyperion.v1.SubmitOrder", ack.GetEngineSeq(), pbCmd)
	}
	writeJSON(w, http.StatusOK, mapOrderAckToDto(ack))
}

func (s *Server) handleModifyOrder(w http.ResponseWriter, r *http.Request) {
	sigID, rawBody, ok := s.authenticateSignedMutation(w, r)
	if !ok {
		return
	}
	orderID, err := parseUint64Strict(r.PathValue("order_id"), "order_id")
	if err != nil || orderID == 0 {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_ORDER_ID", "invalid order_id path parameter", "ORDER_NOT_FOUND")
		return
	}

	var payload ModifyOrderPayload
	if err := json.Unmarshal(rawBody, &payload); err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_JSON", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	bodyAcctID, err := parseUint64Strict(payload.AccountID, "account_id")
	if err != nil || bodyAcctID != sigID.AccountID {
		s.writeError(w, r, http.StatusUnauthorized, "ACCOUNT_MISMATCH", "body account_id must match signed X-Hyperion-Account-Id", "INVALID_SIGNATURE_OR_NONCE")
		return
	}

	if s.limiter != nil {
		dec := s.limiter.AllowAccount(sigID.AccountID, payload.Symbol, true)
		if !dec.Allowed {
			s.writeError(w, r, http.StatusTooManyRequests, dec.Reason, "rate limit or risk mitigation active", dec.Reason)
			return
		}
	}

	newPriceFP, err := parseUint64Strict(payload.NewPriceFP, "new_price_fp")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	newQtyFP, err := parseUint64Strict(payload.NewQtyFP, "new_qty_fp")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}
	instSpec, err := instruments.ValidateOrderAligned(payload.Symbol, false, newPriceFP, newQtyFP)
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_PRICE_OR_QTY", err.Error(), "INVALID_PRICE_OR_QTY")
		return
	}

	ctx, cancel, traceMeta, traceparent := s.propagateDeadline(r)
	defer cancel()

	pbMod := &hyperionv1.ModifyOrder{
		OrderId:        orderID,
		ClientOrderId:  payload.ClientOrderID,
		AccountId:      sigID.AccountID,
		Symbol:         instSpec.Symbol,
		InstrumentId:   uint32(instSpec.ID),
		NewPriceFp:     newPriceFP,
		NewQtyFp:       newQtyFP,
		ClientTsNs:     sigID.TimestampNS,
		Nonce:          sigID.Nonce,
		Signature:      sigID.Signature,
		IdempotencyKey: strings.TrimSpace(r.Header.Get("Idempotency-Key")),
		Trace:          traceMeta,
	}
	resp, gerr := s.mesh.ModifyOrder(ctx, &hyperionv1.ModifyOrderRequest{Modify: pbMod})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	ack := resp.GetAck()
	if ack == nil {
		s.writeError(w, r, http.StatusBadGateway, "EMPTY_UPSTREAM_ACK", "ledger-rs returned empty OrderAck", "")
		return
	}
	if s.wsHub != nil {
		s.wsHub.SendPrivateOrderAck(ack)
	}
	if s.rpBridge != nil {
		s.rpBridge.PublishOrderCommand(instSpec.Symbol, traceparent, "hyperion.v1.ModifyOrder", ack.GetEngineSeq(), pbMod)
	}
	writeJSON(w, http.StatusOK, mapOrderAckToDto(ack))
}

func (s *Server) handleCancelOrder(w http.ResponseWriter, r *http.Request) {
	sigID, _, ok := s.authenticateSignedMutation(w, r)
	if !ok {
		return
	}
	orderID, err := parseUint64Strict(r.PathValue("order_id"), "order_id")
	if err != nil || orderID == 0 {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_ORDER_ID", "invalid order_id path parameter", "ORDER_NOT_FOUND")
		return
	}
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	instSpec, ok := instruments.LookupSymbol(symbol)
	if !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported or missing query symbol", "")
		return
	}

	if s.limiter != nil {
		dec := s.limiter.AllowAccount(sigID.AccountID, instSpec.Symbol, true)
		if !dec.Allowed {
			s.writeError(w, r, http.StatusTooManyRequests, dec.Reason, "rate limit or risk mitigation active", dec.Reason)
			return
		}
	}

	ctx, cancel, traceMeta, traceparent := s.propagateDeadline(r)
	defer cancel()

	pbCancel := &hyperionv1.CancelOrder{
		OrderId:        orderID,
		ClientOrderId:  strings.TrimSpace(r.URL.Query().Get("client_order_id")),
		AccountId:      sigID.AccountID,
		Symbol:         instSpec.Symbol,
		InstrumentId:   uint32(instSpec.ID),
		ClientTsNs:     sigID.TimestampNS,
		Nonce:          sigID.Nonce,
		Signature:      sigID.Signature,
		IdempotencyKey: strings.TrimSpace(r.Header.Get("Idempotency-Key")),
		Trace:          traceMeta,
	}
	resp, gerr := s.mesh.CancelOrder(ctx, &hyperionv1.CancelOrderRequest{Cancel: pbCancel})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	ack := resp.GetAck()
	if ack == nil {
		s.writeError(w, r, http.StatusBadGateway, "EMPTY_UPSTREAM_ACK", "ledger-rs returned empty OrderAck", "")
		return
	}
	if s.wsHub != nil {
		s.wsHub.SendPrivateOrderAck(ack)
	}
	if s.rpBridge != nil {
		s.rpBridge.PublishOrderCommand(instSpec.Symbol, traceparent, "hyperion.v1.CancelOrder", ack.GetEngineSeq(), pbCancel)
	}
	writeJSON(w, http.StatusOK, mapOrderAckToDto(ack))
}

func (s *Server) handleCreditDeposit(w http.ResponseWriter, r *http.Request) {
	var payload CreditDepositPayload
	if err := json.NewDecoder(io.LimitReader(r.Body, 1<<20)).Decode(&payload); err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_JSON", err.Error(), "")
		return
	}
	acctID, err := parseUint64Strict(payload.AccountID, "account_id")
	if err != nil || acctID == 0 {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_ACCOUNT_ID", "invalid account_id", "")
		return
	}
	amtU128, err := parseBigUint128(payload.AmountFP, "amount_fp")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_AMOUNT_FP", err.Error(), "")
		return
	}
	ctx, cancel, traceMeta, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.CreditDeposit(ctx, &hyperionv1.CreditDepositRequest{
		DepositId: payload.DepositID,
		AccountId: acctID,
		Asset:     strings.ToUpper(strings.TrimSpace(payload.Asset)),
		AmountFp:  amtU128,
		TsNs:      traceMeta.IngestTsNs,
		Trace:     traceMeta,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	bal := resp.GetUpdatedBalance()
	writeJSON(w, http.StatusOK, AccountAssetBalanceDto{
		Asset:       bal.GetAsset(),
		AvailableFP: fixedpoint.FromProtoI128(bal.GetAvailableFp()).String(),
		LockedFP:    fixedpoint.FromProtoI128(bal.GetLockedFp()).String(),
		TotalFP:     fixedpoint.FromProtoI128(bal.GetTotalFp()).String(),
	})
}

func (s *Server) handleGetMerkleProof(w http.ResponseWriter, r *http.Request) {
	acctID, err := parseUint64Strict(r.PathValue("account_id"), "account_id")
	if err != nil || acctID == 0 {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_ACCOUNT_ID", "invalid account_id", "")
		return
	}
	var atSeq uint64
	if raw := strings.TrimSpace(r.URL.Query().Get("at_engine_seq")); raw != "" {
		atSeq, err = parseUint64Strict(raw, "at_engine_seq")
		if err != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_SEQ", err.Error(), "")
			return
		}
	}
	ctx, cancel, _, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.GetMerkleLedgerProof(ctx, &hyperionv1.GetMerkleLedgerProofRequest{
		AccountId:   acctID,
		AtEngineSeq: atSeq,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	p := resp.GetProof()
	bals := make([]AccountAssetBalanceDto, 0, len(p.GetBalances()))
	for _, b := range p.GetBalances() {
		bals = append(bals, AccountAssetBalanceDto{
			Asset:       b.GetAsset(),
			AvailableFP: fixedpoint.FromProtoI128(b.GetAvailableFp()).String(),
			LockedFP:    fixedpoint.FromProtoI128(b.GetLockedFp()).String(),
			TotalFP:     fixedpoint.FromProtoI128(b.GetTotalFp()).String(),
		})
	}
	nodes := make([]MerkleProofNodeDto, 0, len(p.GetProofPath()))
	for _, n := range p.GetProofPath() {
		nodes = append(nodes, MerkleProofNodeDto{
			SiblingHash:   hex32OrZero(n.GetSiblingHash()),
			IsLeftSibling: n.GetIsLeftSibling(),
			Depth:         n.GetDepth(),
		})
	}
	writeJSON(w, http.StatusOK, MerkleLedgerProofDto{
		EngineSeq:             u64Str(p.GetEngineSeq()),
		AccountID:             u64Str(p.GetAccountId()),
		Balances:              bals,
		LeafBlake3Hash:        hex32OrZero(p.GetLeafBlake3Hash()),
		StateRootBlake3:       hex32OrZero(p.GetStateRootBlake3()),
		ProofPath:             nodes,
		SystemTotalDepositsFP: fixedpoint.FromProtoU128(p.GetSystemTotalDepositsFp()).String(),
		SystemTotalUserEqFP:   fixedpoint.FromProtoU128(p.GetSystemTotalUserEquityFp()).String(),
		TreasuryFeePoolFP:     fixedpoint.FromProtoI128(p.GetTreasuryFeePoolFp()).String(),
		DoubleEntryBalanced:   p.GetDoubleEntryBalanced(),
		ProofTsNS:             u64Str(p.GetProofTsNs()),
	})
}

func (s *Server) handleRegisterKey(w http.ResponseWriter, r *http.Request) {
	var payload RegisterKeyPayload
	if err := json.NewDecoder(io.LimitReader(r.Body, 64*1024)).Decode(&payload); err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_JSON", err.Error(), "")
		return
	}
	acctID, err := parseUint64Strict(payload.AccountID, "account_id")
	if err != nil || acctID == 0 || strings.TrimSpace(payload.KeyID) == "" {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_KEY_SPEC", "account_id and key_id are required", "")
		return
	}
	switch strings.ToLower(strings.TrimSpace(payload.Scheme)) {
	case "ed25519":
		if err := s.keyStore.RegisterEd25519Hex(acctID, payload.KeyID, payload.PublicKeyHex); err != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_ED25519_KEY", err.Error(), "")
			return
		}
	case "hmac-sha256":
		sec, err := hex.DecodeString(strings.TrimSpace(payload.HMACSecretHex))
		if err != nil || len(sec) == 0 {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_HMAC_SECRET", "hmac_secret_hex must be valid hex", "")
			return
		}
		if err := s.keyStore.RegisterHMAC(acctID, payload.KeyID, sec); err != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_HMAC_SECRET", err.Error(), "")
			return
		}
	default:
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SCHEME", "scheme must be ed25519 or hmac-sha256", "")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"registered": true, "account_id": u64Str(acctID), "key_id": payload.KeyID})
}

func (s *Server) handleGetOrderbook(w http.ResponseWriter, r *http.Request) {
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	if _, ok := instruments.LookupSymbol(symbol); !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported symbol", "")
		return
	}
	depth := uint32(50)
	if rawD := strings.TrimSpace(r.URL.Query().Get("depth")); rawD != "" {
		if parsed, err := strconv.ParseUint(rawD, 10, 32); err == nil && parsed >= 1 && parsed <= 500 {
			depth = uint32(parsed)
		}
	}
	ctx, cancel, _, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.GetL2BookSnapshot(ctx, &hyperionv1.GetL2BookSnapshotRequest{
		Symbol: symbol,
		Depth:  depth,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	writeJSON(w, http.StatusOK, mapL2SnapshotToDto(resp.GetSnapshot()))
}

func (s *Server) handleGetCandles(w http.ResponseWriter, r *http.Request) {
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	if _, ok := instruments.LookupSymbol(symbol); !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported symbol", "")
		return
	}
	interval, err := parseCandleInterval(r.URL.Query().Get("interval"))
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_INTERVAL", err.Error(), "")
		return
	}
	limit := uint32(300)
	if rawL := strings.TrimSpace(r.URL.Query().Get("limit")); rawL != "" {
		if parsed, err := strconv.ParseUint(rawL, 10, 32); err == nil && parsed >= 1 && parsed <= 5000 {
			limit = uint32(parsed)
		}
	}
	ctx, cancel, _, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.GetCandles(ctx, &hyperionv1.GetCandlesRequest{
		Symbol:   symbol,
		Interval: interval,
		Limit:    limit,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	out := make([]OhlcvCandleDto, 0, len(resp.GetCandles()))
	for _, c := range resp.GetCandles() {
		out = append(out, OhlcvCandleDto{
			Symbol:             c.GetSymbol(),
			Interval:           formatCandleInterval(c.GetInterval()),
			BucketOpenTsNS:     u64Str(c.GetBucketOpenTsNs()),
			BucketCloseTsNS:    u64Str(c.GetBucketCloseTsNs()),
			OpenFP:             u64Str(c.GetOpenFp()),
			HighFP:             u64Str(c.GetHighFp()),
			LowFP:              u64Str(c.GetLowFp()),
			CloseFP:            u64Str(c.GetCloseFp()),
			BaseVolumeFP:       fixedpoint.FromProtoU128(c.GetBaseVolumeFp()).String(),
			QuoteVolumeFP:      fixedpoint.FromProtoU128(c.GetQuoteVolumeFp()).String(),
			TakerBuyBaseVolFP:  fixedpoint.FromProtoU128(c.GetTakerBuyBaseVolumeFp()).String(),
			TakerSellBaseVolFP: fixedpoint.FromProtoU128(c.GetTakerSellBaseVolumeFp()).String(),
			VwapFP:             u64Str(c.GetVwapFp()),
			TwapFP:             u64Str(c.GetTwapFp()),
			TradeCount:         u64Str(c.GetTradeCount()),
			IsClosed:           c.GetIsClosed(),
		})
	}
	writeJSON(w, http.StatusOK, out)
}

func (s *Server) handleGetMicrostructure(w http.ResponseWriter, r *http.Request) {
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	if _, ok := instruments.LookupSymbol(symbol); !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported symbol", "")
		return
	}
	ctx, cancel, _, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.GetLatestMicrostructure(ctx, &hyperionv1.GetLatestMicrostructureRequest{Symbol: symbol})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	m := resp.GetMetrics()
	writeJSON(w, http.StatusOK, MicrostructureDto{
		Symbol:             m.GetSymbol(),
		EngineSeq:          u64Str(m.GetEngineSeq()),
		TsNS:               u64Str(m.GetTsNs()),
		MidPriceFP:         u64Str(m.GetMidPriceFp()),
		SpreadFP:           u64Str(m.GetSpreadFp()),
		SpreadBps:          m.GetSpreadBps(),
		RollingVwapFP:      u64Str(m.GetRollingVwapFp()),
		RollingTwapFP:      u64Str(m.GetRollingTwapFp()),
		OfiFP:              fixedpoint.FromProtoI128(m.GetOfiFp()).String(),
		BookImbalanceTop5:  m.GetBookImbalanceTop5Ppm(),
		BookImbalanceTop10: m.GetBookImbalanceTop10Ppm(),
		BookImbalanceTop20: m.GetBookImbalanceTop20Ppm(),
		KylesLambdaFP:      i64Str(m.GetKylesLambdaFp()),
	})
}

func (s *Server) handleListFraudAlerts(w http.ResponseWriter, r *http.Request) {
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	minSev := uint32(50)
	if rawS := strings.TrimSpace(r.URL.Query().Get("min_severity")); rawS != "" {
		if parsed, err := strconv.ParseUint(rawS, 10, 32); err == nil && parsed <= 100 {
			minSev = uint32(parsed)
		}
	}
	ctx, cancel, _, _ := s.propagateDeadline(r)
	defer cancel()

	resp, gerr := s.mesh.ListRecentAlerts(ctx, &hyperionv1.ListRecentAlertsRequest{
		Symbol:           symbol,
		MinSeverityScore: minSev,
		Limit:            100,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	out := make([]AnomalyAlertDto, 0, len(resp.GetAlerts()))
	for _, a := range resp.GetAlerts() {
		offenders := make([]string, 0, len(a.GetOffenderAccountIds()))
		for _, id := range a.GetOffenderAccountIds() {
			offenders = append(offenders, u64Str(id))
		}
		shaps := make([]SHAPAttributionDto, 0, len(a.GetShapAttributions()))
		for _, sh := range a.GetShapAttributions() {
			shaps = append(shaps, SHAPAttributionDto{
				FeatureName:         sh.GetFeatureName(),
				FeatureValueFP:      i64Str(sh.GetFeatureValueFp()),
				BaselineValueFP:     i64Str(sh.GetBaselineValueFp()),
				SHAPContributionPPM: sh.GetShapContributionPpm(),
				ImportanceRank:      sh.GetImportanceRank(),
			})
		}
		out = append(out, AnomalyAlertDto{
			AlertID:            a.GetAlertId(),
			EngineSeq:          u64Str(a.GetEngineSeq()),
			Symbol:             a.GetSymbol(),
			PatternType:        strings.TrimPrefix(a.GetPatternType().String(), "ANOMALY_PATTERN_TYPE_"),
			SeverityScore:      a.GetSeverityScore(),
			ConfidencePPM:      a.GetConfidencePpm(),
			OffenderAccountIDs: offenders,
			SHAPAttributions:   shaps,
			DetectedTsNS:       u64Str(a.GetDetectedTsNs()),
			InferenceLatencyUS: a.GetInferenceLatencyUs(),
			ModelVersion:       a.GetModelVersion(),
		})
	}
	writeJSON(w, http.StatusOK, out)
}

func (s *Server) handleChronosSeek(w http.ResponseWriter, r *http.Request) {
	symbol := strings.TrimSpace(r.URL.Query().Get("symbol"))
	if _, ok := instruments.LookupSymbol(symbol); !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported symbol", "")
		return
	}
	ctx, cancel, traceMeta, _ := s.propagateDeadline(r)
	defer cancel()

	req := &hyperionv1.TimeTravelSeekRequest{
		Symbol:             symbol,
		L2Depth:            50,
		VerifyWalHashChain: true,
		Trace:              traceMeta,
	}
	if rawSeq := strings.TrimSpace(r.URL.Query().Get("target_engine_seq")); rawSeq != "" {
		seq, err := parseUint64Strict(rawSeq, "target_engine_seq")
		if err != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_SEQ", err.Error(), "")
			return
		}
		req.Target = &hyperionv1.TimeTravelSeekRequest_TargetEngineSeq{TargetEngineSeq: seq}
	} else if rawTS := strings.TrimSpace(r.URL.Query().Get("target_ts_ns")); rawTS != "" {
		ts, err := parseUint64Strict(rawTS, "target_ts_ns")
		if err != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_TS", err.Error(), "")
			return
		}
		req.Target = &hyperionv1.TimeTravelSeekRequest_TargetTsNs{TargetTsNs: ts}
	}

	resp, gerr := s.mesh.TimeTravelSeek(ctx, req)
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	writeJSON(w, http.StatusOK, mapL2SnapshotToDto(resp.GetL2Snapshot()))
}

func (s *Server) handleChronosFork(w http.ResponseWriter, r *http.Request) {
	var payload TimelineForkPayload
	if err := json.NewDecoder(io.LimitReader(r.Body, 1<<20)).Decode(&payload); err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_JSON", err.Error(), "")
		return
	}
	if _, ok := instruments.LookupSymbol(payload.Symbol); !ok {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_SYMBOL", "unsupported symbol", "")
		return
	}
	startSeq, err := parseUint64Strict(payload.StartEngineSeq, "start_engine_seq")
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_START_SEQ", err.Error(), "")
		return
	}
	endSeq, err := parseUint64Strict(payload.EndEngineSeq, "end_engine_seq")
	if err != nil || endSeq < startSeq {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_END_SEQ", "end_engine_seq must be >= start_engine_seq", "")
		return
	}

	pol := &hyperionv1.SimulationPolicyOverride{
		MakerFeePpmOverride: payload.MakerFeePPMOverride,
		TakerFeePpmOverride: payload.TakerFeePPMOverride,
	}
	if payload.MaxOrderQtyFPOverride != nil && *payload.MaxOrderQtyFPOverride != "" {
		maxQty, qerr := parseUint64Strict(*payload.MaxOrderQtyFPOverride, "max_order_qty_fp_override")
		if qerr != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_MAX_QTY", qerr.Error(), "")
			return
		}
		pol.MaxOrderQtyFpOverride = &maxQty
	}
	for _, rawAcct := range payload.BlockedAccountIDs {
		acct, aerr := parseUint64Strict(rawAcct, "blocked_account_ids")
		if aerr != nil {
			s.writeError(w, r, http.StatusBadRequest, "INVALID_ACCOUNT_ID", aerr.Error(), "")
			return
		}
		pol.AccountOverrides = append(pol.AccountOverrides, &hyperionv1.AccountPolicyOverride{
			AccountId:                       acct,
			StatusOverride:                  hyperionv1.AccountStatus_ACCOUNT_STATUS_FROZEN,
			CancelRestingOrdersOnActivation: true,
		})
	}

	ctx, cancel, traceMeta, _ := s.propagateDeadline(r)
	defer cancel()

	diff, gerr := s.mesh.TimelineFork(ctx, &hyperionv1.TimelineForkRequest{
		ForkName:       payload.ForkName,
		Symbol:         payload.Symbol,
		StartEngineSeq: startSeq,
		EndEngineSeq:   endSeq,
		PolicyOverride: pol,
		Trace:          traceMeta,
	})
	if gerr != nil {
		s.writeUpstreamError(w, r, gerr)
		return
	}
	writeJSON(w, http.StatusOK, StateTreeDiffDto{
		ForkID:                  diff.GetForkId(),
		Symbol:                  diff.GetSymbol(),
		StartEngineSeq:          u64Str(diff.GetStartEngineSeq()),
		EndEngineSeq:            u64Str(diff.GetEndEngineSeq()),
		BaselineStateRootBlake3: hex32OrZero(diff.GetBaselineStateRootBlake3()),
		ForkedStateRootBlake3:   hex32OrZero(diff.GetForkedStateRootBlake3()),
		StateIdentical:          diff.GetStateIdentical(),
		BaselineClearingPriceFP: u64Str(diff.GetBaselineClearingPriceFp()),
		ForkedClearingPriceFP:   u64Str(diff.GetForkedClearingPriceFp()),
		ClearingPriceDeltaFP:    i64Str(diff.GetClearingPriceDeltaFp()),
		PreventedWashVolumeFP:   fixedpoint.FromProtoU128(diff.GetPreventedWashVolumeFp()).String(),
		PreventedFillsCount:     u64Str(diff.GetPreventedFillsCount()),
		RejectedOrdersCount:     u64Str(diff.GetRejectedOrdersCount()),
		ReplayedEventsCount:     u64Str(diff.GetReplayedEventsCount()),
		ComputationLatencyUS:    diff.GetComputationLatencyUs(),
	})
}

func (s *Server) handleChaosCampaign(w http.ResponseWriter, r *http.Request) {
	rawBody, err := io.ReadAll(io.LimitReader(r.Body, 256*1024))
	if err != nil {
		s.writeError(w, r, http.StatusBadRequest, "INVALID_BODY", err.Error(), "")
		return
	}
	targetURL := strings.TrimRight(s.cfg.ChaosSimHTTPURL, "/") + "/api/v1/chaos/campaigns"
	proxyReq, err := http.NewRequestWithContext(r.Context(), http.MethodPost, targetURL, bytes.NewReader(rawBody))
	if err != nil {
		s.writeError(w, r, http.StatusInternalServerError, "PROXY_INIT_FAILED", err.Error(), "")
		return
	}
	proxyReq.Header.Set("Content-Type", "application/json")
	resp, err := s.httpClient.Do(proxyReq)
	if err != nil {
		s.writeError(w, r, http.StatusServiceUnavailable, "CHAOS_SIM_UNAVAILABLE", err.Error(), "")
		return
	}
	defer resp.Body.Close()
	respBody, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(resp.StatusCode)
	_, _ = w.Write(respBody)
}

func (s *Server) writeUpstreamError(w http.ResponseWriter, r *http.Request, err error) {
	if errors.Is(err, grpcclient.ErrCircuitOpen) {
		s.writeError(w, r, http.StatusServiceUnavailable, "CIRCUIT_BREAKER_OPEN", err.Error(), "CIRCUIT_BREAKER_OPEN")
		return
	}
	if errors.Is(err, context.DeadlineExceeded) {
		s.writeError(w, r, http.StatusGatewayTimeout, "UPSTREAM_DEADLINE_EXCEEDED", err.Error(), "")
		return
	}
	s.writeError(w, r, http.StatusBadGateway, "UPSTREAM_GRPC_ERROR", err.Error(), "")
}

func (s *Server) writeError(w http.ResponseWriter, r *http.Request, status int, code, msg, rejectReason string) {
	traceMeta, _ := s.metrics.ExtractOrMintTrace(r.Header, s.cfg.ServiceName)
	writeJSON(w, status, ErrorEnvelope{
		Code:         code,
		Message:      msg,
		RejectReason: rejectReason,
		TraceID:      traceMeta.TraceId,
		TsNS:         u64Str(traceMeta.IngestTsNs),
	})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
