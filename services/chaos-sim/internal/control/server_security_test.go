package control

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/engine"
)

func TestControlServer_InternalMutationAuthGuard(t *testing.T) {
	t.Setenv("HYPERION_INTERNAL_AUTH_TOKEN", "chaos-internal-secret-token-2026")
	t.Setenv("HYPERION_ENV", "production")

	cfg := engine.DefaultSimulatorConfig()
	cfg.TotalBots = 32
	cfg.WorkerShards = 2
	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		t.Fatalf("NewSimulator failed: %v", err)
	}
	ctrl := NewControlServer(sim, nil)
	defer ctrl.Close()

	h := ctrl.Handler()

	// 1. Read-only /healthz stays accessible for kubelet liveness probes
	reqHealth := httptest.NewRequest(http.MethodGet, "/healthz", nil)
	recHealth := httptest.NewRecorder()
	h.ServeHTTP(recHealth, reqHealth)
	if recHealth.Code != http.StatusOK {
		t.Fatalf("expected /healthz 200, got %d", recHealth.Code)
	}

	// 2. Unauthenticated POST /api/v1/chaos/campaigns MUST be rejected (401)
	body := []byte(`{"regime":"FLASH_CRASH","enabled":true}`)
	reqUnauth := httptest.NewRequest(http.MethodPost, "/api/v1/chaos/campaigns", bytes.NewReader(body))
	recUnauth := httptest.NewRecorder()
	h.ServeHTTP(recUnauth, reqUnauth)
	if recUnauth.Code != http.StatusUnauthorized {
		t.Fatalf("expected unauthenticated POST 401, got %d (%s)", recUnauth.Code, recUnauth.Body.String())
	}

	// 3. Authenticated POST with valid X-Hyperion-Internal-Token MUST succeed (200)
	reqAuth := httptest.NewRequest(http.MethodPost, "/api/v1/chaos/campaigns", bytes.NewReader(body))
	reqAuth.Header.Set("X-Hyperion-Internal-Token", "chaos-internal-secret-token-2026")
	recAuth := httptest.NewRecorder()
	h.ServeHTTP(recAuth, reqAuth)
	if recAuth.Code != http.StatusOK {
		t.Fatalf("expected authenticated POST 200, got %d (%s)", recAuth.Code, recAuth.Body.String())
	}
}
