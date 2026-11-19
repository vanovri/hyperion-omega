package idempotency_test

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/services/gateway-go/internal/idempotency"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

func TestIdempotencyMiddleware_ConcurrentDuplicateStormAndConflict(t *testing.T) {
	mr := miniredis.RunT(t)
	rc := redis.NewClient(&redis.Options{Addr: mr.Addr()})
	t.Cleanup(func() { _ = rc.Close() })

	metrics := telemetry.NewRegistry()
	storeA := idempotency.NewStore(4096, time.Minute, rc, metrics)
	storeB := idempotency.NewStore(4096, time.Minute, rc, metrics)

	var upstreamExecutions atomic.Int64
	upstream := http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		upstreamExecutions.Add(1)
		time.Sleep(15 * time.Millisecond) // simulate ledger WAL group-commit window
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"order_id":"9001","status":"ACCEPTED"}`))
	})

	handlerA := storeA.Wrap(upstream, true)
	handlerB := storeB.Wrap(upstream, true)

	payload := []byte(`{"client_order_id":"c1","price_fp":"6850000000000","qty_fp":"100000000"}`)
	const storm = 64
	startBarrier := make(chan struct{})
	var wg sync.WaitGroup
	statuses := make([]int, storm)

	for i := 0; i < storm; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			<-startBarrier
			req := httptest.NewRequest(http.MethodPost, "/api/v1/orders", bytes.NewReader(payload))
			req.Header.Set("X-Hyperion-Account-Id", "1001")
			req.Header.Set("Idempotency-Key", "idem-storm-uuid-0001")
			rec := httptest.NewRecorder()
			handlerA.ServeHTTP(rec, req)
			statuses[idx] = rec.Code
		}(i)
	}
	close(startBarrier)
	wg.Wait()

	if got := upstreamExecutions.Load(); got != 1 {
		t.Fatalf("expected EXACTLY 1 upstream order execution across %d duplicate concurrent packets, got %d", storm, got)
	}
	for i, code := range statuses {
		if code != http.StatusOK {
			t.Fatalf("goroutine %d got unexpected HTTP %d", i, code)
		}
	}

	// Cross-node duplicate hit on Node B (via L2 Redis persistence) must ALSO not execute upstream!
	reqB := httptest.NewRequest(http.MethodPost, "/api/v1/orders", bytes.NewReader(payload))
	reqB.Header.Set("X-Hyperion-Account-Id", "1001")
	reqB.Header.Set("Idempotency-Key", "idem-storm-uuid-0001")
	recB := httptest.NewRecorder()
	handlerB.ServeHTTP(recB, reqB)
	if recB.Code != http.StatusOK || recB.Header().Get("X-Idempotency-Replayed") != "true" {
		t.Fatalf("expected L2 Redis replay on Node B, got status=%d hdr=%q", recB.Code, recB.Header().Get("X-Idempotency-Replayed"))
	}
	if got := upstreamExecutions.Load(); got != 1 {
		t.Fatalf("Node B executed upstream unexpectedly: count=%d", got)
	}

	// Reusing the same Idempotency-Key with a DIFFERENT payload must return 409 Conflict!
	mutatedPayload := []byte(`{"client_order_id":"c1","price_fp":"9999900000000","qty_fp":"100000000"}`)
	reqConflict := httptest.NewRequest(http.MethodPost, "/api/v1/orders", bytes.NewReader(mutatedPayload))
	reqConflict.Header.Set("X-Hyperion-Account-Id", "1001")
	reqConflict.Header.Set("Idempotency-Key", "idem-storm-uuid-0001")
	recConflict := httptest.NewRecorder()
	handlerA.ServeHTTP(recConflict, reqConflict)
	if recConflict.Code != http.StatusConflict {
		t.Fatalf("expected 409 Conflict on payload mismatch, got %d", recConflict.Code)
	}
}
