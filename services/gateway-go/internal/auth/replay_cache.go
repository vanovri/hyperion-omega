package auth

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/redis/go-redis/v9"
)

const (
	numReplayShards   = 64
	bloomWordsPerGen  = 64 // 64 * 64 = 4,096 bits per generation per shard (262,144 bits total across 64 shards)
	defaultRingPerShd = 2048
)

type nonceKey struct {
	accountID uint64
	nonce     uint64
}

type ringSlot struct {
	key       nonceKey
	expiresNS int64
	occupied  bool
}

// replayShard combines a 2-generation sliding-window Bloom filter (fast negative filter)
// with a bounded pre-allocated Ring Buffer + exact map (zero false-positive rejections,
// deterministic O(1) eviction without full-map GC scans).
// Cache-line padding prevents false sharing across CPU cores under 5,000+ goroutines.
type replayShard struct {
	mu           sync.Mutex
	currBloom    [bloomWordsPerGen]uint64
	prevBloom    [bloomWordsPerGen]uint64
	bloomGenID   int64
	ring         []ringSlot
	exactExpires map[nonceKey]int64
	head         int
	_            [40]byte
}

// NonceKV abstracts Redis SET NX PX for L2 cluster-wide anti-replay enforcement.
type NonceKV interface {
	SetNX(ctx context.Context, key string, value interface{}, expiration time.Duration) *redis.BoolCmd
}

// ReplayGuard enforces strict timestamp window + two-tier (L1 sharded Bloom+Ring, L2 Redis)
// nonce anti-replay protection.
type ReplayGuard struct {
	shards       [numReplayShards]replayShard
	ttlNS        int64
	ttl          time.Duration
	genSpanNS    int64
	redisClient  NonceKV
	redisTimeout time.Duration
}

// NewReplayGuard constructs a 64-shard L1 Bloom+Ring anti-replay cache with optional L2 Redis backing.
func NewReplayGuard(ttl time.Duration, ringPerShard int, redisClient NonceKV) *ReplayGuard {
	if ttl <= 0 {
		ttl = 15 * time.Second
	}
	if ringPerShard <= 0 {
		ringPerShard = defaultRingPerShd
	}
	genSpan := ttl.Nanoseconds() / 2
	if genSpan <= 0 {
		genSpan = int64(time.Second)
	}
	rg := &ReplayGuard{
		ttlNS:        ttl.Nanoseconds(),
		ttl:          ttl,
		genSpanNS:    genSpan,
		redisClient:  redisClient,
		redisTimeout: 25 * time.Millisecond,
	}
	for i := 0; i < numReplayShards; i++ {
		rg.shards[i].ring = make([]ringSlot, ringPerShard)
		rg.shards[i].exactExpires = make(map[nonceKey]int64, ringPerShard)
	}
	return rg
}

// mixHash64 computes SplitMix64 over (accountID, nonce) for shard routing & Kirsch-Mitzenmacher Bloom probes.
func mixHash64(accountID, nonce uint64) (h1, h2 uint64) {
	z := accountID*0x9e3779b97f4a7c15 + nonce
	z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
	z = (z ^ (z >> 27)) * 0x94d049bb133111eb
	h1 = z ^ (z >> 31)

	w := (nonce * 0x94d049bb133111eb) ^ (accountID + 0x517cc1b727220a95)
	w = (w ^ (w >> 30)) * 0xbf58476d1ce4e5b9
	w = (w ^ (w >> 27)) * 0x94d049bb133111eb
	h2 = (w ^ (w >> 31)) | 1
	return h1, h2
}

func bloomTest(bits *[bloomWordsPerGen]uint64, h1, h2 uint64) bool {
	const mask = uint64(bloomWordsPerGen*64 - 1)
	for i := uint64(0); i < 4; i++ {
		bitIdx := (h1 + i*h2) & mask
		if (bits[bitIdx>>6] & (uint64(1) << (bitIdx & 63))) == 0 {
			return false
		}
	}
	return true
}

func bloomAdd(bits *[bloomWordsPerGen]uint64, h1, h2 uint64) {
	const mask = uint64(bloomWordsPerGen*64 - 1)
	for i := uint64(0); i < 4; i++ {
		bitIdx := (h1 + i*h2) & mask
		bits[bitIdx>>6] |= uint64(1) << (bitIdx & 63)
	}
}

// CheckAndRecord atomically verifies that (accountID, nonce) has not been used within TTL
// on either L1 (sharded Bloom + Ring) or L2 (Redis SET NX PX), then records it.
// Returns false if the nonce is a replay.
func (rg *ReplayGuard) CheckAndRecord(ctx context.Context, accountID, nonce uint64, nowNS int64) bool {
	h1, h2 := mixHash64(accountID, nonce)
	shardIdx := h1 & (numReplayShards - 1)
	sh := &rg.shards[shardIdx]
	nk := nonceKey{accountID: accountID, nonce: nonce}
	expNS := nowNS + rg.ttlNS
	genID := nowNS / rg.genSpanNS

	sh.mu.Lock()
	if sh.bloomGenID != genID {
		if genID == sh.bloomGenID+1 {
			sh.prevBloom = sh.currBloom
		} else {
			sh.prevBloom = [bloomWordsPerGen]uint64{}
		}
		sh.currBloom = [bloomWordsPerGen]uint64{}
		sh.bloomGenID = genID
	}

	// Fast path via 2-generation sliding Bloom filter: only inspect exact map on positive Bloom hit.
	if bloomTest(&sh.currBloom, h1, h2) || bloomTest(&sh.prevBloom, h1, h2) {
		if existingExp, exists := sh.exactExpires[nk]; exists && existingExp > nowNS {
			sh.mu.Unlock()
			return false
		}
	}

	// Evict overwritten ring slot deterministically in O(1).
	slot := &sh.ring[sh.head]
	if slot.occupied {
		if curExp, ok := sh.exactExpires[slot.key]; ok && curExp == slot.expiresNS {
			delete(sh.exactExpires, slot.key)
		}
	}
	*slot = ringSlot{
		key:       nk,
		expiresNS: expNS,
		occupied:  true,
	}
	sh.head++
	if sh.head >= len(sh.ring) {
		sh.head = 0
	}
	sh.exactExpires[nk] = expNS
	bloomAdd(&sh.currBloom, h1, h2)
	sh.mu.Unlock()

	// L2 cluster-wide Redis SET NX PX check (gracefully degrades to L1 if Redis is offline).
	if rg.redisClient != nil {
		rctx, cancel := context.WithTimeout(ctx, rg.redisTimeout)
		key := fmt.Sprintf("hyperion:nonce:%d:%d", accountID, nonce)
		ok, err := rg.redisClient.SetNX(rctx, key, "1", rg.ttl).Result()
		cancel()
		if err == nil && !ok {
			return false
		}
	}

	return true
}
