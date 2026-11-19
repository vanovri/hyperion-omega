package main

import (
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/grpcclient"
	"github.com/vanovri/hyperion/services/gateway-go/internal/httpapi"
	"github.com/vanovri/hyperion/services/gateway-go/internal/idempotency"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/redpanda"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
)

func runHealthcheck() int {
	target := os.Getenv("GATEWAY_HEALTHCHECK_URL")
	if target == "" {
		target = "http://127.0.0.1:8081/healthz"
	}
	client := &http.Client{Timeout: 2 * time.Second}
	resp, err := client.Get(target)
	if err != nil {
		return 1
	}
	_ = resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return 1
	}
	return 0
}

func main() {
	if len(os.Args) >= 2 && (os.Args[1] == "--healthcheck" || os.Args[1] == "healthcheck") {
		os.Exit(runHealthcheck())
	}

	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	cfg := config.FromEnv()
	if err := cfg.ValidateProduction(); err != nil {
		logger.Error("production security validation failed", "err", err)
		os.Exit(1)
	}
	metrics := telemetry.Global

	var redisClient *redis.Client
	if cfg.RedisAddr != "" {
		rc := redis.NewClient(&redis.Options{
			Addr:         cfg.RedisAddr,
			Password:     cfg.RedisPassword,
			DB:           cfg.RedisDB,
			PoolSize:     64,
			MinIdleConns: 8,
			DialTimeout:  500 * time.Millisecond,
			ReadTimeout:  200 * time.Millisecond,
			WriteTimeout: 200 * time.Millisecond,
		})
		pingCtx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
		if err := rc.Ping(pingCtx).Err(); err != nil {
			logger.Warn("redis ping unavailable at startup; L1 atomic fallback active until Redis connects", "addr", cfg.RedisAddr, "err", err)
		}
		cancel()
		redisClient = rc
		defer func() { _ = redisClient.Close() }()
	}

	var jwtEdPub ed25519.PublicKey
	if cfg.JWTEd25519PubKeyHex != "" {
		if raw, err := hex.DecodeString(cfg.JWTEd25519PubKeyHex); err == nil && len(raw) == ed25519.PublicKeySize {
			jwtEdPub = ed25519.PublicKey(raw)
		}
	}
	var aegisEdPub ed25519.PublicKey
	if cfg.AegisEd25519PubKeyHex != "" {
		if raw, err := hex.DecodeString(cfg.AegisEd25519PubKeyHex); err == nil && len(raw) == ed25519.PublicKeySize {
			aegisEdPub = ed25519.PublicKey(raw)
		}
	}

	keyStore := auth.NewKeyStore(cfg.BootstrapHMACSecret)
	var nonceKV auth.NonceKV
	var idemRedis redis.Cmdable
	var rlRedis redis.Scripter
	if redisClient != nil {
		nonceKV = redisClient
		idemRedis = redisClient
		rlRedis = redisClient
	}

	replayGuard := auth.NewReplayGuard(cfg.NonceTTL, 2048, nonceKV)
	jwtVerifier := auth.NewJWTVerifier(cfg.JWTSecret, jwtEdPub, cfg.MaxClockSkew)
	sigAuth := auth.NewSignatureAuthenticator(keyStore, replayGuard, cfg.MaxClockSkew)

	limiter := ratelimit.NewHybridLimiter(ratelimit.Config{
		DefaultRPS:      cfg.DefaultRateRPS,
		DefaultBurst:    cfg.DefaultBurstTokens,
		L2SyncInterval:  cfg.L2SyncInterval,
		SpamWindow:      cfg.SpamOrderWindow,
		SpamMaxEvents:   cfg.SpamMaxEventsWindow,
		SpamPenaltyTTL:  cfg.SpamPenaltyTTL,
		AegisEd25519Pub: aegisEdPub,
		RedisClient:     rlRedis,
		Metrics:         metrics,
	})
	defer limiter.Close()

	idemStore := idempotency.NewStore(cfg.IdempotencyMaxEntries, cfg.IdempotencyTTL, idemRedis, metrics)

	mesh, err := grpcclient.NewMeshClients(cfg, metrics)
	if err != nil {
		logger.Error("failed initializing upstream gRPC pools", "err", err)
		os.Exit(1)
	}
	defer mesh.Close()

	wsHub := ws.NewHub(cfg, jwtVerifier, mesh, metrics)
	defer wsHub.Close()

	rpBridge, err := redpanda.NewBridge(cfg.RedpandaBrokers, cfg.RedpandaGroupID, cfg.EnableRedpanda, wsHub, limiter)
	if err != nil {
		logger.Warn("redpanda consumer disabled or deferred", "err", err)
	}
	if rpBridge != nil {
		defer rpBridge.Close()
	}

	srv := httpapi.NewServer(httpapi.Dependencies{
		Config:   cfg,
		KeyStore: keyStore,
		JWTAuth:  jwtVerifier,
		SigAuth:  sigAuth,
		Limiter:  limiter,
		Idem:     idemStore,
		Mesh:     mesh,
		WSHub:    wsHub,
		Redpanda: rpBridge,
		Metrics:  metrics,
	})

	publicHTTP := &http.Server{
		Addr:              cfg.PublicListenAddr,
		Handler:           srv.PublicHandler(),
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       60 * time.Second,
	}
	adminHTTP := &http.Server{
		Addr:              cfg.AdminListenAddr,
		Handler:           srv.AdminHandler(),
		ReadHeaderTimeout: 5 * time.Second,
	}

	errCh := make(chan error, 2)
	go func() {
		logger.Info("hyperion gateway-go public listener online", "addr", cfg.PublicListenAddr)
		if lerr := publicHTTP.ListenAndServe(); lerr != nil && !errors.Is(lerr, http.ErrServerClosed) {
			errCh <- lerr
		}
	}()
	go func() {
		logger.Info("hyperion gateway-go admin/telemetry listener online", "addr", cfg.AdminListenAddr)
		if lerr := adminHTTP.ListenAndServe(); lerr != nil && !errors.Is(lerr, http.ErrServerClosed) {
			errCh <- lerr
		}
	}()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)

	select {
	case sig := <-sigCh:
		logger.Info("shutdown signal received; draining connections", "signal", sig.String())
	case lerr := <-errCh:
		logger.Error("listener terminated with fatal error", "err", lerr)
	}

	shutCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_ = publicHTTP.Shutdown(shutCtx)
	_ = adminHTTP.Shutdown(shutCtx)
}
