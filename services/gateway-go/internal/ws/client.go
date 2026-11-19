package ws

import (
	"bufio"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/gateway-go/internal/instruments"
)

const (
	wsGUID         = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
	maxClientFrame = 64 * 1024

	// RFC 6455 Close Status Codes.
	CloseNormalClosure   uint16 = 1000
	CloseProtocolError   uint16 = 1002
	ClosePolicyViolation uint16 = 1008
	CloseMessageTooBig   uint16 = 1009
	CloseTryAgainLater   uint16 = 1013
)

// WireFormat selects binary little-endian framing (default) vs optional JSON text fallback.
type WireFormat uint8

const (
	FormatBinary WireFormat = iota
	FormatJSON
)

type jsonControlMsg struct {
	Op       string   `json:"op"`
	Channels []string `json:"channels"`
	Symbols  []string `json:"symbols"`
	LastSeq  uint64   `json:"last_seq,omitempty"`
}

// Client represents a single upgraded WebSocket subscriber managed by Hub.
// Designed for minimal memory footprint (< 2 KB per connection without 8 KB bufio bloat)
// so 5,000–65,000 concurrent connections run effortlessly.
type Client struct {
	id             uint64
	accountID      uint64 // 0 if public anonymous subscriber; >0 if authenticated
	format         WireFormat
	conn           net.Conn
	initialBuf     []byte // residual pipelined bytes from HTTP Hijack reader
	hub            *Hub

	channelMask    atomic.Uint32
	instrumentMask atomic.Uint32

	// Per-instrument last delivered L3 engine_seq for server-side sequence-gap detection.
	lastDeliveredSeq [instruments.MaxInstrumentID + 1]uint64

	marketRing    *MarketRingBuffer
	criticalQueue chan *PooledFrame
	wakeCh        chan struct{} // buffered(1) coalesced write-pump wakeup signal
	pongQueue     chan []byte

	closed      atomic.Bool
	closeOnce   sync.Once
	doneCh      chan struct{}
	stallsCount atomic.Uint32
}

// UpgradeHTTP performs a standards-compliant RFC 6455 HTTP/1.1 WebSocket upgrade,
// negotiates `hyperion.bin.v1` or `hyperion.json.v1`, hijacks the raw TCP socket,
// and eliminates per-connection `bufio.ReadWriter` retention.
func UpgradeHTTP(w http.ResponseWriter, r *http.Request) (net.Conn, []byte, WireFormat, error) {
	if !headerContainsToken(r.Header, "Connection", "upgrade") ||
		!strings.EqualFold(strings.TrimSpace(r.Header.Get("Upgrade")), "websocket") {
		http.Error(w, "websocket upgrade required", http.StatusUpgradeRequired)
		return nil, nil, FormatBinary, errors.New("missing websocket upgrade headers")
	}
	if strings.TrimSpace(r.Header.Get("Sec-WebSocket-Version")) != "13" {
		w.Header().Set("Sec-WebSocket-Version", "13")
		http.Error(w, "unsupported websocket version", http.StatusBadRequest)
		return nil, nil, FormatBinary, errors.New("unsupported Sec-WebSocket-Version")
	}
	wsKey := strings.TrimSpace(r.Header.Get("Sec-WebSocket-Key"))
	if wsKey == "" {
		http.Error(w, "missing Sec-WebSocket-Key", http.StatusBadRequest)
		return nil, nil, FormatBinary, errors.New("missing Sec-WebSocket-Key")
	}

	format, selectedProto := negotiateWireFormat(r)

	hj, ok := w.(http.Hijacker)
	if !ok {
		http.Error(w, "hijacking not supported", http.StatusInternalServerError)
		return nil, nil, FormatBinary, errors.New("http.Hijacker unsupported")
	}
	conn, brw, err := hj.Hijack()
	if err != nil {
		return nil, nil, FormatBinary, err
	}

	// Drain any pipelined client bytes already buffered during HTTP header read before dropping brw!
	var residual []byte
	if brw != nil && brw.Reader != nil {
		if n := brw.Reader.Buffered(); n > 0 {
			residual = make([]byte, n)
			_, _ = io.ReadFull(brw.Reader, residual)
		}
	}

	acceptHash := sha1.Sum([]byte(wsKey + wsGUID))
	acceptVal := base64.StdEncoding.EncodeToString(acceptHash[:])

	var resp strings.Builder
	resp.Grow(192)
	resp.WriteString("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ")
	resp.WriteString(acceptVal)
	resp.WriteString("\r\n")
	if selectedProto != "" {
		resp.WriteString("Sec-WebSocket-Protocol: ")
		resp.WriteString(selectedProto)
		resp.WriteString("\r\n")
	}
	resp.WriteString("\r\n")

	_ = conn.SetWriteDeadline(time.Now().Add(5 * time.Second))
	if _, err := conn.Write([]byte(resp.String())); err != nil {
		_ = conn.Close()
		return nil, nil, FormatBinary, err
	}
	_ = conn.SetWriteDeadline(time.Time{})

	if tc, ok := conn.(*net.TCPConn); ok {
		_ = tc.SetNoDelay(true)
	}
	return conn, residual, format, nil
}

func negotiateWireFormat(r *http.Request) (WireFormat, string) {
	qFmt := strings.ToLower(strings.TrimSpace(r.URL.Query().Get("format")))
	offered := r.Header.Values("Sec-WebSocket-Protocol")
	var hasBin, hasJSON bool
	for _, line := range offered {
		for _, tok := range strings.Split(line, ",") {
			switch strings.TrimSpace(tok) {
			case "hyperion.bin.v1":
				hasBin = true
			case "hyperion.json.v1":
				hasJSON = true
			}
		}
	}
	if qFmt == "json" || (hasJSON && !hasBin && qFmt != "binary") {
		if hasJSON {
			return FormatJSON, "hyperion.json.v1"
		}
		return FormatJSON, ""
	}
	if hasBin {
		return FormatBinary, "hyperion.bin.v1"
	}
	return FormatBinary, ""
}

func headerContainsToken(h http.Header, key, wantLower string) bool {
	for _, v := range h.Values(key) {
		for _, tok := range strings.Split(v, ",") {
			if strings.EqualFold(strings.TrimSpace(tok), wantLower) {
				return true
			}
		}
	}
	return false
}

func (c *Client) signalWake() {
	select {
	case c.wakeCh <- struct{}{}:
	default:
	}
}

// EnqueueMarket pushes a non-critical market data frame (retained by caller) into the head-dropping ring buffer.
func (c *Client) EnqueueMarket(f *PooledFrame) {
	if c.closed.Load() {
		f.Release()
		return
	}
	dropped := c.marketRing.Push(f)
	if dropped {
		c.hub.metrics.WSSlowConsumerDropsTotal.Add(1)
	}
	c.signalWake()
}

// EnqueueCritical pushes a critical frame (MSG_EXECUTION_FILL, MSG_FRAUD_ALERT, MSG_ORDER_ACK_PRIVATE, MSG_L2_SNAPSHOT).
// Returns false if the client's critical queue is full and the caller must evict the stalled consumer
// AFTER releasing any Hub shard locks (avoiding RLock -> Lock self-deadlock).
func (c *Client) EnqueueCritical(f *PooledFrame) bool {
	if c.closed.Load() {
		f.Release()
		return true
	}
	select {
	case c.criticalQueue <- f:
		c.signalWake()
		return true
	default:
		f.Release()
		return false
	}
}

// CloseWithStatus sends a best-effort RFC 6455 Close frame and tears down the socket + queues idempotently.
func (c *Client) CloseWithStatus(code uint16, reason string) {
	c.closeOnce.Do(func() {
		c.closed.Store(true)
		close(c.doneCh)

		if code != 0 && c.conn != nil {
			closeFrame := buildRFC6455CloseFrame(code, reason)
			_ = c.conn.SetWriteDeadline(time.Now().Add(50 * time.Millisecond))
			_, _ = c.conn.Write(closeFrame)
		}
		if c.conn != nil {
			_ = c.conn.Close()
		}
		c.hub.unregisterClient(c)
		c.marketRing.Clear()
		for {
			select {
			case f := <-c.criticalQueue:
				f.Release()
			default:
				return
			}
		}
	})
}

func buildRFC6455CloseFrame(code uint16, reason string) []byte {
	if len(reason) > 120 {
		reason = reason[:120]
	}
	payLen := 2 + len(reason)
	out := make([]byte, 2+payLen)
	out[0] = 0x88 // FIN=1, Opcode=0x8 Close
	out[1] = byte(payLen)
	binary.BigEndian.PutUint16(out[2:4], code)
	copy(out[4:], reason)
	return out
}

func buildRFC6455PongFrame(payload []byte) []byte {
	if len(payload) > 125 {
		payload = payload[:125]
	}
	out := make([]byte, 2+len(payload))
	out[0] = 0x8A // FIN=1, Opcode=0xA Pong
	out[1] = byte(len(payload))
	copy(out[2:], payload)
	return out
}

type prefixedConnReader struct {
	prefix []byte
	conn   net.Conn
}

func (pr *prefixedConnReader) Read(p []byte) (int, error) {
	if len(pr.prefix) > 0 {
		n := copy(p, pr.prefix)
		pr.prefix = pr.prefix[n:]
		return n, nil
	}
	return pr.conn.Read(p)
}

var readScratchPool = sync.Pool{
	New: func() any {
		b := make([]byte, 2048)
		return &b
	},
}

// readPump parses incoming RFC 6455 masked client frames without allocating a 4KB bufio.Reader.
func (c *Client) readPump() {
	defer c.hub.clientWG.Done()
	defer c.CloseWithStatus(0, "")

	r := &prefixedConnReader{prefix: c.initialBuf, conn: c.conn}
	c.initialBuf = nil

	var hdrBuf [14]byte
	scratchPtr := readScratchPool.Get().(*[]byte)
	defer readScratchPool.Put(scratchPtr)
	scratch := *scratchPtr

	for !c.closed.Load() {
		if c.hub.cfg.WSReadTimeout > 0 {
			_ = c.conn.SetReadDeadline(time.Now().Add(c.hub.cfg.WSReadTimeout))
		}
		if _, err := io.ReadFull(r, hdrBuf[:2]); err != nil {
			return
		}
		opcode := hdrBuf[0] & 0x0F
		masked := (hdrBuf[1] & 0x80) != 0
		if !masked {
			// RFC 6455 §5.1 requires client-to-server frames to be masked.
			c.hub.metrics.WSProtocolErrorsTotal.Add(1)
			c.CloseWithStatus(CloseProtocolError, "client frame must be masked")
			return
		}
		payLen := uint64(hdrBuf[1] & 0x7F)
		switch payLen {
		case 126:
			if _, err := io.ReadFull(r, hdrBuf[2:4]); err != nil {
				return
			}
			payLen = uint64(binary.BigEndian.Uint16(hdrBuf[2:4]))
		case 127:
			if _, err := io.ReadFull(r, hdrBuf[2:10]); err != nil {
				return
			}
			payLen = binary.BigEndian.Uint64(hdrBuf[2:10])
		}
		if payLen > maxClientFrame {
			c.hub.metrics.WSProtocolErrorsTotal.Add(1)
			c.CloseWithStatus(CloseMessageTooBig, "frame exceeds 64KB limit")
			return
		}
		var maskKey [4]byte
		if _, err := io.ReadFull(r, maskKey[:]); err != nil {
			return
		}

		var payload []byte
		if int(payLen) <= len(scratch) {
			payload = scratch[:payLen]
		} else {
			payload = make([]byte, payLen)
		}
		if payLen > 0 {
			if _, err := io.ReadFull(r, payload); err != nil {
				return
			}
			for i := 0; i < len(payload); i++ {
				payload[i] ^= maskKey[i&3]
			}
		}

		switch opcode {
		case 0x8: // Close
			c.CloseWithStatus(CloseNormalClosure, "")
			return
		case 0x9: // Ping -> reply Pong
			pongCopy := append([]byte(nil), payload...)
			select {
			case c.pongQueue <- pongCopy:
				c.signalWake()
			default:
			}
		case 0xA: // Pong
			continue
		case 0x2: // Binary control frame (ws-binary-protocol.md §1 & §3.8)
			cmd, err := DecodeClientControl(payload)
			if err != nil {
				c.hub.metrics.WSProtocolErrorsTotal.Add(1)
				c.CloseWithStatus(CloseProtocolError, "invalid hyperion binary frame")
				return
			}
			c.applyControlCmd(cmd)
		case 0x1: // Optional JSON text subscription frame (ws-binary-protocol.md §2 note)
			if err := c.applyJSONControl(payload); err != nil {
				c.hub.metrics.WSProtocolErrorsTotal.Add(1)
				c.CloseWithStatus(CloseProtocolError, "invalid json control frame")
				return
			}
		default:
			c.hub.metrics.WSProtocolErrorsTotal.Add(1)
			c.CloseWithStatus(CloseProtocolError, "unsupported opcode")
			return
		}
	}
}

func (c *Client) applyControlCmd(cmd ClientControlCmd) {
	switch cmd.MsgType {
	case CmdSubscribe:
		newCh := c.channelMask.Load() | cmd.ChannelMask
		newInst := c.instrumentMask.Load() | cmd.InstrumentMask
		c.hub.updateClientSubscriptions(c, newCh, newInst)
	case CmdUnsubscribe:
		newCh := c.channelMask.Load() &^ cmd.ChannelMask
		newInst := c.instrumentMask.Load()
		if newCh == 0 {
			newInst &^= cmd.InstrumentMask
		}
		c.hub.updateClientSubscriptions(c, newCh, newInst)
	case CmdResyncRequest:
		c.hub.scheduleResyncForMask(c, cmd.InstrumentMask, cmd.LastKnownEngineSeq)
	}
}

func (c *Client) applyJSONControl(raw []byte) error {
	var msg jsonControlMsg
	if err := json.Unmarshal(raw, &msg); err != nil {
		return err
	}
	var chMask uint32
	for _, ch := range msg.Channels {
		switch strings.ToLower(strings.TrimSpace(ch)) {
		case "l2", "orderbook":
			chMask |= ChanL2
		case "l3", "deltas":
			chMask |= ChanL3
		case "fills", "trades", "executions":
			chMask |= ChanFills
		case "candles", "microstructure":
			chMask |= ChanCandles
		case "fraud", "alerts":
			chMask |= ChanFraud
		case "orders", "private":
			chMask |= ChanPrivateOrders
		}
	}
	if chMask == 0 {
		chMask = ChanAllPublic
	}
	var instMask uint32
	if len(msg.Symbols) == 0 {
		instMask = 0xFFFFFFFF
	} else {
		for _, sym := range msg.Symbols {
			if spec, ok := instruments.LookupSymbol(strings.ToUpper(strings.TrimSpace(sym))); ok {
				instMask |= 1 << spec.ID
			}
		}
	}
	switch strings.ToLower(strings.TrimSpace(msg.Op)) {
	case "subscribe":
		c.applyControlCmd(ClientControlCmd{MsgType: CmdSubscribe, ChannelMask: chMask, InstrumentMask: instMask})
	case "unsubscribe":
		c.applyControlCmd(ClientControlCmd{MsgType: CmdUnsubscribe, ChannelMask: chMask, InstrumentMask: instMask})
	case "resync":
		c.applyControlCmd(ClientControlCmd{MsgType: CmdResyncRequest, ChannelMask: chMask, InstrumentMask: instMask, LastKnownEngineSeq: msg.LastSeq})
	default:
		return errors.New("unknown json op")
	}
	return nil
}

// writePump drains critical frames first, then coalesces up to 32 market ring-buffer frames
// into a single net.Buffers (`writev` / `WSASend`) syscall, detects sequence gaps, and triggers
// automatic L2 snapshot resyncs without ever allocating per-frame byte copies on the fast path.
func (c *Client) writePump() {
	defer c.hub.clientWG.Done()
	defer c.CloseWithStatus(0, "")

	batch := make([]*PooledFrame, 32)
	iov := make(net.Buffers, 0, 36)

	for {
		select {
		case <-c.doneCh:
			return
		case <-c.wakeCh:
		}

		for !c.closed.Load() {
			didWork := false
			iov = iov[:0]
			var toRelease []*PooledFrame

			// 1. Drain Pong replies immediately.
			for {
				select {
				case pongPay := <-c.pongQueue:
					iov = append(iov, buildRFC6455PongFrame(pongPay))
					didWork = true
				default:
					goto drainCritical
				}
			}

		drainCritical:
			// 2. Drain Critical Queue (Private OrderAcks, Fills, Fraud Alerts, Resync Snapshots).
			for len(iov) < 24 {
				select {
				case cf := <-c.criticalQueue:
					if cf.MsgType == MsgL2Snapshot && cf.InstrumentID <= instruments.MaxInstrumentID && cf.EngineSeq > 0 {
						c.lastDeliveredSeq[cf.InstrumentID] = cf.EngineSeq
					}
					iov = append(iov, cf.WireBytes())
					toRelease = append(toRelease, cf)
					didWork = true
				default:
					goto drainMarket
				}
			}

		drainMarket:
			// 3. Drain Non-Critical Market Ring Buffer + check sequence continuity.
			n, droppedPrior, droppedInstMask := c.marketRing.DrainBatch(batch)
			var resyncInstMask uint32
			if droppedPrior > 0 {
				resyncInstMask |= droppedInstMask
			}

			for i := 0; i < n; i++ {
				mf := batch[i]
				batch[i] = nil
				didWork = true

				instID := mf.InstrumentID
				flagDropped := droppedPrior > 0 && i == 0

				if mf.MsgType == MsgL3DeltaBatch && instID > 0 && instID <= instruments.MaxInstrumentID {
					localSeq := c.lastDeliveredSeq[instID]
					if localSeq > 0 && mf.PrevSeq > localSeq {
						// Sequence gap detected -> schedule automatic L2 Snapshot Resync!
						resyncInstMask |= 1 << instID
						flagDropped = true
					}
					if mf.EngineSeq > localSeq {
						c.lastDeliveredSeq[instID] = mf.EngineSeq
					}
				}

				if flagDropped && c.format == FormatBinary {
					// Emit a stack/small-header copy with FlagSlowConsumerDroppedPrior set without mutating shared PooledFrame!
					customHdr := make([]byte, (wsFrontHeadroom-mf.wireStart)+HeaderBytes)
					copy(customHdr, mf.buf[mf.wireStart:wsFrontHeadroom+HeaderBytes])
					hdrFlagIdx := (wsFrontHeadroom - mf.wireStart) + 4
					customHdr[hdrFlagIdx] |= FlagSlowConsumerDroppedPrior
					iov = append(iov, customHdr, mf.PayloadBytes())
				} else {
					iov = append(iov, mf.WireBytes())
				}
				toRelease = append(toRelease, mf)
			}

			if len(iov) > 0 {
				frameCount := uint64(len(toRelease))
				if c.hub.cfg.WSWriteTimeout > 0 {
					_ = c.conn.SetWriteDeadline(time.Now().Add(c.hub.cfg.WSWriteTimeout))
				}
				_, werr := iov.WriteTo(c.conn)
				for _, rf := range toRelease {
					rf.Release()
				}
				if werr != nil {
					if ne, ok := werr.(net.Error); ok && ne.Timeout() {
						stalls := c.stallsCount.Add(1)
						if stalls >= c.hub.cfg.WSMaxConsecutiveStalls {
							c.hub.metrics.WSSlowConsumerEvictsTotal.Add(1)
							c.CloseWithStatus(CloseTryAgainLater, "slow consumer write timeout eviction")
							return
						}
					} else {
						return
					}
				} else {
					c.stallsCount.Store(0)
					c.hub.metrics.WSFramesWrittenTotal.Add(frameCount)
				}
			}

			if resyncInstMask != 0 && c.format == FormatBinary {
				c.hub.scheduleResyncForMask(c, resyncInstMask, 0)
			}

			if !didWork {
				break
			}
		}
	}
}

// DialTestWS opens a lightweight client WebSocket connection (used by torture & unit tests
// without 8KB bufio per-client overhead, supporting 5,000+ concurrent client goroutines).
func DialTestWS(addr, path, subproto string) (net.Conn, *bufio.Reader, error) {
	conn, err := net.DialTimeout("tcp", addr, 5*time.Second)
	if err != nil {
		return nil, nil, err
	}
	if tc, ok := conn.(*net.TCPConn); ok {
		_ = tc.SetNoDelay(true)
	}
	req := "GET " + path + " HTTP/1.1\r\n" +
		"Host: " + addr + "\r\n" +
		"Upgrade: websocket\r\n" +
		"Connection: Upgrade\r\n" +
		"Sec-WebSocket-Version: 13\r\n" +
		"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
	if subproto != "" {
		req += "Sec-WebSocket-Protocol: " + subproto + "\r\n"
	}
	req += "\r\n"
	if _, err := conn.Write([]byte(req)); err != nil {
		_ = conn.Close()
		return nil, nil, err
	}
	br := bufio.NewReaderSize(conn, 256)
	statusLine, err := br.ReadString('\n')
	if err != nil || !strings.Contains(statusLine, "101") {
		_ = conn.Close()
		return nil, nil, errors.New("ws handshake failed: " + statusLine)
	}
	for {
		line, err := br.ReadString('\n')
		if err != nil {
			_ = conn.Close()
			return nil, nil, err
		}
		if line == "\r\n" || line == "\n" {
			break
		}
	}
	return conn, br, nil
}

// WriteMaskedClientFrame sends an RFC 6455 masked client frame (opcode 0x1 Text or 0x2 Binary).
func WriteMaskedClientFrame(w io.Writer, opcode byte, payload []byte) error {
	mask := [4]byte{0x37, 0xFA, 0x21, 0x3D}
	payLen := len(payload)
	var hdr [14]byte
	hdr[0] = 0x80 | (opcode & 0x0F)
	hdrEnd := 2
	switch {
	case payLen <= 125:
		hdr[1] = 0x80 | byte(payLen)
	case payLen <= 65535:
		hdr[1] = 0x80 | 126
		binary.BigEndian.PutUint16(hdr[2:4], uint16(payLen))
		hdrEnd = 4
	default:
		hdr[1] = 0x80 | 127
		binary.BigEndian.PutUint64(hdr[2:10], uint64(payLen))
		hdrEnd = 10
	}
	copy(hdr[hdrEnd:hdrEnd+4], mask[:])
	hdrEnd += 4

	out := make([]byte, hdrEnd+payLen)
	copy(out[:hdrEnd], hdr[:hdrEnd])
	for i := 0; i < payLen; i++ {
		out[hdrEnd+i] = payload[i] ^ mask[i&3]
	}
	_, err := w.Write(out)
	return err
}

// ReadServerFrame reads a single unmasked server-to-client RFC 6455 frame.
func ReadServerFrame(r io.Reader) (opcode byte, payload []byte, err error) {
	var hdr [10]byte
	if _, err := io.ReadFull(r, hdr[:2]); err != nil {
		return 0, nil, err
	}
	opcode = hdr[0] & 0x0F
	payLen := uint64(hdr[1] & 0x7F)
	switch payLen {
	case 126:
		if _, err := io.ReadFull(r, hdr[2:4]); err != nil {
			return 0, nil, err
		}
		payLen = uint64(binary.BigEndian.Uint16(hdr[2:4]))
	case 127:
		if _, err := io.ReadFull(r, hdr[2:10]); err != nil {
			return 0, nil, err
		}
		payLen = binary.BigEndian.Uint64(hdr[2:10])
	}
	payload = make([]byte, payLen)
	if payLen > 0 {
		if _, err := io.ReadFull(r, payload); err != nil {
			return 0, nil, err
		}
	}
	return opcode, payload, nil
}
