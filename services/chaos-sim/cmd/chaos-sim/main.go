package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/control"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/engine"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/transport"
)

func main() {
	if len(os.Args) >= 2 {
		switch os.Args[1] {
		case "--healthcheck", "healthcheck":
			os.Exit(runHealthcheck())
		case "trigger-syndicate":
			os.Exit(runCLISyndicate(os.Args[2:]))
		case "set-regime":
			os.Exit(runCLIRegime(os.Args[2:]))
		case "verify-audit":
			os.Exit(runCLIVerifyAudit(os.Args[2:]))
		}
	}

	cfg := engine.DefaultSimulatorConfig()

	listenAddr := envOr("CHAOS_SIM_LISTEN", ":8086")
	seedVal := flag.Uint64("seed", cfg.MasterSeed, "Deterministic 64-bit PRNG master seed")
	symbolVal := flag.String("symbol", envOr("CHAOS_SIM_SYMBOL", cfg.Symbol), "Canonical instrument symbol (BTC-USDT, ETH-USDT, SOL-USDT, HYPE-USDT)")
	botsVal := flag.Int("bots", cfg.TotalBots, "Total autonomous heterogeneous trading bots (>= 1000 default 1024)")
	workersVal := flag.Int("workers", cfg.WorkerShards, "Parallel simulation worker shards")
	tickMs := flag.Int("tick-ms", 20, "Worker simulation tick interval in milliseconds")
	backendVal := flag.String("backend", envOr("CHAOS_SIM_BACKEND", string(cfg.Backend)), "Target backend: mock | gateway-http | ledger-grpc")
	gatewayURL := flag.String("gateway-url", envOr("HYPERION_GATEWAY_URL", cfg.GatewayHTTPURL), "Gateway HTTP base URL")
	gatewayWS := flag.String("gateway-ws", envOr("HYPERION_GATEWAY_WS", cfg.GatewayWSURL), "Gateway WebSocket URL for slow-loris fault injector")
	ledgerGRPC := flag.String("ledger-grpc", envOr("HYPERION_LEDGER_GRPC", cfg.LedgerGRPCAddr), "Ledger gRPC host:port")
	authScheme := flag.String("auth-scheme", envOr("HYPERION_AUTH_SCHEME", string(cfg.AuthScheme)), "Signature scheme: ed25519 | hmac-sha256")
	listenFlag := flag.String("listen", listenAddr, "Control Plane HTTP/SSE/Metrics listen address")
	enableProxies := flag.Bool("enable-toxic-proxies", envOr("CHAOS_ENABLE_PROXIES", "true") == "true", "Start canonical L4 Toxic TCP Proxies on 19001-19005")
	initRegime := flag.String("regime", envOr("CHAOS_INITIAL_REGIME", "CALM"), "Initial market regime")
	flag.Parse()

	cfg.MasterSeed = *seedVal
	cfg.Symbol = *symbolVal
	cfg.TotalBots = *botsVal
	cfg.WorkerShards = *workersVal
	cfg.TickInterval = time.Duration(*tickMs) * time.Millisecond
	cfg.Backend = engine.BackendMode(strings.ToLower(strings.TrimSpace(*backendVal)))
	cfg.GatewayHTTPURL = *gatewayURL
	cfg.GatewayWSURL = *gatewayWS
	cfg.LedgerGRPCAddr = *ledgerGRPC
	if strings.EqualFold(*authScheme, "hmac-sha256") {
		cfg.AuthScheme = transport.AuthHMACSHA256
	} else {
		cfg.AuthScheme = transport.AuthEd25519
	}

	sim, err := engine.NewSimulator(cfg)
	if err != nil {
		log.Fatalf("[chaos-sim] simulator init failed: %v", err)
	}

	if norm, ok := model.NormalizeRegime(strings.ToUpper(*initRegime)); ok {
		sim.ApplyCampaign(chaos.PresetForRegime(norm), norm != model.RegimeCalm)
	}

	proxies := make(map[string]*chaos.ToxicProxy)
	if *enableProxies {
		for idx, spec := range control.DefaultCanonicalProxySpecs() {
			px := chaos.NewToxicProxy(spec.ListenAddr, spec.UpstreamAddr, cfg.MasterSeed+uint64(idx+1), chaos.DefaultToxicConfig())
			if err := px.Start(); err != nil {
				log.Printf("[chaos-sim] warning: toxic proxy %s (%s -> %s) skipped: %v", spec.Name, spec.ListenAddr, spec.UpstreamAddr, err)
				continue
			}
			proxies[spec.Name] = px
		}
	}

	ctrl := control.NewControlServer(sim, proxies)
	httpSrv := &http.Server{
		Addr:              *listenFlag,
		Handler:           ctrl.Handler(),
		ReadHeaderTimeout: 5 * time.Second,
	}

	sim.Start()
	log.Printf("[chaos-sim] active: bots=%d seed=0x%x symbol=%s backend=%s listen=%s proxies=%d",
		cfg.TotalBots, cfg.MasterSeed, cfg.Symbol, cfg.Backend, *listenFlag, len(proxies))

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		<-sigCh
		log.Printf("[chaos-sim] shutting down cleanly...")
		shutCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = httpSrv.Shutdown(shutCtx)
	}()

	if err := httpSrv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatalf("[chaos-sim] http control server error: %v", err)
	}

	ctrl.Close()
	sim.Stop()
}

func runCLISyndicate(args []string) int {
	fs := flag.NewFlagSet("trigger-syndicate", flag.ContinueOnError)
	ctrlURL := fs.String("control-url", envOr("CHAOS_CONTROL_URL", "http://127.0.0.1:8086"), "Chaos-Sim Control Plane URL")
	attack := fs.String("type", "WASH_TRADING_RING", "WASH_TRADING_RING | LAYERING_SPOOFER | FLASH_CRASH_SWEEP")
	ringSize := fs.Int("ring-size", 4, "Accounts in circular wash ring (3..5)")
	rounds := fs.Int("rounds", 5, "Burst rounds")
	lots := fs.Int64("lots", 12, "Lots per order")
	if err := fs.Parse(args); err != nil {
		return 2
	}

	payload, _ := json.Marshal(engine.SyndicateAttackRequest{
		AttackType: engine.SyndicateAttackType(strings.ToUpper(*attack)),
		RingSize:   *ringSize,
		Rounds:     *rounds,
		Lots:       *lots,
	})
	return postJSONCLI(strings.TrimRight(*ctrlURL, "/")+"/api/v1/chaos/syndicate", payload)
}

func runCLIRegime(args []string) int {
	fs := flag.NewFlagSet("set-regime", flag.ContinueOnError)
	ctrlURL := fs.String("control-url", envOr("CHAOS_CONTROL_URL", "http://127.0.0.1:8086"), "Chaos-Sim Control Plane URL")
	regime := fs.String("regime", "CALM", "CALM | HIGH_VOLATILITY | FLASH_CRASH | SPOOFING_ATTACK | WASH_TRADING_RING | BURST_TSUNAMI_20X | NETWORK_PARTITION")
	enabled := fs.Bool("enabled", true, "Enable or revert campaign to CALM")
	durationMs := fs.Int("duration-ms", 0, "Optional auto-revert duration in ms")
	if err := fs.Parse(args); err != nil {
		return 2
	}

	reqMap := map[string]any{
		"regime":  strings.ToUpper(*regime),
		"enabled": *enabled,
	}
	if *durationMs >= 100 {
		reqMap["duration_ms"] = *durationMs
	}
	payload, _ := json.Marshal(reqMap)
	return postJSONCLI(strings.TrimRight(*ctrlURL, "/")+"/api/v1/chaos/campaigns", payload)
}

func runCLIVerifyAudit(args []string) int {
	fs := flag.NewFlagSet("verify-audit", flag.ContinueOnError)
	ctrlURL := fs.String("control-url", envOr("CHAOS_CONTROL_URL", "http://127.0.0.1:8086"), "Chaos-Sim Control Plane URL")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	return postJSONCLI(strings.TrimRight(*ctrlURL, "/")+"/api/v1/chaos/audit/verify", []byte(`{}`))
}

func postJSONCLI(url string, body []byte) int {
	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Post(url, "application/json", bytes.NewReader(body))
	if err != nil {
		fmt.Fprintf(os.Stderr, "error contacting %s: %v\n", url, err)
		return 1
	}
	defer resp.Body.Close()
	out, _ := io.ReadAll(resp.Body)
	fmt.Println(string(out))
	if resp.StatusCode >= 300 {
		return 1
	}
	return 0
}

func runHealthcheck() int {
	target := envOr("CHAOS_HEALTHCHECK_URL", "http://127.0.0.1:8086/healthz")
	client := &http.Client{Timeout: 2 * time.Second}
	resp, err := client.Get(target)
	if err != nil {
		fmt.Fprintf(os.Stderr, "healthcheck failed: %v\n", err)
		return 1
	}
	_ = resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return 1
	}
	return 0
}

func envOr(key, fallback string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return fallback
}
