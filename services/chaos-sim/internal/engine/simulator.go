package engine

import (
	"context"
	"fmt"
	"runtime"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/auditor"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/chaos"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/fixed"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/mockexchange"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/transport"
)

type BackendMode string

const (
	BackendMock        BackendMode = "mock"
	BackendGatewayHTTP BackendMode = "gateway-http"
	BackendLedgerGRPC  BackendMode = "ledger-grpc"
)

type SimulatorConfig struct {
	MasterSeed        uint64
	Symbol            string
	TotalBots         int // Default 1024 (>= 1,000 heterogeneous bots)
	WorkerShards      int // Default runtime.NumCPU()
	TickInterval      time.Duration
	AuditInterval     time.Duration
	Backend           BackendMode
	GatewayHTTPURL    string
	GatewayWSURL      string
	LedgerGRPCAddr    string
	AuthScheme        transport.AuthScheme
	InitialQuoteUnits int64 // Whole quote units per bot (e.g. 2_000_000 USDT)
	InitialBaseUnits  int64 // Whole base units per bot (e.g. 50 BTC)
	EnableAuditTrack  bool  // Tracks every order/fill in InvariantVerifier
}

func DefaultSimulatorConfig() SimulatorConfig {
	workers := runtime.NumCPU()
	if workers < 4 {
		workers = 4
	}
	return SimulatorConfig{
		MasterSeed:        0x4859504552494F4E, // "HYPERION" deterministic seed
		Symbol:            "BTC-USDT",
		TotalBots:         1024,
		WorkerShards:      workers,
		TickInterval:      20 * time.Millisecond,
		AuditInterval:     2 * time.Second,
		Backend:           BackendMock,
		GatewayHTTPURL:    "http://127.0.0.1:8080",
		GatewayWSURL:      "ws://127.0.0.1:8080/ws",
		LedgerGRPCAddr:    "127.0.0.1:50151",
		AuthScheme:        transport.AuthEd25519,
		InitialQuoteUnits: 2_500_000,
		InitialBaseUnits:  40,
		EnableAuditTrack:  true,
	}
}

type SyndicateAttackType string

const (
	AttackWashRing        SyndicateAttackType = "WASH_TRADING_RING"
	AttackLayeringSpoofer SyndicateAttackType = "LAYERING_SPOOFER"
	AttackFlashSweep      SyndicateAttackType = "FLASH_CRASH_SWEEP"
)

type SyndicateAttackRequest struct {
	AttackType SyndicateAttackType `json:"attack_type"`
	RingSize   int                 `json:"ring_size"` // 3..5 accounts for circular wash ring
	Rounds     int                 `json:"rounds"`    // Number of burst rounds to execute immediately
	Lots       int64               `json:"lots"`      // Size per leg in LotSize multiples
}

type SyndicateAttackResult struct {
	AttackType        SyndicateAttackType `json:"attack_type"`
	OrdersDispatched  int                 `json:"orders_dispatched"`
	FillsMatched      int                 `json:"fills_matched"`
	ParticipantIDs    []string            `json:"participant_account_ids"`
	ExecutedAtNs      int64               `json:"executed_at_ns"`
}

type TelemetrySnapshot struct {
	TimestampNs       int64                    `json:"timestamp_ns"`
	MasterSeed        string                   `json:"master_seed"`
	Symbol            string                   `json:"symbol"`
	Backend           BackendMode              `json:"backend"`
	CampaignID        string                   `json:"campaign_id"`
	Active            bool                     `json:"active"`
	Regime            model.MarketRegime       `json:"regime"`
	TotalBots         int                      `json:"total_bots"`
	MakerBots         int                      `json:"maker_bots"`
	QuantBots         int                      `json:"quant_bots"`
	NoiseBots         int                      `json:"noise_bots"`
	SyndicateBots     int                      `json:"syndicate_bots"`
	OrdersSubmitted   uint64                   `json:"orders_submitted"`
	OrdersAccepted    uint64                   `json:"orders_accepted"`
	OrdersRejected    uint64                   `json:"orders_rejected"`
	OrdersDeduped     uint64                   `json:"orders_deduped"`
	CancelsSubmitted  uint64                   `json:"cancels_submitted"`
	TradesMatched     uint64                   `json:"trades_matched"`
	SyndicateAttacks  uint64                   `json:"syndicate_attacks"`
	OrdersPerSec      uint64                   `json:"orders_per_sec"`
	TradesPerSec      uint64                   `json:"trades_per_sec"`
	LatencyP50Us      uint64                   `json:"latency_p50_us"`
	LatencyP95Us      uint64                   `json:"latency_p95_us"`
	LatencyP99Us      uint64                   `json:"latency_p99_us"`
	MidPriceFP        string                   `json:"mid_price_fp"`
	BestBidFP         string                   `json:"best_bid_fp"`
	BestAskFP         string                   `json:"best_ask_fp"`
	VWAPFP            string                   `json:"vwap_fp"`
	Faults            chaos.FaultStatsSnapshot `json:"faults"`
	SlowLoris         chaos.SlowLorisStats     `json:"slow_loris"`
	InvariantPassed   bool                     `json:"invariant_passed"`
	InvariantAudits   uint64                   `json:"invariant_audits"`
	ViolationCount    int                      `json:"violation_count"`
}

const latRingSize = 1024

// workerShard isolates per-worker bots, deterministic fault PRNG, mutex, and fault
// counters onto dedicated CPU L1/L2 cache lines to prevent cross-core false sharing.
type workerShard struct {
	mu       sync.Mutex
	bots     []*agents.BotAccount
	faultRNG rng.Stream
	counters chaos.LocalFaultCounters
	_     [64]byte
}

// Simulator orchestrates 1,000+ heterogeneous trading agents, adversarial syndicate rings,
// deterministic chaos campaigns, slow-loris WS consumers, and continuous double-entry auditing.
type Simulator struct {
	cfg            SimulatorConfig
	strategyParams agents.StrategyParams

	bots     []*agents.BotAccount
	botsByID map[uint32]*agents.BotAccount
	shards   []workerShard

	// Dedicated syndicate pools
	washRing []*agents.BotAccount
	spoofers []*agents.BotAccount
	sweepers []*agents.BotAccount
	washLeg  atomic.Uint64

	mockEx     *mockexchange.Exchange
	httpClient *transport.GatewayHTTPClient
	grpcClient *transport.LedgerGRPCClient
	verifier   *auditor.InvariantVerifier
	slowLoris  *chaos.SlowLorisPool
	faultStats chaos.FaultCounters

	campaignPtr atomic.Pointer[chaos.CampaignProfile]
	campaignID  atomic.Pointer[string]
	campaignSeq atomic.Uint64
	simActive   atomic.Bool

	// Cache-line isolated atomic counters (zero cross-core false sharing)
	ordersSubmitted  atomic.Uint64
	_       [56]byte
	ordersAccepted   atomic.Uint64
	_       [56]byte
	ordersRejected   atomic.Uint64
	_       [56]byte
	ordersDeduped    atomic.Uint64
	_       [56]byte
	cancelsSubmitted atomic.Uint64
	_       [56]byte
	tradesMatched    atomic.Uint64
	_       [56]byte
	syndicateAttacks atomic.Uint64
	_       [56]byte
	ordersPerSec     atomic.Uint64
	tradesPerSec     atomic.Uint64

	latMu        sync.Mutex
	latRing      [latRingSize]uint64
	latPos       int
	latFill      int
	detIntentBuf []model.OrderIntent // Reusable zero-alloc batch buffer for deterministic stepping

	runMu     sync.Mutex
	auditRW   sync.RWMutex // Linearizable snapshot barrier between parallel workers (RLock) and auditor (Lock)
	runCancel context.CancelFunc
	runWG     sync.WaitGroup

	// Optional live SSE listener hook
	subMu       sync.RWMutex
	subscribers map[uint64]chan TelemetrySnapshot
	nextSubID   atomic.Uint64

	makerCount int
	quantCount int
	noiseCount int
	syndCount  int
}

func NewSimulator(cfg SimulatorConfig) (*Simulator, error) {
	if cfg.TotalBots < 24 {
		cfg.TotalBots = 24
	}
	if cfg.WorkerShards < 1 {
		cfg.WorkerShards = 4
	}
	if cfg.Symbol == "" {
		cfg.Symbol = "BTC-USDT"
	}
	if cfg.TickInterval <= 0 {
		cfg.TickInterval = 20 * time.Millisecond
	}
	if cfg.AuditInterval <= 0 {
		cfg.AuditInterval = 2 * time.Second
	}
	if cfg.InitialQuoteUnits <= 0 {
		cfg.InitialQuoteUnits = 2_500_000
	}
	if cfg.InitialBaseUnits <= 0 {
		cfg.InitialBaseUnits = 40
	}

	spec := model.LookupInstrument(cfg.Symbol)
	initQuote := fixed.MustFromUnits(cfg.InitialQuoteUnits)
	initBase := fixed.MustFromUnits(cfg.InitialBaseUnits)

	sim := &Simulator{
		cfg:            cfg,
		strategyParams: agents.DefaultStrategyParams(),
		bots:           make([]*agents.BotAccount, 0, cfg.TotalBots),
		botsByID:       make(map[uint32]*agents.BotAccount, cfg.TotalBots),
		shards:         make([]workerShard, cfg.WorkerShards),
		verifier:       auditor.NewInvariantVerifier(),
		slowLoris:      chaos.NewSlowLorisPool(cfg.GatewayWSURL, ""),
		subscribers:    make(map[uint64]chan TelemetrySnapshot),
		detIntentBuf:   make([]model.OrderIntent, 0, 1024),
	}

	for w := 0; w < cfg.WorkerShards; w++ {
		sim.shards[w].faultRNG = rng.NewStream(cfg.MasterSeed^0xCAFEBABE, uint64(w)+10_000)
	}

	defProf := chaos.DefaultCalmProfile()
	sim.campaignPtr.Store(&defProf)
	initCampID := "camp-calm-0"
	sim.campaignID.Store(&initCampID)

	exCfg := mockexchange.DefaultExchangeConfig()
	exCfg.Symbol = spec.Symbol
	exCfg.InitialMid = spec.DefaultMidFP
	exCfg.TickSize = spec.TickSizeFP
	exCfg.LotSize = spec.LotSizeFP

	sim.mockEx = mockexchange.NewExchange(exCfg, func(mf model.FillEvent) {
		if bot, ok := sim.botsByID[mf.BotID]; ok {
			bot.Mu.Lock()
			bot.ApplyAsyncFill(mf)
			bot.Mu.Unlock()
		}
		if sim.cfg.EnableAuditTrack {
			sim.verifier.RecordAsyncFill(mf)
		}
	})

	// Partition population:
	// ~12.5% Avellaneda-Stoikov Market Makers, ~31% Quants (half Momentum, half Mean-Reversion),
	// 16 Syndicate Bots (5 Wash Ring, 5 Layering Spoofers, 6 Flash Sweepers), remainder Hawkes Noise.
	syndTotal := 16
	if cfg.TotalBots < 64 {
		syndTotal = 11 // 5 wash + 3 spoof + 3 sweep
	}
	regularTotal := cfg.TotalBots - syndTotal
	makerN := regularTotal / 8
	if makerN < 4 {
		makerN = 4
	}
	quantN := (regularTotal * 5) / 16
	if quantN < 4 {
		quantN = 4
	}
	noiseN := regularTotal - makerN - quantN

	var botID uint32
	addBot := func(prefix string, arch model.BotArchetype, isSyndicate bool) *agents.BotAccount {
		b := agents.NewBotAccount(cfg.MasterSeed, botID, prefix, arch, initQuote, initBase)
		sim.bots = append(sim.bots, b)
		sim.botsByID[botID] = b
		if !isSyndicate {
			shardIdx := int(botID) % cfg.WorkerShards
			sim.shards[shardIdx].bots = append(sim.shards[shardIdx].bots, b)
		}
		sim.mockEx.SeedAccount(b.AccountID, b.BotID, initQuote, initBase)
		if cfg.EnableAuditTrack {
			sim.verifier.RegisterAccount(b.AccountID, initQuote, initBase)
		}
		botID++
		return b
	}

	for i := 0; i < makerN; i++ {
		addBot("mm-as", model.ArchetypeAvellanedaStoikov, false)
	}
	for i := 0; i < quantN; i++ {
		if i%2 == 0 {
			addBot("quant-mom", model.ArchetypeMomentumQuant, false)
		} else {
			addBot("quant-mr", model.ArchetypeMeanRevQuant, false)
		}
	}
	for i := 0; i < noiseN; i++ {
		addBot("retail-hawkes", model.ArchetypeHawkesNoise, false)
	}

	washN := 5
	spoofN := (syndTotal - washN) / 2
	sweepN := syndTotal - washN - spoofN
	for i := 0; i < washN; i++ {
		b := addBot("synd-wash", model.ArchetypeSyndicateWash, true)
		b.RingIndex = uint8(i)
		b.RingSize = uint8(washN)
		sim.washRing = append(sim.washRing, b)
	}
	for i := 0; i < spoofN; i++ {
		b := addBot("synd-spoof", model.ArchetypeSyndicateSpoofer, true)
		sim.spoofers = append(sim.spoofers, b)
	}
	for i := 0; i < sweepN; i++ {
		b := addBot("synd-sweep", model.ArchetypeSyndicateSweep, true)
		sim.sweepers = append(sim.sweepers, b)
	}

	sim.makerCount = makerN
	sim.quantCount = quantN
	sim.noiseCount = noiseN
	sim.syndCount = syndTotal

	if cfg.Backend == BackendGatewayHTTP {
		sim.httpClient = transport.NewGatewayHTTPClient(cfg.GatewayHTTPURL, cfg.AuthScheme, cfg.WorkerShards*64)
	} else if cfg.Backend == BackendLedgerGRPC {
		gc, err := transport.NewLedgerGRPCClient(cfg.LedgerGRPCAddr, cfg.WorkerShards)
		if err != nil {
			return nil, err
		}
		sim.grpcClient = gc
	}

	return sim, nil
}

func (s *Simulator) Verifier() *auditor.InvariantVerifier { return s.verifier }
func (s *Simulator) MockExchange() *mockexchange.Exchange  { return s.mockEx }
func (s *Simulator) Bots() []*agents.BotAccount            { return s.bots }

// Start launches the autonomous multi-agent worker shards, throughput sampler, and continuous invariant auditor.
func (s *Simulator) Start() {
	s.runMu.Lock()
	defer s.runMu.Unlock()
	if s.runCancel != nil {
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	s.runCancel = cancel
	s.simActive.Store(true)

	for w := 0; w < s.cfg.WorkerShards; w++ {
		s.runWG.Add(1)
		go s.workerLoop(ctx, w)
	}

	s.runWG.Add(1)
	go s.syndicateLoop(ctx)

	s.runWG.Add(1)
	go s.telemetryAndAuditLoop(ctx)
}

// Stop cleanly shuts down all worker goroutines, slow-loris sockets, and gRPC pools with zero leaks.
func (s *Simulator) Stop() {
	s.runMu.Lock()
	cancel := s.runCancel
	s.runCancel = nil
	s.runMu.Unlock()

	s.simActive.Store(false)
	if cancel != nil {
		cancel()
	}
	s.runWG.Wait()
	s.slowLoris.Stop()
	if s.grpcClient != nil {
		_ = s.grpcClient.Close()
	}
}

// ApplyCampaign updates the live market regime, fault-injection knobs, and slow-loris WS swarm atomically.
func (s *Simulator) ApplyCampaign(prof chaos.CampaignProfile, enabled bool) string {
	if !enabled {
		prof = chaos.DefaultCalmProfile()
	}
	c := prof
	s.campaignPtr.Store(&c)
	s.mockEx.SetRegime(c.Regime)

	seq := s.campaignSeq.Add(1)
	cid := fmt.Sprintf("camp-%s-%d", strings.ToLowerSafe(string(c.Regime)), seq)
	s.campaignID.Store(&cid)

	if c.SlowLorisConns > 0 && enabled {
		_ = s.slowLoris.Configure(s.cfg.GatewayWSURL, c.SlowLorisConns, c.SlowLorisStallMs)
	} else {
		s.slowLoris.Stop()
	}
	return cid
}

var strings = struct {
	ToLowerSafe func(string) string
}{
	ToLowerSafe: func(in string) string {
		b := make([]byte, len(in))
		for i := 0; i < len(in); i++ {
			ch := in[i]
			if ch >= 'A' && ch <= 'Z' {
				ch += 'a' - 'A'
			} else if ch == '_' {
				ch = '-'
			}
			b[i] = ch
		}
		return string(b)
	},
}

func (s *Simulator) CurrentCampaign() chaos.CampaignProfile {
	p := s.campaignPtr.Load()
	if p == nil {
		return chaos.DefaultCalmProfile()
	}
	return *p
}

// StepDeterministic executes a single 100% deterministic simulation step in canonical BotID order.
// Given the same MasterSeed and sequence of regimes, produces bit-for-bit identical order & fill streams.
func (s *Simulator) StepDeterministic(logicalNowNs int64) int {
	prof := s.campaignPtr.Load()
	params := s.strategyParams
	params.BurstMultiplier = prof.BurstMultiplier

	snap := s.currentMarketSnapshotValue(prof.Regime)
	intents := s.detIntentBuf[:0]

	for _, bot := range s.bots {
		if bot.Archetype == model.ArchetypeSyndicateWash ||
			bot.Archetype == model.ArchetypeSyndicateSpoofer ||
			bot.Archetype == model.ArchetypeSyndicateSweep {
			continue
		}
		bot.Mu.Lock()
		switch bot.Archetype {
		case model.ArchetypeAvellanedaStoikov:
			intents = agents.StepAvellanedaStoikov(bot, &snap, logicalNowNs, params, intents)
		case model.ArchetypeMomentumQuant, model.ArchetypeMeanRevQuant:
			intents = agents.StepQuant(bot, &snap, logicalNowNs, params, intents)
		case model.ArchetypeHawkesNoise:
			intents = agents.StepHawkesNoise(bot, &snap, logicalNowNs, 0.010, params, intents)
		}
		bot.Mu.Unlock()
	}

	sh0 := &s.shards[0]
	sh0.mu.Lock()
	chaos.InjectBatchFaultsLocal(intents, prof, &sh0.faultRNG, &sh0.counters)
	sh0.mu.Unlock()

	ctx := context.Background()
	for _, intent := range intents {
		s.dispatchIntent(ctx, intent, logicalNowNs, prof)
	}
	s.detIntentBuf = intents[:0]
	return len(intents)
}

// GenerateShardBatch generates and fault-injects a batch of OrderIntents for shardIdx without network I/O.
// Uses cache-line isolated per-shard mutexes and local fault counters (zero cross-core false sharing).
func (s *Simulator) GenerateShardBatch(shardIdx int, logicalNowNs int64, dtSec float64, buf []model.OrderIntent) []model.OrderIntent {
	prof := s.campaignPtr.Load()
	params := s.strategyParams
	params.BurstMultiplier = prof.BurstMultiplier
	snap := s.currentMarketSnapshotValue(prof.Regime)

	out := buf[:0]
	sh := &s.shards[shardIdx]
	for _, bot := range sh.bots {
		bot.Mu.Lock()
		switch bot.Archetype {
		case model.ArchetypeAvellanedaStoikov:
			out = agents.StepAvellanedaStoikov(bot, &snap, logicalNowNs, params, out)
		case model.ArchetypeMomentumQuant, model.ArchetypeMeanRevQuant:
			out = agents.StepQuant(bot, &snap, logicalNowNs, params, out)
		case model.ArchetypeHawkesNoise:
			out = agents.StepHawkesNoise(bot, &snap, logicalNowNs, dtSec, params, out)
		}
		bot.Mu.Unlock()
	}

	sh.mu.Lock()
	chaos.InjectBatchFaultsLocal(out, prof, &sh.faultRNG, &sh.counters)
	sh.mu.Unlock()

	return out
}

// TriggerSyndicateAttack executes an on-demand adversarial syndicate burst (Circular Wash Ring,
// Layering/Spoofing, or Flash-Crash Liquidity Sweep) immediately via REST/CLI/Campaign.
func (s *Simulator) TriggerSyndicateAttack(ctx context.Context, req SyndicateAttackRequest) SyndicateAttackResult {
	nowNs := time.Now().UnixNano()
	prof := s.campaignPtr.Load()
	snap := s.currentMarketSnapshot(prof.Regime)

	rounds := req.Rounds
	if rounds <= 0 {
		rounds = 4
	} else if rounds > 200 {
		rounds = 200
	}

	var intents []model.OrderIntent
	var participants []string
	seenPart := make(map[string]struct{})
	addPart := func(id string) {
		if _, ok := seenPart[id]; !ok {
			seenPart[id] = struct{}{}
			participants = append(participants, id)
		}
	}

	switch req.AttackType {
	case AttackWashRing:
		ringSz := req.RingSize
		if ringSz < 3 {
			ringSz = 3
		}
		if ringSz > len(s.washRing) {
			ringSz = len(s.washRing)
		}
		ringSlice := s.washRing[:ringSz]
		for _, b := range ringSlice {
			addPart(b.AccountID)
		}
		lots := req.Lots
		if lots <= 0 {
			lots = 10
		}
		// Execute full rotating cycles across the 3..5 ring accounts
		totalLegs := rounds * ringSz
		for l := 0; l < totalLegs; l++ {
			leg := s.washLeg.Add(1) - 1
			seller := ringSlice[int(leg%uint64(ringSz))]
			buyer := ringSlice[int((leg+1)%uint64(ringSz))]
			// Lock in deterministic ascending BotID order to prevent deadlock!
			first, second := seller, buyer
			if first.BotID > second.BotID {
				first, second = second, first
			}
			first.Mu.Lock()
			second.Mu.Lock()
			intents = agents.StepCircularWashRing(ringSlice, leg, snap, nowNs+int64(l)*2_000, lots, s.strategyParams.MaxFeeBps, intents)
			second.Mu.Unlock()
			first.Mu.Unlock()
		}

	case AttackLayeringSpoofer:
		lots := req.Lots
		if lots <= 0 {
			lots = 25
		}
		for r := 0; r < rounds*3; r++ {
			for _, sp := range s.spoofers {
				addPart(sp.AccountID)
				sp.Mu.Lock()
				var stepOut []model.OrderIntent
				stepOut = agents.StepLayeringSpoofer(sp, snap, nowNs+int64(r)*5_000, lots, s.strategyParams.MaxFeeBps, stepOut)
				sp.Mu.Unlock()
				for _, it := range stepOut {
					s.dispatchIntent(ctx, it, nowNs+int64(r)*5_000, prof)
					intents = append(intents, it)
				}
				snap = s.currentMarketSnapshot(prof.Regime)
			}
		}
		s.syndicateAttacks.Add(1)
		return SyndicateAttackResult{
			AttackType:       req.AttackType,
			OrdersDispatched: len(intents),
			ParticipantIDs:   participants,
			ExecutedAtNs:     nowNs,
		}

	case AttackFlashSweep:
		lots := req.Lots
		if lots <= 0 {
			lots = 50
		}
		for _, sw := range s.sweepers {
			addPart(sw.AccountID)
		}
		for r := 0; r < rounds; r++ {
			for _, sw := range s.sweepers {
				sw.Mu.Lock()
			}
			intents = agents.StepFlashCrashSweep(s.sweepers, snap, nowNs+int64(r)*3_000, lots, s.strategyParams.MaxFeeBps, intents)
			for i := len(s.sweepers) - 1; i >= 0; i-- {
				s.sweepers[i].Mu.Unlock()
			}
		}
	}

	fillsCount := 0
	for _, it := range intents {
		res := s.dispatchIntent(ctx, it, it.TimestampNs, prof)
		fillsCount += len(res.Fills)
	}
	s.syndicateAttacks.Add(1)

	return SyndicateAttackResult{
		AttackType:       req.AttackType,
		OrdersDispatched: len(intents),
		FillsMatched:     fillsCount,
		ParticipantIDs:   participants,
		ExecutedAtNs:     nowNs,
	}
}

func (s *Simulator) dispatchIntent(ctx context.Context, intent model.OrderIntent, serverNowNs int64, prof *chaos.CampaignProfile) model.SubmitResult {
	if intent.Kind == model.ActionCancel {
		s.cancelsSubmitted.Add(1)
	} else {
		s.ordersSubmitted.Add(1)
	}

	// Simulated Network Partition fault at client side when PartitionActive
	if prof != nil && prof.NetworkPartitionActive {
		s.faultStats.PartitionDrops.Add(1)
		return model.SubmitResult{
			ClientOrderID:  intent.ClientOrderID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			TransportDrop:  true,
			RejectReason:   "NETWORK_PARTITION_DROP",
		}
	}

	if s.cfg.EnableAuditTrack {
		s.auditRW.RLock()
		defer s.auditRW.RUnlock()
	}

	if s.cfg.EnableAuditTrack && intent.Kind == model.ActionSubmit {
		s.verifier.RecordIntent(intent)
	}

	bot := s.botsByID[intent.BotID]
	var finalRes model.SubmitResult

	switch s.cfg.Backend {
	case BackendGatewayHTTP:
		finalRes = s.httpClient.SendOrderIntent(ctx, bot, intent)
	case BackendLedgerGRPC:
		finalRes = s.grpcClient.SendOrderIntent(ctx, bot, intent)
	default:
		start := time.Now()
		copies := int(intent.DuplicateCount)
		if copies < 1 {
			copies = 1
		}
		regime := model.RegimeCalm
		if prof != nil {
			regime = prof.Regime
		}
		for c := 0; c < copies; c++ {
			r := s.mockEx.Process(intent, serverNowNs, regime)
			if c == 0 {
				finalRes = r
			} else if r.Status == model.StatusDuplicateDeduped && s.cfg.EnableAuditTrack {
				s.verifier.RecordSubmitResult(intent, r)
			}
		}
		finalRes.LatencyNs = time.Since(start).Nanoseconds()
	}

	s.recordLatencyNs(finalRes.LatencyNs)

	if finalRes.TransportDrop {
		return finalRes
	}

	switch finalRes.Status {
	case model.StatusAccepted, model.StatusPartiallyFilled, model.StatusFilled:
		if intent.Kind == model.ActionSubmit {
			s.ordersAccepted.Add(1)
		}
	case model.StatusRejected:
		s.ordersRejected.Add(1)
	case model.StatusDuplicateDeduped:
		s.ordersDeduped.Add(1)
	}
	if len(finalRes.Fills) > 0 {
		s.tradesMatched.Add(uint64(len(finalRes.Fills)))
	}

	if bot != nil {
		bot.Mu.Lock()
		if intent.Kind == model.ActionCancel {
			if finalRes.Status == model.StatusCanceled {
				bot.ApplyCanceled(intent.TargetOrderID)
			}
		} else {
			bot.ApplySubmitResult(intent, finalRes, s.strategyParams.MaxFeeBps)
		}
		bot.Mu.Unlock()
	}

	if s.cfg.EnableAuditTrack {
		s.verifier.RecordSubmitResult(intent, finalRes)
	}

	return finalRes
}

func (s *Simulator) workerLoop(ctx context.Context, shardIdx int) {
	defer s.runWG.Done()
	ticker := time.NewTicker(s.cfg.TickInterval)
	defer ticker.Stop()

	dtSec := s.cfg.TickInterval.Seconds()
	batchBuf := make([]model.OrderIntent, 0, 256)

	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker.C:
			nowNs := now.UnixNano()
			prof := s.campaignPtr.Load()
			batchBuf = s.GenerateShardBatch(shardIdx, nowNs, dtSec, batchBuf)
			for _, it := range batchBuf {
				if ctx.Err() != nil {
					return
				}
				s.dispatchIntent(ctx, it, nowNs, prof)
			}
		}
	}
}

func (s *Simulator) syndicateLoop(ctx context.Context) {
	defer s.runWG.Done()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			prof := s.campaignPtr.Load()
			if prof == nil {
				continue
			}
			if prof.WashRingActive {
				s.TriggerSyndicateAttack(ctx, SyndicateAttackRequest{
					AttackType: AttackWashRing,
					RingSize:   prof.WashRingSize,
					Rounds:     1,
					Lots:       8,
				})
			}
			if prof.SpooferActive {
				s.TriggerSyndicateAttack(ctx, SyndicateAttackRequest{
					AttackType: AttackLayeringSpoofer,
					Rounds:     1,
					Lots:       20,
				})
			}
			if prof.FlashSweepActive {
				s.TriggerSyndicateAttack(ctx, SyndicateAttackRequest{
					AttackType: AttackFlashSweep,
					Rounds:     1,
					Lots:       35,
				})
			}
		}
	}
}

func (s *Simulator) telemetryAndAuditLoop(ctx context.Context) {
	defer s.runWG.Done()
	rateTicker := time.NewTicker(500 * time.Millisecond)
	auditTicker := time.NewTicker(s.cfg.AuditInterval)
	defer rateTicker.Stop()
	defer auditTicker.Stop()

	lastOrders := s.ordersSubmitted.Load()
	lastTrades := s.tradesMatched.Load()
	lastSample := time.Now()

	for {
		select {
		case <-ctx.Done():
			return
		case now := <-rateTicker.C:
			elapsed := now.Sub(lastSample).Seconds()
			if elapsed > 0.05 {
				curOrders := s.ordersSubmitted.Load()
				curTrades := s.tradesMatched.Load()
				s.ordersPerSec.Store(uint64(float64(curOrders-lastOrders) / elapsed))
				s.tradesPerSec.Store(uint64(float64(curTrades-lastTrades) / elapsed))
				lastOrders = curOrders
				lastTrades = curTrades
				lastSample = now
			}
			s.broadcastTelemetry(s.Telemetry())

		case <-auditTicker.C:
			if s.cfg.EnableAuditTrack && s.cfg.Backend == BackendMock {
				_, _ = s.RunAuditNow()
			}
		}
	}
}

// RunAuditNow triggers an immediate full double-entry balance & order reconciliation.
func (s *Simulator) RunAuditNow() (auditor.AuditReport, error) {
	if s.cfg.Backend == BackendMock {
		if s.cfg.EnableAuditTrack {
			s.auditRW.Lock()
			defer s.auditRW.Unlock()
		}
		return s.verifier.VerifyAgainstServer(s.mockEx)
	}
	return s.verifier.LastReport(), nil
}

func (s *Simulator) currentMarketSnapshotValue(regime model.MarketRegime) model.MarketSnapshot {
	snap := s.mockEx.Snapshot()
	if snap == nil {
		spec := model.LookupInstrument(s.cfg.Symbol)
		return model.MarketSnapshot{
			Symbol:         spec.Symbol,
			InstrumentID:   spec.InstrumentID,
			MidPrice:       spec.DefaultMidFP,
			BestBid:        spec.DefaultMidFP - spec.TickSizeFP,
			BestAsk:        spec.DefaultMidFP + spec.TickSizeFP,
			BidDepth5:      10 * fixed.Scale,
			AskDepth5:      10 * fixed.Scale,
			VWAP:           spec.DefaultMidFP,
			ShortEMA:       spec.DefaultMidFP,
			LongEMA:        spec.DefaultMidFP,
			RealizedVolBps: 25,
			TickSize:       spec.TickSizeFP,
			LotSize:        spec.LotSizeFP,
			Regime:         regime,
		}
	}
	cp := *snap
	cp.Regime = regime
	return cp
}

func (s *Simulator) currentMarketSnapshot(regime model.MarketRegime) *model.MarketSnapshot {
	cp := s.currentMarketSnapshotValue(regime)
	return &cp
}

func (s *Simulator) recordLatencyNs(latNs int64) {
	if latNs < 0 {
		latNs = 0
	}
	latUs := uint64(latNs / 1_000)
	s.latMu.Lock()
	s.latRing[s.latPos] = latUs
	s.latPos = (s.latPos + 1) % latRingSize
	if s.latFill < latRingSize {
		s.latFill++
	}
	s.latMu.Unlock()
}

func (s *Simulator) latencyPercentilesUs() (p50, p95, p99 uint64) {
	s.latMu.Lock()
	n := s.latFill
	if n == 0 {
		s.latMu.Unlock()
		return 0, 0, 0
	}
	cp := make([]uint64, n)
	copy(cp, s.latRing[:n])
	s.latMu.Unlock()

	slices.Sort(cp)
	p50 = cp[(n*50)/100]
	p95 = cp[(n*95)/100]
	idx99 := (n * 99) / 100
	if idx99 >= n {
		idx99 = n - 1
	}
	p99 = cp[idx99]
	return p50, p95, p99
}

func (s *Simulator) aggregateFaultStats() chaos.FaultStatsSnapshot {
	snap := s.faultStats.Snapshot()
	for i := range s.shards {
		sh := &s.shards[i]
		sh.mu.Lock()
		snap.ReorderedPackets += sh.counters.ReorderedPackets
		snap.DuplicatedRequests += sh.counters.DuplicatedRequests
		snap.ClockSkewedRequests += sh.counters.ClockSkewedRequests
		snap.CorruptedSignatures += sh.counters.CorruptedSignatures
		sh.mu.Unlock()
	}
	return snap
}

func (s *Simulator) Telemetry() TelemetrySnapshot {
	prof := s.CurrentCampaign()
	cidPtr := s.campaignID.Load()
	cid := "camp-calm-0"
	if cidPtr != nil {
		cid = *cidPtr
	}
	snap := s.currentMarketSnapshot(prof.Regime)
	auditRep := s.verifier.LastReport()
	p50, p95, p99 := s.latencyPercentilesUs()

	return TelemetrySnapshot{
		TimestampNs:      time.Now().UnixNano(),
		MasterSeed:       fmt.Sprintf("%d", s.cfg.MasterSeed),
		Symbol:           s.cfg.Symbol,
		Backend:          s.cfg.Backend,
		CampaignID:       cid,
		Active:           s.simActive.Load(),
		Regime:           prof.Regime,
		TotalBots:        len(s.bots),
		MakerBots:        s.makerCount,
		QuantBots:        s.quantCount,
		NoiseBots:        s.noiseCount,
		SyndicateBots:    s.syndCount,
		OrdersSubmitted:  s.ordersSubmitted.Load(),
		OrdersAccepted:   s.ordersAccepted.Load(),
		OrdersRejected:   s.ordersRejected.Load(),
		OrdersDeduped:    s.ordersDeduped.Load(),
		CancelsSubmitted: s.cancelsSubmitted.Load(),
		TradesMatched:    s.tradesMatched.Load(),
		SyndicateAttacks: s.syndicateAttacks.Load(),
		OrdersPerSec:     s.ordersPerSec.Load(),
		TradesPerSec:     s.tradesPerSec.Load(),
		LatencyP50Us:     p50,
		LatencyP95Us:     p95,
		LatencyP99Us:     p99,
		MidPriceFP:       fixed.Value(snap.MidPrice).String(),
		BestBidFP:        fixed.Value(snap.BestBid).String(),
		BestAskFP:        fixed.Value(snap.BestAsk).String(),
		VWAPFP:           fixed.Value(snap.VWAP).String(),
		Faults:           s.aggregateFaultStats(),
		SlowLoris:        s.slowLoris.Stats(),
		InvariantPassed:  auditRep.Passed,
		InvariantAudits:  auditRep.AuditSeq,
		ViolationCount:   len(auditRep.Violations),
	}
}

// SubscribeSSE registers a non-blocking SSE telemetry subscriber channel.
func (s *Simulator) SubscribeSSE() (uint64, <-chan TelemetrySnapshot) {
	id := s.nextSubID.Add(1)
	ch := make(chan TelemetrySnapshot, 16)
	s.subMu.Lock()
	s.subscribers[id] = ch
	s.subMu.Unlock()
	return id, ch
}

func (s *Simulator) UnsubscribeSSE(id uint64) {
	s.subMu.Lock()
	ch, ok := s.subscribers[id]
	if ok {
		delete(s.subscribers, id)
		close(ch)
	}
	s.subMu.Unlock()
}

func (s *Simulator) broadcastTelemetry(t TelemetrySnapshot) {
	s.subMu.RLock()
	defer s.subMu.RUnlock()
	for _, ch := range s.subscribers {
		select {
		case ch <- t:
		default:
			// Drop stale frame on slow SSE viewer so simulation never blocks
		}
	}
}
