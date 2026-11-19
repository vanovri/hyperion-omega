package control

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/engine"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

// ProxyTargetSpec binds a named ToxicProxy listen address (e.g. :19001) to an upstream service.
type ProxyTargetSpec struct {
	Name         string `json:"name"`
	ListenAddr   string `json:"listen_addr"`
	UpstreamAddr string `json:"upstream_addr"`
}

func DefaultCanonicalProxySpecs() []ProxyTargetSpec {
	return []ProxyTargetSpec{
		{Name: "ledger-grpc", ListenAddr: "127.0.0.1:19001", UpstreamAddr: "127.0.0.1:50151"},
		{Name: "gateway-http", ListenAddr: "127.0.0.1:19002", UpstreamAddr: "127.0.0.1:8080"},
		{Name: "stream-aggregator", ListenAddr: "127.0.0.1:19003", UpstreamAddr: "127.0.0.1:50152"},
		{Name: "sentinel-ml", ListenAddr: "127.0.0.1:19004", UpstreamAddr: "127.0.0.1:50153"},
		{Name: "chronos-replay", ListenAddr: "127.0.0.1:19005", UpstreamAddr: "127.0.0.1:50154"},
	}
}

// ControlServer exposes the Channel 07 REST, SSE (/events/chaos), and Prometheus (/metrics)
// control plane on port 8086, plus manages optional L4 Toxic TCP Proxies (19001-19005).
type ControlServer struct {
	sim     *engine.Simulator
	proxies map[string]*chaos.ToxicProxy

	timerMu     sync.Mutex
	revertTimer *time.Timer
}

func NewControlServer(sim *engine.Simulator, proxies map[string]*chaos.ToxicProxy) *ControlServer {
	if proxies == nil {
		proxies = make(map[string]*chaos.ToxicProxy)
	}
	return &ControlServer{
		sim:     sim,
		proxies: proxies,
	}
}

func (cs *ControlServer) Close() {
	cs.timerMu.Lock()
	if cs.revertTimer != nil {
		cs.revertTimer.Stop()
		cs.revertTimer = nil
	}
	cs.timerMu.Unlock()

	for _, p := range cs.proxies {
		_ = p.Stop()
	}
}

func (cs *ControlServer) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", cs.handleHealthz)
	mux.HandleFunc("/metrics", cs.handlePrometheusMetrics)
	mux.HandleFunc("/events/chaos", cs.handleSSEStream)
	mux.HandleFunc("/api/v1/chaos/stream", cs.handleSSEStream)
	mux.HandleFunc("/api/v1/chaos/status", cs.handleStatus)
	mux.HandleFunc("/api/v1/chaos/campaigns", cs.handleCampaigns)
	mux.HandleFunc("/api/v1/chaos/regime", cs.handleRegime)
	mux.HandleFunc("/api/v1/chaos/syndicate", cs.handleSyndicate)
	mux.HandleFunc("/api/v1/chaos/faults", cs.handleFaults)
	mux.HandleFunc("/api/v1/chaos/proxies", cs.handleProxies)
	mux.HandleFunc("/api/v1/chaos/proxies/", cs.handleProxyByName)
	mux.HandleFunc("/api/v1/chaos/audit", cs.handleAuditReport)
	mux.HandleFunc("/api/v1/chaos/audit/verify", cs.handleAuditVerifyNow)

	return withCORS(withInternalMutationAuth(mux))
}

func withInternalMutationAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodPost, http.MethodPut, http.MethodPatch, http.MethodDelete:
			expected := strings.TrimSpace(os.Getenv("HYPERION_INTERNAL_AUTH_TOKEN"))
			envMode := strings.ToLower(strings.TrimSpace(os.Getenv("HYPERION_ENV")))
			if expected == "" && envMode == "production" {
				writeError(w, http.StatusUnauthorized, "UNAUTHORIZED_INTERNAL_ENDPOINT", "HYPERION_INTERNAL_AUTH_TOKEN must be configured in production")
				return
			}
			if expected != "" {
				provided := strings.TrimSpace(r.Header.Get("X-Hyperion-Internal-Token"))
				if subtle.ConstantTimeCompare([]byte(provided), []byte(expected)) != 1 {
					writeError(w, http.StatusUnauthorized, "UNAUTHORIZED_INTERNAL_ENDPOINT", "missing or invalid X-Hyperion-Internal-Token")
					return
				}
			}
		}
		next.ServeHTTP(w, r)
	})
}

func withCORS(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		origin := strings.TrimSpace(r.Header.Get("Origin"))
		allowOrigin := "*"
		if rawAllowed := strings.TrimSpace(os.Getenv("HYPERION_CORS_ORIGINS")); rawAllowed != "" {
			allowOrigin = ""
			for _, candidate := range strings.Split(rawAllowed, ",") {
				c := strings.TrimSpace(candidate)
				if c == "*" || (origin != "" && c == origin) {
					allowOrigin = origin
					if c == "*" {
						allowOrigin = "*"
					}
					break
				}
			}
		} else if strings.EqualFold(strings.TrimSpace(os.Getenv("HYPERION_ENV")), "production") {
			if origin == "http://localhost:3000" || origin == "http://127.0.0.1:3000" {
				allowOrigin = origin
			} else {
				allowOrigin = ""
			}
		}
		if allowOrigin != "" {
			w.Header().Set("Access-Control-Allow-Origin", allowOrigin)
		}
		w.Header().Set("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE, OPTIONS")
		w.Header().Set("Access-Control-Allow-Headers", "Content-Type, Authorization, Idempotency-Key, X-Hyperion-Internal-Token")
		if r.Method == http.MethodOptions {
			w.WriteHeader(http.StatusNoContent)
			return
		}
		next.ServeHTTP(w, r)
	})
}

func (cs *ControlServer) handleHealthz(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET required")
		return
	}
	tel := cs.sim.Telemetry()
	status := "ok"
	if !tel.InvariantPassed {
		status = "degraded"
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"status":     status,
		"service":    "chaos-sim",
		"version":    "1.0.0",
		"engine_seq": strconv.FormatUint(tel.OrdersSubmitted, 10),
		"ts_ns":      strconv.FormatInt(time.Now().UnixNano(), 10),
	})
}

func (cs *ControlServer) handleStatus(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET required")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"telemetry": cs.sim.Telemetry(),
		"campaign":  cs.sim.CurrentCampaign(),
		"proxies":   cs.collectProxyStats(),
	})
}

type campaignPayload struct {
	Regime               string   `json:"regime"`
	Enabled              *bool    `json:"enabled"`
	Seed                 string   `json:"seed,omitempty"`
	TargetSymbol         string   `json:"target_symbol,omitempty"`
	DurationMs           int      `json:"duration_ms,omitempty"`
	BurstMultiplier      *float64 `json:"burst_multiplier,omitempty"`
	ReorderProb          *float64 `json:"reorder_prob,omitempty"`
	DuplicateProb        *float64 `json:"duplicate_prob,omitempty"`
	ClockSkewProb        *float64 `json:"clock_skew_prob,omitempty"`
	CorruptSignatureProb *float64 `json:"corrupt_signature_prob,omitempty"`
	SlowLorisConns       *int     `json:"slow_loris_conns,omitempty"`
	SlowLorisStallMs     *int     `json:"slow_loris_stall_ms,omitempty"`
}

// handleCampaigns implements gateway-openapi.yaml POST /api/v1/chaos/campaigns (plus GET).
func (cs *ControlServer) handleCampaigns(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodGet {
		tel := cs.sim.Telemetry()
		writeJSON(w, http.StatusOK, map[string]any{
			"campaign_id": tel.CampaignID,
			"active":      tel.Regime != model.RegimeCalm,
			"regime":      tel.Regime,
			"profile":     cs.sim.CurrentCampaign(),
			"telemetry":   tel,
		})
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET or POST required")
		return
	}

	var req campaignPayload
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64*1024)).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "INVALID_JSON", err.Error())
		return
	}

	normRegime, ok := model.NormalizeRegime(strings.ToUpper(strings.TrimSpace(req.Regime)))
	if !ok {
		writeError(w, http.StatusBadRequest, "INVALID_REGIME", "supported regimes: CALM, HIGH_VOLATILITY, FLASH_CRASH, SPOOFING_ATTACK, WASH_TRADING_RING, BURST_TSUNAMI_20X, NETWORK_PARTITION")
		return
	}

	enabled := true
	if req.Enabled != nil {
		enabled = *req.Enabled
	}

	prof := chaos.PresetForRegime(normRegime)
	if req.BurstMultiplier != nil && *req.BurstMultiplier > 0 {
		prof.BurstMultiplier = *req.BurstMultiplier
	}
	if req.ReorderProb != nil {
		prof.ReorderProb = *req.ReorderProb
	}
	if req.DuplicateProb != nil {
		prof.DuplicateProb = *req.DuplicateProb
	}
	if req.ClockSkewProb != nil {
		prof.ClockSkewProb = *req.ClockSkewProb
	}
	if req.CorruptSignatureProb != nil {
		prof.CorruptSignatureProb = *req.CorruptSignatureProb
	}
	if req.SlowLorisConns != nil {
		prof.SlowLorisConns = *req.SlowLorisConns
	}
	if req.SlowLorisStallMs != nil {
		prof.SlowLorisStallMs = *req.SlowLorisStallMs
	}

	// Propagate NETWORK_PARTITION to managed ToxicProxies automatically
	for _, px := range cs.proxies {
		c := px.Config()
		if enabled && normRegime == model.RegimeNetworkPartition {
			c.Enabled = true
			c.Partitioned = true
		} else if !enabled || normRegime == model.RegimeCalm {
			c.Partitioned = false
		}
		px.UpdateConfig(c)
	}

	campID := cs.sim.ApplyCampaign(prof, enabled)

	cs.timerMu.Lock()
	if cs.revertTimer != nil {
		cs.revertTimer.Stop()
		cs.revertTimer = nil
	}
	if enabled && req.DurationMs >= 100 && normRegime != model.RegimeCalm {
		dur := time.Duration(req.DurationMs) * time.Millisecond
		cs.revertTimer = time.AfterFunc(dur, func() {
			cs.sim.ApplyCampaign(chaos.DefaultCalmProfile(), false)
			for _, px := range cs.proxies {
				c := px.Config()
				c.Partitioned = false
				px.UpdateConfig(c)
			}
		})
	}
	cs.timerMu.Unlock()

	effective := cs.sim.CurrentCampaign()
	writeJSON(w, http.StatusOK, map[string]any{
		"campaign_id": campID,
		"active":      enabled && effective.Regime != model.RegimeCalm,
		"regime":      string(effective.Regime),
		"profile":     effective,
	})
}

func (cs *ControlServer) handleRegime(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodGet {
		cur := cs.sim.CurrentCampaign()
		writeJSON(w, http.StatusOK, map[string]any{
			"regime":  cur.Regime,
			"profile": cur,
		})
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET or POST required")
		return
	}
	var body struct {
		Regime string `json:"regime"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 16*1024)).Decode(&body); err != nil {
		writeError(w, http.StatusBadRequest, "INVALID_JSON", err.Error())
		return
	}
	norm, ok := model.NormalizeRegime(strings.ToUpper(strings.TrimSpace(body.Regime)))
	if !ok {
		writeError(w, http.StatusBadRequest, "INVALID_REGIME", "unknown market regime")
		return
	}
	prof := chaos.PresetForRegime(norm)
	campID := cs.sim.ApplyCampaign(prof, true)
	writeJSON(w, http.StatusOK, map[string]any{
		"campaign_id": campID,
		"active":      norm != model.RegimeCalm,
		"regime":      string(norm),
		"profile":     prof,
	})
}

func (cs *ControlServer) handleSyndicate(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "POST required")
		return
	}
	var req engine.SyndicateAttackRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 32*1024)).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "INVALID_JSON", err.Error())
		return
	}
	switch strings.ToUpper(string(req.AttackType)) {
	case "WASH_TRADING_RING", "WASH_RING", "CIRCULAR_WASH":
		req.AttackType = engine.AttackWashRing
	case "LAYERING_SPOOFER", "SPOOFING", "LAYERING":
		req.AttackType = engine.AttackLayeringSpoofer
	case "FLASH_CRASH_SWEEP", "FLASH_SWEEP", "LIQUIDITY_SWEEP":
		req.AttackType = engine.AttackFlashSweep
	default:
		writeError(w, http.StatusBadRequest, "INVALID_ATTACK_TYPE", "supported: WASH_TRADING_RING, LAYERING_SPOOFER, FLASH_CRASH_SWEEP")
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
	defer cancel()
	res := cs.sim.TriggerSyndicateAttack(ctx, req)
	writeJSON(w, http.StatusOK, res)
}

func (cs *ControlServer) handleFaults(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, map[string]any{
			"profile": cs.sim.CurrentCampaign(),
			"stats":   cs.sim.Telemetry().Faults,
		})
		return
	}
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET or POST required")
		return
	}
	prof := cs.sim.CurrentCampaign()
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 32*1024)).Decode(&prof); err != nil {
		writeError(w, http.StatusBadRequest, "INVALID_JSON", err.Error())
		return
	}
	cid := cs.sim.ApplyCampaign(prof, true)
	writeJSON(w, http.StatusOK, map[string]any{
		"campaign_id": cid,
		"profile":     cs.sim.CurrentCampaign(),
	})
}

func (cs *ControlServer) handleProxies(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET required")
		return
	}
	writeJSON(w, http.StatusOK, cs.collectProxyStats())
}

func (cs *ControlServer) handleProxyByName(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/api/v1/chaos/proxies/")
	px, ok := cs.proxies[name]
	if !ok {
		writeError(w, http.StatusNotFound, "PROXY_NOT_FOUND", fmt.Sprintf("unknown toxic proxy: %s", name))
		return
	}
	if r.Method == http.MethodGet {
		writeJSON(w, http.StatusOK, map[string]any{
			"name":   name,
			"config": px.Config(),
			"stats":  px.Stats(),
		})
		return
	}
	if r.Method != http.MethodPost && r.Method != http.MethodPut {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET or POST required")
		return
	}
	cfg := px.Config()
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 16*1024)).Decode(&cfg); err != nil {
		writeError(w, http.StatusBadRequest, "INVALID_JSON", err.Error())
		return
	}
	px.UpdateConfig(cfg)
	writeJSON(w, http.StatusOK, map[string]any{
		"name":   name,
		"config": px.Config(),
		"stats":  px.Stats(),
	})
}

func (cs *ControlServer) handleAuditReport(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "GET required")
		return
	}
	writeJSON(w, http.StatusOK, cs.sim.Verifier().LastReport())
}

func (cs *ControlServer) handleAuditVerifyNow(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "METHOD_NOT_ALLOWED", "POST required")
		return
	}
	rep, err := cs.sim.RunAuditNow()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "AUDIT_ERROR", err.Error())
		return
	}
	writeJSON(w, http.StatusOK, rep)
}

// handleSSEStream streams live 2Hz telemetry frames over Server-Sent Events (/events/chaos).
func (cs *ControlServer) handleSSEStream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		writeError(w, http.StatusInternalServerError, "STREAM_UNSUPPORTED", "ResponseWriter does not support Flush")
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")

	subID, ch := cs.sim.SubscribeSSE()
	defer cs.sim.UnsubscribeSSE(subID)

	// Immediately emit initial snapshot frame so UI/E2E tests receive state without waiting 500ms.
	initBytes, _ := json.Marshal(cs.sim.Telemetry())
	_, _ = fmt.Fprintf(w, "event: telemetry\ndata: %s\n\n", initBytes)
	flusher.Flush()

	for {
		select {
		case <-r.Context().Done():
			return
		case snap, open := <-ch:
			if !open {
				return
			}
			b, err := json.Marshal(snap)
			if err != nil {
				continue
			}
			if _, wErr := fmt.Fprintf(w, "event: telemetry\ndata: %s\n\n", b); wErr != nil {
				return
			}
			flusher.Flush()
		}
	}
}

func (cs *ControlServer) handlePrometheusMetrics(w http.ResponseWriter, r *http.Request) {
	tel := cs.sim.Telemetry()
	invVal := 0
	if tel.InvariantPassed {
		invVal = 1
	}
	w.Header().Set("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
	var sb strings.Builder
	fmt.Fprintf(&sb, "# HELP hyperion_chaos_orders_submitted_total Total simulated orders submitted.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_orders_submitted_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_orders_submitted_total %d\n", tel.OrdersSubmitted)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_orders_accepted_total Total simulated orders accepted.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_orders_accepted_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_orders_accepted_total %d\n", tel.OrdersAccepted)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_orders_rejected_total Total simulated orders rejected.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_orders_rejected_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_orders_rejected_total %d\n", tel.OrdersRejected)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_orders_deduped_total Total idempotent duplicate orders deduplicated.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_orders_deduped_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_orders_deduped_total %d\n", tel.OrdersDeduped)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_trades_matched_total Total matched trade fills.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_trades_matched_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_trades_matched_total %d\n", tel.TradesMatched)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_orders_per_second Current order throughput per second.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_orders_per_second gauge\n")
	fmt.Fprintf(&sb, "hyperion_chaos_orders_per_second %d\n", tel.OrdersPerSec)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_latency_microseconds Order dispatch latency percentiles in microseconds.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_latency_microseconds gauge\n")
	fmt.Fprintf(&sb, "hyperion_chaos_latency_microseconds{quantile=\"0.5\"} %d\n", tel.LatencyP50Us)
	fmt.Fprintf(&sb, "hyperion_chaos_latency_microseconds{quantile=\"0.95\"} %d\n", tel.LatencyP95Us)
	fmt.Fprintf(&sb, "hyperion_chaos_latency_microseconds{quantile=\"0.99\"} %d\n", tel.LatencyP99Us)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_faults_injected_total Total chaos faults injected by type.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_faults_injected_total counter\n")
	fmt.Fprintf(&sb, "hyperion_chaos_faults_injected_total{type=\"reorder\"} %d\n", tel.Faults.ReorderedPackets)
	fmt.Fprintf(&sb, "hyperion_chaos_faults_injected_total{type=\"duplicate\"} %d\n", tel.Faults.DuplicatedRequests)
	fmt.Fprintf(&sb, "hyperion_chaos_faults_injected_total{type=\"clock_skew\"} %d\n", tel.Faults.ClockSkewedRequests)
	fmt.Fprintf(&sb, "hyperion_chaos_faults_injected_total{type=\"corrupt_sig\"} %d\n", tel.Faults.CorruptedSignatures)
	fmt.Fprintf(&sb, "hyperion_chaos_faults_injected_total{type=\"partition_drop\"} %d\n", tel.Faults.PartitionDrops)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_invariant_passed 1 if all double-entry ledger invariants hold, 0 otherwise.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_invariant_passed gauge\n")
	fmt.Fprintf(&sb, "hyperion_chaos_invariant_passed %d\n", invVal)

	fmt.Fprintf(&sb, "# HELP hyperion_chaos_invariant_violations_total Number of detected invariant violations.\n")
	fmt.Fprintf(&sb, "# TYPE hyperion_chaos_invariant_violations_total gauge\n")
	fmt.Fprintf(&sb, "hyperion_chaos_invariant_violations_total %d\n", tel.ViolationCount)

	for name, px := range cs.proxies {
		st := px.Stats()
		fmt.Fprintf(&sb, "hyperion_chaos_toxic_proxy_active_conns{proxy=%q} %d\n", name, st.ActiveConns)
		fmt.Fprintf(&sb, "hyperion_chaos_toxic_proxy_bytes_total{proxy=%q} %d\n", name, st.BytesForwarded)
	}
	_, _ = w.Write([]byte(sb.String()))
}

func (cs *ControlServer) collectProxyStats() map[string]any {
	out := make(map[string]any, len(cs.proxies))
	for name, px := range cs.proxies {
		out[name] = map[string]any{
			"config": px.Config(),
			"stats":  px.Stats(),
		}
	}
	return out
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, code int, errCode, msg string) {
	writeJSON(w, code, map[string]any{
		"code":     errCode,
		"message":  msg,
		"trace_id": "chaos-sim-ctrl",
		"ts_ns":    strconv.FormatInt(time.Now().UnixNano(), 10),
	})
}
