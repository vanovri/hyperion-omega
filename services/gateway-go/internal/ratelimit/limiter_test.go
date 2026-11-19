package ratelimit_test

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ratelimit"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

func TestHybridLimiter_L1L2RedisLuaSync_SpamPenalty_And_RiskMitigations(t *testing.T) {
	mr := miniredis.RunT(t)
	rc := redis.NewClient(&redis.Options{Addr: mr.Addr()})
	t.Cleanup(func() { _ = rc.Close() })

	aegisPub, aegisPriv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}

	curTime := time.Unix(1_795_015_000, 0)
	clockFn := func() time.Time { return curTime }

	metrics := telemetry.NewRegistry()
	nodeA := ratelimit.NewHybridLimiter(ratelimit.Config{
		DefaultRPS:      10,
		DefaultBurst:    20,
		L2SyncInterval:  time.Hour, // manual deterministic SyncNow in test
		SpamWindow:      time.Second,
		SpamMaxEvents:   25,
		SpamPenaltyTTL:  5 * time.Second,
		AegisEd25519Pub: aegisPub,
		RedisClient:     rc,
		Metrics:         metrics,
	})
	nodeA.SetClock(clockFn)
	t.Cleanup(nodeA.Close)

	nodeB := ratelimit.NewHybridLimiter(ratelimit.Config{
		DefaultRPS:      10,
		DefaultBurst:    20,
		L2SyncInterval:  time.Hour,
		SpamWindow:      time.Second,
		SpamMaxEvents:   25,
		SpamPenaltyTTL:  5 * time.Second,
		AegisEd25519Pub: aegisPub,
		RedisClient:     rc,
		Metrics:         metrics,
	})
	nodeB.SetClock(clockFn)
	t.Cleanup(nodeB.Close)

	// 1. Node A consumes 15 of 20 global burst tokens for account 42, then syncs via Redis Lua script.
	for i := 0; i < 15; i++ {
		dec := nodeA.AllowAccount(42, "BTC-USDT", false)
		if !dec.Allowed {
			t.Fatalf("unexpected throttle at i=%d: %+v", i, dec)
		}
	}
	nodeA.SyncNow(context.Background())

	// Node B consumes 1 token (dirtying its L1 bucket) and syncs with L2 Redis: global remaining becomes 4!
	if dec := nodeB.AllowAccount(42, "BTC-USDT", false); !dec.Allowed {
		t.Fatal("expected initial Node B token allowed")
	}
	nodeB.SyncNow(context.Background())

	allowedOnB := 0
	for i := 0; i < 10; i++ {
		if nodeB.AllowAccount(42, "BTC-USDT", false).Allowed {
			allowedOnB++
		}
	}
	if allowedOnB != 4 {
		t.Fatalf("expected L2 Lua script to cap Node B remaining quota to 4 tokens, got %d", allowedOnB)
	}

	// 2. Order-Spam Auto-Penalty Escalation on account 99: > 25 mutation attempts/sec escalates to quarantine!
	var quarantined bool
	for i := 0; i < 90; i++ {
		dec := nodeA.AllowAccount(99, "BTC-USDT", true)
		if dec.Reason == "SPAM_PENALTY_QUARANTINE" {
			quarantined = true
			break
		}
	}
	if !quarantined {
		t.Fatal("expected spamming account 99 to escalate into SPAM_PENALTY_QUARANTINE")
	}
	if metrics.SpamPenaltiesTotal.Load() < 3 {
		t.Fatalf("expected >= 3 spam penalty escalations, got %d", metrics.SpamPenaltiesTotal.Load())
	}

	// Advance clock past SpamPenaltyTTL (6s) and confirm automatic un-quarantine + token refill!
	curTime = curTime.Add(6 * time.Second)
	if dec := nodeA.AllowAccount(99, "BTC-USDT", false); !dec.Allowed {
		t.Fatalf("expected account 99 to recover after penalty TTL elapsed, got %+v", dec)
	}

	// 3. Redpanda Aegis RiskMitigationCommand (signed Ed25519): FREEZE_ACCOUNT -> propagate L1 + L2 -> RESTORE_NORMAL!
	freezeCmd := &hyperionv1.RiskMitigationCommand{
		CommandId:          "mit-freeze-777",
		IncidentId:         "inc-wash-01",
		Action:             hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT,
		TargetAccountIds:   []uint64{777},
		TargetSymbol:       "BTC-USDT",
		CooldownDurationMs: 30_000,
		IssuedTsNs:         uint64(curTime.UnixNano()),
	}
	// Unsigned must fail signature verification.
	if _, err := nodeA.ApplyRiskMitigation(context.Background(), freezeCmd); err == nil {
		t.Fatal("expected unsigned RiskMitigationCommand to be rejected")
	}
	freezeCmd.Ed25519Signature = ed25519.Sign(aegisPriv, ratelimit.CanonicalMitigationBytes(freezeCmd))

	applied, err := nodeA.ApplyRiskMitigation(context.Background(), freezeCmd)
	if err != nil || !applied {
		t.Fatalf("ApplyRiskMitigation FREEZE failed: applied=%v err=%v", applied, err)
	}
	dec := nodeA.AllowAccount(777, "BTC-USDT", true)
	if dec.Allowed || dec.Reason != "ACCOUNT_FROZEN" {
		t.Fatalf("expected ACCOUNT_FROZEN on account 777, got %+v", dec)
	}

	// Restore normal via signed command.
	restoreCmd := &hyperionv1.RiskMitigationCommand{
		CommandId:        "mit-restore-777",
		IncidentId:       "inc-wash-01",
		Action:           hyperionv1.MitigationActionType_MITIGATION_ACTION_TYPE_RESTORE_NORMAL,
		TargetAccountIds: []uint64{777},
		TargetSymbol:     "BTC-USDT",
		IssuedTsNs:       uint64(curTime.UnixNano()),
	}
	restoreCmd.Ed25519Signature = ed25519.Sign(aegisPriv, ratelimit.CanonicalMitigationBytes(restoreCmd))
	if _, err := nodeA.ApplyRiskMitigation(context.Background(), restoreCmd); err != nil {
		t.Fatalf("ApplyRiskMitigation RESTORE failed: %v", err)
	}
	if dec := nodeA.AllowAccount(777, "BTC-USDT", true); !dec.Allowed {
		t.Fatalf("expected account 777 restored to normal, got %+v", dec)
	}
}
