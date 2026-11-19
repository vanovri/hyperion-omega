package engine_test

import (
	"bufio"
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/auditor"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/control"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/engine"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/transport"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"google.golang.org/grpc"
)

func TestThousandBotsDeterministicReproducibility(t *testing.T) {
	runSim := func() (engine.TelemetrySnapshot, auditor.AuditReport) {
		cfg := engine.DefaultSimulatorConfig()
		cfg.MasterSeed = 0xDEADBEEF2026
		cfg.TotalBots = 1024 // >= 1,000 autonomous heterogeneous bots
		cfg.WorkerShards = 8
		sim, err := engine.NewSimulator(cfg)
		if err != nil {
			t.Fatalf("NewSimulator failed: %v", err)
		}
		defer sim.Stop()

		baseTs := int64(1_795_000_000_000_000_000)
		for step := 0; step < 30; step++ {
			if step == 10 {
				sim.ApplyCampaign(chaos.PresetForRegime(model.RegimeHighVolatility), true)
			} else if step == 20 {
				sim.ApplyCampaign(chaos.PresetForRegime(model.RegimeFlashCrash), true)
			}
			sim.StepDeterministic(baseTs + int64(step)*10_000_000)
		}

		rep, err := sim.RunAuditNow()
		if err != nil {
			t.Fatalf("RunAuditNow failed: %v", err)
		}
		return sim.Telemetry(), rep
	}

	tel1, rep1 := runSim()
	tel2, rep2 := runSim()

	if !rep1.Passed || !rep2.Passed {
		t.Fatalf("audit failed: rep1=%+v rep2=%+v", rep1.Violations, rep2.Violations)
	}
	if tel1.OrdersSubmitted == 0 || tel1.TradesMatched == 0 {
		t.Fatalf("expected active orders & trades: %+v", tel1)
	}
	if tel1.OrdersSubmitted != tel2.OrdersSubmitted ||
		tel1.TradesMatched != tel2.TradesMatched ||
		tel1.MidPriceFP != tel2.MidPriceFP ||
		tel1.VWAPFP != tel2.VWAPFP ||
		rep1.FillsVerified != rep2.FillsVerified {
		t.Fatalf("determinism violated!\nrun1=%+v\nrun2=%+v", tel1, tel2)
	}
}

func TestParallelChaosTortureSyndicateAndSubCentInvariantVerifier(t *testing.T) {
	baseGoroutines := runtime.NumGoroutine()

	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 1024
	cfg.WorkerShards = 8
	cfg.TickInterval = 5 * time.Millisecond
	cfg.AuditInterval = 50 * time.Millisecond

	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		t.Fatal(err)
	}

	// Enable chaotic conditions: 20x burst + packet reordering + idempotent duplicates + clock skew + corrupt sigs
	prof := chaos.PresetForRegime(model.RegimeBurstTsunami)
	prof.DuplicateProb = 0.15
	prof.DuplicateCopies = 3
	prof.ReorderProb = 0.20
	prof.ClockSkewProb = 0.05
	prof.CorruptSignatureProb = 0.05
	sim.ApplyCampaign(prof, true)

	sim.Start()
	time.Sleep(120 * time.Millisecond)

	// Concurrently fire all 3 Adversarial Syndicate Attacks while parallel shards run
	ctx := context.Background()
	washRes := sim.TriggerSyndicateAttack(ctx, engine.SyndicateAttackRequest{
		AttackType: engine.AttackWashRing,
		RingSize:   4,
		Rounds:     5,
		Lots:       12,
	})
	if washRes.OrdersDispatched == 0 || len(washRes.ParticipantIDs) != 4 {
		t.Fatalf("unexpected wash ring result: %+v", washRes)
	}

	spoofRes := sim.TriggerSyndicateAttack(ctx, engine.SyndicateAttackRequest{
		AttackType: engine.AttackLayeringSpoofer,
		Rounds:     3,
		Lots:       20,
	})
	if spoofRes.OrdersDispatched == 0 {
		t.Fatalf("unexpected spoofer result: %+v", spoofRes)
	}

	sweepRes := sim.TriggerSyndicateAttack(ctx, engine.SyndicateAttackRequest{
		AttackType: engine.AttackFlashSweep,
		Rounds:     2,
		Lots:       30,
	})
	if sweepRes.OrdersDispatched == 0 {
		t.Fatalf("unexpected flash sweep result: %+v", sweepRes)
	}

	time.Sleep(80 * time.Millisecond)
	sim.Stop()

	// Verify 0-unit (1e-8 = 1/1,000,000th of a cent) conservation & account balance reconciliation
	rep, err := sim.RunAuditNow()
	if err != nil {
		t.Fatal(err)
	}
	if !rep.Passed || len(rep.Violations) != 0 {
		t.Fatalf("invariant verifier detected violations: %+v", rep)
	}
	if rep.MaxAbsQuoteDriftUnits != 0 || rep.MaxAbsBaseDriftUnits != 0 {
		t.Fatalf("expected 0 drift units, got quote=%d base=%d", rep.MaxAbsQuoteDriftUnits, rep.MaxAbsBaseDriftUnits)
	}
	if rep.GlobalQuoteConservationDiff != "0" || rep.GlobalBaseConservationDiff != "0" {
		t.Fatalf("global double-entry conservation drift: quote=%s base=%s", rep.GlobalQuoteConservationDiff, rep.GlobalBaseConservationDiff)
	}
	if rep.DuplicateDedupes == 0 {
		t.Fatalf("expected idempotent duplicate deduplications to be verified, got 0")
	}

	// Prove auditor immediately catches a synthetic 1-cent phantom fill
	negVerifier := auditor.NewInvariantVerifier()
	negVerifier.RegisterAccount("10001", 100*fixed.Scale, 10*fixed.Scale)
	negVerifier.InjectSyntheticPhantomFill(model.FillEvent{
		TradeID:       999999,
		ClientOrderID: 888888, // Unknown never-submitted order!
		AccountID:     "10001",
		Side:          model.SideBuy,
		Price:         50_000 * fixed.Scale,
		Quantity:      fixed.CentUnits,
	})
	negRep := negVerifier.LastReport()
	if negRep.Passed || len(negRep.Violations) == 0 || negRep.Violations[0].Type != auditor.ViolationPhantomFill {
		t.Fatalf("expected PHANTOM_FILL violation to be caught, got %+v", negRep)
	}

	time.Sleep(40 * time.Millisecond)
	if after := runtime.NumGoroutine(); after > baseGoroutines+6 {
		t.Fatalf("goroutine leak after Simulator.Stop(): before=%d after=%d", baseGoroutines, after)
	}
}

func TestControlPlaneAPIAndSSEAndSignedTransports(t *testing.T) {
	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 128
	cfg.WorkerShards = 4
	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		t.Fatal(err)
	}
	sim.Start()
	defer sim.Stop()

	px := chaos.NewToxicProxy("127.0.0.1:0", "127.0.0.1:50151", 1, chaos.DefaultToxicConfig())
	if err := px.Start(); err != nil {
		t.Fatal(err)
	}
	ctrl := control.NewControlServer(sim, map[string]*chaos.ToxicProxy{"ledger-grpc": px})
	defer ctrl.Close()

	ts := httptest.NewServer(ctrl.Handler())
	defer ts.Close()

	// 1. Healthz
	resp, err := http.Get(ts.URL + "/healthz")
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("healthz failed: %v", err)
	}
	_ = resp.Body.Close()

	// 2. OpenAPI POST /api/v1/chaos/campaigns
	campReq := `{"regime":"SPOOFING_ATTACK","enabled":true,"duration_ms":500}`
	resp, err = http.Post(ts.URL+"/api/v1/chaos/campaigns", "application/json", strings.NewReader(campReq))
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("campaigns POST failed: %v status=%d", err, resp.StatusCode)
	}
	var campOut map[string]any
	_ = json.NewDecoder(resp.Body).Decode(&campOut)
	_ = resp.Body.Close()
	if campOut["regime"] != "SPOOFING_ATTACK" || campOut["active"] != true {
		t.Fatalf("unexpected campaign response: %+v", campOut)
	}

	// 3. On-demand Syndicate REST trigger
	syndReq := `{"attack_type":"WASH_TRADING_RING","ring_size":4,"rounds":2,"lots":5}`
	resp, err = http.Post(ts.URL+"/api/v1/chaos/syndicate", "application/json", strings.NewReader(syndReq))
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("syndicate POST failed: %v", err)
	}
	_ = resp.Body.Close()

	// 4. Toxic Proxy mutation
	pxReq := `{"enabled":true,"base_latency_ms":15,"pareto_jitter_ms":5,"pareto_alpha":1.6}`
	resp, err = http.Post(ts.URL+"/api/v1/chaos/proxies/ledger-grpc", "application/json", strings.NewReader(pxReq))
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("proxy POST failed: %v", err)
	}
	_ = resp.Body.Close()

	// 5. Audit Verify Now
	resp, err = http.Post(ts.URL+"/api/v1/chaos/audit/verify", "application/json", strings.NewReader(`{}`))
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("audit verify POST failed: %v", err)
	}
	_ = resp.Body.Close()

	// 6. Prometheus /metrics
	resp, err = http.Get(ts.URL + "/metrics")
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("metrics GET failed: %v", err)
	}
	mBody, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	if !bytes.Contains(mBody, []byte("hyperion_chaos_invariant_passed 1")) {
		t.Fatalf("missing invariant metric in /metrics:\n%s", string(mBody))
	}

	// 7. SSE Live Stream (/events/chaos)
	sseCtx, sseCancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer sseCancel()
	sseReq, _ := http.NewRequestWithContext(sseCtx, http.MethodGet, ts.URL+"/events/chaos", nil)
	sseResp, err := http.DefaultClient.Do(sseReq)
	if err != nil || sseResp.StatusCode != http.StatusOK {
		t.Fatalf("SSE stream failed: %v", err)
	}
	br := bufio.NewReader(sseResp.Body)
	line1, _ := br.ReadString('\n')
	line2, _ := br.ReadString('\n')
	_ = sseResp.Body.Close()
	if !strings.HasPrefix(line1, "event: telemetry") || !strings.HasPrefix(line2, "data: {") {
		t.Fatalf("unexpected SSE frame: %q / %q", line1, line2)
	}

	// 8. Verify GatewayHTTPClient §6.2 Ed25519 signature preimage + LedgerGRPCClient transport
	bot := agents.NewBotAccount(42, 0, "test", model.ArchetypeMomentumQuant, 100_000*fixed.Scale, 5*fixed.Scale)
	gwSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		rawBody, _ := io.ReadAll(r.Body)
		tsHdr := r.Header.Get("X-Hyperion-Timestamp")
		nonceHdr := r.Header.Get("X-Hyperion-Nonce")
		sigHex := r.Header.Get("X-Hyperion-Signature")
		sum := sha256.Sum256(rawBody)
		preimage := tsHdr + "\n" + nonceHdr + "\n" + r.Method + "\n" + r.URL.Path + "\n" + hex.EncodeToString(sum[:])
		sigBytes, _ := hex.DecodeString(sigHex)
		if !ed25519.Verify(bot.PubKey, []byte(preimage), sigBytes) {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"engine_seq":"1","order_id":"9001","client_order_id":"csim-0-1","account_id":"10001","symbol":"BTC-USDT","status":"ACCEPTED","filled_qty_fp":"0","remaining_qty_fp":"100000000","avg_fill_price_fp":"0","engine_ts_ns":"1000"}`))
	}))
	defer gwSrv.Close()

	gwClient := transport.NewGatewayHTTPClient(gwSrv.URL, transport.AuthEd25519, 4)
	cid, idem, nonce := bot.NextClientIDs()
	intent := model.OrderIntent{
		ClientOrderID:  cid,
		IdempotencyKey: idem,
		AccountNum:     bot.AccountNum,
		AccountID:      bot.AccountID,
		InstrumentID:   1,
		BotID:          bot.BotID,
		Kind:           model.ActionSubmit,
		Side:           model.SideBuy,
		OrderType:      model.OrderTypeLimit,
		Symbol:         "BTC-USDT",
		Price:          68_000 * fixed.Scale,
		Quantity:       1 * fixed.Scale,
		TimestampNs:    time.Now().UnixNano(),
		Nonce:          nonce,
	}
	gwRes := gwClient.SendOrderIntent(context.Background(), bot, intent)
	if gwRes.Status != model.StatusAccepted || gwRes.ServerOrderID != 9001 {
		t.Fatalf("GatewayHTTPClient failed verification: %+v", gwRes)
	}

	// Corrupt signature must trigger HTTP 401 -> StatusRejected
	badIntent := intent
	badIntent.CorruptSignature = true
	badRes := gwClient.SendOrderIntent(context.Background(), bot, badIntent)
	if badRes.Status != model.StatusRejected {
		t.Fatalf("expected rejected status on corrupt signature, got %+v", badRes)
	}

	// Verify LedgerGRPCClient against stub gRPC OrderbookServiceServer
	grpcLn, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	grpcSrv := grpc.NewServer()
	hyperionv1.RegisterOrderbookServiceServer(grpcSrv, &stubOrderbookServer{})
	go func() { _ = grpcSrv.Serve(grpcLn) }()
	defer grpcSrv.Stop()

	lClient, err := transport.NewLedgerGRPCClient(grpcLn.Addr().String(), 2)
	if err != nil {
		t.Fatal(err)
	}
	defer lClient.Close()

	grpcRes := lClient.SendOrderIntent(context.Background(), bot, intent)
	if grpcRes.Status != model.StatusAccepted || grpcRes.ServerOrderID != 7777 {
		t.Fatalf("LedgerGRPCClient failed: %+v", grpcRes)
	}
}

type stubOrderbookServer struct {
	hyperionv1.UnimplementedOrderbookServiceServer
}

func (s *stubOrderbookServer) SubmitOrder(_ context.Context, req *hyperionv1.SubmitOrderRequest) (*hyperionv1.SubmitOrderResponse, error) {
	return &hyperionv1.SubmitOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:      1,
			OrderId:        7777,
			ClientOrderId:  req.Order.ClientOrderId,
			AccountId:      req.Order.AccountId,
			Symbol:         req.Order.Symbol,
			Status:         hyperionv1.OrderStatus_ORDER_STATUS_ACCEPTED,
			RemainingQtyFp: req.Order.QtyFp,
		},
	}, nil
}
