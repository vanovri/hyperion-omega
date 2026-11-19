package chaos_test

import (
	"bufio"
	"crypto/sha1"
	"encoding/base64"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
)

func TestDeterministicBatchFaultInjection(t *testing.T) {
	prof := chaos.PresetForRegime(model.RegimeBurstTsunami)
	prof.DuplicateProb = 0.50
	prof.DuplicateCopies = 3
	prof.ReorderProb = 0.50
	prof.ClockSkewProb = 0.50
	prof.CorruptSignatureProb = 0.25

	makeBatch := func() []model.OrderIntent {
		b := make([]model.OrderIntent, 64)
		for i := range b {
			b[i] = model.OrderIntent{
				ClientOrderID:  uint64(i + 1),
				IdempotencyKey: uint64(1000 + i),
				Kind:           model.ActionSubmit,
				Price:          100_00000000,
				Quantity:       1_00000000,
			}
		}
		return b
	}

	b1, b2 := makeBatch(), makeBatch()
	s1, s2 := rng.NewStream(777, 1), rng.NewStream(777, 1)
	var c1, c2 chaos.FaultCounters

	chaos.InjectBatchFaults(b1, &prof, &s1, &c1)
	chaos.InjectBatchFaults(b2, &prof, &s2, &c2)

	for i := range b1 {
		if b1[i] != b2[i] {
			t.Fatalf("non-deterministic fault injection at index %d: %+v vs %+v", i, b1[i], b2[i])
		}
	}
	st := c1.Snapshot()
	if st.DuplicatedRequests == 0 || st.ReorderedPackets == 0 || st.ClockSkewedRequests == 0 {
		t.Fatalf("expected non-zero fault counters, got %+v", st)
	}
}

func TestToxicProxyLatencyJitterAndPartitionZeroLeak(t *testing.T) {
	baseGoroutines := runtime.NumGoroutine()

	// Start simple TCP echo upstream
	upLn, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer upLn.Close()

	go func() {
		for {
			c, err := upLn.Accept()
			if err != nil {
				return
			}
			go func(conn net.Conn) {
				defer conn.Close()
				_, _ = io.Copy(conn, conn)
			}(c)
		}
	}()

	px := chaos.NewToxicProxy("127.0.0.1:0", upLn.Addr().String(), 999, chaos.ToxicConfig{
		Enabled:        true,
		BaseLatencyMs:  8.0,
		ParetoJitterMs: 2.0,
		ParetoAlpha:    1.8,
		MaxJitterMs:    25.0,
		SliceChunkSize: 8,
	})
	if err := px.Start(); err != nil {
		t.Fatal(err)
	}

	conn, err := net.DialTimeout("tcp", px.Addr(), 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	msg := []byte("HYPERION-TOXIC-PROXY-PING")
	start := time.Now()
	if _, err := conn.Write(msg); err != nil {
		t.Fatal(err)
	}
	reply := make([]byte, len(msg))
	if _, err := io.ReadFull(conn, reply); err != nil {
		t.Fatal(err)
	}
	elapsed := time.Since(start)
	if string(reply) != string(msg) {
		t.Fatalf("corrupted echo payload: %q", string(reply))
	}
	if elapsed < 8*time.Millisecond {
		t.Fatalf("expected >=8ms injected latency, got %v", elapsed)
	}

	// Activate Network Partition -> existing socket must sever immediately
	px.UpdateConfig(chaos.ToxicConfig{Enabled: true, Partitioned: true})
	_ = conn.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, err = conn.Read(reply[:1])
	if err == nil {
		t.Fatal("expected socket reset on partition activation")
	}
	_ = conn.Close()

	if err := px.Stop(); err != nil {
		t.Fatal(err)
	}
	_ = upLn.Close()

	time.Sleep(40 * time.Millisecond)
	if after := runtime.NumGoroutine(); after > baseGoroutines+4 {
		t.Fatalf("goroutine leak in ToxicProxy: before=%d after=%d", baseGoroutines, after)
	}
}

func TestSlowLorisWebSocketPoolHandshakeAndZeroLeak(t *testing.T) {
	const wsGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		key := r.Header.Get("Sec-WebSocket-Key")
		if key == "" {
			http.Error(w, "missing ws key", http.StatusBadRequest)
			return
		}
		hj, ok := w.(http.Hijacker)
		if !ok {
			return
		}
		conn, bufrw, err := hj.Hijack()
		if err != nil {
			return
		}
		defer conn.Close()

		sum := sha1.Sum([]byte(key + wsGUID))
		accept := base64.StdEncoding.EncodeToString(sum[:])
		_, _ = bufrw.WriteString("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + "\r\n\r\n")
		_ = bufrw.Flush()

		// Read binary CMD_SUBSCRIBE frame header then flood dummy frames
		br := bufio.NewReader(conn)
		var subHdr [6]byte
		_, _ = io.ReadFull(br, subHdr[:])

		payload := make([]byte, 256)
		for i := 0; i < 20; i++ {
			_ = conn.SetWriteDeadline(time.Now().Add(100 * time.Millisecond))
			if _, err := conn.Write(payload); err != nil {
				return
			}
			time.Sleep(10 * time.Millisecond)
		}
	}))
	defer srv.Close()

	wsURL := "ws" + strings.TrimPrefix(srv.URL, "http") + "/ws"
	baseG := runtime.NumGoroutine()

	pool := chaos.NewSlowLorisPool(wsURL, "")
	if err := pool.Configure(wsURL, 6, 30); err != nil {
		t.Fatal(err)
	}
	time.Sleep(120 * time.Millisecond)
	st := pool.Stats()
	if st.BytesTrickled == 0 && st.ActiveClients == 0 {
		t.Fatalf("expected slowloris activity, got %+v", st)
	}
	pool.Stop()
	srv.CloseClientConnections()

	time.Sleep(50 * time.Millisecond)
	if after := runtime.NumGoroutine(); after > baseG+4 {
		t.Fatalf("goroutine leak in SlowLorisPool: before=%d after=%d", baseG, after)
	}
}
