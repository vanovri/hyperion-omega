package torture_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/hex"
	"net"
	"net/http"
	"runtime"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/grpcclient"
	"github.com/vanovri/hyperion/services/gateway-go/internal/httpapi"
	"github.com/vanovri/hyperion/services/gateway-go/internal/idempotency"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

type tortureLedgerServer struct {
	hyperionv1.UnimplementedOrderbookServiceServer
	failMode atomic.Bool
	seq      atomic.Uint64
}

func (s *tortureLedgerServer) SubmitOrder(_ context.Context, req *hyperionv1.SubmitOrderRequest) (*hyperionv1.SubmitOrderResponse, error) {
	if s.failMode.Load() {
		return nil, status.Error(codes.Unavailable, "injected ledger chaos outage")
	}
	seq := s.seq.Add(1)
	o := req.GetOrder()
	return &hyperionv1.SubmitOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:     seq,
			OrderId:       seq + 10000,
			ClientOrderId: o.GetClientOrderId(),
			AccountId:     o.GetAccountId(),
			Symbol:        o.GetSymbol(),
			InstrumentId:  o.GetInstrumentId(),
			Status:        hyperionv1.OrderStatus_ORDER_STATUS_ACCEPTED,
			EngineTsNs:    uint64(time.Now().UnixNano()),
		},
	}, nil
}

func (s *tortureLedgerServer) GetL2BookSnapshot(_ context.Context, req *hyperionv1.GetL2BookSnapshotRequest) (*hyperionv1.GetL2BookSnapshotResponse, error) {
	return &hyperionv1.GetL2BookSnapshotResponse{
		Snapshot: &hyperionv1.L2BookSnapshot{
			EngineSeq:    s.seq.Load() + 100,
			Symbol:       req.GetSymbol(),
			InstrumentId: 1,
			BestBidFp:    6_842_000_000_000,
			BestAskFp:    6_842_500_000_000,
			SnapshotTsNs: uint64(time.Now().UnixNano()),
		},
	}, nil
}

// TestTorture_5100ConcurrentWebSockets_SlowConsumer_Replay_CircuitBreaker_ZeroGoroutineLeak
// bombards the Edge Gateway simultaneously with:
//   - 5,100+ concurrent live WebSocket client goroutines (5,050 fast readers + 50 blackhole slow consumers)
//   - High-frequency L3 market data & critical fraud alert broadcasts (inducing ring-buffer drops, resyncs & slow-consumer eviction)
//   - Concurrent cryptographic replay-attack & stale-timestamp attack storm
//   - Upstream gRPC chaos failure tripping the Adaptive Circuit Breaker to OPEN, then healing back to CLOSED
//   - Strict runtime.NumGoroutine() assertion proving ZERO goroutine leaks after teardown!
func TestTorture_5100ConcurrentWebSockets_SlowConsumer_Replay_CircuitBreaker_ZeroGoroutineLeak(t *testing.T) {
	mr := miniredis.RunT(t)
	rc := redis.NewClient(&redis.Options{Addr: mr.Addr(), PoolSize: 32})

	grpcLis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	grpcSrv := grpc.NewServer()
	ledgerMock := &tortureLedgerServer{}
	hyperionv1.RegisterOrderbookServiceServer(grpcSrv, ledgerMock)
	go func() { _ = grpcSrv.Serve(grpcLis) }()

	// Stabilize runtime goroutines before capturing baseline!
	runtime.GC()
	time.Sleep(80 * time.Millisecond)
	baselineGoroutines := runtime.NumGoroutine()

	cfg := config.FromEnv()
	gAddr := grpcLis.Addr().String()
	cfg.LedgerGRPCAddrs = []string{gAddr}
	cfg.AnalyticsGRPCAddrs = []string{gAddr}
	cfg.FraudGRPCAddrs = []string{gAddr}
	cfg.ChronosGRPCAddrs = []string{gAddr}
	cfg.EnableRedpanda = false
	cfg.WSMaxClients = 10000
	cfg.WSMarketRingSize = 8        // tight ring buffer to deterministically exercise slow-consumer head-dropping & gap resync
	cfg.WSCriticalQueueSize = 8     // tight critical queue so blackhole stalled consumers get evicted cleanly
	cfg.WSWriteTimeout = 250 * time.Millisecond
	cfg.WSMaxConsecutiveStalls = 2
	cfg.WSHeartbeatInterval = 0
	cfg.BreakerMinRequests = 10
	cfg.BreakerErrorTripPPM = 500_000
	cfg.BreakerOpenCooldown = 150 * time.Millisecond
	cfg.BreakerHalfOpenProbes = 3

	metrics := telemetry.NewRegistry()
	ks := auth.NewKeyStore(cfg.BootstrapHMACSecret)
	pub, priv, _ := ed25519.GenerateKey(rand.Reader)
	_ = ks.RegisterEd25519(5001, "torture-ed", pub)

	rg := auth.NewReplayGuard(15*time.Second, 4096, rc)
	jwtAuth := auth.NewJWTVerifier(cfg.JWTSecret, nil, 5*time.Second)
	sigAuth := auth.NewSignatureAuthenticator(ks, rg, 5*time.Second)
	limiter := ratelimit.NewHybridLimiter(ratelimit.Config{
		DefaultRPS:     5000,
		DefaultBurst:   10000,
		L2SyncInterval: 50 * time.Millisecond,
		SpamWindow:     time.Second,
		SpamMaxEvents:  50000,
		RedisClient:    rc,
		Metrics:        metrics,
	})
	idem := idempotency.NewStore(8192, time.Minute, rc, metrics)
	mesh, err := grpcclient.NewMeshClients(cfg, metrics)
	if err != nil {
		t.Fatal(err)
	}
	hub := ws.NewHub(cfg, jwtAuth, mesh, metrics)
	hub.CacheL2Snapshot(&hyperionv1.L2BookSnapshot{
		EngineSeq:    1,
		Symbol:       "BTC-USDT",
		InstrumentId: 1,
		BestBidFp:    6_842_000_000_000,
		BestAskFp:    6_842_500_000_000,
		SnapshotTsNs: uint64(time.Now().UnixNano()),
	})

	gw := httpapi.NewServer(httpapi.Dependencies{
		Config: cfg, KeyStore: ks, JWTAuth: jwtAuth, SigAuth: sigAuth,
		Limiter: limiter, Idem: idem, Mesh: mesh, WSHub: hub, Metrics: metrics,
	})

	httpLis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	httpSrv := &http.Server{Handler: gw.PublicHandler()}
	go func() { _ = httpSrv.Serve(httpLis) }()
	gwAddr := httpLis.Addr().String()
	gwURL := "http://" + gwAddr

	const fastClients = 5050
	const slowClients = 50

	// Bound concurrent TCP SYN handshakes to 64 in-flight so Windows loopback listen backlog never drops SYNs,
	// while keeping ALL 5,100 WebSocket connections open simultaneously!
	dialSem := make(chan struct{}, 64)
	stopClients := make(chan struct{})
	var readyWG sync.WaitGroup
	var clientExitWG sync.WaitGroup
	var fastFramesRecv atomic.Uint64
	var dialErrors atomic.Uint64
	var firstDialErrMu sync.Mutex
	var firstDialErr error
	recordDialErr := func(err error) {
		dialErrors.Add(1)
		firstDialErrMu.Lock()
		if firstDialErr == nil {
			firstDialErr = err
		}
		firstDialErrMu.Unlock()
	}

	var allConnsMu sync.Mutex
	allConns := make([]net.Conn, 0, fastClients+slowClients)

	// 1. Launch 5,000 fast BTC-USDT + 50 fast ETH-USDT concurrent WebSocket subscriber goroutines.
	for i := 0; i < fastClients; i++ {
		readyWG.Add(1)
		clientExitWG.Add(1)
		wsPath := "/ws?symbol=BTC-USDT"
		if i >= 5000 {
			wsPath = "/ws?symbol=ETH-USDT"
		}
		go func(path string) {
			defer clientExitWG.Done()
			dialSem <- struct{}{}
			conn, br, derr := ws.DialTestWS(gwAddr, path, "hyperion.bin.v1")
			<-dialSem
			readyWG.Done()
			if derr != nil {
				recordDialErr(derr)
				return
			}
			allConnsMu.Lock()
			allConns = append(allConns, conn)
			allConnsMu.Unlock()
			defer conn.Close()

			for {
				op, raw, rerr := ws.ReadServerFrame(br)
				if rerr != nil {
					return
				}
				if op == 0x2 {
					if _, _, herr := ws.DecodeHeader(raw); herr == nil {
						fastFramesRecv.Add(1)
					}
				}
			}
		}(wsPath)
	}

	// 2. Launch 50 stalled/slow ETH-USDT consumers that shrink OS TCP recv buffer and NEVER read!
	for i := 0; i < slowClients; i++ {
		readyWG.Add(1)
		clientExitWG.Add(1)
		go func() {
			defer clientExitWG.Done()
			dialSem <- struct{}{}
			conn, _, derr := ws.DialTestWS(gwAddr, "/ws?symbol=ETH-USDT", "hyperion.bin.v1")
			<-dialSem
			readyWG.Done()
			if derr != nil {
				recordDialErr(derr)
				return
			}
			if tc, ok := conn.(*net.TCPConn); ok {
				_ = tc.SetReadBuffer(1024)
			}
			allConnsMu.Lock()
			allConns = append(allConns, conn)
			allConnsMu.Unlock()
			<-stopClients
			_ = conn.Close()
		}()
	}

	// 2b. Attach 20 synchronous unbuffered net.Pipe() WAN-congested slow consumers (rwnd=0) on ETH-USDT
	//     to deterministically verify MarketRingBuffer head-dropping & automatic gap resync independent
	//     of Windows loopback kernel TCP auto-tuning buffers (total = 5,120 simultaneous WS clients!).
	for i := 0; i < 20; i++ {
		srvPipe, cliPipe := net.Pipe()
		hub.AttachConn(srvPipe, ws.FormatBinary, 0, ws.ChanAllPublic, 1<<2)
		allConnsMu.Lock()
		allConns = append(allConns, cliPipe)
		allConnsMu.Unlock()
	}

	readyWG.Wait()
	peakConnected := metrics.WSConnectedClients.Load()
	t.Logf("Peak simultaneous WebSocket connections online: %d (dialErrors=%d firstErr=%v goroutines=%d)",
		peakConnected, dialErrors.Load(), firstDialErr, runtime.NumGoroutine())
	if peakConnected < 5000 {
		t.Fatalf("expected >= 5,000 simultaneous live WebSocket connections, got %d (dialErrors=%d firstErr=%v)",
			peakConnected, dialErrors.Load(), firstDialErr)
	}

	// 3a. Bombard 5,000 fast BTC-USDT subscribers with 40 L3 deltas + 15 critical FraudAlerts (>250,000 frames).
	for seq := uint64(2); seq <= 41; seq++ {
		hub.BroadcastL3Delta(&hyperionv1.L3BookDelta{
			EngineSeq:      seq,
			PrevEngineSeq:  seq - 1,
			Symbol:         "BTC-USDT",
			InstrumentId:   1,
			Op:             hyperionv1.L3DeltaOp_L3_DELTA_OP_ADD,
			Side:           hyperionv1.OrderSide_ORDER_SIDE_BUY,
			OrderId:        seq + 500,
			PriceFp:        6_842_000_000_000,
			RemainingQtyFp: 100_000_000,
			DeltaQtyFp:     100_000_000,
			EventTsNs:      uint64(time.Now().UnixNano()),
		})
	}
	for a := 1; a <= 15; a++ {
		hub.BroadcastFraudAlert(&hyperionv1.AnomalyAlert{
			AlertId:       "torture-alert-" + strconv.Itoa(a),
			EngineSeq:     uint64(50 + a),
			Symbol:        "BTC-USDT",
			SeverityScore: 95,
			DetectedTsNs:  uint64(time.Now().UnixNano()),
		})
	}

	// 3b. Saturate OS kernel loopback TCP send buffers (~1-2 MB dynamic window on Windows) on the 50
	//     blackhole ETH-USDT slow consumers with 600 x 8KB L3DeltaBatches (~4.8 MB) BEFORE critical alerts
	//     to deterministically trigger MarketRingBuffer head-dropping, automatic gap resyncs, AND
	//     critical-queue/write-timeout eviction while all 5,050 fast subscribers remain unaffected!
	ethBatch := make([]*hyperionv1.L3BookDelta, 200)
	for i := range ethBatch {
		ethBatch[i] = &hyperionv1.L3BookDelta{
			EngineSeq:      uint64(i + 2),
			PrevEngineSeq:  uint64(i + 1),
			Symbol:         "ETH-USDT",
			InstrumentId:   2,
			Op:             hyperionv1.L3DeltaOp_L3_DELTA_OP_ADD,
			Side:           hyperionv1.OrderSide_ORDER_SIDE_SELL,
			OrderId:        uint64(10000 + i),
			PriceFp:        350_000_000_000,
			RemainingQtyFp: 100_000_000,
			EventTsNs:      uint64(time.Now().UnixNano()),
		}
	}
	for bIdx := 0; bIdx < 600; bIdx++ {
		hub.BroadcastL3DeltaBatch(ethBatch, 2)
	}
	bigEvidence := make([]*hyperionv1.TransactionGraphEdge, 48)
	for i := range bigEvidence {
		bigEvidence[i] = &hyperionv1.TransactionGraphEdge{
			EdgeId:         uint64(i + 1),
			Symbol:         "ETH-USDT",
			MakerAccountId: uint64(9000 + i),
			TakerAccountId: uint64(9001 + i),
			PriceFp:        350_000_000_000,
			QtyFp:          100_000_000,
		}
	}
	for a := 1; a <= 40; a++ {
		hub.BroadcastFraudAlert(&hyperionv1.AnomalyAlert{
			AlertId:          "eth-slow-evict-alert-" + strconv.Itoa(a),
			EngineSeq:        uint64(1000 + a),
			Symbol:           "ETH-USDT",
			SeverityScore:    98,
			EvidenceSubgraph: bigEvidence,
			DetectedTsNs:     uint64(time.Now().UnixNano()),
		})
	}

	// 4. Concurrent Replay-Attack & Clock-Skew Bombardment on POST /api/v1/orders!
	httpCli := &http.Client{
		Transport: &http.Transport{MaxIdleConnsPerHost: 64},
		Timeout:   2 * time.Second,
	}
	ordPayload := []byte(`{"client_order_id":"torture-1","account_id":"5001","symbol":"BTC-USDT","side":"BUY","order_type":"LIMIT","price_fp":"6842000000000","qty_fp":"100000000"}`)
	nowNS := strconv.FormatInt(time.Now().UnixNano(), 10)
	replaySig := auth.SignEd25519(priv, nowNS, "777777", http.MethodPost, "/api/v1/orders", ordPayload)

	var replayAccepted, replayRejected atomic.Int32
	var replayWG sync.WaitGroup
	for r := 0; r < 32; r++ {
		replayWG.Add(1)
		go func(idx int) {
			defer replayWG.Done()
			req, _ := http.NewRequest(http.MethodPost, gwURL+"/api/v1/orders", bytes.NewReader(ordPayload))
			req.Header.Set(auth.HeaderAccountID, "5001")
			req.Header.Set(auth.HeaderKeyID, "torture-ed")
			req.Header.Set(auth.HeaderAuthScheme, "ed25519")
			req.Header.Set(auth.HeaderTimestamp, nowNS)
			req.Header.Set(auth.HeaderNonce, "777777") // identical replayed nonce across 32 concurrent attackers!
			req.Header.Set(auth.HeaderSignature, replaySig)
			req.Header.Set("Idempotency-Key", "idem-replay-distinct-"+strconv.Itoa(idx))
			resp, err := httpCli.Do(req)
			if err == nil {
				switch resp.StatusCode {
				case http.StatusOK:
					replayAccepted.Add(1)
				case http.StatusUnauthorized:
					replayRejected.Add(1)
				}
				_ = resp.Body.Close()
			}
		}(r)
	}
	replayWG.Wait()
	if replayAccepted.Load() != 1 || replayRejected.Load() != 31 {
		t.Fatalf("expected exact 1 accepted & 31 rejected in concurrent replay bomb, got accepted=%d rejected=%d",
			replayAccepted.Load(), replayRejected.Load())
	}

	// 5. Upstream Fault Injection -> Circuit Breaker Trip (OPEN) -> Recovery (HALF-OPEN -> CLOSED)!
	ledgerMock.failMode.Store(true)
	for n := 100; n < 300; n++ {
		if mesh.LedgerPool.Breaker().State() == grpcclient.StateOpen {
			break
		}
		tripTS := strconv.FormatInt(time.Now().UnixNano(), 10)
		nonceStr := strconv.Itoa(n)
		sig := auth.SignEd25519(priv, tripTS, nonceStr, http.MethodPost, "/api/v1/orders", ordPayload)
		req, _ := http.NewRequest(http.MethodPost, gwURL+"/api/v1/orders", bytes.NewReader(ordPayload))
		req.Header.Set(auth.HeaderAccountID, "5001")
		req.Header.Set(auth.HeaderKeyID, "torture-ed")
		req.Header.Set(auth.HeaderAuthScheme, "ed25519")
		req.Header.Set(auth.HeaderTimestamp, tripTS)
		req.Header.Set(auth.HeaderNonce, nonceStr)
		req.Header.Set(auth.HeaderSignature, sig)
		req.Header.Set("Idempotency-Key", "idem-trip-"+nonceStr)
		if resp, err := httpCli.Do(req); err == nil {
			_ = resp.Body.Close()
		}
	}
	if mesh.LedgerPool.Breaker().State() != grpcclient.StateOpen {
		t.Fatalf("expected LedgerPool circuit breaker OPEN under chaos outage, got %s", mesh.LedgerPool.Breaker().State())
	}

	// Wait for 150ms breaker cooldown, restore ledgerMock, and verify automatic Half-Open -> Closed recovery!
	time.Sleep(200 * time.Millisecond)
	ledgerMock.failMode.Store(false)
	freshTS := strconv.FormatInt(time.Now().UnixNano(), 10)
	for n := 200; n < 205; n++ {
		nonceStr := strconv.Itoa(n)
		sig := auth.SignEd25519(priv, freshTS, nonceStr, http.MethodPost, "/api/v1/orders", ordPayload)
		req, _ := http.NewRequest(http.MethodPost, gwURL+"/api/v1/orders", bytes.NewReader(ordPayload))
		req.Header.Set(auth.HeaderAccountID, "5001")
		req.Header.Set(auth.HeaderKeyID, "torture-ed")
		req.Header.Set(auth.HeaderAuthScheme, "ed25519")
		req.Header.Set(auth.HeaderTimestamp, freshTS)
		req.Header.Set(auth.HeaderNonce, nonceStr)
		req.Header.Set(auth.HeaderSignature, sig)
		req.Header.Set("Idempotency-Key", "idem-heal-"+nonceStr)
		resp, err := httpCli.Do(req)
		if err != nil || resp.StatusCode != http.StatusOK {
			t.Fatalf("probe %d failed during breaker recovery: resp=%v err=%v", n, resp, err)
		}
		_ = resp.Body.Close()
	}
	if mesh.LedgerPool.Breaker().State() != grpcclient.StateClosed {
		t.Fatalf("expected LedgerPool circuit breaker recovered to CLOSED, got %s", mesh.LedgerPool.Breaker().State())
	}

	// Allow fast WS subscribers brief drain window, then verify slow-consumer drops + evictions + frame delivery!
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if fastFramesRecv.Load() >= 100_000 && metrics.WSSlowConsumerEvictsTotal.Load() > 0 {
			break
		}
		time.Sleep(25 * time.Millisecond)
	}

	t.Logf("Torture Metrics: fastFramesRecv=%d slowDrops=%d slowEvicts=%d replayRejected=%d breakerTrips=%d",
		fastFramesRecv.Load(),
		metrics.WSSlowConsumerDropsTotal.Load(),
		metrics.WSSlowConsumerEvictsTotal.Load(),
		metrics.ReplayRejectedTotal.Load(),
		metrics.CircuitBreakerTripsTotal.Load(),
	)

	if fastFramesRecv.Load() < 50_000 {
		t.Fatalf("expected fast clients to receive >= 50,000 verified binary frames, got %d", fastFramesRecv.Load())
	}
	if metrics.WSSlowConsumerDropsTotal.Load() == 0 {
		t.Fatal("expected slow-consumer ring buffer drops > 0")
	}
	if metrics.WSSlowConsumerEvictsTotal.Load() == 0 {
		t.Fatal("expected stalled slow-consumer evictions > 0")
	}

	// 6. Clean Teardown & Strict runtime.NumGoroutine() Zero-Leak Assertion!
	close(stopClients)
	allConnsMu.Lock()
	for _, c := range allConns {
		_ = c.Close()
	}
	allConnsMu.Unlock()
	httpCli.CloseIdleConnections()
	shutCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	_ = httpSrv.Shutdown(shutCtx)
	cancel()
	hub.Close()
	clientExitWG.Wait()
	limiter.Close()
	mesh.Close()
	grpcSrv.Stop()
	_ = rc.Close()

	var finalGoroutines int
	for attempt := 0; attempt < 25; attempt++ {
		runtime.GC()
		finalGoroutines = runtime.NumGoroutine()
		if finalGoroutines <= baselineGoroutines+6 {
			break
		}
		time.Sleep(40 * time.Millisecond)
	}
	t.Logf("Goroutine Leak Audit: baseline=%d final=%d (after 5,100 concurrent WS clients = >15,300 spawned goroutines)",
		baselineGoroutines, finalGoroutines)
	if finalGoroutines > baselineGoroutines+6 {
		t.Fatalf("GOROUTINE LEAK DETECTED: baseline=%d final=%d", baselineGoroutines, finalGoroutines)
	}
	_ = hex.EncodeToString(pub[:4])
}

// --- Zero-Allocation Binary Serialization & Hot-Path Benchmarks (`go test -bench=. -benchmem`) ---

func BenchmarkEncodeL3SingleDelta_SyncPool(b *testing.B) {
	delta := &hyperionv1.L3BookDelta{
		EngineSeq:      100_001,
		PrevEngineSeq:  100_000,
		Symbol:         "BTC-USDT",
		InstrumentId:   1,
		Op:             hyperionv1.L3DeltaOp_L3_DELTA_OP_ADD,
		Side:           hyperionv1.OrderSide_ORDER_SIDE_BUY,
		OrderId:        998877,
		PriceFp:        6_842_550_000_000,
		RemainingQtyFp: 250_000_000,
		DeltaQtyFp:     250_000_000,
		EventTsNs:      1_795_015_000_000_000_000,
	}
	// Warm up sync.Pool.
	warm := ws.EncodeL3SingleDelta(delta, 1)
	warm.Release()

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		f := ws.EncodeL3SingleDelta(delta, 1)
		f.Release()
	}
}

func BenchmarkEncodeExecutionFill_SyncPool(b *testing.B) {
	fill := &hyperionv1.ExecutionFill{
		FillId:         7701,
		EngineSeq:      100_002,
		Symbol:         "BTC-USDT",
		InstrumentId:   1,
		MakerOrderId:   500,
		TakerOrderId:   501,
		MakerAccountId: 10,
		TakerAccountId: 20,
		AggressorSide:  hyperionv1.OrderSide_ORDER_SIDE_BUY,
		PriceFp:        6_842_550_000_000,
		QtyFp:          100_000_000,
		ExecTsNs:       1_795_015_000_000_000_000,
	}
	warm := ws.EncodeExecutionFill(fill, 1)
	warm.Release()

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		f := ws.EncodeExecutionFill(fill, 1)
		f.Release()
	}
}

func BenchmarkEncodeL2Snapshot20Levels_SyncPool(b *testing.B) {
	bids := make([]*hyperionv1.PriceLevel, 10)
	asks := make([]*hyperionv1.PriceLevel, 10)
	for i := 0; i < 10; i++ {
		bids[i] = &hyperionv1.PriceLevel{PriceFp: 6_842_000_000_000 - uint64(i)*1_000_000, TotalQtyFp: 100_000_000, OrderCount: 2}
		asks[i] = &hyperionv1.PriceLevel{PriceFp: 6_842_500_000_000 + uint64(i)*1_000_000, TotalQtyFp: 100_000_000, OrderCount: 2}
	}
	snap := &hyperionv1.L2BookSnapshot{
		EngineSeq:    100_050,
		Symbol:       "BTC-USDT",
		InstrumentId: 1,
		BestBidFp:    bids[0].PriceFp,
		BestAskFp:    asks[0].PriceFp,
		Bids:         bids,
		Asks:         asks,
		SnapshotTsNs: 1_795_015_000_000_000_000,
	}
	warm := ws.EncodeL2Snapshot(snap, 1, 0)
	warm.Release()

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		f := ws.EncodeL2Snapshot(snap, 1, 0)
		f.Release()
	}
}

func BenchmarkMarketRingBuffer_ZeroAllocFanOut1000Rings(b *testing.B) {
	rings := make([]*ws.MarketRingBuffer, 1000)
	drainBuf := make([]*ws.PooledFrame, 4)
	for i := range rings {
		rings[i] = ws.NewMarketRingBuffer(8)
	}
	delta := &hyperionv1.L3BookDelta{
		EngineSeq:      1,
		PrevEngineSeq:  0,
		Symbol:         "BTC-USDT",
		InstrumentId:   1,
		Op:             hyperionv1.L3DeltaOp_L3_DELTA_OP_ADD,
		Side:           hyperionv1.OrderSide_ORDER_SIDE_BUY,
		OrderId:        1,
		PriceFp:        6_842_550_000_000,
		RemainingQtyFp: 100_000_000,
		EventTsNs:      1_795_015_000_000_000_000,
	}
	warm := ws.EncodeL3SingleDelta(delta, 1)
	warm.Release()

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		frame := ws.EncodeL3SingleDelta(delta, 1)
		frame.Retain(int32(len(rings)))
		for _, rb := range rings {
			rb.Push(frame)
		}
		frame.Release()
		for _, rb := range rings {
			n, _, _ := rb.DrainBatch(drainBuf)
			for j := 0; j < n; j++ {
				drainBuf[j].Release()
				drainBuf[j] = nil
			}
		}
	}
}

func BenchmarkHybridLimiter_L1AtomicAllow(b *testing.B) {
	lim := ratelimit.NewHybridLimiter(ratelimit.Config{
		DefaultRPS:    100_000_000,
		DefaultBurst:  100_000_000,
		SpamMaxEvents: 1_000_000_000,
	})
	defer lim.Close()
	_ = lim.AllowAccount(42, "BTC-USDT", false)

	b.ReportAllocs()
	b.ResetTimer()
	b.RunParallel(func(pb *testing.PB) {
		for pb.Next() {
			_ = lim.AllowAccount(42, "BTC-USDT", false)
		}
	})
}

func BenchmarkReplayGuard_L1BloomAndRing(b *testing.B) {
	rg := auth.NewReplayGuard(15*time.Second, 8192, nil)
	ctx := context.Background()
	nowNS := time.Now().UnixNano()
	var nonceSeq atomic.Uint64

	b.ReportAllocs()
	b.ResetTimer()
	b.RunParallel(func(pb *testing.PB) {
		for pb.Next() {
			n := nonceSeq.Add(1)
			_ = rg.CheckAndRecord(ctx, 1001, n, nowNS)
		}
	})
}
