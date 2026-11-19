package ws_test

import (
	"encoding/binary"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"github.com/vanovri/hyperion/services/gateway-go/internal/ws"
	"google.golang.org/protobuf/proto"
)

func TestBinaryFraming_ByteExactComplianceAndProtocol1002Error(t *testing.T) {
	cfg := config.FromEnv()
	cfg.WSHeartbeatInterval = 0 // deterministic manual frames in unit test
	metrics := telemetry.NewRegistry()
	hub := ws.NewHub(cfg, nil, nil, metrics)
	t.Cleanup(hub.Close)

	var root [32]byte
	for i := range root {
		root[i] = byte(i + 1)
	}
	hub.CacheL2Snapshot(&hyperionv1.L2BookSnapshot{
		EngineSeq:       100,
		Symbol:          "BTC-USDT",
		InstrumentId:    1,
		BestBidFp:       6_842_500_000_000,
		BestAskFp:       6_842_600_000_000,
		SnapshotTsNs:    1_795_015_000_000_000_000,
		StateRootBlake3: root[:],
		Bids: []*hyperionv1.PriceLevel{
			{PriceFp: 6_842_500_000_000, TotalQtyFp: 250_000_000, OrderCount: 3},
		},
		Asks: []*hyperionv1.PriceLevel{
			{PriceFp: 6_842_600_000_000, TotalQtyFp: 150_000_000, OrderCount: 2},
		},
	})

	srv := httptest.NewServer(http.HandlerFunc(hub.ServeWS))
	t.Cleanup(srv.Close)
	addr := strings.TrimPrefix(srv.URL, "http://")

	// 1. Connect binary subscriber (`hyperion.bin.v1`) and subscribe via binary CMD_SUBSCRIBE (0x81).
	conn, br, err := ws.DialTestWS(addr, "/ws", "hyperion.bin.v1")
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	subPayload := ws.EncodeClientControl(ws.CmdSubscribe, ws.ChanL2|ws.ChanL3|ws.ChanFills|ws.ChanFraud, 1<<1, 0, uint64(time.Now().UnixNano()))
	if err := ws.WriteMaskedClientFrame(conn, 0x2, subPayload); err != nil {
		t.Fatal(err)
	}

	// Expect initial resync notice (0x08) + L2 Snapshot (0x02) with exact 56 + 2*20 = 96 byte payload!
	_ = conn.SetReadDeadline(time.Now().Add(3 * time.Second))
	op1, raw1, err := ws.ReadServerFrame(br)
	if err != nil || op1 != 0x2 {
		t.Fatalf("expected binary frame 1, op=%d err=%v", op1, err)
	}
	hdr1, _, err := ws.DecodeHeader(raw1)
	if err != nil || hdr1.MsgType != ws.MsgGapResyncNotice {
		t.Fatalf("expected MsgGapResyncNotice, got hdr=%+v err=%v", hdr1, err)
	}

	op2, raw2, err := ws.ReadServerFrame(br)
	if err != nil || op2 != 0x2 {
		t.Fatalf("expected binary L2 snapshot frame, op=%d err=%v", op2, err)
	}
	hdr2, pay2, err := ws.DecodeHeader(raw2)
	if err != nil {
		t.Fatalf("DecodeHeader L2 failed: %v", err)
	}
	if hdr2.MsgType != ws.MsgL2Snapshot || hdr2.InstrumentID != 1 || hdr2.EngineSeq != 100 || len(pay2) != 96 {
		t.Fatalf("unexpected L2 header/len: hdr=%+v len=%d", hdr2, len(pay2))
	}
	if gotBid := binary.LittleEndian.Uint64(pay2[0:8]); gotBid != 6_842_500_000_000 {
		t.Fatalf("best_bid_fp mismatch: %d", gotBid)
	}

	// 2. Broadcast L3 delta with sequence gap (prev_engine_seq=105 > local_seq=100) -> triggers automatic snapshot resync!
	hub.BroadcastL3Delta(&hyperionv1.L3BookDelta{
		EngineSeq:      106,
		PrevEngineSeq:  105, // gap from 100!
		Symbol:         "BTC-USDT",
		InstrumentId:   1,
		Op:             hyperionv1.L3DeltaOp_L3_DELTA_OP_ADD,
		Side:           hyperionv1.OrderSide_ORDER_SIDE_BUY,
		OrderId:        77,
		PriceFp:        6_842_400_000_000,
		RemainingQtyFp: 100_000_000,
		DeltaQtyFp:     100_000_000,
		EventTsNs:      uint64(time.Now().UnixNano()),
	})

	// Read delta frame (must carry FlagSlowConsumerDroppedPrior = 0x04 due to detected gap) + subsequent resync!
	_, rawDelta, err := ws.ReadServerFrame(br)
	if err != nil {
		t.Fatal(err)
	}
	hdrD, payD, err := ws.DecodeHeader(rawDelta)
	if err != nil || hdrD.MsgType != ws.MsgL3DeltaBatch || len(payD) != 56 {
		t.Fatalf("unexpected delta frame: hdr=%+v len=%d err=%v", hdrD, len(payD), err)
	}
	if (hdrD.Flags & ws.FlagSlowConsumerDroppedPrior) == 0 {
		t.Fatalf("expected FlagSlowConsumerDroppedPrior set on sequence gap, flags=0x%02x", hdrD.Flags)
	}

	// Read automatic resync notice + snapshot triggered by gap!
	_, rawNotice, err := ws.ReadServerFrame(br)
	if err != nil {
		t.Fatal(err)
	}
	hdrN, _, err := ws.DecodeHeader(rawNotice)
	if err != nil || hdrN.MsgType != ws.MsgGapResyncNotice {
		t.Fatalf("expected automatic MsgGapResyncNotice after gap, got %+v err=%v", hdrN, err)
	}
	_, rawResyncSnap, err := ws.ReadServerFrame(br)
	if err != nil {
		t.Fatal(err)
	}
	hdrRS, _, err := ws.DecodeHeader(rawResyncSnap)
	if err != nil || hdrRS.MsgType != ws.MsgL2Snapshot || (hdrRS.Flags&ws.FlagSnapshotResync) == 0 {
		t.Fatalf("expected FlagSnapshotResync L2 frame, got %+v err=%v", hdrRS, err)
	}

	// 3. Broadcast FraudAlert (0x06) and verify exact Protobuf roundtrip inside binary payload!
	hub.BroadcastFraudAlert(&hyperionv1.AnomalyAlert{
		AlertId:       "alert-spoof-9",
		EngineSeq:     107,
		Symbol:        "BTC-USDT",
		SeverityScore: 94,
	})
	_, rawAlert, err := ws.ReadServerFrame(br)
	if err != nil {
		t.Fatal(err)
	}
	hdrA, payA, err := ws.DecodeHeader(rawAlert)
	if err != nil || hdrA.MsgType != ws.MsgFraudAlert {
		t.Fatalf("expected MsgFraudAlert, got %+v err=%v", hdrA, err)
	}
	var decodedAlert hyperionv1.AnomalyAlert
	if err := proto.Unmarshal(payA, &decodedAlert); err != nil || decodedAlert.GetAlertId() != "alert-spoof-9" {
		t.Fatalf("AnomalyAlert protobuf decode failed: %v", err)
	}

	// 4. JSON fallback subscriber (`?format=json`) receives clean UTF-8 text frames!
	jConn, jBR, err := ws.DialTestWS(addr, "/ws?format=json", "hyperion.json.v1")
	if err != nil {
		t.Fatal(err)
	}
	defer jConn.Close()
	if err := ws.WriteMaskedClientFrame(jConn, 0x1, []byte(`{"op":"subscribe","channels":["fills"],"symbols":["BTC-USDT"]}`)); err != nil {
		t.Fatal(err)
	}
	time.Sleep(15 * time.Millisecond)
	hub.BroadcastExecutionFill(&hyperionv1.ExecutionFill{
		FillId:       88,
		EngineSeq:    108,
		Symbol:       "BTC-USDT",
		InstrumentId: 1,
		PriceFp:      6_842_550_000_000,
		QtyFp:        50_000_000,
	})
	_ = jConn.SetReadDeadline(time.Now().Add(2 * time.Second))
	jOp, jPay, err := ws.ReadServerFrame(jBR)
	if err != nil || jOp != 0x1 {
		t.Fatalf("expected text JSON frame (op=1), got op=%d err=%v", jOp, err)
	}
	var jMap map[string]any
	if err := json.Unmarshal(jPay, &jMap); err != nil || jMap["type"] != "execution_fill" {
		t.Fatalf("unexpected JSON fallback frame: %s", string(jPay))
	}

	// 5. Corrupted magic byte MUST close connection with RFC 6455 status 1002 Protocol Error!
	badConn, badBR, err := ws.DialTestWS(addr, "/ws", "hyperion.bin.v1")
	if err != nil {
		t.Fatal(err)
	}
	defer badConn.Close()
	corrupt := make([]byte, 48)
	binary.LittleEndian.PutUint16(corrupt[0:2], 0xDEAD) // wrong magic!
	corrupt[2] = 1
	_ = ws.WriteMaskedClientFrame(badConn, 0x2, corrupt)
	_ = badConn.SetReadDeadline(time.Now().Add(2 * time.Second))
	closeOp, closePay, err := ws.ReadServerFrame(badBR)
	if err != nil || closeOp != 0x8 || len(closePay) < 2 || binary.BigEndian.Uint16(closePay[:2]) != ws.CloseProtocolError {
		t.Fatalf("expected RFC 6455 Close 1002 Protocol Error, got op=%d pay=%x err=%v", closeOp, closePay, err)
	}
}
