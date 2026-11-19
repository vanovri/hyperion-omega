package auth

import (
	"crypto/ed25519"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"
)

const (
	HeaderAccountID  = "X-Hyperion-Account-Id"
	HeaderKeyID      = "X-Hyperion-Key-Id"
	HeaderTimestamp  = "X-Hyperion-Timestamp"
	HeaderNonce      = "X-Hyperion-Nonce"
	HeaderSignature  = "X-Hyperion-Signature"
	HeaderAuthScheme = "X-Hyperion-Auth-Scheme"
)

var (
	ErrMissingSigHeaders = errors.New("missing required X-Hyperion-* authentication headers")
	ErrClockSkewExceeded = errors.New("request timestamp outside allowed 5s window")
	ErrUnknownOrRevoked  = errors.New("unknown, revoked, or mismatched API key")
	ErrSignatureMismatch = errors.New("cryptographic signature verification failed")
	ErrNonceReplayed     = errors.New("duplicate or replayed nonce rejected")
)

// SignedIdentity captures the cryptographically authenticated trading request metadata.
type SignedIdentity struct {
	AccountID   uint64
	KeyID       string
	Scheme      Scheme
	TimestampNS uint64
	Nonce       uint64
	Signature   []byte
	BodySHA256  string
}

// SignatureAuthenticator verifies Ed25519 and HMAC-SHA256 signed requests per
// .agents/rules/00-hyperion-contract.md §6.2:
//
//	<X-Hyperion-Timestamp>\n<X-Hyperion-Nonce>\n<HTTP_METHOD_UPPERCASE>\n<REQUEST_PATH>\n<SHA256_HEX_LOWERCASE(RAW_BODY)>
type SignatureAuthenticator struct {
	keyStore     *KeyStore
	replayGuard  *ReplayGuard
	maxSkewNS    int64
	nowFunc      func() time.Time
}

// NewSignatureAuthenticator constructs a low-latency cryptographic request verifier.
func NewSignatureAuthenticator(ks *KeyStore, rg *ReplayGuard, maxClockSkew time.Duration) *SignatureAuthenticator {
	if maxClockSkew <= 0 {
		maxClockSkew = 5 * time.Second
	}
	return &SignatureAuthenticator{
		keyStore:    ks,
		replayGuard: rg,
		maxSkewNS:   maxClockSkew.Nanoseconds(),
		nowFunc:     time.Now,
	}
}

// SetClock overrides the wall clock for deterministic unit testing.
func (sa *SignatureAuthenticator) SetClock(fn func() time.Time) {
	if fn != nil {
		sa.nowFunc = fn
	}
}

// BuildCanonicalPreimage constructs the exact UTF-8 preimage specified in 00-hyperion-contract.md §6.2.
func BuildCanonicalPreimage(tsHeader, nonceHeader, method, requestPath, bodySHA256Hex string) []byte {
	upperMethod := strings.ToUpper(method)
	n := len(tsHeader) + 1 + len(nonceHeader) + 1 + len(upperMethod) + 1 + len(requestPath) + 1 + len(bodySHA256Hex)
	buf := make([]byte, 0, n)
	buf = append(buf, tsHeader...)
	buf = append(buf, '\n')
	buf = append(buf, nonceHeader...)
	buf = append(buf, '\n')
	buf = append(buf, upperMethod...)
	buf = append(buf, '\n')
	buf = append(buf, requestPath...)
	buf = append(buf, '\n')
	buf = append(buf, bodySHA256Hex...)
	return buf
}

// HashBodyHex computes lowercase SHA-256 hex digest of rawBody.
func HashBodyHex(rawBody []byte) string {
	sum := sha256.Sum256(rawBody)
	var hexBuf [64]byte
	hex.Encode(hexBuf[:], sum[:])
	return string(hexBuf[:])
}

// SignEd25519 signs canonical preimage using Ed25519 private key and returns lowercase hex signature.
func SignEd25519(priv ed25519.PrivateKey, tsHeader, nonceHeader, method, path string, rawBody []byte) string {
	preimage := BuildCanonicalPreimage(tsHeader, nonceHeader, method, path, HashBodyHex(rawBody))
	sig := ed25519.Sign(priv, preimage)
	return hex.EncodeToString(sig)
}

// SignHMACSHA256 signs canonical preimage using HMAC-SHA256 and returns lowercase hex MAC.
func SignHMACSHA256(secret []byte, tsHeader, nonceHeader, method, path string, rawBody []byte) string {
	preimage := BuildCanonicalPreimage(tsHeader, nonceHeader, method, path, HashBodyHex(rawBody))
	mac := hmac.New(sha256.New, secret)
	_, _ = mac.Write(preimage)
	return hex.EncodeToString(mac.Sum(nil))
}

func parseNonce(raw string) (uint64, error) {
	trimmed := strings.TrimSpace(raw)
	if strings.HasPrefix(trimmed, "0x") || strings.HasPrefix(trimmed, "0X") {
		return strconv.ParseUint(trimmed[2:], 16, 64)
	}
	if v, err := strconv.ParseUint(trimmed, 10, 64); err == nil {
		return v, nil
	}
	return strconv.ParseUint(trimmed, 16, 64)
}

// Authenticate validates headers, clock skew, Ed25519/HMAC-SHA256 signature, and L1+L2 nonce uniqueness.
// Cryptographic signature is verified BEFORE inserting into ReplayGuard so forged unauthenticated
// packets cannot poison or exhaust legitimate account nonce state.
func (sa *SignatureAuthenticator) Authenticate(r *http.Request, rawBody []byte) (SignedIdentity, error) {
	acctRaw := strings.TrimSpace(r.Header.Get(HeaderAccountID))
	keyID := strings.TrimSpace(r.Header.Get(HeaderKeyID))
	tsRaw := strings.TrimSpace(r.Header.Get(HeaderTimestamp))
	nonceRaw := strings.TrimSpace(r.Header.Get(HeaderNonce))
	sigHex := strings.TrimSpace(r.Header.Get(HeaderSignature))
	schemeRaw := Scheme(strings.ToLower(strings.TrimSpace(r.Header.Get(HeaderAuthScheme))))

	if keyID == "" {
		keyID = "default"
	}
	if acctRaw == "" || tsRaw == "" || nonceRaw == "" || sigHex == "" {
		return SignedIdentity{}, ErrMissingSigHeaders
	}

	accountID, err := strconv.ParseUint(acctRaw, 10, 64)
	if err != nil || accountID == 0 {
		return SignedIdentity{}, fmt.Errorf("%w: invalid account id", ErrMissingSigHeaders)
	}
	tsNS, err := strconv.ParseUint(tsRaw, 10, 64)
	if err != nil {
		return SignedIdentity{}, fmt.Errorf("%w: invalid timestamp", ErrMissingSigHeaders)
	}
	// Accept millisecond timestamps gracefully if a client supplies 13-digit ms epoch,
	// otherwise enforce nanosecond epoch per 00-hyperion-contract.md §6.2.
	tsCompareNS := int64(tsNS)
	if tsNS > 1_000_000_000_000 && tsNS < 10_000_000_000_000 {
		tsCompareNS = int64(tsNS) * 1_000_000
	}

	nowNS := sa.nowFunc().UnixNano()
	diff := nowNS - tsCompareNS
	if diff < 0 {
		diff = -diff
	}
	if diff > sa.maxSkewNS {
		return SignedIdentity{}, ErrClockSkewExceeded
	}

	nonceVal, err := parseNonce(nonceRaw)
	if err != nil {
		return SignedIdentity{}, fmt.Errorf("%w: invalid nonce", ErrMissingSigHeaders)
	}

	sigBytes, err := hex.DecodeString(sigHex)
	if err != nil {
		return SignedIdentity{}, ErrSignatureMismatch
	}

	keyRec, ok := sa.keyStore.Resolve(accountID, keyID, schemeRaw)
	if !ok {
		return SignedIdentity{}, ErrUnknownOrRevoked
	}

	bodyHash := HashBodyHex(rawBody)
	validSig := verifyPreimage(keyRec, sigBytes, tsRaw, nonceRaw, r.Method, r.URL.Path, bodyHash)
	if !validSig && r.URL.RawQuery != "" {
		// Interop fallback if client signed RequestURI including query parameters (e.g., DELETE ?symbol=BTC-USDT).
		validSig = verifyPreimage(keyRec, sigBytes, tsRaw, nonceRaw, r.Method, r.URL.RequestURI(), bodyHash)
	}
	if !validSig {
		return SignedIdentity{}, ErrSignatureMismatch
	}

	if sa.replayGuard != nil && !sa.replayGuard.CheckAndRecord(r.Context(), accountID, nonceVal, nowNS) {
		return SignedIdentity{}, ErrNonceReplayed
	}

	return SignedIdentity{
		AccountID:   accountID,
		KeyID:       keyID,
		Scheme:      keyRec.Scheme,
		TimestampNS: uint64(tsCompareNS),
		Nonce:       nonceVal,
		Signature:   sigBytes,
		BodySHA256:  bodyHash,
	}, nil
}

func verifyPreimage(rec KeyRecord, sigBytes []byte, tsRaw, nonceRaw, method, path, bodyHash string) bool {
	preimage := BuildCanonicalPreimage(tsRaw, nonceRaw, method, path, bodyHash)
	switch rec.Scheme {
	case SchemeEd25519:
		if len(sigBytes) != ed25519.SignatureSize || len(rec.Ed25519Pub) != ed25519.PublicKeySize {
			return false
		}
		return ed25519.Verify(rec.Ed25519Pub, preimage, sigBytes)
	case SchemeHMACSHA256:
		if len(sigBytes) != sha256.Size || len(rec.HMACSecret) == 0 {
			return false
		}
		var macBuf [sha256.Size]byte
		mac := hmac.New(sha256.New, rec.HMACSecret)
		_, _ = mac.Write(preimage)
		sum := mac.Sum(macBuf[:0])
		return hmac.Equal(sigBytes, sum)
	default:
		return false
	}
}
