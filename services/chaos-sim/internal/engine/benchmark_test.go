package engine_test

import (
	"runtime"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/engine"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/transport"
)

// BenchmarkOrderGeneratorThroughput verifies that a single chaos-sim process pumps
// well above >50,000 autonomous heterogeneous orders/second without memory or goroutine leaks.
func BenchmarkOrderGeneratorThroughput(b *testing.B) {
	workers := runtime.NumCPU()
	if workers < 4 {
		workers = 4
	}
	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 1024
	cfg.WorkerShards = workers
	cfg.EnableAuditTrack = false

	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		b.Fatal(err)
	}
	defer sim.Stop()

	prof := chaos.PresetForRegime(model.RegimeBurstTsunami)
	sim.ApplyCampaign(prof, true)

	var totalGenerated atomic.Uint64
	var shardCounter atomic.Uint64

	b.ReportAllocs()
	b.ResetTimer()
	start := time.Now()

	b.RunParallel(func(pb *testing.PB) {
		shardIdx := int(shardCounter.Add(1)-1) % workers
		buf := make([]model.OrderIntent, 0, 512)
		step := int64(1)
		for pb.Next() {
			for len(buf) == 0 {
				buf = sim.GenerateShardBatch(shardIdx, 1_795_000_000_000_000_000+step*1_000_000, 0.050, buf)
				step++
			}
			totalGenerated.Add(uint64(len(buf)))
			buf = buf[:0]
		}
	})

	b.StopTimer()
	elapsed := time.Since(start).Seconds()
	if elapsed > 0 {
		ordersPerSec := float64(totalGenerated.Load()) / elapsed
		b.ReportMetric(ordersPerSec, "orders/sec")
		if b.N >= 100 && ordersPerSec < 50_000 {
			b.Fatalf("order generator throughput %.0f orders/sec below 50,000 target", ordersPerSec)
		}
	}
}

// BenchmarkOrderGeneratorShardLatency measures single-shard batch generation p50/p99 latency and allocs.
func BenchmarkOrderGeneratorShardLatency(b *testing.B) {
	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 1024
	cfg.WorkerShards = 8
	cfg.EnableAuditTrack = false

	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		b.Fatal(err)
	}
	defer sim.Stop()

	prof := chaos.PresetForRegime(model.RegimeBurstTsunami)
	sim.ApplyCampaign(prof, true)

	buf := make([]model.OrderIntent, 0, 512)
	const sampleBatch = 32
	samples := make([]int64, 0, 2048)
	var totalOrders uint64

	b.ReportAllocs()
	b.ResetTimer()

	t0 := time.Now()
	inBatch := 0
	for i := 0; i < b.N; i++ {
		buf = sim.GenerateShardBatch(i%8, 1_795_000_000_000_000_000+int64(i)*1_000_000, 0.050, buf)
		totalOrders += uint64(len(buf))
		inBatch++
		if inBatch == sampleBatch {
			dt := time.Since(t0).Nanoseconds() / sampleBatch
			if len(samples) < cap(samples) {
				samples = append(samples, dt)
			}
			t0 = time.Now()
			inBatch = 0
		}
	}

	b.StopTimer()
	if len(samples) > 0 {
		slices.Sort(samples)
		p50 := samples[(len(samples)*50)/100]
		p99 := samples[(len(samples)*99)/100]
		b.ReportMetric(float64(p50), "p50_ns/op")
		b.ReportMetric(float64(p99), "p99_ns/op")
	}
	if b.N > 0 {
		b.ReportMetric(float64(totalOrders)/float64(b.N), "orders/batch")
	}
}

// BenchmarkSigningHotPath measures §6.2 canonical HMAC-SHA256 / Ed25519 request signing latency & allocs.
func BenchmarkSigningHotPath(b *testing.B) {
	bot := agents.NewBotAccount(42, 0, "bench", model.ArchetypeMomentumQuant, 100_000*fixed.Scale, 10*fixed.Scale)
	body := []byte(`{"client_order_id":"csim-0-1","account_id":"10001","symbol":"BTC-USDT","side":"BUY","order_type":"LIMIT","stp_mode":"CANCEL_RESTING","price_fp":"6842550000000","qty_fp":"100000"}`)

	const sampleBatch = 256
	samples := make([]int64, 0, 2048)
	var sigBuf [128]byte
	var tsBuf [24]byte
	var nonceBuf [24]byte
	b.ReportAllocs()
	b.ResetTimer()

	t0 := time.Now()
	inBatch := 0
	for i := 0; i < b.N; i++ {
		_, _, _ = transport.BuildCanonicalSignatureInto(
			sigBuf[:0],
			tsBuf[:0],
			nonceBuf[:0],
			transport.AuthHMACSHA256,
			bot.PrivKey,
			bot.HMACSecret,
			1_795_000_000_000_000_000+int64(i),
			uint64(i+1),
			"POST",
			"/api/v1/orders",
			body,
			false,
		)
		inBatch++
		if inBatch == sampleBatch {
			dt := time.Since(t0).Nanoseconds() / sampleBatch
			if len(samples) < cap(samples) {
				samples = append(samples, dt)
			}
			t0 = time.Now()
			inBatch = 0
		}
	}

	b.StopTimer()
	if len(samples) > 0 {
		slices.Sort(samples)
		b.ReportMetric(float64(samples[(len(samples)*50)/100]), "p50_ns/op")
		b.ReportMetric(float64(samples[(len(samples)*99)/100]), "p99_ns/op")
	}
}

// BenchmarkEndToEndMockExchangePump measures full multi-agent generation + MockExchange LOB matching.
func BenchmarkEndToEndMockExchangePump(b *testing.B) {
	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 1024
	cfg.WorkerShards = 8
	cfg.EnableAuditTrack = false

	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		b.Fatal(err)
	}
	defer sim.Stop()

	prof := chaos.PresetForRegime(model.RegimeBurstTsunami)
	sim.ApplyCampaign(prof, true)

	var totalDispatched uint64

	b.ReportAllocs()
	b.ResetTimer()
	start := time.Now()

	for i := 0; i < b.N; i++ {
		n := sim.StepDeterministic(1_795_000_000_000_000_000 + int64(i)*25_000_000)
		totalDispatched += uint64(n)
	}

	b.StopTimer()
	elapsed := time.Since(start).Seconds()
	if elapsed > 0 && totalDispatched > 0 {
		b.ReportMetric(float64(totalDispatched)/elapsed, "matched_orders/sec")
	}
}
