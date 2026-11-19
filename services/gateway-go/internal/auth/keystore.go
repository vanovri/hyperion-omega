package auth

import (
	"crypto/ed25519"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
	"sync"
)

// Scheme identifies the cryptographic request-signing algorithm.
type Scheme string

const (
	SchemeEd25519    Scheme = "ed25519"
	SchemeHMACSHA256 Scheme = "hmac-sha256"
)

// KeyRecord holds verified key material bound to a specific trading account.
type KeyRecord struct {
	AccountID  uint64
	KeyID      string
	Scheme     Scheme
	Ed25519Pub ed25519.PublicKey
	HMACSecret []byte
	Active     bool
}

// KeyStore manages per-account Ed25519 public keys and HMAC-SHA256 secrets.
// Read-heavy lookups use RWMutex; bootstrap HMAC fallback enables frictionless
// Chaos-Sim / E2E testing when deterministic dev keys are configured.
type KeyStore struct {
	mu                  sync.RWMutex
	keys                map[string]KeyRecord // composite key "<account_id>:<key_id>"
	bootstrapHMACSecret []byte
}

// NewKeyStore creates a KeyStore with an optional bootstrap HMAC secret for dev/sim accounts.
func NewKeyStore(bootstrapHMACSecret []byte) *KeyStore {
	var secretCopy []byte
	if len(bootstrapHMACSecret) > 0 {
		secretCopy = append([]byte(nil), bootstrapHMACSecret...)
	}
	return &KeyStore{
		keys:                make(map[string]KeyRecord, 256),
		bootstrapHMACSecret: secretCopy,
	}
}

func compositeKey(accountID uint64, keyID string) string {
	return fmt.Sprintf("%d:%s", accountID, keyID)
}

// RegisterEd25519 registers an Ed25519 public key (32 bytes) for an account.
func (ks *KeyStore) RegisterEd25519(accountID uint64, keyID string, pub ed25519.PublicKey) error {
	if len(pub) != ed25519.PublicKeySize {
		return fmt.Errorf("invalid ed25519 public key length: %d", len(pub))
	}
	pubCopy := append(ed25519.PublicKey(nil), pub...)
	ks.mu.Lock()
	ks.keys[compositeKey(accountID, keyID)] = KeyRecord{
		AccountID:  accountID,
		KeyID:      keyID,
		Scheme:     SchemeEd25519,
		Ed25519Pub: pubCopy,
		Active:     true,
	}
	ks.mu.Unlock()
	return nil
}

// RegisterEd25519Hex registers a hex-encoded 32-byte Ed25519 public key.
func (ks *KeyStore) RegisterEd25519Hex(accountID uint64, keyID, pubHex string) error {
	raw, err := hex.DecodeString(strings.TrimSpace(pubHex))
	if err != nil {
		return fmt.Errorf("decode ed25519 hex: %w", err)
	}
	return ks.RegisterEd25519(accountID, keyID, ed25519.PublicKey(raw))
}

// RegisterHMAC registers an HMAC-SHA256 shared secret for an account.
func (ks *KeyStore) RegisterHMAC(accountID uint64, keyID string, secret []byte) error {
	if len(secret) == 0 {
		return fmt.Errorf("empty hmac secret")
	}
	secCopy := append([]byte(nil), secret...)
	ks.mu.Lock()
	ks.keys[compositeKey(accountID, keyID)] = KeyRecord{
		AccountID:  accountID,
		KeyID:      keyID,
		Scheme:     SchemeHMACSHA256,
		HMACSecret: secCopy,
		Active:     true,
	}
	ks.mu.Unlock()
	return nil
}

// Revoke disables a previously registered key immediately.
func (ks *KeyStore) Revoke(accountID uint64, keyID string) {
	ks.mu.Lock()
	k := compositeKey(accountID, keyID)
	if rec, ok := ks.keys[k]; ok {
		rec.Active = false
		ks.keys[k] = rec
	}
	ks.mu.Unlock()
}

// Resolve retrieves the KeyRecord for (accountID, keyID, requestedScheme).
// If no explicit record was registered and requestedScheme is hmac-sha256 with a non-empty
// bootstrap secret, deterministic dev HMAC credentials are supported for "default"/"dev-hmac"
// or derived via HMAC-SHA256(bootstrap, "<accountID>:<keyID>") when keyID starts with "sim-".
func (ks *KeyStore) Resolve(accountID uint64, keyID string, requestedScheme Scheme) (KeyRecord, bool) {
	ks.mu.RLock()
	rec, ok := ks.keys[compositeKey(accountID, keyID)]
	ks.mu.RUnlock()
	if ok {
		if !rec.Active {
			return KeyRecord{}, false
		}
		if requestedScheme != "" && rec.Scheme != requestedScheme {
			return KeyRecord{}, false
		}
		return rec, true
	}

	if len(ks.bootstrapHMACSecret) > 0 && (requestedScheme == SchemeHMACSHA256 || requestedScheme == "") {
		if keyID == "default" || keyID == "dev-hmac" || keyID == "bootstrap" {
			return KeyRecord{
				AccountID:  accountID,
				KeyID:      keyID,
				Scheme:     SchemeHMACSHA256,
				HMACSecret: ks.bootstrapHMACSecret,
				Active:     true,
			}, true
		}
		if strings.HasPrefix(keyID, "sim-") {
			mac := hmac.New(sha256.New, ks.bootstrapHMACSecret)
			_, _ = fmt.Fprintf(mac, "%d:%s", accountID, keyID)
			return KeyRecord{
				AccountID:  accountID,
				KeyID:      keyID,
				Scheme:     SchemeHMACSHA256,
				HMACSecret: mac.Sum(nil),
				Active:     true,
			}, true
		}
	}
	return KeyRecord{}, false
}
