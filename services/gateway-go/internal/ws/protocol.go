package ws

import (
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"sync"
	"sync/atomic"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"google.golang.org/protobuf/proto"
)

const (
	// Wire Header Constants (contracts/ws-binary-protocol.md §1).
	FrameMagic   uint16 = fixedpoint.WSFrameMagic   // 0x4859
	FrameVersion uint8  = fixedpoint.WSFrameVersion // 0x01
	HeaderBytes  int    = fixedpoint.WSHeaderLen    // 32

	// RFC 6455 Front Headroom (max 10 bytes for 0x82 + 127 + 8B length).
	wsFrontHeadroom = 10

	// Header Flag Bits.
	FlagCRC32CPresent            uint8 = 0x01
	FlagSnapshotResync           uint8 = 0x02
	FlagSlowConsumerDroppedPrior uint8 = 0x04
	FlagReplayTimeline           uint8 = 0x08

	// Server -> Client Message Types (0x01..0x08).
	MsgHeartbeat        uint8 = 0x01
	MsgL2Snapshot       uint8 = 0x02
	MsgL3DeltaBatch     uint8 = 0x03
	MsgExecutionFill    uint8 = 0x04
	MsgCandleMicro      uint8 = 0x05
	MsgFraudAlert       uint8 = 0x06
	MsgOrderAckPrivate  uint8 = 0x07
	MsgGapResyncNotice  uint8 = 0x08

	// Client -> Server Control Types (0x81..0x83).
	CmdSubscribe     uint8 = 0x81
	CmdUnsubscribe   uint8 = 0x82
	CmdResyncRequest uint8 = 0x83

	// Subscription Channel Bitmask (ws-binary-protocol.md §3.8).
	ChanL2            uint32 = 0x01
	ChanL3            uint32 = 0x02
	ChanFills         uint32 = 0x04
	ChanCandles       uint32 = 0x08
	ChanFraud         uint32 = 0x10
	ChanPrivateOrders uint32 = 0x20
	ChanAllPublic     uint32 = ChanL2 | ChanL3 | ChanFills | ChanCandles | ChanFraud
)

var (
	castagnoliTable = crc32.MakeTable(crc32.Castagnoli)

	ErrInvalidMagicOrVersion = errors.New("ws frame failed magic 0x4859 or version 1 check")
	ErrPayloadLengthMismatch = errors.New("ws frame byte length does not match 32 + payload_len")
	ErrCRC32CMismatch        = errors.New("ws frame payload CRC32C checksum mismatch")
)

// Header represents the decoded 32-byte Little-Endian Hyperion binary frame header.
type Header struct {
	Magic        uint16
	Version      uint8
	MsgType      uint8
	Flags        uint8
	InstrumentID uint16
	PayloadLen   uint32
	CRC32C       uint32
	EngineSeq    uint64
	TimestampNS  uint64
}

// ClientControlCmd represents decoded 16-byte client subscription/resync payload.
type ClientControlCmd struct {
	MsgType            uint8
	ChannelMask        uint32
	InstrumentMask     uint32
	LastKnownEngineSeq uint64
}

type poolTier uint8

const (
	tierSmall poolTier = iota
	tierLarge
	tierOversize
)

const (
	smallBufCap = wsFrontHeadroom + HeaderBytes + 512
	largeBufCap = wsFrontHeadroom + HeaderBytes + 8192
)

// PooledFrame holds a pre-framed RFC 6455 + Hyperion 32B LE binary message
// backed by a tiered sync.Pool with lock-free atomic reference counting.
// Once constructed, buf[wireStart:wireEnd] is strictly immutable until refs hits 0.
type PooledFrame struct {
	buf          []byte
	wireStart    int
	wireEnd      int
	MsgType      uint8
	Flags        uint8
	InstrumentID uint16
	EngineSeq    uint64
	PrevSeq      uint64
	tier         poolTier
	refs         atomic.Int32
}

var (
	smallFramePool = sync.Pool{
		New: func() any {
			return &PooledFrame{
				buf:  make([]byte, smallBufCap),
				tier: tierSmall,
			}
		},
	}
	largeFramePool = sync.Pool{
		New: func() any {
			return &PooledFrame{
				buf:  make([]byte, largeBufCap),
				tier: tierLarge,
			}
		},
	}
)

// AcquireFrame borrows a *PooledFrame sized for payloadLen with initial refcount = 1.
func AcquireFrame(wsOpcode byte, msgType, flags uint8, instrumentID uint16, engineSeq, tsNS uint64, payloadLen int) (*PooledFrame, []byte) {
	needed := wsFrontHeadroom + HeaderBytes + payloadLen
	var f *PooledFrame
	switch {
	case needed <= smallBufCap:
		f = smallFramePool.Get().(*PooledFrame)
	case needed <= largeBufCap:
		f = largeFramePool.Get().(*PooledFrame)
	default:
		f = &PooledFrame{
			buf:  make([]byte, needed),
			tier: tierOversize,
		}
	}
	f.MsgType = msgType
	f.Flags = flags
	f.InstrumentID = instrumentID
	f.EngineSeq = engineSeq
	f.PrevSeq = 0
	f.wireEnd = needed
	f.refs.Store(1)

	// Write RFC 6455 unmasked server-to-client frame header in front headroom.
	wsBodyLen := HeaderBytes + payloadLen
	f.wireStart = writeRFC6455ServerHeader(f.buf[:wsFrontHeadroom], wsOpcode, wsBodyLen)

	// Write 32-byte Hyperion Little-Endian Header at buf[wsFrontHeadroom : wsFrontHeadroom+32].
	hdr := f.buf[wsFrontHeadroom : wsFrontHeadroom+HeaderBytes]
	binary.LittleEndian.PutUint16(hdr[0:2], FrameMagic)
	hdr[2] = FrameVersion
	hdr[3] = msgType
	hdr[4] = flags
	hdr[5] = 0
	binary.LittleEndian.PutUint16(hdr[6:8], instrumentID)
	binary.LittleEndian.PutUint32(hdr[8:12], uint32(payloadLen))
	binary.LittleEndian.PutUint32(hdr[12:16], 0) // populated in FinalizeFrame if FlagCRC32CPresent
	binary.LittleEndian.PutUint64(hdr[16:24], engineSeq)
	binary.LittleEndian.PutUint64(hdr[24:32], tsNS)

	payloadSlice := f.buf[wsFrontHeadroom+HeaderBytes : needed]
	return f, payloadSlice
}

// AcquireTextFrame borrows a *PooledFrame containing an RFC 6455 unmasked UTF-8 Text frame (JSON fallback).
func AcquireTextFrame(jsonPayload []byte, msgType uint8, instrumentID uint16, engineSeq uint64) *PooledFrame {
	needed := wsFrontHeadroom + len(jsonPayload)
	var f *PooledFrame
	switch {
	case needed <= smallBufCap:
		f = smallFramePool.Get().(*PooledFrame)
	case needed <= largeBufCap:
		f = largeFramePool.Get().(*PooledFrame)
	default:
		f = &PooledFrame{
			buf:  make([]byte, needed),
			tier: tierOversize,
		}
	}
	f.MsgType = msgType
	f.Flags = 0
	f.InstrumentID = instrumentID
	f.EngineSeq = engineSeq
	f.PrevSeq = 0
	f.wireEnd = needed
	f.refs.Store(1)
	f.wireStart = writeRFC6455ServerHeader(f.buf[:wsFrontHeadroom], 0x1, len(jsonPayload))
	copy(f.buf[wsFrontHeadroom:needed], jsonPayload)
	return f
}

// FinalizeFrame computes and writes the Castagnoli CRC-32C digest if FlagCRC32CPresent is set.
func (f *PooledFrame) FinalizeFrame() {
	if (f.Flags & FlagCRC32CPresent) != 0 {
		payload := f.buf[wsFrontHeadroom+HeaderBytes : f.wireEnd]
		digest := crc32.Checksum(payload, castagnoliTable)
		binary.LittleEndian.PutUint32(f.buf[wsFrontHeadroom+12:wsFrontHeadroom+16], digest)
	}
}

// WireBytes returns the complete RFC 6455 wire frame ready for zero-copy socket Write / writev.
func (f *PooledFrame) WireBytes() []byte {
	return f.buf[f.wireStart:f.wireEnd]
}

// HyperionBytes returns the 32-byte Hyperion header + payload (excluding RFC 6455 framing).
func (f *PooledFrame) HyperionBytes() []byte {
	return f.buf[wsFrontHeadroom:f.wireEnd]
}

// PayloadBytes returns only the payload slice following the 32-byte Hyperion header.
func (f *PooledFrame) PayloadBytes() []byte {
	return f.buf[wsFrontHeadroom+HeaderBytes : f.wireEnd]
}

// Retain increments the atomic reference count by delta (must be > 0 while refs > 0).
func (f *PooledFrame) Retain(delta int32) {
	f.refs.Add(delta)
}

// Release decrements the atomic reference count and recycles the buffer to sync.Pool when refs == 0.
func (f *PooledFrame) Release() {
	if f == nil {
		return
	}
	rem := f.refs.Add(-1)
	if rem != 0 {
		return
	}
	switch f.tier {
	case tierSmall:
		smallFramePool.Put(f)
	case tierLarge:
		largeFramePool.Put(f)
	}
}

func writeRFC6455ServerHeader(front []byte, opcode byte, length int) int {
	finOp := byte(0x80) | (opcode & 0x0F)
	switch {
	case length <= 125:
		front[8] = finOp
		front[9] = byte(length)
		return 8
	case length <= 65535:
		front[6] = finOp
		front[7] = 126
		binary.BigEndian.PutUint16(front[8:10], uint16(length))
		return 6
	default:
		front[0] = finOp
		front[1] = 127
		binary.BigEndian.PutUint64(front[2:10], uint64(length))
		return 0
	}
}

// DecodeHeader validates and parses a 32-byte Little-Endian Hyperion frame header + payload.
func DecodeHeader(raw []byte) (Header, []byte, error) {
	if len(raw) < HeaderBytes {
		return Header{}, nil, ErrInvalidMagicOrVersion
	}
	magic := binary.LittleEndian.Uint16(raw[0:2])
	ver := raw[2]
	if magic != FrameMagic || ver != FrameVersion {
		return Header{}, nil, ErrInvalidMagicOrVersion
	}
	payLen := binary.LittleEndian.Uint32(raw[8:12])
	if len(raw) != HeaderBytes+int(payLen) {
		return Header{}, nil, ErrPayloadLengthMismatch
	}
	h := Header{
		Magic:        magic,
		Version:      ver,
		MsgType:      raw[3],
		Flags:        raw[4],
		InstrumentID: binary.LittleEndian.Uint16(raw[6:8]),
		PayloadLen:   payLen,
		CRC32C:       binary.LittleEndian.Uint32(raw[12:16]),
		EngineSeq:    binary.LittleEndian.Uint64(raw[16:24]),
		TimestampNS:  binary.LittleEndian.Uint64(raw[24:32]),
	}
	payload := raw[HeaderBytes:]
	if (h.Flags & FlagCRC32CPresent) != 0 {
		if crc32.Checksum(payload, castagnoliTable) != h.CRC32C {
			return Header{}, nil, ErrCRC32CMismatch
		}
	}
	return h, payload, nil
}

// DecodeClientControl parses a 0x81/0x82/0x83 client control binary frame.
func DecodeClientControl(raw []byte) (ClientControlCmd, error) {
	hdr, payload, err := DecodeHeader(raw)
	if err != nil {
		return ClientControlCmd{}, err
	}
	if hdr.MsgType != CmdSubscribe && hdr.MsgType != CmdUnsubscribe && hdr.MsgType != CmdResyncRequest {
		return ClientControlCmd{}, fmt.Errorf("unexpected client control msg_type 0x%02x", hdr.MsgType)
	}
	if len(payload) != 16 {
		return ClientControlCmd{}, ErrPayloadLengthMismatch
	}
	return ClientControlCmd{
		MsgType:            hdr.MsgType,
		ChannelMask:        binary.LittleEndian.Uint32(payload[0:4]),
		InstrumentMask:     binary.LittleEndian.Uint32(payload[4:8]),
		LastKnownEngineSeq: binary.LittleEndian.Uint64(payload[8:16]),
	}, nil
}

// EncodeClientControl encodes a 48-byte (32B header + 16B payload) client control frame (used in tests/benchmarks).
func EncodeClientControl(msgType uint8, chanMask, instMask uint32, lastKnownSeq, tsNS uint64) []byte {
	out := make([]byte, HeaderBytes+16)
	binary.LittleEndian.PutUint16(out[0:2], FrameMagic)
	out[2] = FrameVersion
	out[3] = msgType
	out[4] = FlagCRC32CPresent
	out[5] = 0
	binary.LittleEndian.PutUint16(out[6:8], 0)
	binary.LittleEndian.PutUint32(out[8:12], 16)
	binary.LittleEndian.PutUint64(out[16:24], 0)
	binary.LittleEndian.PutUint64(out[24:32], tsNS)

	pay := out[HeaderBytes:]
	binary.LittleEndian.PutUint32(pay[0:4], chanMask)
	binary.LittleEndian.PutUint32(pay[4:8], instMask)
	binary.LittleEndian.PutUint64(pay[8:16], lastKnownSeq)
	binary.LittleEndian.PutUint32(out[12:16], crc32.Checksum(pay, castagnoliTable))
	return out
}

// --- Zero-Alloc Pooled Server -> Client Frame Builders (ws-binary-protocol.md §3) ---

// EncodeHeartbeat builds 0x01 MSG_HEARTBEAT (payload_len = 16).
func EncodeHeartbeat(monotonicNS uint64, connectedClients, queueDepth uint32, tsNS uint64) *PooledFrame {
	f, p := AcquireFrame(0x2, MsgHeartbeat, FlagCRC32CPresent, 0, 0, tsNS, 16)
	binary.LittleEndian.PutUint64(p[0:8], monotonicNS)
	binary.LittleEndian.PutUint32(p[8:12], connectedClients)
	binary.LittleEndian.PutUint32(p[12:16], queueDepth)
	f.FinalizeFrame()
	return f
}

// EncodeGapResyncNotice builds 0x08 MSG_GAP_RESYNC_NOTICE (payload_len = 16).
func EncodeGapResyncNotice(instrumentID uint16, expectedSeq, snapshotSeq, tsNS uint64) *PooledFrame {
	f, p := AcquireFrame(0x2, MsgGapResyncNotice, FlagCRC32CPresent|FlagSnapshotResync|FlagSlowConsumerDroppedPrior, instrumentID, snapshotSeq, tsNS, 16)
	binary.LittleEndian.PutUint64(p[0:8], expectedSeq)
	binary.LittleEndian.PutUint64(p[8:16], snapshotSeq)
	f.FinalizeFrame()
	return f
}

// EncodeL2Snapshot builds 0x02 MSG_L2_SNAPSHOT (payload_len = 56 + (Nb+Na)*20).
func EncodeL2Snapshot(snap *hyperionv1.L2BookSnapshot, instrumentID uint16, extraFlags uint8) *PooledFrame {
	bids := snap.GetBids()
	asks := snap.GetAsks()
	nb := len(bids)
	na := len(asks)
	if nb > 65535 {
		nb = 65535
	}
	if na > 65535 {
		na = 65535
	}
	payLen := 56 + (nb+na)*20
	f, p := AcquireFrame(0x2, MsgL2Snapshot, FlagCRC32CPresent|extraFlags, instrumentID, snap.GetEngineSeq(), snap.GetSnapshotTsNs(), payLen)
	binary.LittleEndian.PutUint64(p[0:8], snap.GetBestBidFp())
	binary.LittleEndian.PutUint64(p[8:16], snap.GetBestAskFp())
	binary.LittleEndian.PutUint16(p[16:18], uint16(nb))
	binary.LittleEndian.PutUint16(p[18:20], uint16(na))
	binary.LittleEndian.PutUint32(p[20:24], 0)
	clear(p[24:56])
	copy(p[24:56], snap.GetStateRootBlake3())

	off := 56
	for i := 0; i < nb; i++ {
		lv := bids[i]
		binary.LittleEndian.PutUint64(p[off:off+8], lv.GetPriceFp())
		binary.LittleEndian.PutUint64(p[off+8:off+16], lv.GetTotalQtyFp())
		binary.LittleEndian.PutUint32(p[off+16:off+20], lv.GetOrderCount())
		off += 20
	}
	for i := 0; i < na; i++ {
		lv := asks[i]
		binary.LittleEndian.PutUint64(p[off:off+8], lv.GetPriceFp())
		binary.LittleEndian.PutUint64(p[off+8:off+16], lv.GetTotalQtyFp())
		binary.LittleEndian.PutUint32(p[off+16:off+20], lv.GetOrderCount())
		off += 20
	}
	f.FinalizeFrame()
	return f
}

// EncodeL3DeltaBatch builds 0x03 MSG_L3_DELTA_BATCH (payload_len = 16 + K*40).
func EncodeL3DeltaBatch(deltas []*hyperionv1.L3BookDelta, instrumentID uint16) *PooledFrame {
	k := len(deltas)
	if k == 0 {
		return nil
	}
	if k > 65535 {
		k = 65535
	}
	first := deltas[0]
	last := deltas[k-1]
	payLen := 16 + k*40
	f, p := AcquireFrame(0x2, MsgL3DeltaBatch, FlagCRC32CPresent, instrumentID, last.GetEngineSeq(), last.GetEventTsNs(), payLen)
	f.PrevSeq = first.GetPrevEngineSeq()

	binary.LittleEndian.PutUint64(p[0:8], first.GetPrevEngineSeq())
	binary.LittleEndian.PutUint16(p[8:10], uint16(k))
	clear(p[10:16])

	off := 16
	for i := 0; i < k; i++ {
		d := deltas[i]
		binary.LittleEndian.PutUint64(p[off:off+8], d.GetEngineSeq())
		binary.LittleEndian.PutUint64(p[off+8:off+16], d.GetOrderId())
		binary.LittleEndian.PutUint64(p[off+16:off+24], d.GetPriceFp())
		binary.LittleEndian.PutUint64(p[off+24:off+32], d.GetRemainingQtyFp())
		p[off+32] = uint8(d.GetOp())
		p[off+33] = uint8(d.GetSide())
		binary.LittleEndian.PutUint16(p[off+34:off+36], 0)
		var signFlag int32
		if d.GetDeltaQtyFp() > 0 {
			signFlag = 1
		} else if d.GetDeltaQtyFp() < 0 {
			signFlag = -1
		}
		binary.LittleEndian.PutUint32(p[off+36:off+40], uint32(signFlag))
		off += 40
	}
	f.FinalizeFrame()
	return f
}

// EncodeL3SingleDelta encodes a single L3BookDelta without allocating a wrapper slice.
func EncodeL3SingleDelta(d *hyperionv1.L3BookDelta, instrumentID uint16) *PooledFrame {
	const payLen = 16 + 40
	f, p := AcquireFrame(0x2, MsgL3DeltaBatch, FlagCRC32CPresent, instrumentID, d.GetEngineSeq(), d.GetEventTsNs(), payLen)
	f.PrevSeq = d.GetPrevEngineSeq()

	binary.LittleEndian.PutUint64(p[0:8], d.GetPrevEngineSeq())
	binary.LittleEndian.PutUint16(p[8:10], 1)
	clear(p[10:16])

	off := 16
	binary.LittleEndian.PutUint64(p[off:off+8], d.GetEngineSeq())
	binary.LittleEndian.PutUint64(p[off+8:off+16], d.GetOrderId())
	binary.LittleEndian.PutUint64(p[off+16:off+24], d.GetPriceFp())
	binary.LittleEndian.PutUint64(p[off+24:off+32], d.GetRemainingQtyFp())
	p[off+32] = uint8(d.GetOp())
	p[off+33] = uint8(d.GetSide())
	binary.LittleEndian.PutUint16(p[off+34:off+36], 0)
	var signFlag int32
	if d.GetDeltaQtyFp() > 0 {
		signFlag = 1
	} else if d.GetDeltaQtyFp() < 0 {
		signFlag = -1
	}
	binary.LittleEndian.PutUint32(p[off+36:off+40], uint32(signFlag))
	f.FinalizeFrame()
	return f
}

// EncodeExecutionFill builds aligned 64-byte 0x04 MSG_EXECUTION_FILL.
func EncodeExecutionFill(fill *hyperionv1.ExecutionFill, instrumentID uint16) *PooledFrame {
	f, p := AcquireFrame(0x2, MsgExecutionFill, FlagCRC32CPresent, instrumentID, fill.GetEngineSeq(), fill.GetExecTsNs(), 64)
	binary.LittleEndian.PutUint64(p[0:8], fill.GetFillId())
	binary.LittleEndian.PutUint64(p[8:16], fill.GetMakerOrderId())
	binary.LittleEndian.PutUint64(p[16:24], fill.GetTakerOrderId())
	binary.LittleEndian.PutUint64(p[24:32], fill.GetMakerAccountId())
	binary.LittleEndian.PutUint64(p[32:40], fill.GetTakerAccountId())
	binary.LittleEndian.PutUint64(p[40:48], fill.GetPriceFp())
	binary.LittleEndian.PutUint64(p[48:56], fill.GetQtyFp())
	p[56] = uint8(fill.GetAggressorSide())
	clear(p[57:64])
	f.FinalizeFrame()
	return f
}

// EncodeCandleMicro builds 96-byte 0x05 MSG_CANDLE_MICRO combining OHLCV + MicrostructureMetrics.
func EncodeCandleMicro(candle *hyperionv1.OhlcvCandle, micro *hyperionv1.MicrostructureMetrics, instrumentID uint16) *PooledFrame {
	var seq, tsNS uint64
	if candle != nil {
		seq = candle.GetLastEngineSeq()
		tsNS = candle.GetBucketCloseTsNs()
	}
	if micro != nil {
		if micro.GetEngineSeq() > seq {
			seq = micro.GetEngineSeq()
		}
		if micro.GetTsNs() > tsNS {
			tsNS = micro.GetTsNs()
		}
	}
	f, p := AcquireFrame(0x2, MsgCandleMicro, FlagCRC32CPresent, instrumentID, seq, tsNS, 96)
	clear(p)
	if candle != nil {
		p[0] = uint8(candle.GetInterval())
		if candle.GetIsClosed() {
			p[1] = 1
		}
		binary.LittleEndian.PutUint64(p[8:16], candle.GetBucketOpenTsNs())
		binary.LittleEndian.PutUint64(p[16:24], candle.GetOpenFp())
		binary.LittleEndian.PutUint64(p[24:32], candle.GetHighFp())
		binary.LittleEndian.PutUint64(p[32:40], candle.GetLowFp())
		binary.LittleEndian.PutUint64(p[40:48], candle.GetCloseFp())
		if bv := candle.GetBaseVolumeFp(); bv != nil {
			binary.LittleEndian.PutUint64(p[48:56], bv.GetLo())
		}
		binary.LittleEndian.PutUint64(p[56:64], candle.GetVwapFp())
		binary.LittleEndian.PutUint64(p[88:96], candle.GetTradeCount())
	}
	if micro != nil {
		if candle == nil {
			binary.LittleEndian.PutUint64(p[56:64], micro.GetRollingVwapFp())
		}
		if ofi := micro.GetOfiFp(); ofi != nil {
			binary.LittleEndian.PutUint64(p[64:72], ofi.GetLo())
		}
		binary.LittleEndian.PutUint64(p[72:80], uint64(micro.GetBookImbalanceTop5Ppm()))
		binary.LittleEndian.PutUint64(p[80:88], uint64(micro.GetKylesLambdaFp()))
	}
	f.FinalizeFrame()
	return f
}

// EncodeFraudAlert builds 0x06 MSG_FRAUD_ALERT by marshaling AnomalyAlert directly into pooled memory.
func EncodeFraudAlert(alert *hyperionv1.AnomalyAlert, instrumentID uint16) (*PooledFrame, error) {
	payLen := proto.Size(alert)
	f, p := AcquireFrame(0x2, MsgFraudAlert, FlagCRC32CPresent, instrumentID, alert.GetEngineSeq(), alert.GetDetectedTsNs(), payLen)
	_, err := proto.MarshalOptions{}.MarshalAppend(p[:0], alert)
	if err != nil {
		f.Release()
		return nil, err
	}
	f.FinalizeFrame()
	return f, nil
}

// EncodeOrderAckPrivate builds 0x07 MSG_ORDER_ACK_PRIVATE directly into pooled memory.
func EncodeOrderAckPrivate(ack *hyperionv1.OrderAck, instrumentID uint16) (*PooledFrame, error) {
	payLen := proto.Size(ack)
	f, p := AcquireFrame(0x2, MsgOrderAckPrivate, FlagCRC32CPresent, instrumentID, ack.GetEngineSeq(), ack.GetEngineTsNs(), payLen)
	_, err := proto.MarshalOptions{}.MarshalAppend(p[:0], ack)
	if err != nil {
		f.Release()
		return nil, err
	}
	f.FinalizeFrame()
	return f, nil
}
