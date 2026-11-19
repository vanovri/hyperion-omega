package idempotency

import (
	"bytes"
	"container/list"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/redis/go-redis/v9"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

const numIdemShards = 64

// StoredResponse represents a deterministic cached HTTP mutation response.
type StoredResponse struct {
	BodySHA256  string            `json:"body_sha256"`
	StatusCode  int               `json:"status_code"`
	ContentType string            `json:"content_type"`
	Headers     map[string]string `json:"headers,omitempty"`
	Body        []byte            `json:"body"`
	ExpiresNS   int64             `json:"expires_ns"`
}

type idemEntry struct {
	compositeKey string
	bodySHA256   string
	inFlight     bool
	done         chan struct{}
	resp         StoredResponse
	elem         *list.Element
}

type idemShard struct {
	mu       sync.Mutex
	capacity int
	lru      *list.List
	items    map[string]*idemEntry
	_        [32]byte
}

// Store implements a 64-shard bounded LRU + singleflight in-flight coalescer + optional Redis L2 store.
type Store struct {
	shards       [numIdemShards]idemShard
	ttl          time.Duration
	ttlNS        int64
	redisClient  redis.Cmdable
	redisTimeout time.Duration
	metrics      *telemetry.Registry
}

// NewStore creates a two-tier Idempotency-Key store.
func NewStore(maxEntries int, ttl time.Duration, redisClient redis.Cmdable, metrics *telemetry.Registry) *Store {
	if maxEntries < numIdemShards {
		maxEntries = numIdemShards * 64
	}
	if ttl <= 0 {
		ttl = 5 * time.Minute
	}
	if metrics == nil {
		metrics = telemetry.Global
	}
	perShard := maxEntries / numIdemShards
	if perShard < 8 {
		perShard = 8
	}
	s := &Store{
		ttl:          ttl,
		ttlNS:        ttl.Nanoseconds(),
		redisClient:  redisClient,
		redisTimeout: 35 * time.Millisecond,
		metrics:      metrics,
	}
	for i := 0; i < numIdemShards; i++ {
		s.shards[i].capacity = perShard
		s.shards[i].lru = list.New()
		s.shards[i].items = make(map[string]*idemEntry, perShard)
	}
	return s
}

func fnvShard(s string) uint64 {
	var h uint64 = 14695981039346656037
	for i := 0; i < len(s); i++ {
		h ^= uint64(s[i])
		h *= 1099511628211
	}
	return h & (numIdemShards - 1)
}

func hashBytesHex(b []byte) string {
	sum := sha256.Sum256(b)
	var out [64]byte
	hex.Encode(out[:], sum[:])
	return string(out[:])
}

type responseRecorder struct {
	http.ResponseWriter
	status int
	buf    bytes.Buffer
}

func (rr *responseRecorder) WriteHeader(code int) {
	rr.status = code
}

func (rr *responseRecorder) Write(p []byte) (int, error) {
	if rr.status == 0 {
		rr.status = http.StatusOK
	}
	return rr.buf.Write(p)
}

// Wrap protects mutating HTTP endpoints against duplicate network deliveries.
// Only mutating HTTP methods (POST, PUT, PATCH, DELETE) are intercepted.
func (s *Store) Wrap(next http.Handler, requireHeader bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodGet || r.Method == http.MethodHead || r.Method == http.MethodOptions {
			next.ServeHTTP(w, r)
			return
		}

		idemKey := strings.TrimSpace(r.Header.Get("Idempotency-Key"))
		if idemKey == "" {
			idemKey = strings.TrimSpace(r.Header.Get("X-Idempotency-Key"))
		}
		if idemKey == "" {
			if requireHeader {
				writeIdemError(w, r, http.StatusBadRequest, "MISSING_IDEMPOTENCY_KEY", "Idempotency-Key header (8..128 chars) is required")
				return
			}
			next.ServeHTTP(w, r)
			return
		}
		if len(idemKey) < 8 || len(idemKey) > 128 {
			writeIdemError(w, r, http.StatusBadRequest, "INVALID_IDEMPOTENCY_KEY", "Idempotency-Key length must be in [8, 128]")
			return
		}

		rawBody, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		if err != nil {
			writeIdemError(w, r, http.StatusBadRequest, "BODY_READ_ERROR", "failed to read request payload")
			return
		}
		r.Body = io.NopCloser(bytes.NewReader(rawBody))
		bodyHash := hashBytesHex(rawBody)

		acctScope := strings.TrimSpace(r.Header.Get("X-Hyperion-Account-Id"))
		if acctScope == "" {
			acctScope = strings.TrimSpace(r.Header.Get("Authorization"))
		}
		compKey := acctScope + "|" + r.Method + "|" + r.URL.Path + "|" + idemKey
		nowNS := time.Now().UnixNano()

		sh := &s.shards[fnvShard(compKey)]
		sh.mu.Lock()
		if existing, ok := sh.items[compKey]; ok {
			if !existing.inFlight && existing.resp.ExpiresNS <= nowNS {
				sh.lru.Remove(existing.elem)
				delete(sh.items, compKey)
			} else {
				if existing.bodySHA256 != bodyHash {
					sh.mu.Unlock()
					s.metrics.IdempotencyConflictsTotal.Add(1)
					writeIdemError(w, r, http.StatusConflict, "DUPLICATE_IDEMPOTENCY_KEY", "Idempotency-Key reused with different request payload")
					return
				}
				sh.lru.MoveToFront(existing.elem)
				if existing.inFlight {
					waitCh := existing.done
					sh.mu.Unlock()
					select {
					case <-waitCh:
						sh.mu.Lock()
						finalEntry, stillThere := sh.items[compKey]
						var finalResp StoredResponse
						if stillThere && !finalEntry.inFlight {
							finalResp = finalEntry.resp
						}
						sh.mu.Unlock()
						if stillThere && finalResp.StatusCode != 0 {
							s.metrics.IdempotencyHitsTotal.Add(1)
							replayResponse(w, finalResp)
							return
						}
						writeIdemError(w, r, http.StatusServiceUnavailable, "INFLIGHT_ABORTED", "concurrent primary request aborted; safe to retry")
						return
					case <-r.Context().Done():
						return
					}
				}
				cached := existing.resp
				sh.mu.Unlock()
				s.metrics.IdempotencyHitsTotal.Add(1)
				replayResponse(w, cached)
				return
			}
		}

		// Reserve L1 in-flight slot before checking L2 Redis so local goroutines coalesce cleanly.
		entry := &idemEntry{
			compositeKey: compKey,
			bodySHA256:   bodyHash,
			inFlight:     true,
			done:         make(chan struct{}),
		}
		if sh.lru.Len() >= sh.capacity {
			// Evict oldest completed entry if available.
			for back := sh.lru.Back(); back != nil; back = back.Prev() {
				cand := back.Value.(*idemEntry)
				if !cand.inFlight {
					sh.lru.Remove(back)
					delete(sh.items, cand.compositeKey)
					break
				}
			}
		}
		entry.elem = sh.lru.PushFront(entry)
		sh.items[compKey] = entry
		sh.mu.Unlock()

		// Check L2 Redis if configured (cross-node duplicate protection).
		redisKey := "hyperion:idem:" + hashBytesHex([]byte(compKey))
		if s.redisClient != nil {
			rctx, cancel := context.WithTimeout(r.Context(), s.redisTimeout)
			rawJSON, rerr := s.redisClient.Get(rctx, redisKey).Bytes()
			cancel()
			if rerr == nil && len(rawJSON) > 0 {
				var stored StoredResponse
				if json.Unmarshal(rawJSON, &stored) == nil && stored.StatusCode != 0 {
					if stored.BodySHA256 != bodyHash {
						s.abortInFlight(sh, compKey, entry)
						s.metrics.IdempotencyConflictsTotal.Add(1)
						writeIdemError(w, r, http.StatusConflict, "DUPLICATE_IDEMPOTENCY_KEY", "Idempotency-Key reused with different request payload")
						return
					}
					s.commitInFlight(sh, entry, stored)
					s.metrics.IdempotencyHitsTotal.Add(1)
					replayResponse(w, stored)
					return
				}
			}
		}

		rec := &responseRecorder{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(rec, r)

		// Do not cache transient 5xx or 429 failures so clients can safely retry with the same Idempotency-Key.
		if rec.status >= 500 || rec.status == http.StatusTooManyRequests || rec.status == http.StatusUnauthorized {
			s.abortInFlight(sh, compKey, entry)
			w.WriteHeader(rec.status)
			_, _ = w.Write(rec.buf.Bytes())
			return
		}

		ct := rec.Header().Get("Content-Type")
		if ct == "" {
			ct = "application/json; charset=utf-8"
		}
		bodyCopy := append([]byte(nil), rec.buf.Bytes()...)
		committed := StoredResponse{
			BodySHA256:  bodyHash,
			StatusCode:  rec.status,
			ContentType: ct,
			Body:        bodyCopy,
			ExpiresNS:   time.Now().UnixNano() + s.ttlNS,
		}
		s.commitInFlight(sh, entry, committed)

		if s.redisClient != nil {
			if payload, merr := json.Marshal(committed); merr == nil {
				rctx, cancel := context.WithTimeout(context.Background(), s.redisTimeout)
				_ = s.redisClient.Set(rctx, redisKey, payload, s.ttl).Err()
				cancel()
			}
		}

		w.Header().Set("Content-Type", ct)
		w.Header().Set("X-Idempotency-Replayed", "false")
		w.WriteHeader(rec.status)
		_, _ = w.Write(bodyCopy)
	})
}

func (s *Store) commitInFlight(sh *idemShard, entry *idemEntry, resp StoredResponse) {
	sh.mu.Lock()
	entry.resp = resp
	entry.inFlight = false
	close(entry.done)
	sh.mu.Unlock()
}

func (s *Store) abortInFlight(sh *idemShard, compKey string, entry *idemEntry) {
	sh.mu.Lock()
	if cur, ok := sh.items[compKey]; ok && cur == entry {
		sh.lru.Remove(entry.elem)
		delete(sh.items, compKey)
	}
	entry.inFlight = false
	close(entry.done)
	sh.mu.Unlock()
}

func replayResponse(w http.ResponseWriter, resp StoredResponse) {
	if resp.ContentType != "" {
		w.Header().Set("Content-Type", resp.ContentType)
	}
	for k, v := range resp.Headers {
		w.Header().Set(k, v)
	}
	w.Header().Set("X-Idempotency-Replayed", "true")
	w.WriteHeader(resp.StatusCode)
	_, _ = w.Write(resp.Body)
}

func writeIdemError(w http.ResponseWriter, r *http.Request, status int, code, msg string) {
	traceMeta, _ := telemetry.Global.ExtractOrMintTrace(r.Header, "gateway-go")
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_, _ = fmt.Fprintf(w, `{"code":%q,"message":%q,"reject_reason":%q,"trace_id":%q,"ts_ns":"%d"}`,
		code, msg, code, traceMeta.TraceId, traceMeta.IngestTsNs)
}
