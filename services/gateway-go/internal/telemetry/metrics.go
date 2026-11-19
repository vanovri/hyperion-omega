package telemetry

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"net/http"
	"strings"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
)

// Registry stores lock-free atomic counters and gauges exposed on /metrics in
// standard Prometheus text exposition format (0.0.4) with zero mutex contention.
type Registry struct {
	HTTPRequestsTotal          atomic.Uint64
	HTTPAuthFailuresTotal      atomic.Uint64
	ReplayRejectedTotal        atomic.Uint64
	RateLimitedTotal           atomic.Uint64
	SpamPenaltiesTotal         atomic.Uint64
	RiskMitigationsApplied     atomic.Uint64
	IdempotencyHitsTotal       atomic.Uint64
	IdempotencyConflictsTotal  atomic.Uint64
	GRPCRequestsTotal          atomic.Uint64
	GRPCRetriesTotal           atomic.Uint64
	GRPCFailuresTotal          atomic.Uint64
	CircuitBreakerTripsTotal   atomic.Uint64
	WSConnectedClients         atomic.Int64
	WSMessagesBroadcastTotal   atomic.Uint64
	WSFramesWrittenTotal       atomic.Uint64
	WSSlowConsumerDropsTotal   atomic.Uint64
	WSSlowConsumerEvictsTotal  atomic.Uint64
	WSSnapshotResyncsTotal     atomic.Uint64
	WSProtocolErrorsTotal      atomic.Uint64
	LatestEngineSeq            atomic.Uint64
	traceSeq                   atomic.Uint64
	traceSeedHi                uint64
}

// Global is the default process-wide lock-free telemetry registry.
var Global = NewRegistry()

// NewRegistry creates an initialized lock-free telemetry registry.
func NewRegistry() *Registry {
	var seed [8]byte
	_, _ = rand.Read(seed[:])
	return &Registry{
		traceSeedHi: binary.LittleEndian.Uint64(seed[:]),
	}
}

// ServeHTTP renders Prometheus text format metrics on GET /metrics.
func (r *Registry) ServeHTTP(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
	var b strings.Builder
	b.Grow(2048)

	writeCounter(&b, "hyperion_gateway_http_requests_total", "Total HTTP requests processed by gateway-go.", r.HTTPRequestsTotal.Load())
	writeCounter(&b, "hyperion_gateway_auth_failures_total", "Total authentication failures.", r.HTTPAuthFailuresTotal.Load())
	writeCounter(&b, "hyperion_gateway_replay_rejected_total", "Total replay/timestamp rejections.", r.ReplayRejectedTotal.Load())
	writeCounter(&b, "hyperion_gateway_rate_limited_total", "Total requests throttled by hybrid L1/L2 limiter.", r.RateLimitedTotal.Load())
	writeCounter(&b, "hyperion_gateway_spam_penalties_total", "Total automatic order-spam penalties applied.", r.SpamPenaltiesTotal.Load())
	writeCounter(&b, "hyperion_gateway_risk_mitigations_total", "Total Redpanda RiskMitigationCommands enforced.", r.RiskMitigationsApplied.Load())
	writeCounter(&b, "hyperion_gateway_idempotency_hits_total", "Total duplicate requests short-circuited by idempotency store.", r.IdempotencyHitsTotal.Load())
	writeCounter(&b, "hyperion_gateway_idempotency_conflicts_total", "Total idempotency key payload hash conflicts.", r.IdempotencyConflictsTotal.Load())
	writeCounter(&b, "hyperion_gateway_grpc_requests_total", "Total upstream gRPC calls.", r.GRPCRequestsTotal.Load())
	writeCounter(&b, "hyperion_gateway_grpc_retries_total", "Total idempotent gRPC retries.", r.GRPCRetriesTotal.Load())
	writeCounter(&b, "hyperion_gateway_grpc_failures_total", "Total upstream gRPC errors.", r.GRPCFailuresTotal.Load())
	writeCounter(&b, "hyperion_gateway_circuit_breaker_trips_total", "Total adaptive circuit breaker open transitions.", r.CircuitBreakerTripsTotal.Load())
	writeGauge(&b, "hyperion_gateway_ws_connected_clients", "Currently connected WebSocket subscribers.", r.WSConnectedClients.Load())
	writeCounter(&b, "hyperion_gateway_ws_broadcasts_total", "Total fan-out events published to WebSocket hub.", r.WSMessagesBroadcastTotal.Load())
	writeCounter(&b, "hyperion_gateway_ws_frames_written_total", "Total WebSocket frames written across subscribers.", r.WSFramesWrittenTotal.Load())
	writeCounter(&b, "hyperion_gateway_ws_slow_consumer_drops_total", "Total non-critical market data frames dropped on slow consumer ring buffers.", r.WSSlowConsumerDropsTotal.Load())
	writeCounter(&b, "hyperion_gateway_ws_slow_consumer_evictions_total", "Total stalled WebSocket connections evicted.", r.WSSlowConsumerEvictsTotal.Load())
	writeCounter(&b, "hyperion_gateway_ws_snapshot_resyncs_total", "Total automatic sequence-gap L2 snapshot resyncs triggered.", r.WSSnapshotResyncsTotal.Load())
	writeCounter(&b, "hyperion_gateway_ws_protocol_errors_total", "Total client frames rejected with WebSocket 1002 Protocol Error.", r.WSProtocolErrorsTotal.Load())
	writeGauge(&b, "hyperion_gateway_latest_engine_seq", "Highest engine_seq observed by gateway-go.", int64(r.LatestEngineSeq.Load()))

	_, _ = w.Write([]byte(b.String()))
}

func writeCounter(b *strings.Builder, name, help string, val uint64) {
	fmt.Fprintf(b, "# HELP %s %s\n# TYPE %s counter\n%s %d\n", name, help, name, name, val)
}

func writeGauge(b *strings.Builder, name, help string, val int64) {
	fmt.Fprintf(b, "# HELP %s %s\n# TYPE %s gauge\n%s %d\n", name, help, name, name, val)
}

// ExtractOrMintTrace parses W3C traceparent ("00-<32hex>-<16hex>-<2hex>") from HTTP headers
// or generates a fast lock-free W3C TraceMetadata envelope without crypto/rand syscall contention.
func (r *Registry) ExtractOrMintTrace(h http.Header, sourceService string) (*hyperionv1.TraceMetadata, string) {
	nowNS := uint64(time.Now().UnixNano())
	seq := r.traceSeq.Add(1)

	var spanBuf [8]byte
	binary.BigEndian.PutUint64(spanBuf[:], r.traceSeedHi^seq^nowNS)
	spanHex := hex.EncodeToString(spanBuf[:])

	var traceHex string
	if raw := strings.TrimSpace(h.Get("Traceparent")); raw != "" {
		parts := strings.Split(raw, "-")
		if len(parts) == 4 && len(parts[1]) == 32 && isLowerHex(parts[1]) {
			traceHex = parts[1]
		}
	}
	if traceHex == "" {
		var traceBuf [16]byte
		binary.BigEndian.PutUint64(traceBuf[0:8], r.traceSeedHi+nowNS)
		binary.BigEndian.PutUint64(traceBuf[8:16], seq)
		traceHex = hex.EncodeToString(traceBuf[:])
	}

	corrID := strings.TrimSpace(h.Get("X-Correlation-Id"))
	if corrID == "" {
		corrID = strings.TrimSpace(h.Get("X-Request-Id"))
	}
	if corrID == "" {
		corrID = traceHex
	}

	traceparent := "00-" + traceHex + "-" + spanHex + "-01"
	return &hyperionv1.TraceMetadata{
		TraceId:       traceHex,
		SpanId:        spanHex,
		CorrelationId: corrID,
		SourceService: sourceService,
		IngestTsNs:    nowNS,
	}, traceparent
}

func isLowerHex(s string) bool {
	for i := 0; i < len(s); i++ {
		c := s[i]
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')) {
			return false
		}
	}
	return true
}
