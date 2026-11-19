package httpapi_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/grpcclient"
	"github.com/vanovri/hyperion/services/gateway-go/internal/httpapi"
	"github.com/vanovri/hyperion/services/gateway-go/internal/idempotency"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/redpanda"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"
)

type stubBackendMesh struct {
	hyperionv1.UnimplementedOrderbookServiceServer
	hyperionv1.UnimplementedStreamAnalyticsServiceServer
	hyperionv1.UnimplementedFraudTelemetryServiceServer
	hyperionv1.UnimplementedChronosReplayServiceServer
}

func (s *stubBackendMesh) SubmitOrder(_ context.Context, req *hyperionv1.SubmitOrderRequest) (*hyperionv1.SubmitOrderResponse, error) {
	o := req.GetOrder()
	notional, _ := fixedpoint.ToProtoU128(fixedpoint.ComputeNotionalFP(o.GetPriceFp(), o.GetQtyFp()))
	return &hyperionv1.SubmitOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:      901,
			OrderId:        4401,
			ClientOrderId:  o.GetClientOrderId(),
			AccountId:      o.GetAccountId(),
			Symbol:         o.GetSymbol(),
			InstrumentId:   o.GetInstrumentId(),
			Status:         hyperionv1.OrderStatus_ORDER_STATUS_FILLED,
			FilledQtyFp:    o.GetQtyFp(),
			RemainingQtyFp: 0,
			AvgFillPriceFp: o.GetPriceFp(),
			EngineTsNs:     uint64(time.Now().UnixNano()),
			Fills: []*hyperionv1.ExecutionFill{
				{
					FillId:         1,
					EngineSeq:      901,
					Symbol:         o.GetSymbol(),
					MakerOrderId:   4000,
					TakerOrderId:   4401,
					MakerAccountId: 88,
					TakerAccountId: o.GetAccountId(),
					AggressorSide:  o.GetSide(),
					PriceFp:        o.GetPriceFp(),
					QtyFp:          o.GetQtyFp(),
					NotionalFp:     notional,
					MakerFeeFp:     -100_000,
					TakerFeeFp:     250_000,
					ExecTsNs:       uint64(time.Now().UnixNano()),
				},
			},
		},
	}, nil
}

func (s *stubBackendMesh) ModifyOrder(_ context.Context, req *hyperionv1.ModifyOrderRequest) (*hyperionv1.ModifyOrderResponse, error) {
	m := req.GetModify()
	return &hyperionv1.ModifyOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:      902,
			OrderId:        m.GetOrderId(),
			ClientOrderId:  m.GetClientOrderId(),
			AccountId:      m.GetAccountId(),
			Symbol:         m.GetSymbol(),
			Status:         hyperionv1.OrderStatus_ORDER_STATUS_MODIFIED,
			RemainingQtyFp: m.GetNewQtyFp(),
			EngineTsNs:     uint64(time.Now().UnixNano()),
		},
	}, nil
}

func (s *stubBackendMesh) CancelOrder(_ context.Context, req *hyperionv1.CancelOrderRequest) (*hyperionv1.CancelOrderResponse, error) {
	c := req.GetCancel()
	return &hyperionv1.CancelOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:  903,
			OrderId:    c.GetOrderId(),
			AccountId:  c.GetAccountId(),
			Symbol:     c.GetSymbol(),
			Status:     hyperionv1.OrderStatus_ORDER_STATUS_CANCELED,
			EngineTsNs: uint64(time.Now().UnixNano()),
		},
	}, nil
}

func (s *stubBackendMesh) CreditDeposit(_ context.Context, req *hyperionv1.CreditDepositRequest) (*hyperionv1.CreditDepositResponse, error) {
	amt := fixedpoint.FromProtoU128(req.GetAmountFp())
	iAmt, _ := fixedpoint.ToProtoI128(amt)
	zeroI128, _ := fixedpoint.ToProtoI128(big.NewInt(0))
	return &hyperionv1.CreditDepositResponse{
		EngineSeq:  904,
		JournalSeq: 12,
		UpdatedBalance: &hyperionv1.AccountAssetBalance{
			Asset:       req.GetAsset(),
			AvailableFp: iAmt,
			LockedFp:    zeroI128,
			TotalFp:     iAmt,
		},
	}, nil
}

func (s *stubBackendMesh) GetMerkleLedgerProof(_ context.Context, req *hyperionv1.GetMerkleLedgerProofRequest) (*hyperionv1.GetMerkleLedgerProofResponse, error) {
	dep, _ := fixedpoint.ToProtoU128(big.NewInt(1_000_000_000_000))
	fee, _ := fixedpoint.ToProtoI128(big.NewInt(0))
	return &hyperionv1.GetMerkleLedgerProofResponse{
		Proof: &hyperionv1.MerkleLedgerProof{
			EngineSeq:             904,
			AccountId:             req.GetAccountId(),
			SystemTotalDepositsFp: dep,
			SystemTotalUserEquityFp: dep,
			TreasuryFeePoolFp:     fee,
			DoubleEntryBalanced:   true,
			ProofTsNs:             uint64(time.Now().UnixNano()),
		},
	}, nil
}

func (s *stubBackendMesh) GetL2BookSnapshot(_ context.Context, req *hyperionv1.GetL2BookSnapshotRequest) (*hyperionv1.GetL2BookSnapshotResponse, error) {
	return &hyperionv1.GetL2BookSnapshotResponse{
		Snapshot: &hyperionv1.L2BookSnapshot{
			EngineSeq:    905,
			Symbol:       req.GetSymbol(),
			BestBidFp:    6_842_000_000_000,
			BestAskFp:    6_843_000_000_000,
			MidPriceFp:   6_842_500_000_000,
			SpreadFp:     1_000_000_000,
			SnapshotTsNs: uint64(time.Now().UnixNano()),
		},
	}, nil
}

func (s *stubBackendMesh) GetCandles(_ context.Context, req *hyperionv1.GetCandlesRequest) (*hyperionv1.GetCandlesResponse, error) {
	vol, _ := fixedpoint.ToProtoU128(big.NewInt(500_000_000))
	return &hyperionv1.GetCandlesResponse{
		Candles: []*hyperionv1.OhlcvCandle{
			{
				Symbol:                 req.GetSymbol(),
				Interval:               req.GetInterval(),
				OpenFp:                 6_840_000_000_000,
				HighFp:                 6_850_000_000_000,
				LowFp:                  6_839_000_000_000,
				CloseFp:                6_845_000_000_000,
				BaseVolumeFp:           vol,
				QuoteVolumeFp:          vol,
				TakerBuyBaseVolumeFp:   vol,
				TakerSellBaseVolumeFp:  vol,
				VwapFp:                 6_844_000_000_000,
				TwapFp:                 6_843_000_000_000,
				TradeCount:             12,
				IsClosed:               true,
			},
		},
	}, nil
}

func (s *stubBackendMesh) GetLatestMicrostructure(_ context.Context, req *hyperionv1.GetLatestMicrostructureRequest) (*hyperionv1.GetLatestMicrostructureResponse, error) {
	ofi, _ := fixedpoint.ToProtoI128(big.NewInt(-25_000_000))
	return &hyperionv1.GetLatestMicrostructureResponse{
		Metrics: &hyperionv1.MicrostructureMetrics{
			Symbol:              req.GetSymbol(),
			EngineSeq:           905,
			MidPriceFp:          6_842_500_000_000,
			SpreadFp:            1_000_000_000,
			SpreadBps:           1,
			RollingVwapFp:       6_844_000_000_000,
			RollingTwapFp:       6_843_000_000_000,
			OfiFp:               ofi,
			BookImbalanceTop5Ppm: 120_000,
			KylesLambdaFp:       4500,
		},
	}, nil
}

func (s *stubBackendMesh) ListRecentAlerts(_ context.Context, _ *hyperionv1.ListRecentAlertsRequest) (*hyperionv1.ListRecentAlertsResponse, error) {
	return &hyperionv1.ListRecentAlertsResponse{
		Alerts: []*hyperionv1.AnomalyAlert{
			{
				AlertId:            "alert-001",
				EngineSeq:          905,
				Symbol:             "BTC-USDT",
				PatternType:        hyperionv1.AnomalyPatternType_ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE,
				SeverityScore:      97,
				ConfidencePpm:      991_000,
				OffenderAccountIds: []uint64{901, 902, 903},
			},
		},
	}, nil
}

func (s *stubBackendMesh) TimeTravelSeek(_ context.Context, req *hyperionv1.TimeTravelSeekRequest) (*hyperionv1.TimeTravelSeekResponse, error) {
	return &hyperionv1.TimeTravelSeekResponse{
		ResolvedEngineSeq: 500,
		L2Snapshot: &hyperionv1.L2BookSnapshot{
			EngineSeq: 500,
			Symbol:    req.GetSymbol(),
			BestBidFp: 6_800_000_000_000,
			BestAskFp: 6_801_000_000_000,
		},
	}, nil
}

func (s *stubBackendMesh) TimelineFork(_ context.Context, req *hyperionv1.TimelineForkRequest) (*hyperionv1.StateTreeDiffResponse, error) {
	prevWash, _ := fixedpoint.ToProtoU128(big.NewInt(2_500_000_000))
	return &hyperionv1.StateTreeDiffResponse{
		ForkId:                  "fork-1",
		Symbol:                  req.GetSymbol(),
		StartEngineSeq:          req.GetStartEngineSeq(),
		EndEngineSeq:            req.GetEndEngineSeq(),
		StateIdentical:          false,
		BaselineClearingPriceFp: 6_840_000_000_000,
		ForkedClearingPriceFp:   6_845_000_000_000,
		ClearingPriceDeltaFp:    5_000_000_000,
		PreventedWashVolumeFp:   prevWash,
		ReplayedEventsCount:     100,
		ComputationLatencyUs:    420,
	}, nil
}

func TestOpenAPIEndpoints_EndToEndIntegration_And_RedpandaDispatch(t *testing.T) {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	grpcSrv := grpc.NewServer()
	stub := &stubBackendMesh{}
	hyperionv1.RegisterOrderbookServiceServer(grpcSrv, stub)
	hyperionv1.RegisterStreamAnalyticsServiceServer(grpcSrv, stub)
	hyperionv1.RegisterFraudTelemetryServiceServer(grpcSrv, stub)
	hyperionv1.RegisterChronosReplayServiceServer(grpcSrv, stub)
	go func() { _ = grpcSrv.Serve(lis) }()
	t.Cleanup(grpcSrv.Stop)

	chaosMock := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"campaign_id":"camp-1","active":true,"regime":"SPOOFING_ATTACK"}`))
	}))
	t.Cleanup(chaosMock.Close)

	mr := miniredis.RunT(t)
	rc := redis.NewClient(&redis.Options{Addr: mr.Addr()})
	t.Cleanup(func() { _ = rc.Close() })

	cfg := config.FromEnv()
	addr := lis.Addr().String()
	cfg.LedgerGRPCAddrs = []string{addr}
	cfg.AnalyticsGRPCAddrs = []string{addr}
	cfg.FraudGRPCAddrs = []string{addr}
	cfg.ChronosGRPCAddrs = []string{addr}
	cfg.ChaosSimHTTPURL = chaosMock.URL
	cfg.EnableRedpanda = false
	cfg.WSHeartbeatInterval = 0

	metrics := telemetry.NewRegistry()
	ks := auth.NewKeyStore(cfg.BootstrapHMACSecret)
	jwtAuth := auth.NewJWTVerifier(cfg.JWTSecret, nil, 5*time.Second)
	rg := auth.NewReplayGuard(15*time.Second, 512, rc)
	sigAuth := auth.NewSignatureAuthenticator(ks, rg, 5*time.Second)
	lim := ratelimit.NewHybridLimiter(ratelimit.Config{DefaultRPS: 500, DefaultBurst: 1000, RedisClient: rc, Metrics: metrics})
	t.Cleanup(lim.Close)
	idem := idempotency.NewStore(1024, time.Minute, rc, metrics)
	mesh, err := grpcclient.NewMeshClients(cfg, metrics)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(mesh.Close)
	hub := ws.NewHub(cfg, jwtAuth, mesh, metrics)
	t.Cleanup(hub.Close)
	rp, err := redpanda.NewBridge(nil, "", false, hub, lim)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(rp.Close)

	gw := httpapi.NewServer(httpapi.Dependencies{
		Config: cfg, KeyStore: ks, JWTAuth: jwtAuth, SigAuth: sigAuth,
		Limiter: lim, Idem: idem, Mesh: mesh, WSHub: hub, Redpanda: rp, Metrics: metrics,
	})
	httpSrv := httptest.NewServer(gw.PublicHandler())
	t.Cleanup(httpSrv.Close)

	jwtTok, _ := auth.MintHS256(cfg.JWTSecret, auth.Claims{
		Subject: "e2e-operator", AccountID: 10042, IssuedAt: time.Now().Unix(), ExpiresAt: time.Now().Add(time.Hour).Unix(),
	})

	// 1. Register Ed25519 trading key via POST /api/v1/auth/keys
	pub, priv, _ := ed25519.GenerateKey(rand.Reader)
	regBody, _ := json.Marshal(httpapi.RegisterKeyPayload{
		AccountID: "10042", KeyID: "ed-main", Scheme: "ed25519", PublicKeyHex: hex.EncodeToString(pub),
	})
	regReq, _ := http.NewRequest(http.MethodPost, httpSrv.URL+"/api/v1/auth/keys", bytes.NewReader(regBody))
	regReq.Header.Set("Authorization", "Bearer "+jwtTok)
	regResp, err := http.DefaultClient.Do(regReq)
	if err != nil || regResp.StatusCode != http.StatusOK {
		t.Fatalf("register key failed: status=%v err=%v", regResp, err)
	}
	_ = regResp.Body.Close()

	// 2. Submit signed Ed25519 order -> Modify -> Cancel!
	ordBody := []byte(`{"client_order_id":"cli-001","account_id":"10042","symbol":"BTC-USDT","side":"BUY","order_type":"LIMIT","price_fp":"6842500000000","qty_fp":"100000000"}`)
	ts := strconv.FormatInt(time.Now().UnixNano(), 10)
	sig := auth.SignEd25519(priv, ts, "1", http.MethodPost, "/api/v1/orders", ordBody)
	ordReq, _ := http.NewRequest(http.MethodPost, httpSrv.URL+"/api/v1/orders", bytes.NewReader(ordBody))
	ordReq.Header.Set(auth.HeaderAccountID, "10042")
	ordReq.Header.Set(auth.HeaderKeyID, "ed-main")
	ordReq.Header.Set(auth.HeaderAuthScheme, "ed25519")
	ordReq.Header.Set(auth.HeaderTimestamp, ts)
	ordReq.Header.Set(auth.HeaderNonce, "1")
	ordReq.Header.Set(auth.HeaderSignature, sig)
	ordReq.Header.Set("Idempotency-Key", "idem-submit-0001")

	ordResp, err := http.DefaultClient.Do(ordReq)
	if err != nil || ordResp.StatusCode != http.StatusOK {
		t.Fatalf("POST /api/v1/orders failed: resp=%v err=%v", ordResp, err)
	}
	var ackDto httpapi.OrderAckDto
	_ = json.NewDecoder(ordResp.Body).Decode(&ackDto)
	_ = ordResp.Body.Close()
	if ackDto.OrderID != "4401" || ackDto.Status != "FILLED" || len(ackDto.Fills) != 1 {
		t.Fatalf("unexpected OrderAckDto: %+v", ackDto)
	}

	// Modify order
	modBody := []byte(`{"client_order_id":"cli-001","account_id":"10042","symbol":"BTC-USDT","new_price_fp":"6843000000000","new_qty_fp":"200000000"}`)
	sigMod := auth.SignEd25519(priv, ts, "2", http.MethodPatch, "/api/v1/orders/4401", modBody)
	modReq, _ := http.NewRequest(http.MethodPatch, httpSrv.URL+"/api/v1/orders/4401", bytes.NewReader(modBody))
	modReq.Header.Set(auth.HeaderAccountID, "10042")
	modReq.Header.Set(auth.HeaderKeyID, "ed-main")
	modReq.Header.Set(auth.HeaderTimestamp, ts)
	modReq.Header.Set(auth.HeaderNonce, "2")
	modReq.Header.Set(auth.HeaderSignature, sigMod)
	modReq.Header.Set("Idempotency-Key", "idem-modify-0002")
	modResp, err := http.DefaultClient.Do(modReq)
	if err != nil || modResp.StatusCode != http.StatusOK {
		t.Fatalf("PATCH /api/v1/orders/4401 failed: %v %v", modResp, err)
	}
	_ = modResp.Body.Close()

	// Cancel order
	sigCan := auth.SignEd25519(priv, ts, "3", http.MethodDelete, "/api/v1/orders/4401", nil)
	canReq, _ := http.NewRequest(http.MethodDelete, httpSrv.URL+"/api/v1/orders/4401?symbol=BTC-USDT", nil)
	canReq.Header.Set(auth.HeaderAccountID, "10042")
	canReq.Header.Set(auth.HeaderKeyID, "ed-main")
	canReq.Header.Set(auth.HeaderTimestamp, ts)
	canReq.Header.Set(auth.HeaderNonce, "3")
	canReq.Header.Set(auth.HeaderSignature, sigCan)
	canReq.Header.Set("Idempotency-Key", "idem-cancel-0003")
	canResp, err := http.DefaultClient.Do(canReq)
	if err != nil || canResp.StatusCode != http.StatusOK {
		t.Fatalf("DELETE /api/v1/orders/4401 failed: %v %v", canResp, err)
	}
	_ = canResp.Body.Close()

	// 3. Verify remaining OpenAPI endpoints (/healthz, /metrics, deposits, merkle-proof, orderbook, candles, microstructure, fraud, chronos, chaos)!
	endpoints := []struct {
		method string
		path   string
		body   string
		auth   bool
	}{
		{http.MethodGet, "/healthz", "", false},
		{http.MethodGet, "/metrics", "", false},
		{http.MethodPost, "/api/v1/accounts/deposits", `{"deposit_id":"dep-1","account_id":"10042","asset":"USDT","amount_fp":"500000000000"}`, true},
		{http.MethodGet, "/api/v1/accounts/10042/merkle-proof", "", true},
		{http.MethodGet, "/api/v1/market/orderbook?symbol=BTC-USDT&depth=20", "", false},
		{http.MethodGet, "/api/v1/analytics/candles?symbol=BTC-USDT&interval=1m&limit=10", "", false},
		{http.MethodGet, "/api/v1/analytics/microstructure?symbol=BTC-USDT", "", false},
		{http.MethodGet, "/api/v1/fraud/alerts?symbol=BTC-USDT&min_severity=80", "", true},
		{http.MethodGet, "/api/v1/chronos/seek?symbol=BTC-USDT&target_engine_seq=500", "", true},
		{http.MethodPost, "/api/v1/chronos/fork", `{"fork_name":"early-freeze","symbol":"BTC-USDT","start_engine_seq":"400","end_engine_seq":"500","blocked_account_ids":["901"]}`, true},
		{http.MethodPost, "/api/v1/chaos/campaigns", `{"regime":"SPOOFING_ATTACK","enabled":true}`, true},
	}
	for _, tc := range endpoints {
		req, _ := http.NewRequest(tc.method, httpSrv.URL+tc.path, bytes.NewReader([]byte(tc.body)))
		if tc.auth {
			req.Header.Set("Authorization", "Bearer "+jwtTok)
		}
		if tc.method == http.MethodPost {
			req.Header.Set("Content-Type", "application/json")
			req.Header.Set("Idempotency-Key", "idem-"+tc.path)
		}
		resp, err := http.DefaultClient.Do(req)
		if err != nil || resp.StatusCode != http.StatusOK {
			t.Fatalf("%s %s failed: status=%v err=%v", tc.method, tc.path, resp, err)
		}
		_ = resp.Body.Close()
	}

	// 4. Dispatch Redpanda RiskMitigationCommand via Bridge and verify immediate HTTP 429 ACCOUNT_FROZEN!
	rawMit, _ := proto.Marshal(&hyperionv1.RiskMitigationCommand{
		CommandId:          "mit-freeze-10042",
		Action:             hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT,
		TargetAccountIds:   []uint64{10042},
		CooldownDurationMs: 60_000,
	})
	if err := rp.DispatchPayload(context.Background(), redpanda.TopicRiskMitigations, rawMit); err != nil {
		t.Fatal(err)
	}
	sigBlocked := auth.SignEd25519(priv, ts, "4", http.MethodPost, "/api/v1/orders", ordBody)
	blockedReq, _ := http.NewRequest(http.MethodPost, httpSrv.URL+"/api/v1/orders", bytes.NewReader(ordBody))
	blockedReq.Header.Set(auth.HeaderAccountID, "10042")
	blockedReq.Header.Set(auth.HeaderKeyID, "ed-main")
	blockedReq.Header.Set(auth.HeaderTimestamp, ts)
	blockedReq.Header.Set(auth.HeaderNonce, "4")
	blockedReq.Header.Set(auth.HeaderSignature, sigBlocked)
	blockedReq.Header.Set("Idempotency-Key", "idem-blocked-0004")
	blockedResp, err := http.DefaultClient.Do(blockedReq)
	if err != nil || blockedResp.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("expected HTTP 429 TooManyRequests after Redpanda FREEZE_ACCOUNT, got %v %v", blockedResp, err)
	}
	_ = blockedResp.Body.Close()
}
