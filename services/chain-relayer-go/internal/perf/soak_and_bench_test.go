package perf_test

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"runtime"
	"runtime/pprof"
	"sort"
	"testing"
	"time"

	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/feebump"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/nonce"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/outbox"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/perf"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/reorg"
)

// TestSoak500kCycles_ZeroSteadyStateMemoryLeak executes 500,000 consecutive cycles of
// sliding-window ingestion, EIP-1559 fee calculation, nonce allocation/finalization, and
// bounded outbox staging/promotion, proving 0 steady-state heap leak & valid pprof snapshot.
func TestSoak500kCycles_ZeroSteadyStateMemoryLeak(t *testing.T) {
	const totalCycles = 500_000
	const warmupCycles = 50_000

	win, err := reorg.NewSlidingWindow(256, 12, 12)
	if err != nil {
		t.Fatal(err)
	}
	nm := nonce.NewManager(0)
	store := outbox.NewStore(256)
	policy := feebump.DefaultPolicy()

	goroutinesBefore := runtime.NumGoroutine()

	var prevHash [32]byte
	var warmupHeapAlloc uint64
	var warmupHeapObjects uint64

	for i := uint64(0); i < totalCycles; i++ {
		var h [32]byte
		binary.LittleEndian.PutUint64(h[0:8], i+1)
		binary.LittleEndian.PutUint64(h[8:16], 0x4859504552494F4E)
		copy(h[16:32], prevHash[0:16])

		hdr := reorg.BlockHeader{
			Number:     i,
			Hash:       h,
			ParentHash: prevHash,
			Timestamp:  1_795_000_000 + i*2,
			BaseFeeWei: 1_000_000_000 + (i%64)*10_000_000,
		}
		if !win.FastIngestContiguous(hdr) {
			t.Fatalf("contiguous ingest failed at %d", i)
		}
		prevHash = h

		q0, err := feebump.InitialQuote(policy, hdr.BaseFeeWei, 150_000_000)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := feebump.BumpQuote(policy, q0, hdr.BaseFeeWei, 150_000_000); err != nil {
			t.Fatal(err)
		}

		// Exercise bounded nonce + outbox ring every 16th block (31,250 full transactions across 500k blocks)
		if (i & 15) == 0 {
			txID := fmt.Sprintf("soak-tx-%d", i)
			leased, _, err := nm.AcquireOrReuse(txID)
			if err != nil {
				t.Fatal(err)
			}
			_ = leased
			if err := nm.MarkFinalized(txID); err != nil {
				t.Fatal(err)
			}

			depID := fmt.Sprintf("soak-dep-%d", i)
			store.StageDeposit(outbox.StagedDeposit{
				DepositID:   depID,
				ChainID:     31337,
				AccountID:   1001 + (i & 7),
				BlockNumber: i,
				BlockHash:   h,
				AmountFpLo:  100_000_000,
			})
			if i >= 16 {
				store.PromoteFinalizedDeposits(
					i-12,
					i,
					hdr.Timestamp*1_000_000_000,
					"hyperion.omega.settlement.deposits.v1",
					win.IsCanonicalAt,
					func(d outbox.StagedDeposit) any { return d.DepositID },
				)
				_ = store.DrainUnpublishedEvents(64)
			}
		}

		if i == warmupCycles {
			runtime.GC()
			runtime.GC()
			var ms runtime.MemStats
			runtime.ReadMemStats(&ms)
			warmupHeapAlloc = ms.HeapAlloc
			warmupHeapObjects = ms.HeapObjects
		}
	}

	runtime.GC()
	runtime.GC()
	var finalMs runtime.MemStats
	runtime.ReadMemStats(&finalMs)

	var heapProfile bytes.Buffer
	if err := pprof.WriteHeapProfile(&heapProfile); err != nil {
		t.Fatalf("pprof.WriteHeapProfile failed: %v", err)
	}
	if heapProfile.Len() == 0 {
		t.Fatalf("empty pprof heap profile")
	}

	goroutinesAfter := runtime.NumGoroutine()
	if goroutinesAfter > goroutinesBefore+1 {
		t.Fatalf("goroutine leak detected: before=%d after=%d", goroutinesBefore, goroutinesAfter)
	}

	// Steady-state heap after 450,000 additional cycles must stay strictly bounded within < 256 KiB variance
	var deltaBytes int64 = int64(finalMs.HeapAlloc) - int64(warmupHeapAlloc)
	t.Logf(
		"[SOAK 500,000 CYCLES] WarmupHeap=%d B (%d objs) -> FinalHeap=%d B (%d objs) | NetDelta=%d B | pprofBytes=%d | Goroutines=%d->%d",
		warmupHeapAlloc,
		warmupHeapObjects,
		finalMs.HeapAlloc,
		finalMs.HeapObjects,
		deltaBytes,
		heapProfile.Len(),
		goroutinesBefore,
		goroutinesAfter,
	)
	if deltaBytes > 256*1024 {
		t.Fatalf("unbounded heap growth detected across 500k soak cycles: +%d bytes", deltaBytes)
	}
}

var (
	benchSinkMax uint64
	benchSinkTip uint64
)

func BenchmarkMicroOptPass1_Baseline(b *testing.B) {
	p1 := perf.NewPass1BaselineTracker()
	h := sha256.Sum256([]byte("hdr"))
	p := sha256.Sum256([]byte("parent"))
	var m, tip uint64
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		m, tip = p1.StepAndQuote(uint64(i), h, p, 2_200_000_000+uint64(i&63), 150_000_000+uint64(i&31), 1_000_000_000+uint64(i&15))
	}
	benchSinkMax, benchSinkTip = m, tip
}

func BenchmarkMicroOptPass2_ZeroCopyRing(b *testing.B) {
	p2 := perf.NewPass2ZeroCopyRing()
	h := sha256.Sum256([]byte("hdr"))
	p := sha256.Sum256([]byte("parent"))
	var m, tip uint64
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		m, tip = p2.StepAndQuote(uint64(i), h, p, 2_200_000_000+uint64(i&63), 150_000_000+uint64(i&31), 1_000_000_000+uint64(i&15))
	}
	benchSinkMax, benchSinkTip = m, tip
}

func BenchmarkMicroOptPass3_CacheLineAligned(b *testing.B) {
	p3 := perf.NewPass3CacheLineAligned()
	h := sha256.Sum256([]byte("hdr"))
	p := sha256.Sum256([]byte("parent"))
	var m, tip uint64
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		m, tip = p3.StepAndQuote(uint64(i), h, p, 2_200_000_000+uint64(i&63), 150_000_000+uint64(i&31), 1_000_000_000+uint64(i&15))
	}
	benchSinkMax, benchSinkTip = m, tip
}

func TestMicroOpt3PassEquivalenceAndPercentileDistribution(t *testing.T) {
	p1 := perf.NewPass1BaselineTracker()
	p2 := perf.NewPass2ZeroCopyRing()
	p3 := perf.NewPass3CacheLineAligned()
	h := sha256.Sum256([]byte("hdr"))
	p := sha256.Sum256([]byte("parent"))

	for i := uint64(0); i < 1000; i++ {
		max1, tip1 := p1.StepAndQuote(i, h, p, 2_200_000_000+i, 150_000_000+i, 1_000_000_000+i)
		max2, tip2 := p2.StepAndQuote(i, h, p, 2_200_000_000+i, 150_000_000+i, 1_000_000_000+i)
		max3, tip3 := p3.StepAndQuote(i, h, p, 2_200_000_000+i, 150_000_000+i, 1_000_000_000+i)
		if max1 != max2 || max2 != max3 || tip1 != tip2 || tip2 != tip3 {
			t.Fatalf("3-pass math mismatch at %d: (%d,%d) vs (%d,%d) vs (%d,%d)", i, max1, tip1, max2, tip2, max3, tip3)
		}
	}

	const batches = 100
	measurePercentiles := func(name string, opsPerBatch uint64, fn func(base uint64)) (p50 float64, p99 float64) {
		samples := make([]float64, batches)
		for b := 0; b < batches; b++ {
			start := time.Now()
			fn(uint64(b) * opsPerBatch)
			samples[b] = float64(time.Since(start).Nanoseconds()) / float64(opsPerBatch)
		}
		sort.Float64s(samples)
		p50 = samples[batches/2]
		p99 = samples[(batches*99)/100]
		t.Logf("[MICRO-OPT %s] p50=%.2f ns/op | p99=%.2f ns/op", name, p50, p99)
		return p50, p99
	}

	measurePercentiles("Pass1_Baseline", 2_000, func(base uint64) {
		var acc uint64
		for j := uint64(0); j < 2_000; j++ {
			m, tip := p1.StepAndQuote(base+j, h, p, 2_200_000_000+(j&63), 150_000_000+(j&31), 1_000_000_000+(j&15))
			acc ^= m ^ tip
		}
		benchSinkMax = acc
	})
	measurePercentiles("Pass2_ZeroCopyRing", 50_000, func(base uint64) {
		var acc uint64
		for j := uint64(0); j < 50_000; j++ {
			m, tip := p2.StepAndQuote(base+j, h, p, 2_200_000_000+(j&63), 150_000_000+(j&31), 1_000_000_000+(j&15))
			acc ^= m ^ tip
		}
		benchSinkMax = acc
	})
	measurePercentiles("Pass3_CacheLineAligned", 200_000, func(base uint64) {
		var acc uint64
		for j := uint64(0); j < 200_000; j++ {
			m, tip := p3.StepAndQuote(base+j, h, p, 2_200_000_000+(j&63), 150_000_000+(j&31), 1_000_000_000+(j&15))
			acc ^= m ^ tip
		}
		benchSinkMax = acc
	})
}
