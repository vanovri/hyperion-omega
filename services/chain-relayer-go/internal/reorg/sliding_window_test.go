package reorg

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"testing"
)

type mapFetcher map[uint64]BlockHeader

func (m mapFetcher) HeaderByNumber(_ context.Context, num uint64) (BlockHeader, error) {
	h, ok := m[num]
	if !ok {
		return BlockHeader{}, errors.New("not found")
	}
	return h, nil
}

func makeChain(startNum uint64, count int, parent [32]byte, branchTag uint64) []BlockHeader {
	out := make([]BlockHeader, count)
	prev := parent
	for i := 0; i < count; i++ {
		num := startNum + uint64(i)
		var b [48]byte
		binary.LittleEndian.PutUint64(b[0:8], num)
		binary.LittleEndian.PutUint64(b[8:16], branchTag)
		copy(b[16:48], prev[:])
		h := sha256.Sum256(b[:])
		out[i] = BlockHeader{
			Number:     num,
			Hash:       h,
			ParentHash: prev,
			Timestamp:  1_700_000_000 + num*2,
			BaseFeeWei: 1_000_000_000,
		}
		prev = h
	}
	return out
}

func TestSlidingWindow_ConfigValidationAndFinalityBoundaries(t *testing.T) {
	if _, err := NewSlidingWindow(15, 12, 6); !errors.Is(err, ErrInvalidWindowConfig) {
		t.Fatalf("non-power-of-two capacity must fail")
	}
	if _, err := NewSlidingWindow(32, 0, 0); !errors.Is(err, ErrInvalidWindowConfig) {
		t.Fatalf("zero finality must fail")
	}
	if _, err := NewSlidingWindow(32, 6, 7); !errors.Is(err, ErrInvalidWindowConfig) {
		t.Fatalf("maxReorgDepth > minFinalityBlocks must fail")
	}

	w, err := NewSlidingWindow(64, 12, 12)
	if err != nil {
		t.Fatalf("valid config failed: %v", err)
	}

	ctx := context.Background()
	if _, err := w.Ingest(ctx, BlockHeader{}, nil); !errors.Is(err, ErrZeroBlockHash) {
		t.Fatalf("zero hash must fail")
	}

	chain := makeChain(0, 15, [32]byte{}, 1)
	for i := 0; i < 12; i++ {
		det, err := w.Ingest(ctx, chain[i], nil)
		if err != nil || det.Detected {
			t.Fatalf("ingest %d failed: %v", i, err)
		}
	}

	// At tip == 11 (blocks 0..11 = 12 blocks):
	// Block 0 has confirmations = (11 - 0) + 1 = 12 -> Finalized == true!
	// Block 1 has confirmations = (11 - 1) + 1 = 11 -> Finalized == false!
	fin0, conf0 := w.IsFinalized(0, chain[0].Hash)
	if !fin0 || conf0 != 12 {
		t.Fatalf("block 0 must be finalized at 12 confirmations: fin=%v conf=%d", fin0, conf0)
	}
	fin1, conf1 := w.IsFinalized(1, chain[1].Hash)
	if fin1 || conf1 != 11 {
		t.Fatalf("block 1 must NOT be finalized at 11 confirmations: fin=%v conf=%d", fin1, conf1)
	}
}

func TestSlidingWindow_ReorgDepths1_6_12_AndExceed13(t *testing.T) {
	ctx := context.Background()
	w, err := NewSlidingWindow(64, 12, 12)
	if err != nil {
		t.Fatal(err)
	}

	base := makeChain(0, 20, [32]byte{}, 1)
	for _, b := range base {
		if _, err := w.Ingest(ctx, b, nil); err != nil {
			t.Fatal(err)
		}
	}

	// 1. Shallow 1-block reorg at block 19 (ancestor = 18)
	fork1 := makeChain(19, 2, base[18].Hash, 2) // 19', 20'
	f1 := mapFetcher{19: fork1[0], 20: fork1[1]}
	det1, err := w.Ingest(ctx, fork1[1], f1)
	if err != nil || !det1.Detected || det1.ReorgDepth != 1 || det1.CommonAncestorBlockNumber != 18 {
		t.Fatalf("1-block reorg mismatch: %+v err=%v", det1, err)
	}
	if canon, _ := w.IsCanonicalAt(19, base[19].Hash); canon {
		t.Fatalf("orphaned block 19 still reported canonical!")
	}
	if canon, _ := w.IsCanonicalAt(19, fork1[0].Hash); !canon {
		t.Fatalf("new fork block 19' must be canonical!")
	}

	// 2. Exact 12-block boundary reorg from tip 20' (ancestor = 8, depth = 20 - 8 = 12 == maxReorgDepth)
	fork12 := makeChain(9, 13, base[8].Hash, 3) // 9''..21''
	f12 := make(mapFetcher)
	for _, b := range fork12 {
		f12[b.Number] = b
	}
	det12, err := w.Ingest(ctx, fork12[len(fork12)-1], f12)
	if err != nil || !det12.Detected || det12.ReorgDepth != 12 || det12.CommonAncestorBlockNumber != 8 {
		t.Fatalf("12-block boundary reorg failed: %+v err=%v", det12, err)
	}

	// 3. 13-block reorg (depth = 21 - 8 = 13 > maxReorgDepth = 12) -> MUST reject!
	fork13 := makeChain(9, 14, base[8].Hash, 4) // 9'''..22''' from ancestor 8 while tip is 21'' (21 - 8 = 13)
	f13 := make(mapFetcher)
	for _, b := range fork13 {
		f13[b.Number] = b
	}
	if _, err := w.Ingest(ctx, fork13[len(fork13)-1], f13); !errors.Is(err, ErrDeepReorgExceededMax) {
		t.Fatalf("13-block reorg exceeding maxReorgDepth=12 must return ErrDeepReorgExceededMax, got %v", err)
	}
}
