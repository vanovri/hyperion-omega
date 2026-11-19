package auth

import (
	"context"
	"crypto/ed25519"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"
)

var (
	ErrMissingAuthToken = errors.New("missing bearer JWT or session cookie")
	ErrInvalidJWTFormat = errors.New("malformed JWT structure")
	ErrUnsupportedAlg   = errors.New("unsupported or disallowed JWT alg")
	ErrInvalidJWTSig    = errors.New("invalid JWT cryptographic signature")
	ErrJWTExpired       = errors.New("JWT token expired or not yet valid")
)

// Claims represents authenticated UI/API session identity extracted from JWT.
type Claims struct {
	Subject   string   `json:"sub"`
	AccountID uint64   `json:"account_id,omitempty"`
	Roles     []string `json:"roles,omitempty"`
	JWTID     string   `json:"jti,omitempty"`
	IssuedAt  int64    `json:"iat"`
	NotBefore int64    `json:"nbf,omitempty"`
	ExpiresAt int64    `json:"exp"`
}

type jwtHeader struct {
	Alg string `json:"alg"`
	Typ string `json:"typ"`
}

// JWTVerifier validates HS256 and EdDSA JWT tokens as well as session cookies.
type JWTVerifier struct {
	hmacSecret    []byte
	ed25519Pub    ed25519.PublicKey
	clockSkewSecs int64
}

// NewJWTVerifier constructs a dual-algorithm (HS256 + EdDSA) JWT verifier.
func NewJWTVerifier(hmacSecret []byte, ed25519Pub ed25519.PublicKey, skew time.Duration) *JWTVerifier {
	var secCopy []byte
	if len(hmacSecret) > 0 {
		secCopy = append([]byte(nil), hmacSecret...)
	}
	var pubCopy ed25519.PublicKey
	if len(ed25519Pub) == ed25519.PublicKeySize {
		pubCopy = append(ed25519.PublicKey(nil), ed25519Pub...)
	}
	skewSecs := int64(skew.Seconds())
	if skewSecs <= 0 {
		skewSecs = 5
	}
	return &JWTVerifier{
		hmacSecret:    secCopy,
		ed25519Pub:    pubCopy,
		clockSkewSecs: skewSecs,
	}
}

// MintHS256 issues a signed HS256 JWT token.
func MintHS256(secret []byte, claims Claims) (string, error) {
	if len(secret) == 0 {
		return "", errors.New("empty HS256 secret")
	}
	hdrJSON, _ := json.Marshal(jwtHeader{Alg: "HS256", Typ: "JWT"})
	payJSON, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	signingInput := base64.RawURLEncoding.EncodeToString(hdrJSON) + "." + base64.RawURLEncoding.EncodeToString(payJSON)
	mac := hmac.New(sha256.New, secret)
	_, _ = mac.Write([]byte(signingInput))
	sig := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	return signingInput + "." + sig, nil
}

// MintEdDSA issues a signed Ed25519 (EdDSA) JWT token.
func MintEdDSA(priv ed25519.PrivateKey, claims Claims) (string, error) {
	if len(priv) != ed25519.PrivateKeySize {
		return "", errors.New("invalid Ed25519 private key")
	}
	hdrJSON, _ := json.Marshal(jwtHeader{Alg: "EdDSA", Typ: "JWT"})
	payJSON, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	signingInput := base64.RawURLEncoding.EncodeToString(hdrJSON) + "." + base64.RawURLEncoding.EncodeToString(payJSON)
	sig := ed25519.Sign(priv, []byte(signingInput))
	return signingInput + "." + base64.RawURLEncoding.EncodeToString(sig), nil
}

// Verify parses and cryptographically verifies a JWT string at time now.
func (v *JWTVerifier) Verify(token string, now time.Time) (Claims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return Claims{}, ErrInvalidJWTFormat
	}
	hdrBytes, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return Claims{}, ErrInvalidJWTFormat
	}
	var hdr jwtHeader
	if err := json.Unmarshal(hdrBytes, &hdr); err != nil {
		return Claims{}, ErrInvalidJWTFormat
	}

	signingInput := parts[0] + "." + parts[1]
	sigBytes, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return Claims{}, ErrInvalidJWTFormat
	}

	switch hdr.Alg {
	case "HS256":
		if len(v.hmacSecret) == 0 {
			return Claims{}, ErrUnsupportedAlg
		}
		mac := hmac.New(sha256.New, v.hmacSecret)
		_, _ = mac.Write([]byte(signingInput))
		if !hmac.Equal(sigBytes, mac.Sum(nil)) {
			return Claims{}, ErrInvalidJWTSig
		}
	case "EdDSA":
		if len(v.ed25519Pub) != ed25519.PublicKeySize || len(sigBytes) != ed25519.SignatureSize {
			return Claims{}, ErrUnsupportedAlg
		}
		if !ed25519.Verify(v.ed25519Pub, []byte(signingInput), sigBytes) {
			return Claims{}, ErrInvalidJWTSig
		}
	default:
		return Claims{}, fmt.Errorf("%w: %q", ErrUnsupportedAlg, hdr.Alg)
	}

	payBytes, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return Claims{}, ErrInvalidJWTFormat
	}
	var claims Claims
	if err := json.Unmarshal(payBytes, &claims); err != nil {
		return Claims{}, ErrInvalidJWTFormat
	}

	nowSec := now.Unix()
	if claims.ExpiresAt == 0 || nowSec > claims.ExpiresAt+v.clockSkewSecs {
		return Claims{}, ErrJWTExpired
	}
	if claims.NotBefore > 0 && nowSec+v.clockSkewSecs < claims.NotBefore {
		return Claims{}, ErrJWTExpired
	}
	return claims, nil
}

// ExtractRequestClaims inspects Authorization: Bearer <JWT> or Cookie: hyperion_session=<JWT>.
func (v *JWTVerifier) ExtractRequestClaims(r *http.Request, now time.Time) (Claims, error) {
	var raw string
	if authz := strings.TrimSpace(r.Header.Get("Authorization")); authz != "" {
		if len(authz) > 7 && strings.EqualFold(authz[:7], "Bearer ") {
			raw = strings.TrimSpace(authz[7:])
		}
	}
	if raw == "" {
		if c, err := r.Cookie("hyperion_session"); err == nil {
			raw = strings.TrimSpace(c.Value)
		}
	}
	if raw == "" {
		return Claims{}, ErrMissingAuthToken
	}
	return v.Verify(raw, now)
}

type ctxKey int

const (
	claimsCtxKey ctxKey = iota + 1
	signedAcctCtxKey
)

// WithClaims attaches verified JWT Claims to request context.
func WithClaims(ctx context.Context, c Claims) context.Context {
	return context.WithValue(ctx, claimsCtxKey, c)
}

// ClaimsFromContext retrieves verified JWT Claims from context.
func ClaimsFromContext(ctx context.Context) (Claims, bool) {
	c, ok := ctx.Value(claimsCtxKey).(Claims)
	return c, ok
}

// WithSignedIdentity attaches verified cryptographic trading identity to context.
func WithSignedIdentity(ctx context.Context, id SignedIdentity) context.Context {
	return context.WithValue(ctx, signedAcctCtxKey, id)
}

// SignedIdentityFromContext retrieves verified trading identity from context.
func SignedIdentityFromContext(ctx context.Context) (SignedIdentity, bool) {
	id, ok := ctx.Value(signedAcctCtxKey).(SignedIdentity)
	return id, ok
}
