package chaos

import (
	"bufio"
	"context"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"net"
	"net/url"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const wsGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

type SlowLorisStats struct {
	TargetURL       string `json:"target_url"`
	ActiveClients   int64  `json:"active_clients"`
	BytesTrickled   uint64 `json:"bytes_trickled"`
	EvictedByServer uint64 `json:"evicted_by_server"`
	DialErrors      uint64 `json:"dial_errors"`
}

// SlowLorisPool maintains N RFC-6455 WebSocket connections that subscribe to high-volume
// market streams and intentionally stall socket reads (trickle reading small byte chunks)
// to test Gateway slow-consumer ring-buffer dropping and eviction without OOM.
type SlowLorisPool struct {
	mu          sync.Mutex
	targetURL   string
	subscribePayload string

	cancel context.CancelFunc
	wg     sync.WaitGroup
	conns  map[net.Conn]struct{}

	activeClients   atomic.Int64
	bytesTrickled   atomic.Uint64
	evictedByServer atomic.Uint64
	dialErrors      atomic.Uint64
}

func NewSlowLorisPool(targetURL, subscribePayload string) *SlowLorisPool {
	return &SlowLorisPool{
		targetURL:        targetURL,
		subscribePayload: subscribePayload,
		conns:            make(map[net.Conn]struct{}),
	}
}

// Configure starts or stops the Slow-Loris WebSocket consumer swarm cleanly.
func (s *SlowLorisPool) Configure(targetURL string, count int, stallMs int) error {
	s.Stop()
	if count <= 0 {
		return nil
	}
	if count > 2048 {
		count = 2048
	}
	if stallMs <= 0 {
		stallMs = 200
	}

	s.mu.Lock()
	if targetURL != "" {
		s.targetURL = targetURL
	}
	urlStr := s.targetURL
	subMsg := s.subscribePayload
	ctx, cancel := context.WithCancel(context.Background())
	s.cancel = cancel
	s.mu.Unlock()

	if urlStr == "" {
		return errors.New("slowloris: target WebSocket URL is empty")
	}

	for i := 0; i < count; i++ {
		s.wg.Add(1)
		go s.runWorker(ctx, i, urlStr, subMsg, time.Duration(stallMs)*time.Millisecond)
	}
	return nil
}

func (s *SlowLorisPool) Stop() {
	s.mu.Lock()
	cancel := s.cancel
	s.cancel = nil
	conns := make([]net.Conn, 0, len(s.conns))
	for c := range s.conns {
		conns = append(conns, c)
	}
	s.mu.Unlock()

	if cancel != nil {
		cancel()
	}
	for _, c := range conns {
		_ = c.SetDeadline(time.Now())
		_ = c.Close()
	}
	s.wg.Wait()
}

func (s *SlowLorisPool) Stats() SlowLorisStats {
	s.mu.Lock()
	u := s.targetURL
	s.mu.Unlock()
	return SlowLorisStats{
		TargetURL:       u,
		ActiveClients:   s.activeClients.Load(),
		BytesTrickled:   s.bytesTrickled.Load(),
		EvictedByServer: s.evictedByServer.Load(),
		DialErrors:      s.dialErrors.Load(),
	}
}

func (s *SlowLorisPool) trackConn(c net.Conn, add bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if add {
		s.conns[c] = struct{}{}
	} else {
		delete(s.conns, c)
	}
}

func (s *SlowLorisPool) runWorker(ctx context.Context, workerID int, rawURL, subMsg string, stall time.Duration) {
	defer s.wg.Done()

	conn, br, err := dialRawWebSocket(ctx, rawURL, workerID)
	if err != nil {
		s.dialErrors.Add(1)
		return
	}
	s.trackConn(conn, true)
	s.activeClients.Add(1)
	defer func() {
		s.activeClients.Add(-1)
		s.trackConn(conn, false)
		_ = conn.Close()
	}()

	if tcp, ok := conn.(*net.TCPConn); ok {
		// Shrink OS TCP receive buffer to induce genuine TCP backpressure quickly.
		_ = tcp.SetReadBuffer(2048)
	}

	if subMsg != "" {
		if err := writeMaskedTextFrame(conn, []byte(subMsg)); err != nil {
			return
		}
	} else {
		// Send canonical 48-byte ws-binary-protocol.md CMD_SUBSCRIBE (0x81) frame
		if err := writeBinarySubscribeFrame(conn); err != nil {
			return
		}
	}

	// Trickle-read tiny 16-byte slices separated by stall intervals.
	var tiny [16]byte
	timer := time.NewTimer(stall)
	defer timer.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-timer.C:
		}
		_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
		n, rErr := br.Read(tiny[:])
		if n > 0 {
			s.bytesTrickled.Add(uint64(n))
		}
		if rErr != nil {
			if ctx.Err() == nil {
				s.evictedByServer.Add(1)
			}
			return
		}
		timer.Reset(stall)
	}
}

func dialRawWebSocket(ctx context.Context, rawURL string, workerID int) (net.Conn, *bufio.Reader, error) {
	u, err := url.Parse(rawURL)
	if err != nil {
		return nil, nil, err
	}
	host := u.Host
	if !strings.Contains(host, ":") {
		if u.Scheme == "wss" {
			host += ":443"
		} else {
			host += ":80"
		}
	}
	reqURI := u.RequestURI()
	if reqURI == "" {
		reqURI = "/"
	}

	var keyRaw [16]byte
	binary.LittleEndian.PutUint64(keyRaw[0:8], uint64(workerID+1)*0x9E3779B97F4A7C15)
	binary.LittleEndian.PutUint64(keyRaw[8:16], 0xBF58476D1CE4E5B9)
	wsKey := base64.StdEncoding.EncodeToString(keyRaw[:])

	dialer := net.Dialer{Timeout: 3 * time.Second}
	conn, err := dialer.DialContext(ctx, "tcp", host)
	if err != nil {
		return nil, nil, err
	}
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))

	req := fmt.Sprintf(
		"GET %s HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: hyperion.bin.v1\r\n\r\n",
		reqURI, u.Host, wsKey,
	)
	if _, err := conn.Write([]byte(req)); err != nil {
		_ = conn.Close()
		return nil, nil, err
	}

	br := bufio.NewReaderSize(conn, 1024)
	statusLine, err := br.ReadString('\n')
	if err != nil || !strings.Contains(statusLine, "101") {
		_ = conn.Close()
		return nil, nil, fmt.Errorf("slowloris: handshake rejected: %s (%v)", strings.TrimSpace(statusLine), err)
	}

	sum := sha1.Sum([]byte(wsKey + wsGUID))
	expectedAccept := base64.StdEncoding.EncodeToString(sum[:])
	gotAccept := ""
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			_ = conn.Close()
			return nil, nil, err
		}
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			break
		}
		if strings.HasPrefix(strings.ToLower(trimmed), "sec-websocket-accept:") {
			gotAccept = strings.TrimSpace(trimmed[len("sec-websocket-accept:"):])
		}
	}
	if gotAccept != expectedAccept {
		_ = conn.Close()
		return nil, nil, errors.New("slowloris: invalid Sec-WebSocket-Accept")
	}

	_ = conn.SetDeadline(time.Time{})
	return conn, br, nil
}

func writeMaskedTextFrame(w net.Conn, payload []byte) error {
	mask := [4]byte{0x37, 0xFA, 0x21, 0x3D}
	var hdr []byte
	n := len(payload)
	hdr = append(hdr, 0x81) // FIN=1, Opcode=1 (Text)
	if n < 126 {
		hdr = append(hdr, byte(0x80|n))
	} else if n <= 65535 {
		hdr = append(hdr, 0x80|126, byte(n>>8), byte(n))
	} else {
		return errors.New("slowloris: subscribe frame too large")
	}
	hdr = append(hdr, mask[:]...)
	frame := make([]byte, len(hdr)+n)
	copy(frame, hdr)
	for i := 0; i < n; i++ {
		frame[len(hdr)+i] = payload[i] ^ mask[i&3]
	}
	_, err := w.Write(frame)
	return err
}

// writeBinarySubscribeFrame encodes a 48-byte ws-binary-protocol.md CMD_SUBSCRIBE (0x81)
// frame masked per RFC 6455 (opcode 0x82 binary).
func writeBinarySubscribeFrame(w net.Conn) error {
	var raw [48]byte
	binary.LittleEndian.PutUint16(raw[0:2], 0x4859) // magic 'HY'
	raw[2] = 0x01                                   // version = 1
	raw[3] = 0x81                                   // CMD_SUBSCRIBE
	raw[4] = 0x00                                   // flags
	raw[5] = 0x00                                   // reserved
	binary.LittleEndian.PutUint16(raw[6:8], 0)      // global
	binary.LittleEndian.PutUint32(raw[8:12], 16)    // payload_len = 16
	binary.LittleEndian.PutUint32(raw[12:16], 0)    // crc32c
	binary.LittleEndian.PutUint64(raw[16:24], 0)    // engine_seq
	binary.LittleEndian.PutUint64(raw[24:32], uint64(time.Now().UnixNano()))

	// Payload: channel_mask = 0x1F (L2|L3|FILLS|CANDLES|FRAUD), instrument_mask = 0xFFFFFFFF
	binary.LittleEndian.PutUint32(raw[32:36], 0x1F)
	binary.LittleEndian.PutUint32(raw[36:40], 0xFFFFFFFF)
	binary.LittleEndian.PutUint64(raw[40:48], 0)

	mask := [4]byte{0xA5, 0x5A, 0x3C, 0xC3}
	var frame [2 + 4 + 48]byte
	frame[0] = 0x82 // FIN=1, Opcode=2 (Binary)
	frame[1] = 0x80 | 48
	copy(frame[2:6], mask[:])
	for i := 0; i < 48; i++ {
		frame[6+i] = raw[i] ^ mask[i&3]
	}
	_, err := w.Write(frame[:])
	return err
}
