package ws

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/auth"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/instruments"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
)

const numHubShards = 32

// SnapshotProvider fetches a fresh Level-2 order book snapshot on sequence-gap resync
// when the local hot cache is empty or older than client's last known sequence.
type SnapshotProvider interface {
	GetL2BookSnapshot(ctx context.Context, req *hyperionv1.GetL2BookSnapshotRequest) (*hyperionv1.GetL2BookSnapshotResponse, error)
}

type clientShard struct {
	mu      sync.RWMutex
	clients map[uint64]*Client
	_       [40]byte
}

type resyncTask struct {
	client          *Client
	instrumentID    uint16
	lastKnownEngSeq uint64
}

// Hub is the Zero-Copy Binary WebSocket Multiplexer fanning out market deltas,
// private execution/order events, and fraud alerts across tens of thousands of subscribers.
type Hub struct {
	cfg        config.Config
	jwtAuth    *auth.JWTVerifier
	snapSource SnapshotProvider
	metrics    *telemetry.Registry
	startTime  time.Time

	nextClientID atomic.Uint64
	shards       [numHubShards]clientShard

	// Per-account private subscriber index for O(1) private OrderAck / ExecutionFill routing.
	acctMu   sync.RWMutex
	byAcctID map[uint64]map[uint64]*Client

	// Hot in-memory L2 snapshot cache per instrument_id (1..4) for instant sequence-gap resync.
	snapMu    sync.RWMutex
	latestL2  [instruments.MaxInstrumentID + 1]*hyperionv1.L2BookSnapshot
	latestMic [instruments.MaxInstrumentID + 1]*hyperionv1.MicrostructureMetrics

	resyncCh chan resyncTask
	stopOnce sync.Once
	stopCh   chan struct{}
	bgWG     sync.WaitGroup
	clientWG sync.WaitGroup
}

// NewHub initializes and starts the sharded WebSocket fan-out engine.
func NewHub(cfg config.Config, jwtAuth *auth.JWTVerifier, snapSource SnapshotProvider, metrics *telemetry.Registry) *Hub {
	if cfg.WSMarketRingSize <= 0 {
		cfg.WSMarketRingSize = 256
	}
	if cfg.WSCriticalQueueSize <= 0 {
		cfg.WSCriticalQueueSize = 128
	}
	if cfg.WSMaxClients <= 0 {
		cfg.WSMaxClients = 65536
	}
	if cfg.WSMaxConsecutiveStalls == 0 {
		cfg.WSMaxConsecutiveStalls = 3
	}
	if metrics == nil {
		metrics = telemetry.Global
	}

	h := &Hub{
		cfg:        cfg,
		jwtAuth:    jwtAuth,
		snapSource: snapSource,
		metrics:    metrics,
		startTime:  time.Now(),
		byAcctID:   make(map[uint64]map[uint64]*Client, 256),
		resyncCh:   make(chan resyncTask, 4096),
		stopCh:     make(chan struct{}),
	}
	for i := 0; i < numHubShards; i++ {
		h.shards[i].clients = make(map[uint64]*Client, 256)
	}

	h.bgWG.Add(2)
	go h.heartbeatLoop()
	go h.resyncWorkerLoop()
	return h
}

// ServeWS upgrades HTTP GET /ws to a zero-copy RFC 6455 WebSocket subscriber.
func (h *Hub) ServeWS(w http.ResponseWriter, r *http.Request) {
	if int(h.metrics.WSConnectedClients.Load()) >= h.cfg.WSMaxClients {
		http.Error(w, "gateway max websocket capacity reached", http.StatusServiceUnavailable)
		return
	}

	var accountID uint64
	if h.jwtAuth != nil {
		if claims, err := h.jwtAuth.ExtractRequestClaims(r, time.Now()); err == nil {
			accountID = claims.AccountID
		} else if tok := r.URL.Query().Get("token"); tok != "" {
			if claims, err := h.jwtAuth.Verify(tok, time.Now()); err == nil {
				accountID = claims.AccountID
			}
		}
	}
	// SEC-03: Never trust unauthenticated X-Hyperion-Account-Id headers for private WS streams;
	// private account streams bind strictly to cryptographically verified JWT claims.

	conn, residual, format, err := UpgradeHTTP(w, r)
	if err != nil {
		return
	}

	cid := h.nextClientID.Add(1)
	c := &Client{
		id:            cid,
		accountID:     accountID,
		format:        format,
		conn:          conn,
		initialBuf:    residual,
		hub:           h,
		marketRing:    NewMarketRingBuffer(h.cfg.WSMarketRingSize),
		criticalQueue: make(chan *PooledFrame, h.cfg.WSCriticalQueueSize),
		wakeCh:        make(chan struct{}, 1),
		pongQueue:     make(chan []byte, 4),
		doneCh:        make(chan struct{}),
	}

	// Default subscription: if query supplies `?channels=all` or `?symbol=BTC-USDT`, auto-subscribe;
	// otherwise subscribe public channels for requested symbol if provided.
	if qSym := r.URL.Query().Get("symbol"); qSym != "" {
		if spec, ok := instruments.LookupSymbol(qSym); ok {
			c.channelMask.Store(ChanAllPublic)
			c.instrumentMask.Store(1 << spec.ID)
		}
	}

	h.registerClient(c)
	h.clientWG.Add(2)
	go c.writePump()
	go c.readPump()
}

// AttachConn registers an already-upgraded net.Conn (e.g., throttled net.Pipe WAN simulator) directly
// into the Hub with initial subscription masks.
func (h *Hub) AttachConn(conn net.Conn, format WireFormat, accountID uint64, chanMask, instMask uint32) *Client {
	cid := h.nextClientID.Add(1)
	c := &Client{
		id:            cid,
		accountID:     accountID,
		format:        format,
		conn:          conn,
		hub:           h,
		marketRing:    NewMarketRingBuffer(h.cfg.WSMarketRingSize),
		criticalQueue: make(chan *PooledFrame, h.cfg.WSCriticalQueueSize),
		wakeCh:        make(chan struct{}, 1),
		pongQueue:     make(chan []byte, 4),
		doneCh:        make(chan struct{}),
	}
	c.channelMask.Store(chanMask)
	c.instrumentMask.Store(instMask)
	h.registerClient(c)
	h.clientWG.Add(2)
	go c.writePump()
	go c.readPump()
	return c
}

func (h *Hub) registerClient(c *Client) {
	sh := &h.shards[c.id&(numHubShards-1)]
	sh.mu.Lock()
	sh.clients[c.id] = c
	sh.mu.Unlock()

	if c.accountID > 0 {
		h.acctMu.Lock()
		m := h.byAcctID[c.accountID]
		if m == nil {
			m = make(map[uint64]*Client, 2)
			h.byAcctID[c.accountID] = m
		}
		m[c.id] = c
		h.acctMu.Unlock()
	}
	h.metrics.WSConnectedClients.Add(1)
}

func (h *Hub) unregisterClient(c *Client) {
	sh := &h.shards[c.id&(numHubShards-1)]
	sh.mu.Lock()
	_, existed := sh.clients[c.id]
	delete(sh.clients, c.id)
	sh.mu.Unlock()

	if c.accountID > 0 {
		h.acctMu.Lock()
		if m := h.byAcctID[c.accountID]; m != nil {
			delete(m, c.id)
			if len(m) == 0 {
				delete(h.byAcctID, c.accountID)
			}
		}
		h.acctMu.Unlock()
	}
	if existed {
		h.metrics.WSConnectedClients.Add(-1)
	}
}

func (h *Hub) updateClientSubscriptions(c *Client, chMask, instMask uint32) {
	c.channelMask.Store(chMask)
	c.instrumentMask.Store(instMask)
	if (chMask & (ChanL2 | ChanL3)) != 0 {
		h.scheduleResyncForMask(c, instMask, 0)
	}
}

// CacheL2Snapshot updates the hot in-memory L2 snapshot for an instrument and updates LatestEngineSeq.
func (h *Hub) CacheL2Snapshot(snap *hyperionv1.L2BookSnapshot) uint16 {
	if snap == nil {
		return 0
	}
	instID := uint16(snap.GetInstrumentId())
	if instID == 0 {
		if spec, ok := instruments.LookupSymbol(snap.GetSymbol()); ok {
			instID = spec.ID
		}
	}
	if instID > 0 && instID <= instruments.MaxInstrumentID {
		h.snapMu.Lock()
		h.latestL2[instID] = snap
		h.snapMu.Unlock()
	}
	h.advanceEngineSeq(snap.GetEngineSeq())
	return instID
}

func (h *Hub) advanceEngineSeq(seq uint64) {
	for {
		cur := h.metrics.LatestEngineSeq.Load()
		if seq <= cur || h.metrics.LatestEngineSeq.CompareAndSwap(cur, seq) {
			return
		}
	}
}

// BroadcastL2Snapshot fans out an L2BookSnapshot to all subscribers of ChanL2 on that instrument.
func (h *Hub) BroadcastL2Snapshot(snap *hyperionv1.L2BookSnapshot) {
	instID := h.CacheL2Snapshot(snap)
	if instID == 0 {
		return
	}
	binFrame := EncodeL2Snapshot(snap, instID, 0)
	var jsonFrame *PooledFrame
	h.fanOutPublic(ChanL2, instID, true, binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw, _ := json.Marshal(map[string]any{
				"type":         "l2_snapshot",
				"symbol":       snap.GetSymbol(),
				"engine_seq":   strconv.FormatUint(snap.GetEngineSeq(), 10),
				"best_bid_fp":  strconv.FormatUint(snap.GetBestBidFp(), 10),
				"best_ask_fp":  strconv.FormatUint(snap.GetBestAskFp(), 10),
				"bid_levels":   len(snap.GetBids()),
				"ask_levels":   len(snap.GetAsks()),
			})
			jsonFrame = AcquireTextFrame(raw, MsgL2Snapshot, instID, snap.GetEngineSeq())
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// BroadcastL3Delta fans out a single L3BookDelta using zero-copy PooledFrame retention.
func (h *Hub) BroadcastL3Delta(delta *hyperionv1.L3BookDelta) {
	if delta == nil {
		return
	}
	instID := resolveInstID(uint16(delta.GetInstrumentId()), delta.GetSymbol())
	if instID == 0 {
		return
	}
	h.advanceEngineSeq(delta.GetEngineSeq())
	binFrame := EncodeL3SingleDelta(delta, instID)
	var jsonFrame *PooledFrame
	h.fanOutPublic(ChanL3, instID, false, binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"l3_delta","symbol":%q,"engine_seq":"%d","prev_engine_seq":"%d","order_id":"%d","price_fp":"%d","remaining_qty_fp":"%d"}`,
				delta.GetSymbol(), delta.GetEngineSeq(), delta.GetPrevEngineSeq(), delta.GetOrderId(), delta.GetPriceFp(), delta.GetRemainingQtyFp())
			jsonFrame = AcquireTextFrame([]byte(raw), MsgL3DeltaBatch, instID, delta.GetEngineSeq())
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// BroadcastL3DeltaBatch fans out a packed batch of 1..N L3BookDeltas for an instrument.
func (h *Hub) BroadcastL3DeltaBatch(deltas []*hyperionv1.L3BookDelta, instID uint16) {
	if len(deltas) == 0 {
		return
	}
	if instID == 0 {
		instID = resolveInstID(uint16(deltas[0].GetInstrumentId()), deltas[0].GetSymbol())
	}
	if instID == 0 {
		return
	}
	last := deltas[len(deltas)-1]
	h.advanceEngineSeq(last.GetEngineSeq())
	binFrame := EncodeL3DeltaBatch(deltas, instID)
	var jsonFrame *PooledFrame
	h.fanOutPublic(ChanL3, instID, false, binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"l3_delta_batch","symbol":%q,"engine_seq":"%d","count":%d}`,
				last.GetSymbol(), last.GetEngineSeq(), len(deltas))
			jsonFrame = AcquireTextFrame([]byte(raw), MsgL3DeltaBatch, instID, last.GetEngineSeq())
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// BroadcastExecutionFill fans out public trade fills AND routes private execution notifications
// directly to maker_account_id and taker_account_id subscribers.
func (h *Hub) BroadcastExecutionFill(fill *hyperionv1.ExecutionFill) {
	if fill == nil {
		return
	}
	instID := resolveInstID(uint16(fill.GetInstrumentId()), fill.GetSymbol())
	if instID == 0 {
		return
	}
	h.advanceEngineSeq(fill.GetEngineSeq())
	binFrame := EncodeExecutionFill(fill, instID)
	var jsonFrame *PooledFrame
	makeJSON := func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"execution_fill","symbol":%q,"fill_id":"%d","engine_seq":"%d","price_fp":"%d","qty_fp":"%d"}`,
				fill.GetSymbol(), fill.GetFillId(), fill.GetEngineSeq(), fill.GetPriceFp(), fill.GetQtyFp())
			jsonFrame = AcquireTextFrame([]byte(raw), MsgExecutionFill, instID, fill.GetEngineSeq())
		}
		return jsonFrame
	}

	h.fanOutPublic(ChanFills, instID, true, binFrame, makeJSON)

	// Private fill notification to maker and taker accounts if subscribed to ChanPrivateOrders.
	if maker := fill.GetMakerAccountId(); maker > 0 {
		h.deliverToAccount(maker, binFrame, makeJSON)
	}
	if taker := fill.GetTakerAccountId(); taker > 0 && taker != fill.GetMakerAccountId() {
		h.deliverToAccount(taker, binFrame, makeJSON)
	}

	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// BroadcastCandleMicro fans out candlestick + microstructure updates on the droppable ring buffer.
func (h *Hub) BroadcastCandleMicro(candle *hyperionv1.OhlcvCandle, micro *hyperionv1.MicrostructureMetrics) {
	var sym string
	if candle != nil {
		sym = candle.GetSymbol()
	} else if micro != nil {
		sym = micro.GetSymbol()
	}
	spec, ok := instruments.LookupSymbol(sym)
	if !ok {
		return
	}
	if micro != nil {
		h.snapMu.Lock()
		h.latestMic[spec.ID] = micro
		h.snapMu.Unlock()
	}
	binFrame := EncodeCandleMicro(candle, micro, spec.ID)
	var jsonFrame *PooledFrame
	h.fanOutPublic(ChanCandles, spec.ID, false, binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"candle_micro","symbol":%q,"engine_seq":"%d"}`, sym, binFrame.EngineSeq)
			jsonFrame = AcquireTextFrame([]byte(raw), MsgCandleMicro, spec.ID, binFrame.EngineSeq)
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// BroadcastFraudAlert fans out Sentinel-ML AnomalyAlert on the critical channel.
func (h *Hub) BroadcastFraudAlert(alert *hyperionv1.AnomalyAlert) {
	if alert == nil {
		return
	}
	instID := resolveInstID(0, alert.GetSymbol())
	binFrame, err := EncodeFraudAlert(alert, instID)
	if err != nil {
		return
	}
	var jsonFrame *PooledFrame
	h.fanOutPublic(ChanFraud, instID, true, binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"fraud_alert","alert_id":%q,"symbol":%q,"severity_score":%d,"engine_seq":"%d"}`,
				alert.GetAlertId(), alert.GetSymbol(), alert.GetSeverityScore(), alert.GetEngineSeq())
			jsonFrame = AcquireTextFrame([]byte(raw), MsgFraudAlert, instID, alert.GetEngineSeq())
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

// SendPrivateOrderAck delivers MSG_ORDER_ACK_PRIVATE exclusively to the owning account's critical queue.
func (h *Hub) SendPrivateOrderAck(ack *hyperionv1.OrderAck) {
	if ack == nil || ack.GetAccountId() == 0 {
		return
	}
	instID := resolveInstID(uint16(ack.GetInstrumentId()), ack.GetSymbol())
	binFrame, err := EncodeOrderAckPrivate(ack, instID)
	if err != nil {
		return
	}
	var jsonFrame *PooledFrame
	h.deliverToAccount(ack.GetAccountId(), binFrame, func() *PooledFrame {
		if jsonFrame == nil {
			raw := fmt.Sprintf(`{"type":"order_ack","order_id":"%d","client_order_id":%q,"status":%q,"engine_seq":"%d"}`,
				ack.GetOrderId(), ack.GetClientOrderId(), ack.GetStatus().String(), ack.GetEngineSeq())
			jsonFrame = AcquireTextFrame([]byte(raw), MsgOrderAckPrivate, instID, ack.GetEngineSeq())
		}
		return jsonFrame
	})
	if jsonFrame != nil {
		jsonFrame.Release()
	}
	binFrame.Release()
}

func (h *Hub) fanOutPublic(chanBit uint32, instID uint16, critical bool, binFrame *PooledFrame, lazyJSON func() *PooledFrame) {
	h.metrics.WSMessagesBroadcastTotal.Add(1)
	instBit := uint32(0xFFFFFFFF)
	if instID > 0 && instID < 32 {
		instBit = 1 << instID
	}

	var toEvict []*Client
	for s := 0; s < numHubShards; s++ {
		sh := &h.shards[s]
		sh.mu.RLock()
		for _, c := range sh.clients {
			if (c.channelMask.Load() & chanBit) == 0 {
				continue
			}
			if instID > 0 && (c.instrumentMask.Load()&instBit) == 0 {
				continue
			}
			var target *PooledFrame
			if c.format == FormatJSON {
				target = lazyJSON()
			} else {
				target = binFrame
			}
			target.Retain(1)
			if critical {
				if !c.EnqueueCritical(target) {
					toEvict = append(toEvict, c)
				}
			} else {
				c.EnqueueMarket(target)
			}
		}
		sh.mu.RUnlock()
	}

	for _, c := range toEvict {
		h.metrics.WSSlowConsumerEvictsTotal.Add(1)
		c.CloseWithStatus(ClosePolicyViolation, "slow consumer critical queue overflow")
	}
}

func (h *Hub) deliverToAccount(accountID uint64, binFrame *PooledFrame, lazyJSON func() *PooledFrame) {
	var toEvict []*Client
	h.acctMu.RLock()
	subs := h.byAcctID[accountID]
	for _, c := range subs {
		if (c.channelMask.Load() & ChanPrivateOrders) == 0 {
			continue
		}
		var target *PooledFrame
		if c.format == FormatJSON {
			target = lazyJSON()
		} else {
			target = binFrame
		}
		target.Retain(1)
		if !c.EnqueueCritical(target) {
			toEvict = append(toEvict, c)
		}
	}
	h.acctMu.RUnlock()

	for _, c := range toEvict {
		h.metrics.WSSlowConsumerEvictsTotal.Add(1)
		c.CloseWithStatus(ClosePolicyViolation, "slow consumer critical queue overflow")
	}
}

func (h *Hub) scheduleResyncForMask(c *Client, instMask uint32, lastKnownSeq uint64) {
	for id := uint16(1); id <= instruments.MaxInstrumentID; id++ {
		if (instMask & (1 << id)) == 0 {
			continue
		}
		select {
		case h.resyncCh <- resyncTask{client: c, instrumentID: id, lastKnownEngSeq: lastKnownSeq}:
		default:
		}
	}
}

func (h *Hub) resyncWorkerLoop() {
	defer h.bgWG.Done()
	for {
		select {
		case <-h.stopCh:
			return
		case task := <-h.resyncCh:
			if task.client == nil || task.client.closed.Load() {
				continue
			}
			h.executeSnapshotResync(task)
		}
	}
}

func (h *Hub) executeSnapshotResync(task resyncTask) {
	spec, ok := instruments.LookupID(task.instrumentID)
	if !ok {
		return
	}

	h.snapMu.RLock()
	snap := h.latestL2[task.instrumentID]
	h.snapMu.RUnlock()

	if (snap == nil || (task.lastKnownEngSeq > 0 && snap.GetEngineSeq() <= task.lastKnownEngSeq)) && h.snapSource != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
		resp, err := h.snapSource.GetL2BookSnapshot(ctx, &hyperionv1.GetL2BookSnapshotRequest{
			Symbol: spec.Symbol,
			Depth:  50,
		})
		cancel()
		if err == nil && resp.GetSnapshot() != nil {
			snap = resp.GetSnapshot()
			h.CacheL2Snapshot(snap)
		}
	}
	if snap == nil {
		// Deterministic empty baseline snapshot if ledger has not yet emitted trades.
		snap = &hyperionv1.L2BookSnapshot{
			EngineSeq:    h.metrics.LatestEngineSeq.Load(),
			Symbol:       spec.Symbol,
			InstrumentId: uint32(spec.ID),
			SnapshotTsNs: uint64(time.Now().UnixNano()),
		}
	}

	h.metrics.WSSnapshotResyncsTotal.Add(1)
	nowNS := uint64(time.Now().UnixNano())
	if task.client.format == FormatBinary {
		notice := EncodeGapResyncNotice(task.instrumentID, task.lastKnownEngSeq, snap.GetEngineSeq(), nowNS)
		if !task.client.EnqueueCritical(notice) {
			h.metrics.WSSlowConsumerEvictsTotal.Add(1)
			task.client.CloseWithStatus(ClosePolicyViolation, "slow consumer resync queue overflow")
			return
		}

		snapFrame := EncodeL2Snapshot(snap, task.instrumentID, FlagSnapshotResync)
		if !task.client.EnqueueCritical(snapFrame) {
			h.metrics.WSSlowConsumerEvictsTotal.Add(1)
			task.client.CloseWithStatus(ClosePolicyViolation, "slow consumer resync queue overflow")
			return
		}
	} else {
		raw := fmt.Sprintf(`{"type":"l2_snapshot_resync","symbol":%q,"engine_seq":"%d"}`, snap.GetSymbol(), snap.GetEngineSeq())
		if !task.client.EnqueueCritical(AcquireTextFrame([]byte(raw), MsgL2Snapshot, task.instrumentID, snap.GetEngineSeq())) {
			h.metrics.WSSlowConsumerEvictsTotal.Add(1)
			task.client.CloseWithStatus(ClosePolicyViolation, "slow consumer resync queue overflow")
		}
	}
}

func (h *Hub) heartbeatLoop() {
	defer h.bgWG.Done()
	if h.cfg.WSHeartbeatInterval <= 0 {
		return
	}
	ticker := time.NewTicker(h.cfg.WSHeartbeatInterval)
	defer ticker.Stop()
	for {
		select {
		case <-h.stopCh:
			return
		case <-ticker.C:
			nowNS := uint64(time.Now().UnixNano())
			monoNS := uint64(time.Since(h.startTime).Nanoseconds())
			connected := uint32(h.metrics.WSConnectedClients.Load())
			hb := EncodeHeartbeat(monoNS, connected, 0, nowNS)
			var toEvict []*Client
			for s := 0; s < numHubShards; s++ {
				sh := &h.shards[s]
				sh.mu.RLock()
				for _, c := range sh.clients {
					if c.format == FormatBinary {
						hb.Retain(1)
						if !c.EnqueueCritical(hb) {
							toEvict = append(toEvict, c)
						}
					}
				}
				sh.mu.RUnlock()
			}
			hb.Release()
			for _, c := range toEvict {
				h.metrics.WSSlowConsumerEvictsTotal.Add(1)
				c.CloseWithStatus(ClosePolicyViolation, "slow consumer heartbeat queue overflow")
			}
		}
	}
}

// Close disconnects all active WebSocket subscribers and waits for every pump & worker goroutine
// to terminate cleanly (zero goroutine leak guarantee).
func (h *Hub) Close() {
	h.stopOnce.Do(func() {
		close(h.stopCh)
	})
	h.bgWG.Wait()

	var all []*Client
	for s := 0; s < numHubShards; s++ {
		sh := &h.shards[s]
		sh.mu.RLock()
		for _, c := range sh.clients {
			all = append(all, c)
		}
		sh.mu.RUnlock()
	}
	for _, c := range all {
		c.CloseWithStatus(CloseNormalClosure, "gateway shutting down")
	}
	h.clientWG.Wait()
}

func resolveInstID(rawID uint16, symbol string) uint16 {
	if rawID > 0 && rawID <= instruments.MaxInstrumentID {
		return rawID
	}
	if spec, ok := instruments.LookupSymbol(symbol); ok {
		return spec.ID
	}
	return 0
}
