package redpanda_test

import (
	"context"
	"testing"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/redpanda"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
	"google.golang.org/protobuf/proto"
)

func TestBridge_DispatchAllCanonicalTopics(t *testing.T) {
	cfg := config.FromEnv()
	cfg.WSHeartbeatInterval = 0
	metrics := telemetry.NewRegistry()
	hub := ws.NewHub(cfg, nil, nil, metrics)
	t.Cleanup(hub.Close)

	lim := ratelimit.NewHybridLimiter(ratelimit.Config{DefaultRPS: 100, DefaultBurst: 200, Metrics: metrics})
	t.Cleanup(lim.Close)

	bridge, err := redpanda.NewBridge(nil, "", false, hub, lim)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(bridge.Close)

	ctx := context.Background()
	cases := []struct {
		topic string
		msg   proto.Message
	}{
		{
			redpanda.TopicMarketL3Deltas,
			&hyperionv1.L3BookDelta{EngineSeq: 11, PrevEngineSeq: 10, Symbol: "BTC-USDT", InstrumentId: 1, PriceFp: 6_842_000_000_000, RemainingQtyFp: 100_000_000},
		},
		{
			redpanda.TopicMarketL2Snaps,
			&hyperionv1.L2BookSnapshot{EngineSeq: 12, Symbol: "BTC-USDT", InstrumentId: 1, BestBidFp: 6_842_000_000_000, BestAskFp: 6_842_500_000_000},
		},
		{
			redpanda.TopicMarketFills,
			&hyperionv1.ExecutionFill{FillId: 1, EngineSeq: 13, Symbol: "BTC-USDT", InstrumentId: 1, PriceFp: 6_842_500_000_000, QtyFp: 50_000_000},
		},
		{
			redpanda.TopicAnalyticsCandle,
			&hyperionv1.OhlcvCandle{Symbol: "BTC-USDT", Interval: hyperionv1.CandleInterval_CANDLE_INTERVAL_1S, CloseFp: 6_842_500_000_000, LastEngineSeq: 13},
		},
		{
			redpanda.TopicAnalyticsMicro,
			&hyperionv1.MicrostructureMetrics{Symbol: "BTC-USDT", EngineSeq: 13, MidPriceFp: 6_842_250_000_000},
		},
		{
			redpanda.TopicFraudAlerts,
			&hyperionv1.AnomalyAlert{AlertId: "a-1", EngineSeq: 14, Symbol: "BTC-USDT", SeverityScore: 91},
		},
		{
			redpanda.TopicRiskMitigations,
			&hyperionv1.RiskMitigationCommand{
				CommandId:          "m-1",
				Action:             hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT,
				TargetAccountIds:   []uint64{8080},
				CooldownDurationMs: 10_000,
				IssuedTsNs:         uint64(time.Now().UnixNano()),
			},
		},
	}

	for _, tc := range cases {
		raw, merr := proto.Marshal(tc.msg)
		if merr != nil {
			t.Fatal(merr)
		}
		if err := bridge.DispatchPayload(ctx, tc.topic, raw); err != nil {
			t.Fatalf("DispatchPayload(%s) failed: %v", tc.topic, err)
		}
	}

	if metrics.WSMessagesBroadcastTotal.Load() < 6 {
		t.Fatalf("expected >= 6 WS hub broadcasts from Redpanda topics, got %d", metrics.WSMessagesBroadcastTotal.Load())
	}
	if dec := lim.AllowAccount(8080, "BTC-USDT", true); dec.Allowed || dec.Reason != "ACCOUNT_FROZEN" {
		t.Fatalf("expected account 8080 frozen via Redpanda risk topic, got %+v", dec)
	}
	if err := bridge.DispatchPayload(ctx, "unknown.topic", []byte{1, 2}); err == nil {
		t.Fatal("expected error for unrecognized topic")
	}
}
