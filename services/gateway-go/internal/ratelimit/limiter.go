package ratelimit

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/binary"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

const (
	numLimiterShards = 64
	numHotSlots      = 1024
	milliPerToken    = int64(1000)
)

// tokenBucketLuaScript performs atomic L2 global token-bucket refill, node delta deduction,
// and cross-node freeze/throttle synchronization in Redis using 100% integer milli-token math.
const tokenBucketLuaScript = `
local vals = redis.call('HMGET', KEYS[1], 'tokens_milli', 'last_ms', 'frozen_until_ms', 'override_rps')
local now_ms = tonumber(ARGV[1])
local def_rps = tonumber(ARGV[2])
local def_burst = tonumber(ARGV[3])
local consumed = tonumber(ARGV[4])
local in_frozen = tonumber(ARGV[5])
local in_override = tonumber(ARGV[6])
local ttl_ms = tonumber(ARGV[7])

local frozen = tonumber(vals[3]) or 0
local override_rps = tonumber(vals[4]) or 0

if in_override == -1 then
    frozen = 0
    override_rps = 0
else
    if in_frozen > frozen then
        frozen = in_frozen
    end
    if in_override > 0 then
        override_rps = in_override
    end
end

if frozen > 0 and frozen <= now_ms then
    frozen = 0
end

local eff_rps = def_rps
if override_rps > 0 then
    eff_rps = override_rps
end
local burst = eff_rps * 2000
if override_rps == 0 and def_burst > 0 then
    burst = def_burst
end

local tokens = tonumber(vals[1])
local last_ms = tonumber(vals[2])
if tokens == nil or last_ms == nil then
    tokens = burst
    last_ms = now_ms
else
    local elapsed = now_ms - last_ms
    if elapsed > 0 then
        tokens = tokens + (elapsed * eff_rps)
        if tokens > burst then
            tokens = burst
        end
        last_ms = now_ms
    end
end

if consumed > 0 then
    tokens = tokens - consumed
    if tokens < 0 then
        tokens = 0
    end
end

redis.call('HMSET', KEYS[1], 'tokens_milli', tokens, 'last_ms', last_ms, 'frozen_until_ms', frozen, 'override_rps', override_rps)
redis.call('PEXPIRE', KEYS[1], ttl_ms)
return {tokens, frozen, eff_rps}
`

// Decision describes the outcome of a HybridLimiter check.
type Decision struct {
	Allowed         bool
	Reason          string // "OK", "RATE_LIMITED", "ACCOUNT_FROZEN", "SPAM_PENALTY_QUARANTINE", "MARKET_VOLATILITY_HALT"
	RemainingTokens int64
	EffectiveRPS    int64
	PenaltyLevel    int32
}

type accountBucket struct {
	accountID            uint64
	tokensMilli          atomic.Int64
	lastRefillNS         atomic.Int64
	pendingConsumedMilli atomic.Int64
	effectiveRPS         atomic.Int64
	effectiveBurstMilli  atomic.Int64
	frozenUntilNS        atomic.Int64
	overrideUntilNS      atomic.Int64
	overrideRPS          atomic.Int64
	clearL2Flag          atomic.Bool
	dirty                atomic.Bool

	// Order-spam / velocity burst detector state.
	spamWindowStartNS atomic.Int64
	spamWindowEvents  atomic.Int64
	penaltyLevel      atomic.Int32
	penaltyExpiresNS  atomic.Int64
	hasRiskState      atomic.Bool // fast-path guard: skips freeze/penalty/override checks when false
	_                 [40]byte    // isolate per-account hot atomic fields onto dedicated cache lines
}

type bucketShard struct {
	mu      sync.RWMutex
	buckets map[uint64]*accountBucket
	_       [40]byte
}

// Config configures the two-tier HybridLimiter.
type Config struct {
	DefaultRPS          int64
	DefaultBurst        int64
	L2SyncInterval      time.Duration
	SpamWindow          time.Duration
	SpamMaxEvents       int64
	SpamPenaltyTTL      time.Duration
	AegisEd25519Pub     ed25519.PublicKey
	RedisClient         redis.Scripter
	Metrics             *telemetry.Registry
}

// HybridLimiter implements a two-tier Token-Bucket Rate Limiter:
//   - L1: Lock-free atomic per-account milli-token bucket per gateway node (<100ns check).
//   - L2: Asynchronous quota & risk-state reconciliation via atomic Redis Lua script.
//   - Auto-Penalty: Progressive rate cut & quarantine on order-spamming bursts.
//   - Redpanda Risk Consumer: Sub-millisecond account freeze/throttle from RiskMitigationCommand.
type HybridLimiter struct {
	hotSlots        [numHotSlots]atomic.Pointer[accountBucket]
	shards          [numLimiterShards]bucketShard
	defaultRPS      int64
	defaultBurstMil int64
	syncInterval    time.Duration
	spamWindowNS    int64
	spamMaxEvents   int64
	spamPenaltyNS   int64
	aegisPub        ed25519.PublicKey
	redisScripter   redis.Scripter
	luaScript       *redis.Script
	metrics         *telemetry.Registry
	nowFunc         func() time.Time

	regimeMu     sync.RWMutex
	symbolRegime map[string]hyperionv1.MarketRegime

	stopOnce sync.Once
	stopCh   chan struct{}
	wg       sync.WaitGroup
}

// NewHybridLimiter creates and starts the L1/L2 hybrid rate limiter background synchronizer.
func NewHybridLimiter(cfg Config) *HybridLimiter {
	if cfg.DefaultRPS <= 0 {
		cfg.DefaultRPS = 200
	}
	if cfg.DefaultBurst <= 0 {
		cfg.DefaultBurst = cfg.DefaultRPS * 2
	}
	if cfg.L2SyncInterval <= 0 {
		cfg.L2SyncInterval = 50 * time.Millisecond
	}
	if cfg.SpamWindow <= 0 {
		cfg.SpamWindow = time.Second
	}
	if cfg.SpamMaxEvents <= 0 {
		cfg.SpamMaxEvents = 500
	}
	if cfg.SpamPenaltyTTL <= 0 {
		cfg.SpamPenaltyTTL = 10 * time.Second
	}
	if cfg.Metrics == nil {
		cfg.Metrics = telemetry.Global
	}

	hl := &HybridLimiter{
		defaultRPS:      cfg.DefaultRPS,
		defaultBurstMil: cfg.DefaultBurst * milliPerToken,
		syncInterval:    cfg.L2SyncInterval,
		spamWindowNS:    cfg.SpamWindow.Nanoseconds(),
		spamMaxEvents:   cfg.SpamMaxEvents,
		spamPenaltyNS:   cfg.SpamPenaltyTTL.Nanoseconds(),
		aegisPub:        append(ed25519.PublicKey(nil), cfg.AegisEd25519Pub...),
		redisScripter:   cfg.RedisClient,
		luaScript:       redis.NewScript(tokenBucketLuaScript),
		metrics:         cfg.Metrics,
		nowFunc:         time.Now,
		symbolRegime:    make(map[string]hyperionv1.MarketRegime, 8),
		stopCh:          make(chan struct{}),
	}
	for i := 0; i < numLimiterShards; i++ {
		hl.shards[i].buckets = make(map[uint64]*accountBucket, 64)
	}

	if hl.redisScripter != nil {
		hl.wg.Add(1)
		go hl.syncLoop()
	}
	return hl
}

// SetClock overrides wall clock for deterministic testing.
func (hl *HybridLimiter) SetClock(fn func() time.Time) {
	if fn != nil {
		hl.nowFunc = fn
	}
}

// Close stops the asynchronous Redis L2 sync goroutine cleanly without leaks.
func (hl *HybridLimiter) Close() {
	hl.stopOnce.Do(func() {
		close(hl.stopCh)
	})
	hl.wg.Wait()
}

func (hl *HybridLimiter) getOrCreateBucket(accountID uint64, nowNS int64) *accountBucket {
	// Lock-free direct-mapped L1 cache lookup (zero RWMutex & zero Go map hashing on hot path).
	slotIdx := accountID & (numHotSlots - 1)
	if cached := hl.hotSlots[slotIdx].Load(); cached != nil && cached.accountID == accountID {
		return cached
	}

	idx := (accountID * 0x9e3779b97f4a7c15) & (numLimiterShards - 1)
	sh := &hl.shards[idx]

	sh.mu.RLock()
	b, ok := sh.buckets[accountID]
	sh.mu.RUnlock()
	if ok {
		hl.hotSlots[slotIdx].Store(b)
		return b
	}

	sh.mu.Lock()
	if b, ok = sh.buckets[accountID]; ok {
		sh.mu.Unlock()
		hl.hotSlots[slotIdx].Store(b)
		return b
	}
	b = &accountBucket{accountID: accountID}
	b.tokensMilli.Store(hl.defaultBurstMil)
	b.lastRefillNS.Store(nowNS)
	b.effectiveRPS.Store(hl.defaultRPS)
	b.effectiveBurstMilli.Store(hl.defaultBurstMil)
	b.spamWindowStartNS.Store(nowNS)
	sh.buckets[accountID] = b
	sh.mu.Unlock()
	hl.hotSlots[slotIdx].Store(b)
	return b
}

// CheckSymbolRegime verifies whether a target symbol is currently halted.
func (hl *HybridLimiter) CheckSymbolRegime(symbol string) hyperionv1.MarketRegime {
	if symbol == "" {
		return hyperionv1.MarketRegime_MARKET_REGIME_NORMAL
	}
	hl.regimeMu.RLock()
	reg := hl.symbolRegime[symbol]
	hl.regimeMu.RUnlock()
	if reg == hyperionv1.MarketRegime_MARKET_REGIME_UNSPECIFIED {
		return hyperionv1.MarketRegime_MARKET_REGIME_NORMAL
	}
	return reg
}

// AllowAccount checks L1 atomic quota, order-spam velocity, and active risk freezes/throttles.
// isOrderMutation enables aggressive spam burst detection for order endpoints.
func (hl *HybridLimiter) AllowAccount(accountID uint64, symbol string, isOrderMutation bool) Decision {
	if symbol != "" && isOrderMutation {
		if hl.CheckSymbolRegime(symbol) == hyperionv1.MarketRegime_MARKET_REGIME_HALTED {
			hl.metrics.RateLimitedTotal.Add(1)
			return Decision{Allowed: false, Reason: "MARKET_VOLATILITY_HALT"}
		}
	}

	nowNS := hl.nowFunc().UnixNano()
	b := hl.getOrCreateBucket(accountID, nowNS)

	// 1 & 2. Fast-path bypass when account has no active freeze, penalty, or risk override.
	if b.hasRiskState.Load() {
		if frozenUntil := b.frozenUntilNS.Load(); frozenUntil > nowNS {
			hl.metrics.RateLimitedTotal.Add(1)
			reason := "ACCOUNT_FROZEN"
			if b.penaltyLevel.Load() >= 3 {
				reason = "SPAM_PENALTY_QUARANTINE"
			}
			return Decision{
				Allowed:      false,
				Reason:       reason,
				EffectiveRPS: 0,
				PenaltyLevel: b.penaltyLevel.Load(),
			}
		}

		if penExp := b.penaltyExpiresNS.Load(); penExp > 0 && nowNS >= penExp {
			if b.penaltyExpiresNS.CompareAndSwap(penExp, 0) {
				b.penaltyLevel.Store(0)
				hl.recomputeEffectiveQuota(b, nowNS)
			}
		}
		if ovExp := b.overrideUntilNS.Load(); ovExp > 0 && nowNS >= ovExp {
			if b.overrideUntilNS.CompareAndSwap(ovExp, 0) {
				b.overrideRPS.Store(0)
				hl.recomputeEffectiveQuota(b, nowNS)
			}
		}
		if b.frozenUntilNS.Load() <= nowNS && b.penaltyExpiresNS.Load() == 0 && b.overrideUntilNS.Load() == 0 && b.overrideRPS.Load() == 0 {
			b.hasRiskState.Store(false)
		}
	}

	// 3. Sliding-window order-spam detector (counts all mutation attempts, even throttled ones).
	if isOrderMutation {
		winStart := b.spamWindowStartNS.Load()
		if nowNS-winStart >= hl.spamWindowNS {
			if b.spamWindowStartNS.CompareAndSwap(winStart, nowNS) {
				b.spamWindowEvents.Store(0)
			}
		}
		evCount := b.spamWindowEvents.Add(1)
		if evCount > hl.spamMaxEvents {
			b.spamWindowStartNS.Store(nowNS)
			b.spamWindowEvents.Store(0)
			newLevel := b.penaltyLevel.Add(1)
			exp := nowNS + hl.spamPenaltyNS
			b.penaltyExpiresNS.Store(exp)
			b.hasRiskState.Store(true)
			hl.metrics.SpamPenaltiesTotal.Add(1)
			if newLevel >= 3 {
				b.frozenUntilNS.Store(exp)
				b.tokensMilli.Store(0)
				markDirty(b)
				hl.metrics.RateLimitedTotal.Add(1)
				return Decision{
					Allowed:      false,
					Reason:       "SPAM_PENALTY_QUARANTINE",
					EffectiveRPS: 0,
					PenaltyLevel: newLevel,
				}
			}
			hl.recomputeEffectiveQuota(b, nowNS)
			markDirty(b)
		}
	}

	// 4. Lock-free integer milli-token refill + CAS consumption.
	effRPS := b.effectiveRPS.Load()
	effBurst := b.effectiveBurstMilli.Load()
	hl.refillBucket(b, nowNS, effRPS, effBurst)

	for {
		cur := b.tokensMilli.Load()
		if cur < milliPerToken {
			hl.metrics.RateLimitedTotal.Add(1)
			return Decision{
				Allowed:         false,
				Reason:          "RATE_LIMITED",
				RemainingTokens: 0,
				EffectiveRPS:    effRPS,
				PenaltyLevel:    b.penaltyLevel.Load(),
			}
		}
		next := cur - milliPerToken
		if b.tokensMilli.CompareAndSwap(cur, next) {
			b.pendingConsumedMilli.Add(milliPerToken)
			markDirty(b)
			return Decision{
				Allowed:         true,
				Reason:          "OK",
				RemainingTokens: next / milliPerToken,
				EffectiveRPS:    effRPS,
				PenaltyLevel:    b.penaltyLevel.Load(),
			}
		}
	}
}

func markDirty(b *accountBucket) {
	if !b.dirty.Load() {
		b.dirty.Store(true)
	}
}

func (hl *HybridLimiter) recomputeEffectiveQuota(b *accountBucket, nowNS int64) {
	rps := hl.defaultRPS
	burst := hl.defaultBurstMil

	if ov := b.overrideRPS.Load(); ov > 0 && (b.overrideUntilNS.Load() == 0 || b.overrideUntilNS.Load() > nowNS) {
		rps = ov
		burst = ov * 2 * milliPerToken
	}

	switch b.penaltyLevel.Load() {
	case 1:
		rps = maxInt64(1, rps/2)
		burst = maxInt64(milliPerToken, burst/2)
	case 2:
		rps = maxInt64(1, rps/10)
		burst = maxInt64(milliPerToken, burst/10)
	}

	b.effectiveRPS.Store(rps)
	b.effectiveBurstMilli.Store(burst)
	for {
		cur := b.tokensMilli.Load()
		if cur <= burst || b.tokensMilli.CompareAndSwap(cur, burst) {
			break
		}
	}
}

func (hl *HybridLimiter) refillBucket(b *accountBucket, nowNS, effRPS, effBurst int64) {
	last := b.lastRefillNS.Load()
	elapsedNS := nowNS - last
	if elapsedNS <= 0 {
		return
	}
	// Integer milli-tokens added = (elapsedNS * effRPS) / 1_000_000 (since 1s = 1e9 ns and 1 token = 1e3 milli).
	addedMilli := (elapsedNS * effRPS) / 1_000_000
	if addedMilli <= 0 {
		return
	}
	if !b.lastRefillNS.CompareAndSwap(last, nowNS) {
		return
	}
	for {
		cur := b.tokensMilli.Load()
		next := cur + addedMilli
		if next > effBurst {
			next = effBurst
		}
		if b.tokensMilli.CompareAndSwap(cur, next) {
			return
		}
	}
}

// CanonicalMitigationBytes constructs deterministic signing preimage for RiskMitigationCommand
// when Ed25519 signature verification is enabled.
func CanonicalMitigationBytes(cmd *hyperionv1.RiskMitigationCommand) []byte {
	var buf bytes.Buffer
	buf.WriteString(cmd.GetCommandId())
	buf.WriteByte('|')
	buf.WriteString(cmd.GetIncidentId())
	buf.WriteByte('|')
	var tmp [8]byte
	binary.LittleEndian.PutUint32(tmp[:4], uint32(cmd.GetAction()))
	buf.Write(tmp[:4])
	for _, acct := range cmd.GetTargetAccountIds() {
		binary.LittleEndian.PutUint64(tmp[:], acct)
		buf.Write(tmp[:])
	}
	buf.WriteString(cmd.GetTargetSymbol())
	binary.LittleEndian.PutUint32(tmp[:4], cmd.GetRateLimitOverrideRps())
	buf.Write(tmp[:4])
	binary.LittleEndian.PutUint64(tmp[:], cmd.GetIssuedTsNs())
	buf.Write(tmp[:])
	return buf.Bytes()
}

// ApplyRiskMitigation enforces a signed RiskMitigationCommand received from Redpanda
// (`hyperion.risk.mitigations.v1`). Respects dry_run, requires_human_approval, and expiry.
func (hl *HybridLimiter) ApplyRiskMitigation(ctx context.Context, cmd *hyperionv1.RiskMitigationCommand) (bool, error) {
	if cmd == nil {
		return false, fmt.Errorf("nil RiskMitigationCommand")
	}
	if len(hl.aegisPub) == ed25519.PublicKeySize {
		sig := cmd.GetEd25519Signature()
		if len(sig) != ed25519.SignatureSize || !ed25519.Verify(hl.aegisPub, CanonicalMitigationBytes(cmd), sig) {
			return false, fmt.Errorf("invalid RiskMitigationCommand ed25519 signature")
		}
	}
	if cmd.GetDryRun() || cmd.GetRequiresHumanApproval() {
		return false, nil
	}

	nowNS := hl.nowFunc().UnixNano()
	if exp := cmd.GetExpiresTsNs(); exp > 0 && int64(exp) <= nowNS {
		return false, nil
	}

	durNS := int64(cmd.GetCooldownDurationMs()) * int64(time.Millisecond)
	if durNS <= 0 {
		if exp := cmd.GetExpiresTsNs(); exp > uint64(nowNS) {
			durNS = int64(exp) - nowNS
		} else {
			durNS = int64(60 * time.Second)
		}
	}
	untilNS := nowNS + durNS

	switch cmd.GetAction() {
	case hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT:
		for _, acct := range cmd.GetTargetAccountIds() {
			b := hl.getOrCreateBucket(acct, nowNS)
			b.frozenUntilNS.Store(untilNS)
			b.hasRiskState.Store(true)
			b.tokensMilli.Store(0)
			markDirty(b)
		}
	case hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_THROTTLE_RATE_LIMIT:
		overrideRPS := int64(cmd.GetRateLimitOverrideRps())
		if overrideRPS <= 0 {
			overrideRPS = maxInt64(1, hl.defaultRPS/10)
		}
		for _, acct := range cmd.GetTargetAccountIds() {
			b := hl.getOrCreateBucket(acct, nowNS)
			b.overrideRPS.Store(overrideRPS)
			b.overrideUntilNS.Store(untilNS)
			b.hasRiskState.Store(true)
			hl.recomputeEffectiveQuota(b, nowNS)
			markDirty(b)
		}
	case hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_ENABLE_VOLATILITY_GUARD:
		if sym := cmd.GetTargetSymbol(); sym != "" {
			hl.regimeMu.Lock()
			hl.symbolRegime[sym] = hyperionv1.MarketRegime_MARKET_REGIME_VOLATILITY_GUARD
			hl.regimeMu.Unlock()
		}
	case hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_RESTORE_NORMAL:
		for _, acct := range cmd.GetTargetAccountIds() {
			b := hl.getOrCreateBucket(acct, nowNS)
			b.frozenUntilNS.Store(0)
			b.overrideRPS.Store(0)
			b.overrideUntilNS.Store(0)
			b.penaltyLevel.Store(0)
			b.penaltyExpiresNS.Store(0)
			b.hasRiskState.Store(false)
			b.clearL2Flag.Store(true)
			hl.recomputeEffectiveQuota(b, nowNS)
			b.tokensMilli.Store(hl.defaultBurstMil)
			markDirty(b)
		}
		if sym := cmd.GetTargetSymbol(); sym != "" {
			hl.regimeMu.Lock()
			hl.symbolRegime[sym] = hyperionv1.MarketRegime_MARKET_REGIME_NORMAL
			hl.regimeMu.Unlock()
		}
	}

	hl.metrics.RiskMitigationsApplied.Add(1)
	if hl.redisScripter != nil {
		hl.SyncNow(ctx)
	}
	return true, nil
}

func (hl *HybridLimiter) syncLoop() {
	defer hl.wg.Done()
	ticker := time.NewTicker(hl.syncInterval)
	defer ticker.Stop()
	for {
		select {
		case <-hl.stopCh:
			return
		case <-ticker.C:
			ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
			hl.SyncNow(ctx)
			cancel()
		}
	}
}

// SyncNow reconciles dirty L1 buckets with global L2 Redis state via atomic Lua script.
func (hl *HybridLimiter) SyncNow(ctx context.Context) {
	if hl.redisScripter == nil {
		return
	}
	now := hl.nowFunc()
	nowMS := now.UnixMilli()
	nowNS := now.UnixNano()
	ttlMS := int64(60_000)

	for i := 0; i < numLimiterShards; i++ {
		sh := &hl.shards[i]
		sh.mu.RLock()
		snapshot := make([]*accountBucket, 0, len(sh.buckets))
		for _, b := range sh.buckets {
			if b.dirty.Load() {
				snapshot = append(snapshot, b)
			}
		}
		sh.mu.RUnlock()

		for _, b := range snapshot {
			b.dirty.Store(false)
			consumed := b.pendingConsumedMilli.Swap(0)
			frozenMS := int64(0)
			if f := b.frozenUntilNS.Load(); f > nowNS {
				frozenMS = f / 1_000_000
			}
			overrideArg := b.effectiveRPS.Load()
			if overrideArg == hl.defaultRPS && b.overrideRPS.Load() == 0 && b.penaltyLevel.Load() == 0 {
				overrideArg = 0
			}
			if b.clearL2Flag.Swap(false) {
				overrideArg = -1
				frozenMS = 0
			}

			key := fmt.Sprintf("hyperion:rl:%d", b.accountID)
			res, err := hl.luaScript.Run(ctx, hl.redisScripter, []string{key},
				nowMS, hl.defaultRPS, hl.defaultBurstMil, consumed, frozenMS, overrideArg, ttlMS).Int64Slice()
			if err != nil {
				// Restore un-synced delta so quota accounting stays exact on transient Redis hiccup.
				if consumed > 0 {
					b.pendingConsumedMilli.Add(consumed)
					b.dirty.Store(true)
				}
				continue
			}
			if len(res) >= 3 {
				globalTokensMilli := res[0]
				globalFrozenMS := res[1]
				globalEffRPS := res[2]

				if globalFrozenMS > nowMS {
					globalFrozenNS := globalFrozenMS * 1_000_000
					if globalFrozenNS > b.frozenUntilNS.Load() {
						b.frozenUntilNS.Store(globalFrozenNS)
						b.hasRiskState.Store(true)
					}
				}
				if globalEffRPS > 0 && globalEffRPS < b.effectiveRPS.Load() {
					b.effectiveRPS.Store(globalEffRPS)
				}
				// Cap local L1 tokens to global remaining quota to prevent multi-node burst amplification.
				for {
					cur := b.tokensMilli.Load()
					if cur <= globalTokensMilli || b.tokensMilli.CompareAndSwap(cur, globalTokensMilli) {
						break
					}
				}
			}
		}
	}
}

func maxInt64(a, b int64) int64 {
	if a > b {
		return a
	}
	return b
}
