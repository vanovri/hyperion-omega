package auth_test

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"errors"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/alicebob/miniredis/v2"
	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
)

func TestSignatureAuth_Ed25519AndHMAC_ReplayAndClockSkew(t *testing.T) {
	mr := miniredis.RunT(t)
	rc := redis.NewClient(&redis.Options{Addr: mr.Addr()})
	t.Cleanup(func() { _ = rc.Close() })

	ks := auth.NewKeyStore([]byte("bootstrap-dev-secret"))
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	if err := ks.RegisterEd25519(1001, "ed-key-1", pub); err != nil {
		t.Fatal(err)
	}
	hmacSecret := []byte("institutional-hmac-secret-32byte")
	if err := ks.RegisterHMAC(2002, "hmac-key-1", hmacSecret); err != nil {
		t.Fatal(err)
	}

	rgNodeA := auth.NewReplayGuard(15*time.Second, 512, rc)
	rgNodeB := auth.NewReplayGuard(15*time.Second, 512, rc)

	fixedNow := time.Unix(1_795_015_000, 0)
	authNodeA := auth.NewSignatureAuthenticator(ks, rgNodeA, 5*time.Second)
	authNodeA.SetClock(func() time.Time { return fixedNow })

	authNodeB := auth.NewSignatureAuthenticator(ks, rgNodeB, 5*time.Second)
	authNodeB.SetClock(func() time.Time { return fixedNow })

	body := []byte(`{"client_order_id":"ord-1","account_id":"1001","symbol":"BTC-USDT","side":"BUY","order_type":"LIMIT","price_fp":"6842550000000","qty_fp":"100000000"}`)
	tsStr := strconv.FormatInt(fixedNow.UnixNano(), 10)
	nonceStr := "90001"
	sigHex := auth.SignEd25519(priv, tsStr, nonceStr, http.MethodPost, "/api/v1/orders", body)

	makeReq := func(acct, keyID, scheme, ts, nonce, sig string, raw []byte) *http.Request {
		r := httptest.NewRequest(http.MethodPost, "/api/v1/orders", bytes.NewReader(raw))
		r.Header.Set(auth.HeaderAccountID, acct)
		r.Header.Set(auth.HeaderKeyID, keyID)
		r.Header.Set(auth.HeaderAuthScheme, scheme)
		r.Header.Set(auth.HeaderTimestamp, ts)
		r.Header.Set(auth.HeaderNonce, nonce)
		r.Header.Set(auth.HeaderSignature, sig)
		return r
	}

	// 1. Forged signature with unused nonce MUST fail without poisoning nonce 90001!
	badSig := auth.SignEd25519(priv, tsStr, nonceStr, http.MethodPost, "/api/v1/orders", []byte(`{"tampered":true}`))
	if _, err := authNodeA.Authenticate(makeReq("1001", "ed-key-1", "ed25519", tsStr, nonceStr, badSig, body), body); !errors.Is(err, auth.ErrSignatureMismatch) {
		t.Fatalf("expected ErrSignatureMismatch, got %v", err)
	}

	// 2. Legitimate Ed25519 request with nonce 90001 succeeds on Node A.
	id, err := authNodeA.Authenticate(makeReq("1001", "ed-key-1", "ed25519", tsStr, nonceStr, sigHex, body), body)
	if err != nil {
		t.Fatalf("valid Ed25519 auth failed: %v", err)
	}
	if id.AccountID != 1001 || id.Nonce != 90001 {
		t.Fatalf("unexpected identity: %+v", id)
	}

	// 3. Exact replay on Node A (L1 Bloom+Ring hit) MUST be rejected.
	if _, err := authNodeA.Authenticate(makeReq("1001", "ed-key-1", "ed25519", tsStr, nonceStr, sigHex, body), body); !errors.Is(err, auth.ErrNonceReplayed) {
		t.Fatalf("expected L1 ErrNonceReplayed, got %v", err)
	}

	// 4. Cross-node replay on Node B (L1 cold, L2 Redis SET NX hit) MUST ALSO be rejected!
	if _, err := authNodeB.Authenticate(makeReq("1001", "ed-key-1", "ed25519", tsStr, nonceStr, sigHex, body), body); !errors.Is(err, auth.ErrNonceReplayed) {
		t.Fatalf("expected L2 Redis ErrNonceReplayed on Node B, got %v", err)
	}

	// 5. Clock-skew > 5s (e.g., -6s stale packet) MUST be rejected.
	staleTS := strconv.FormatInt(fixedNow.Add(-6*time.Second).UnixNano(), 10)
	staleSig := auth.SignEd25519(priv, staleTS, "90002", http.MethodPost, "/api/v1/orders", body)
	if _, err := authNodeA.Authenticate(makeReq("1001", "ed-key-1", "ed25519", staleTS, "90002", staleSig, body), body); !errors.Is(err, auth.ErrClockSkewExceeded) {
		t.Fatalf("expected ErrClockSkewExceeded, got %v", err)
	}

	// 6. Valid HMAC-SHA256 request succeeds.
	hmacBody := []byte(`{"client_order_id":"ord-2","account_id":"2002","symbol":"ETH-USDT"}`)
	hmacSig := auth.SignHMACSHA256(hmacSecret, tsStr, "0x1a2b", http.MethodPost, "/api/v1/orders", hmacBody)
	if _, err := authNodeA.Authenticate(makeReq("2002", "hmac-key-1", "hmac-sha256", tsStr, "0x1a2b", hmacSig, hmacBody), hmacBody); err != nil {
		t.Fatalf("valid HMAC-SHA256 auth failed: %v", err)
	}
}

func TestReplayGuard_ZeroFalsePositivesUnderHighSaturation(t *testing.T) {
	rg := auth.NewReplayGuard(10*time.Second, 2048, nil)
	nowNS := time.Now().UnixNano()
	ctx := context.Background()

	// Insert 40,000 unique (account, nonce) pairs concurrently; zero legitimate unique nonces may be rejected.
	const workers = 16
	const perWorker = 2500
	var falsePositives atomic.Uint64
	var wg sync.WaitGroup
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(workerID int) {
			defer wg.Done()
			acct := uint64(5000 + workerID)
			for i := 1; i <= perWorker; i++ {
				if !rg.CheckAndRecord(ctx, acct, uint64(i), nowNS) {
					falsePositives.Add(1)
				}
			}
		}(w)
	}
	wg.Wait()
	if got := falsePositives.Load(); got != 0 {
		t.Fatalf("expected 0 false positives across 40,000 unique nonces, got %d", got)
	}
}

func TestJWTVerifier_HS256AndEdDSA(t *testing.T) {
	secret := []byte("jwt-hs256-super-secret")
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_795_015_000, 0)
	v := auth.NewJWTVerifier(secret, pub, 2*time.Second)

	hsTok, err := auth.MintHS256(secret, auth.Claims{
		Subject:   "trader-ui",
		AccountID: 777,
		Roles:     []string{"trader", "auditor"},
		IssuedAt:  now.Unix(),
		ExpiresAt: now.Add(5 * time.Minute).Unix(),
	})
	if err != nil {
		t.Fatal(err)
	}
	req := httptest.NewRequest(http.MethodGet, "/api/v1/fraud/alerts", nil)
	req.Header.Set("Authorization", "Bearer "+hsTok)
	claims, err := v.ExtractRequestClaims(req, now)
	if err != nil || claims.AccountID != 777 {
		t.Fatalf("HS256 verify failed: claims=%+v err=%v", claims, err)
	}

	edTok, err := auth.MintEdDSA(priv, auth.Claims{
		Subject:   "sre-operator",
		AccountID: 888,
		IssuedAt:  now.Unix(),
		ExpiresAt: now.Add(5 * time.Minute).Unix(),
	})
	if err != nil {
		t.Fatal(err)
	}
	cookieReq := httptest.NewRequest(http.MethodGet, "/api/v1/chronos/seek", nil)
	cookieReq.AddCookie(&http.Cookie{Name: "hyperion_session", Value: edTok})
	edClaims, err := v.ExtractRequestClaims(cookieReq, now)
	if err != nil || edClaims.AccountID != 888 {
		t.Fatalf("EdDSA session cookie verify failed: claims=%+v err=%v", edClaims, err)
	}

	// Expired check.
	if _, err := v.Verify(edTok, now.Add(10*time.Minute)); !errors.Is(err, auth.ErrJWTExpired) {
		t.Fatalf("expected ErrJWTExpired, got %v", err)
	}
}
