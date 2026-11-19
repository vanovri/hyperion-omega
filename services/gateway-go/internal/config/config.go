package config

import (
	"errors"
	"os"
	"strconv"
	"strings"
	"time"
)

const (
	DefaultDevJWTSecret           = "hyperion-institutional-jwt-dev-secret-2026"
	DefaultDevBootstrapHMACSecret = "hyperion-trading-hmac-dev-key-2026"
)

// Config holds immutable runtime configuration for services/gateway-go.
// All monetary/quantity thresholds use integer fixed-point (scale 1e8) and
// all ratio/error thresholds use integer parts-per-million (ppm, scale 1e6).
type Config struct {
	Environment string
	ServiceName string
	Version     string

	// Canonical HTTP/WS (:8080) and Health/Metrics (:8081) listeners.
	PublicListenAddr string
	AdminListenAddr  string

	// Upstream gRPC endpoints (canonical ports 50151..50154).
	LedgerGRPCAddrs    []string
	AnalyticsGRPCAddrs []string
	FraudGRPCAddrs     []string
	ChronosGRPCAddrs   []string

	// Upstream HTTP control-plane proxy for Chaos-Sim (:8086).
	ChaosSimHTTPURL string

	// Infrastructure endpoints.
	RedisAddr       string
	RedisPassword   string
	RedisDB         int
	RedpandaBrokers []string
	RedpandaGroupID string
	EnableRedpanda  bool

	// Cryptographic auth parameters.
	JWTSecret             []byte
	JWTEd25519PubKeyHex   string
	AegisEd25519PubKeyHex string
	BootstrapHMACSecret   []byte
	InternalAuthToken     string
	CORSOrigins           []string
	MaxClockSkew          time.Duration
	NonceTTL              time.Duration

	// Hybrid L1/L2 Token-Bucket Rate Limiter parameters.
	DefaultRateRPS       int64
	DefaultBurstTokens   int64
	L2SyncInterval       time.Duration
	SpamOrderWindow      time.Duration
	SpamMaxEventsWindow  int64
	SpamPenaltyTTL       time.Duration

	// Multiplexed gRPC pool & Adaptive Circuit Breaker parameters.
	GRPCPoolSizePerTarget int
	DefaultRPCTimeout     time.Duration
	MaxIdempotentRetries  int
	RetryBaseBackoff      time.Duration
	BreakerWindowBuckets  int
	BreakerBucketDuration time.Duration
	BreakerMinRequests    uint64
	BreakerErrorTripPPM   int64 // e.g., 500_000 = 50% error ratio
	BreakerOpenCooldown   time.Duration
	BreakerHalfOpenProbes uint32

	// Idempotency middleware parameters.
	IdempotencyMaxEntries int
	IdempotencyTTL        time.Duration

	// WebSocket multiplexer & backpressure parameters.
	WSMaxClients           int
	WSMarketRingSize       int // Power-of-two per-client droppable ring buffer size
	WSCriticalQueueSize    int // Bounded critical private event channel capacity
	WSWriteTimeout         time.Duration
	WSReadTimeout          time.Duration
	WSHeartbeatInterval    time.Duration
	WSMaxConsecutiveStalls uint32
}

// FromEnv constructs Config from environment variables with canonical HYPERION defaults
// matching .agents/rules/00-hyperion-contract.md.
func FromEnv() Config {
	return Config{
		Environment:           strings.ToLower(envStr("HYPERION_ENV", "development")),
		ServiceName:           envStr("HYPERION_SERVICE_NAME", "gateway-go"),
		Version:               envStr("HYPERION_VERSION", "1.0.0"),
		PublicListenAddr:      envStr("GATEWAY_PUBLIC_ADDR", ":8080"),
		AdminListenAddr:       envStr("GATEWAY_ADMIN_ADDR", ":8081"),
		LedgerGRPCAddrs:       envCSV("LEDGER_GRPC_ADDRS", []string{"localhost:50151"}),
		AnalyticsGRPCAddrs:    envCSV("ANALYTICS_GRPC_ADDRS", []string{"localhost:50152"}),
		FraudGRPCAddrs:        envCSV("FRAUD_GRPC_ADDRS", []string{"localhost:50153"}),
		ChronosGRPCAddrs:      envCSV("CHRONOS_GRPC_ADDRS", []string{"localhost:50154"}),
		ChaosSimHTTPURL:       envStr("CHAOS_SIM_HTTP_URL", "http://localhost:8086"),
		RedisAddr:             envStrAllowEmpty("REDIS_ADDR", "localhost:6379"),
		RedisPassword:         envStr("REDIS_PASSWORD", ""),
		RedisDB:               int(envInt("REDIS_DB", 0)),
		RedpandaBrokers:       envCSV("REDPANDA_BROKERS", []string{"localhost:19092"}),
		RedpandaGroupID:       envStr("REDPANDA_GROUP_ID", "hyperion-gateway-go-v1"),
		EnableRedpanda:        envBool("ENABLE_REDPANDA", true),
		JWTSecret:             []byte(envStr("HYPERION_JWT_SECRET", DefaultDevJWTSecret)),
		JWTEd25519PubKeyHex:   envStr("HYPERION_JWT_ED25519_PUB_HEX", ""),
		AegisEd25519PubKeyHex: envStr("HYPERION_AEGIS_ED25519_PUB_HEX", ""),
		BootstrapHMACSecret:   []byte(envStr("HYPERION_BOOTSTRAP_HMAC_SECRET", DefaultDevBootstrapHMACSecret)),
		InternalAuthToken:     envStr("HYPERION_INTERNAL_AUTH_TOKEN", ""),
		CORSOrigins:           envCSV("HYPERION_CORS_ORIGINS", nil),
		MaxClockSkew:          time.Duration(envInt("AUTH_MAX_CLOCK_SKEW_MS", 5000)) * time.Millisecond,
		NonceTTL:              time.Duration(envInt("AUTH_NONCE_TTL_MS", 15000)) * time.Millisecond,
		DefaultRateRPS:        envInt("RATELIMIT_DEFAULT_RPS", 200),
		DefaultBurstTokens:    envInt("RATELIMIT_DEFAULT_BURST", 400),
		L2SyncInterval:        time.Duration(envInt("RATELIMIT_L2_SYNC_MS", 50)) * time.Millisecond,
		SpamOrderWindow:       time.Duration(envInt("RATELIMIT_SPAM_WINDOW_MS", 1000)) * time.Millisecond,
		SpamMaxEventsWindow:   envInt("RATELIMIT_SPAM_MAX_EVENTS", 500),
		SpamPenaltyTTL:        time.Duration(envInt("RATELIMIT_SPAM_PENALTY_MS", 10000)) * time.Millisecond,
		GRPCPoolSizePerTarget: int(envInt("GRPC_POOL_CONNS_PER_TARGET", 4)),
		DefaultRPCTimeout:     time.Duration(envInt("GRPC_DEFAULT_TIMEOUT_MS", 1500)) * time.Millisecond,
		MaxIdempotentRetries:  int(envInt("GRPC_MAX_IDEMPOTENT_RETRIES", 2)),
		RetryBaseBackoff:      time.Duration(envInt("GRPC_RETRY_BASE_BACKOFF_MS", 15)) * time.Millisecond,
		BreakerWindowBuckets:  int(envInt("BREAKER_WINDOW_BUCKETS", 10)),
		BreakerBucketDuration: time.Duration(envInt("BREAKER_BUCKET_MS", 1000)) * time.Millisecond,
		BreakerMinRequests:    uint64(envInt("BREAKER_MIN_REQUESTS", 20)),
		BreakerErrorTripPPM:   envInt("BREAKER_ERROR_TRIP_PPM", 500_000),
		BreakerOpenCooldown:   time.Duration(envInt("BREAKER_OPEN_COOLDOWN_MS", 2000)) * time.Millisecond,
		BreakerHalfOpenProbes: uint32(envInt("BREAKER_HALF_OPEN_PROBES", 5)),
		IdempotencyMaxEntries: int(envInt("IDEMPOTENCY_MAX_ENTRIES", 65536)),
		IdempotencyTTL:        time.Duration(envInt("IDEMPOTENCY_TTL_SEC", 300)) * time.Second,
		WSMaxClients:          int(envInt("WS_MAX_CLIENTS", 65536)),
		WSMarketRingSize:      int(envInt("WS_MARKET_RING_SIZE", 256)),
		WSCriticalQueueSize:   int(envInt("WS_CRITICAL_QUEUE_SIZE", 128)),
		WSWriteTimeout:        time.Duration(envInt("WS_WRITE_TIMEOUT_MS", 1500)) * time.Millisecond,
		WSReadTimeout:         time.Duration(envInt("WS_READ_TIMEOUT_MS", 60000)) * time.Millisecond,
		WSHeartbeatInterval:   time.Duration(envInt("WS_HEARTBEAT_INTERVAL_MS", 10000)) * time.Millisecond,
		WSMaxConsecutiveStalls: uint32(envInt("WS_MAX_CONSECUTIVE_STALLS", 3)),
	}
}

func envStr(key, fallback string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return fallback
}

func envStrAllowEmpty(key, fallback string) string {
	v, ok := os.LookupEnv(key)
	if !ok {
		return fallback
	}
	trimmed := strings.TrimSpace(v)
	if strings.EqualFold(trimmed, "none") || strings.EqualFold(trimmed, "disabled") {
		return ""
	}
	return trimmed
}

func envInt(key string, fallback int64) int64 {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		if parsed, err := strconv.ParseInt(v, 10, 64); err == nil {
			return parsed
		}
	}
	return fallback
}

func envBool(key string, fallback bool) bool {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		if parsed, err := strconv.ParseBool(v); err == nil {
			return parsed
		}
	}
	return fallback
}

func envCSV(key string, fallback []string) []string {
	raw := strings.TrimSpace(os.Getenv(key))
	if raw == "" {
		return fallback
	}
	parts := strings.Split(raw, ",")
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		if trimmed := strings.TrimSpace(p); trimmed != "" {
			out = append(out, trimmed)
		}
	}
	if len(out) == 0 {
		return fallback
	}
	return out
}

// ValidateProduction enforces fail-closed cryptographic hygiene (SEC-05) when HYPERION_ENV=production.
func (c Config) ValidateProduction() error {
	if !strings.EqualFold(strings.TrimSpace(c.Environment), "production") {
		return nil
	}
	if string(c.JWTSecret) == DefaultDevJWTSecret || len(c.JWTSecret) < 32 {
		return errors.New("SECURITY_FAIL_CLOSED: HYPERION_JWT_SECRET must be a >=32-byte non-default secret in production")
	}
	if string(c.BootstrapHMACSecret) == DefaultDevBootstrapHMACSecret || len(c.BootstrapHMACSecret) < 32 {
		return errors.New("SECURITY_FAIL_CLOSED: HYPERION_BOOTSTRAP_HMAC_SECRET must be a >=32-byte non-default secret in production")
	}
	return nil
}
