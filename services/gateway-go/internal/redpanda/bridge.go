package redpanda

import (
	"context"
	"fmt"
	"strconv"
	"sync"
	"time"

	"github.com/twmb/franz-go/pkg/kgo"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
	"google.golang.org/protobuf/proto"
)

// Canonical Redpanda topic names from .agents/rules/00-hyperion-contract.md §4.2.
const (
	TopicOrderCommands   = "hyperion.orders.commands.v1"
	TopicMarketL3Deltas  = "hyperion.market.l3-deltas.v1"
	TopicMarketL2Snaps   = "hyperion.market.l2-snapshots.v1"
	TopicMarketFills     = "hyperion.market.executions.v1"
	TopicAnalyticsCandle = "hyperion.analytics.candles.v1"
	TopicAnalyticsMicro  = "hyperion.analytics.microstructure.v1"
	TopicFraudAlerts     = "hyperion.fraud.alerts.v1"
	TopicRiskMitigations = "hyperion.risk.mitigations.v1"
)

var ConsumerTopics = []string{
	TopicMarketL3Deltas,
	TopicMarketL2Snaps,
	TopicMarketFills,
	TopicAnalyticsCandle,
	TopicAnalyticsMicro,
	TopicFraudAlerts,
	TopicRiskMitigations,
}

// Bridge consumes Protobuf events from Redpanda topics, routes market/execution/fraud
// streams into the zero-copy WebSocket Hub, applies Aegis RiskMitigationCommands to
// the HybridLimiter, and publishes async order command audit envelopes.
type Bridge struct {
	hub     *ws.Hub
	limiter *ratelimit.HybridLimiter
	client  *kgo.Client

	stopOnce sync.Once
	cancel   context.CancelFunc
	wg       sync.WaitGroup
}

// NewBridge creates a Redpanda <-> WebSocket/RateLimiter bridge.
// If brokers is empty or enableKafka is false, Bridge operates in local dispatch-only mode.
func NewBridge(brokers []string, groupID string, enableKafka bool, hub *ws.Hub, limiter *ratelimit.HybridLimiter) (*Bridge, error) {
	ctx, cancel := context.WithCancel(context.Background())
	b := &Bridge{
		hub:     hub,
		limiter: limiter,
		cancel:  cancel,
	}

	if enableKafka && len(brokers) > 0 {
		if groupID == "" {
			groupID = "hyperion-gateway-go-v1"
		}
		cl, err := kgo.NewClient(
			kgo.SeedBrokers(brokers...),
			kgo.ConsumerGroup(groupID),
			kgo.ConsumeTopics(ConsumerTopics...),
			kgo.RecordPartitioner(kgo.StickyKeyPartitioner(nil)), // Kafka murmur2_random compatible
			kgo.ProducerBatchCompression(kgo.Lz4Compression()),
			kgo.FetchMaxWait(25*time.Millisecond),
		)
		if err != nil {
			cancel()
			return nil, fmt.Errorf("kgo.NewClient: %w", err)
		}
		b.client = cl
		b.wg.Add(1)
		go b.consumeLoop(ctx)
	}
	return b, nil
}

func (b *Bridge) consumeLoop(ctx context.Context) {
	defer b.wg.Done()
	for {
		fetches := b.client.PollFetches(ctx)
		if ctx.Err() != nil || fetches.IsClientClosed() {
			return
		}
		fetches.EachRecord(func(rec *kgo.Record) {
			_ = b.DispatchPayload(ctx, rec.Topic, rec.Value)
		})
	}
}

// DispatchPayload decodes a canonical Protobuf payload from topic and routes it to Hub / HybridLimiter.
func (b *Bridge) DispatchPayload(ctx context.Context, topic string, payload []byte) error {
	switch topic {
	case TopicMarketL3Deltas:
		var msg hyperionv1.L3BookDelta
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastL3Delta(&msg)
		}

	case TopicMarketL2Snaps:
		var msg hyperionv1.L2BookSnapshot
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastL2Snapshot(&msg)
		}

	case TopicMarketFills:
		var msg hyperionv1.ExecutionFill
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastExecutionFill(&msg)
		}

	case TopicAnalyticsCandle:
		var msg hyperionv1.OhlcvCandle
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastCandleMicro(&msg, nil)
		}

	case TopicAnalyticsMicro:
		var msg hyperionv1.MicrostructureMetrics
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastCandleMicro(nil, &msg)
		}

	case TopicFraudAlerts:
		var msg hyperionv1.AnomalyAlert
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.hub != nil {
			b.hub.BroadcastFraudAlert(&msg)
		}

	case TopicRiskMitigations:
		var msg hyperionv1.RiskMitigationCommand
		if err := proto.Unmarshal(payload, &msg); err != nil {
			return err
		}
		if b.limiter != nil {
			_, err := b.limiter.ApplyRiskMitigation(ctx, &msg)
			return err
		}
	default:
		return fmt.Errorf("unrecognized topic %q", topic)
	}
	return nil
}

// PublishOrderCommand asynchronously publishes an authenticated order command to
// `hyperion.orders.commands.v1` partitioned deterministically by `<SYMBOL>` with W3C headers.
func (b *Bridge) PublishOrderCommand(symbol, traceparent, schemaType string, engineSeq uint64, msg proto.Message) {
	if b == nil || b.client == nil || msg == nil {
		return
	}
	raw, err := proto.Marshal(msg)
	if err != nil {
		return
	}
	rec := &kgo.Record{
		Topic: TopicOrderCommands,
		Key:   []byte(symbol),
		Value: raw,
		Headers: []kgo.RecordHeader{
			{Key: "traceparent", Value: []byte(traceparent)},
			{Key: "x-engine-seq", Value: []byte(strconv.FormatUint(engineSeq, 10))},
			{Key: "x-schema-type", Value: []byte(schemaType)},
			{Key: "x-source-service", Value: []byte("gateway-go")},
		},
	}
	b.client.Produce(context.Background(), rec, nil)
}

// Close flushes pending async Kafka writes and stops the consumer goroutine cleanly.
func (b *Bridge) Close() {
	if b == nil {
		return
	}
	b.stopOnce.Do(func() {
		if b.cancel != nil {
			b.cancel()
		}
		if b.client != nil {
			ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
			_ = b.client.Flush(ctx)
			cancel()
			b.client.Close()
		}
	})
	b.wg.Wait()
}
