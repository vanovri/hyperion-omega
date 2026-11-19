package nonce

import (
	"errors"
	"fmt"
	"sync"
	"testing"
)

func TestNonceManager_GapHealingIdempotencyAndConcurrency(t *testing.T) {
	m := NewManager(10)

	if _, _, err := m.AcquireOrReuse(""); !errors.Is(err, ErrEmptyTxID) {
		t.Fatalf("empty txID must fail")
	}

	n0, reused0, err := m.AcquireOrReuse("tx-a")
	if err != nil || reused0 || n0 != 10 {
		t.Fatalf("expected 10, got %d reused=%v err=%v", n0, reused0, err)
	}
	// Idempotent reuse on fee bump
	n0Again, reusedAgain, err := m.AcquireOrReuse("tx-a")
	if err != nil || !reusedAgain || n0Again != 10 {
		t.Fatalf("expected idempotent reuse of 10, got %d reused=%v err=%v", n0Again, reusedAgain, err)
	}

	n1, _, _ := m.AcquireOrReuse("tx-b") // 11
	n2, _, _ := m.AcquireOrReuse("tx-c") // 12
	if n1 != 11 || n2 != 12 {
		t.Fatalf("expected 11, 12 got %d, %d", n1, n2)
	}

	// Abort middle nonce 11 -> next tx-d must heal gap 11 immediately!
	if err := m.AbortPreBroadcast("tx-b", 11); err != nil {
		t.Fatalf("abort failed: %v", err)
	}
	nHealed, _, err := m.AcquireOrReuse("tx-d")
	if err != nil || nHealed != 11 {
		t.Fatalf("expected healed gap 11, got %d err=%v", nHealed, err)
	}

	// Finalize tx-a and verify double-acquire after finalize returns ErrAlreadyFinalized
	if err := m.MarkFinalized("tx-a"); err != nil {
		t.Fatalf("finalize failed: %v", err)
	}
	if _, _, err := m.AcquireOrReuse("tx-a"); !errors.Is(err, ErrAlreadyFinalized) {
		t.Fatalf("re-acquiring finalized tx must return ErrAlreadyFinalized, got %v", err)
	}

	// ReopenAfterReorg restores exact nonce
	m.ReopenAfterReorg("tx-a", 10)
	nReopened, reusedReop, err := m.AcquireOrReuse("tx-a")
	if err != nil || !reusedReop || nReopened != 10 {
		t.Fatalf("reopen after reorg failed: %d %v %v", nReopened, reusedReop, err)
	}

	// High-concurrency zero-collision stress (64 goroutines x 50 leases = 3,200 unique nonces)
	mc := NewManager(0)
	const workers = 64
	const perWorker = 50
	seen := make([]uint64, workers*perWorker)
	var wg sync.WaitGroup
	for w := 0; w < workers; w++ {
		w := w
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < perWorker; j++ {
				id := fmt.Sprintf("w-%d-%d", w, j)
				nn, _, err := mc.AcquireOrReuse(id)
				if err != nil {
					t.Errorf("concurrent acquire failed: %v", err)
					return
				}
				seen[w*perWorker+j] = nn
			}
		}()
	}
	wg.Wait()

	uniq := make(map[uint64]struct{}, workers*perWorker)
	for _, v := range seen {
		if _, dup := uniq[v]; dup {
			t.Fatalf("COLLISION DETECTED on nonce %d!", v)
		}
		uniq[v] = struct{}{}
	}
	if uint64(len(uniq)) != workers*perWorker {
		t.Fatalf("expected %d unique nonces, got %d", workers*perWorker, len(uniq))
	}
}
