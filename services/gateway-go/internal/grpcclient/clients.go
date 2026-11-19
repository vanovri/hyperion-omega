package grpcclient

import (
	"context"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/config"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"google.golang.org/grpc"
)

// MeshClients bundles pooled, circuit-broken gRPC clients to all four Hyperion backend engines.
type MeshClients struct {
	LedgerPool    *ConnPool
	AnalyticsPool *ConnPool
	FraudPool     *ConnPool
	ChronosPool   *ConnPool
}

// NewMeshClients initializes multiplexed gRPC pools for ledger-rs (:50151),
// stream-aggregator (:50152), sentinel-ml (:50153), and chronos-replay (:50154).
func NewMeshClients(cfg config.Config, metrics *telemetry.Registry, extraOpts ...grpc.DialOption) (*MeshClients, error) {
	if metrics == nil {
		metrics = telemetry.Global
	}
	makePool := func(name string, targets []string) (*ConnPool, error) {
		cb := NewAdaptiveCircuitBreaker(BreakerConfig{
			Name:           name,
			NumBuckets:     cfg.BreakerWindowBuckets,
			BucketDuration: cfg.BreakerBucketDuration,
			MinRequests:    cfg.BreakerMinRequests,
			ErrorTripPPM:   cfg.BreakerErrorTripPPM,
			OpenCooldown:   cfg.BreakerOpenCooldown,
			HalfOpenProbes: cfg.BreakerHalfOpenProbes,
			Metrics:        metrics,
		})
		return NewConnPool(PoolConfig{
			ServiceName:          name,
			Targets:              targets,
			ConnsPerTarget:       cfg.GRPCPoolSizePerTarget,
			DefaultTimeout:       cfg.DefaultRPCTimeout,
			MaxIdempotentRetries: cfg.MaxIdempotentRetries,
			RetryBaseBackoff:     cfg.RetryBaseBackoff,
			Breaker:              cb,
			DialOptions:          extraOpts,
			Metrics:              metrics,
		})
	}

	ledgerPool, err := makePool("ledger-rs", cfg.LedgerGRPCAddrs)
	if err != nil {
		return nil, err
	}
	analyticsPool, err := makePool("stream-aggregator", cfg.AnalyticsGRPCAddrs)
	if err != nil {
		_ = ledgerPool.Close()
		return nil, err
	}
	fraudPool, err := makePool("sentinel-ml", cfg.FraudGRPCAddrs)
	if err != nil {
		_ = ledgerPool.Close()
		_ = analyticsPool.Close()
		return nil, err
	}
	chronosPool, err := makePool("chronos-replay", cfg.ChronosGRPCAddrs)
	if err != nil {
		_ = ledgerPool.Close()
		_ = analyticsPool.Close()
		_ = fraudPool.Close()
		return nil, err
	}

	return &MeshClients{
		LedgerPool:    ledgerPool,
		AnalyticsPool: analyticsPool,
		FraudPool:     fraudPool,
		ChronosPool:   chronosPool,
	}, nil
}

// Close tears down all upstream gRPC pools cleanly.
func (m *MeshClients) Close() {
	if m == nil {
		return
	}
	if m.LedgerPool != nil {
		_ = m.LedgerPool.Close()
	}
	if m.AnalyticsPool != nil {
		_ = m.AnalyticsPool.Close()
	}
	if m.FraudPool != nil {
		_ = m.FraudPool.Close()
	}
	if m.ChronosPool != nil {
		_ = m.ChronosPool.Close()
	}
}

// --- Non-Idempotent Mutations (idempotent = false: NEVER blind-retried by pool) ---

func (m *MeshClients) SubmitOrder(ctx context.Context, req *hyperionv1.SubmitOrderRequest) (*hyperionv1.SubmitOrderResponse, error) {
	var resp *hyperionv1.SubmitOrderResponse
	err := m.LedgerPool.InvokeUnary(ctx, false, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) CancelOrder(ctx context.Context, req *hyperionv1.CancelOrderRequest) (*hyperionv1.CancelOrderResponse, error) {
	var resp *hyperionv1.CancelOrderResponse
	err := m.LedgerPool.InvokeUnary(ctx, false, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).CancelOrder(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) ModifyOrder(ctx context.Context, req *hyperionv1.ModifyOrderRequest) (*hyperionv1.ModifyOrderResponse, error) {
	var resp *hyperionv1.ModifyOrderResponse
	err := m.LedgerPool.InvokeUnary(ctx, false, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).ModifyOrder(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) CreditDeposit(ctx context.Context, req *hyperionv1.CreditDepositRequest) (*hyperionv1.CreditDepositResponse, error) {
	var resp *hyperionv1.CreditDepositResponse
	err := m.LedgerPool.InvokeUnary(ctx, false, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).CreditDeposit(cctx, req)
		return callErr
	})
	return resp, err
}

// --- Idempotent Read & Sandbox Queries (idempotent = true: automatic backoff retry) ---

func (m *MeshClients) GetL2BookSnapshot(ctx context.Context, req *hyperionv1.GetL2BookSnapshotRequest) (*hyperionv1.GetL2BookSnapshotResponse, error) {
	var resp *hyperionv1.GetL2BookSnapshotResponse
	err := m.LedgerPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).GetL2BookSnapshot(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) GetMerkleLedgerProof(ctx context.Context, req *hyperionv1.GetMerkleLedgerProofRequest) (*hyperionv1.GetMerkleLedgerProofResponse, error) {
	var resp *hyperionv1.GetMerkleLedgerProofResponse
	err := m.LedgerPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).GetMerkleLedgerProof(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) VerifySystemLedgerInvariant(ctx context.Context, req *hyperionv1.VerifySystemLedgerInvariantRequest) (*hyperionv1.VerifySystemLedgerInvariantResponse, error) {
	var resp *hyperionv1.VerifySystemLedgerInvariantResponse
	err := m.LedgerPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewOrderbookServiceClient(cc).VerifySystemLedgerInvariant(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) GetCandles(ctx context.Context, req *hyperionv1.GetCandlesRequest) (*hyperionv1.GetCandlesResponse, error) {
	var resp *hyperionv1.GetCandlesResponse
	err := m.AnalyticsPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewStreamAnalyticsServiceClient(cc).GetCandles(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) GetLatestMicrostructure(ctx context.Context, req *hyperionv1.GetLatestMicrostructureRequest) (*hyperionv1.GetLatestMicrostructureResponse, error) {
	var resp *hyperionv1.GetLatestMicrostructureResponse
	err := m.AnalyticsPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewStreamAnalyticsServiceClient(cc).GetLatestMicrostructure(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) ListRecentAlerts(ctx context.Context, req *hyperionv1.ListRecentAlertsRequest) (*hyperionv1.ListRecentAlertsResponse, error) {
	var resp *hyperionv1.ListRecentAlertsResponse
	err := m.FraudPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewFraudTelemetryServiceClient(cc).ListRecentAlerts(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) InspectFraudSubgraph(ctx context.Context, req *hyperionv1.InspectFraudSubgraphRequest) (*hyperionv1.InspectFraudSubgraphResponse, error) {
	var resp *hyperionv1.InspectFraudSubgraphResponse
	err := m.FraudPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewFraudTelemetryServiceClient(cc).InspectFraudSubgraph(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) TimeTravelSeek(ctx context.Context, req *hyperionv1.TimeTravelSeekRequest) (*hyperionv1.TimeTravelSeekResponse, error) {
	var resp *hyperionv1.TimeTravelSeekResponse
	err := m.ChronosPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewChronosReplayServiceClient(cc).TimeTravelSeek(cctx, req)
		return callErr
	})
	return resp, err
}

func (m *MeshClients) TimelineFork(ctx context.Context, req *hyperionv1.TimelineForkRequest) (*hyperionv1.StateTreeDiffResponse, error) {
	var resp *hyperionv1.StateTreeDiffResponse
	err := m.ChronosPool.InvokeUnary(ctx, true, func(cctx context.Context, cc *grpc.ClientConn) error {
		var callErr error
		resp, callErr = hyperionv1.NewChronosReplayServiceClient(cc).TimelineFork(cctx, req)
		return callErr
	})
	return resp, err
}
