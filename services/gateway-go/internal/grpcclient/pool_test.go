package grpcclient_test

import (
	"context"
	"errors"
	"net"
	"sync/atomic"
	"testing"
	"time"

	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"github.com/vanovri/hyperion/services/gateway-go/internal/grpcclient"
	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

type mockOrderbookServer struct {
	hyperionv1.UnimplementedOrderbookServiceServer
	submitCalls   atomic.Int64
	snapCalls     atomic.Int64
	failSnapCount atomic.Int64
	failAll       atomic.Bool
	sleepDur      atomic.Int64
	sawTraceHdr   atomic.Bool
}

func (m *mockOrderbookServer) SubmitOrder(ctx context.Context, req *hyperionv1.SubmitOrderRequest) (*hyperionv1.SubmitOrderResponse, error) {
	m.submitCalls.Add(1)
	if md, ok := metadata.FromIncomingContext(ctx); ok && len(md.Get("traceparent")) > 0 {
		m.sawTraceHdr.Store(true)
	}
	if d := m.sleepDur.Load(); d > 0 {
		select {
		case <-time.After(time.Duration(d)):
		case <-ctx.Done():
			return nil, status.Error(codes.DeadlineExceeded, "context deadline exceeded")
		}
	}
	if m.failAll.Load() {
		return nil, status.Error(codes.Unavailable, "simulated ledger node crash")
	}
	return &hyperionv1.SubmitOrderResponse{
		Ack: &hyperionv1.OrderAck{
			EngineSeq:     101,
			OrderId:       5001,
			ClientOrderId: req.GetOrder().GetClientOrderId(),
			AccountId:     req.GetOrder().GetAccountId(),
			Symbol:        req.GetOrder().GetSymbol(),
			Status:        hyperionv1.OrderStatus_ORDER_STATUS_ACCEPTED,
		},
	}, nil
}

func (m *mockOrderbookServer) GetL2BookSnapshot(ctx context.Context, req *hyperionv1.GetL2BookSnapshotRequest) (*hyperionv1.GetL2BookSnapshotResponse, error) {
	c := m.snapCalls.Add(1)
	if m.failAll.Load() || c <= m.failSnapCount.Load() {
		return nil, status.Error(codes.Unavailable, "transient network blip")
	}
	return &hyperionv1.GetL2BookSnapshotResponse{
		Snapshot: &hyperionv1.L2BookSnapshot{
			EngineSeq: 250,
			Symbol:    req.GetSymbol(),
			BestBidFp: 6_840_000_000_000,
			BestAskFp: 6_841_000_000_000,
		},
	}, nil
}

func TestConnPool_IdempotentRetry_NonIdempotentNoRetry_Deadline_And_BreakerRecovery(t *testing.T) {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	srv := grpc.NewServer()
	mock := &mockOrderbookServer{}
	hyperionv1.RegisterOrderbookServiceServer(srv, mock)
	go func() { _ = srv.Serve(lis) }()
	t.Cleanup(srv.Stop)

	metrics := telemetry.NewRegistry()
	curTime := time.Now()
	cb := grpcclient.NewAdaptiveCircuitBreaker(grpcclient.BreakerConfig{
		Name:           "ledger-test",
		NumBuckets:     5,
		BucketDuration: time.Second,
		MinRequests:    6,
		ErrorTripPPM:   500_000, // 50%
		OpenCooldown:   2 * time.Second,
		HalfOpenProbes: 3,
		Metrics:        metrics,
	})
	cb.SetClock(func() time.Time { return curTime })

	pool, err := grpcclient.NewConnPool(grpcclient.PoolConfig{
		ServiceName:          "ledger-rs",
		Targets:              []string{lis.Addr().String()},
		ConnsPerTarget:       4,
		DefaultTimeout:       500 * time.Millisecond,
		MaxIdempotentRetries: 2,
		RetryBaseBackoff:     5 * time.Millisecond,
		Breaker:              cb,
		Metrics:              metrics,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = pool.Close() })

	// 1. Idempotent GetL2BookSnapshot automatically retries 2 transient Unavailable failures and succeeds on attempt 3!
	mock.failSnapCount.Store(2)
	err = pool.InvokeUnary(context.Background(), true, func(cctx context.Context, cc *grpc.ClientConn) error {
		_, rerr := hyperionv1.NewOrderbookServiceClient(cc).GetL2BookSnapshot(cctx, &hyperionv1.GetL2BookSnapshotRequest{Symbol: "BTC-USDT"})
		return rerr
	})
	if err != nil {
		t.Fatalf("expected idempotent retry to succeed on 3rd attempt, got %v", err)
	}
	if got := mock.snapCalls.Load(); got != 3 {
		t.Fatalf("expected 3 snapshot attempts, got %d", got)
	}

	// 2. Non-idempotent SubmitOrder NEVER retries on Unavailable (attempts == 1)!
	cb2 := grpcclient.NewAdaptiveCircuitBreaker(grpcclient.BreakerConfig{MinRequests: 100, Metrics: metrics})
	poolNoRetry, _ := grpcclient.NewConnPool(grpcclient.PoolConfig{
		ServiceName:          "ledger-mutation",
		Targets:              []string{lis.Addr().String()},
		ConnsPerTarget:       2,
		MaxIdempotentRetries: 3,
		Breaker:              cb2,
		Metrics:              metrics,
	})
	t.Cleanup(func() { _ = poolNoRetry.Close() })

	mock.failAll.Store(true)
	_ = poolNoRetry.InvokeUnary(context.Background(), false, func(cctx context.Context, cc *grpc.ClientConn) error {
		_, rerr := hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, &hyperionv1.SubmitOrderRequest{
			Order: &hyperionv1.SubmitOrder{ClientOrderId: "no-retry-1", AccountId: 1, Symbol: "BTC-USDT"},
		})
		return rerr
	})
	if got := mock.submitCalls.Load(); got != 1 {
		t.Fatalf("CRITICAL: non-idempotent SubmitOrder retried %d times (must be 1)", got)
	}
	mock.failAll.Store(false)

	// 3. W3C Trace header + strict caller deadline propagation.
	traceCtx := grpcclient.WithTraceHeaders(context.Background(), "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01", "corr-99")
	mock.sleepDur.Store(int64(120 * time.Millisecond))
	shortCtx, cancel := context.WithTimeout(traceCtx, 25*time.Millisecond)
	err = pool.InvokeUnary(shortCtx, false, func(cctx context.Context, cc *grpc.ClientConn) error {
		_, rerr := hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, &hyperionv1.SubmitOrderRequest{
			Order: &hyperionv1.SubmitOrder{ClientOrderId: "dl-1", AccountId: 1, Symbol: "BTC-USDT"},
		})
		return rerr
	})
	cancel()
	mock.sleepDur.Store(0)
	if status.Code(err) != codes.DeadlineExceeded {
		t.Fatalf("expected DeadlineExceeded propagation, got %v", err)
	}
	if !mock.sawTraceHdr.Load() {
		t.Fatal("expected traceparent metadata propagated to upstream gRPC server")
	}

	// 4. Adaptive Circuit Breaker Trip -> Fast Fail -> Half-Open Probe -> Full Closed Recovery!
	mock.failAll.Store(true)
	for i := 0; i < 8; i++ {
		_ = pool.InvokeUnary(context.Background(), false, func(cctx context.Context, cc *grpc.ClientConn) error {
			_, rerr := hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, &hyperionv1.SubmitOrderRequest{
				Order: &hyperionv1.SubmitOrder{ClientOrderId: "trip", AccountId: 1, Symbol: "BTC-USDT"},
			})
			return rerr
		})
	}
	if cb.State() != grpcclient.StateOpen {
		t.Fatalf("expected breaker StateOpen after >50%% failures, got %s", cb.State())
	}

	// Fast-fail check while Open.
	beforeCalls := mock.submitCalls.Load()
	err = pool.InvokeUnary(context.Background(), false, func(cctx context.Context, cc *grpc.ClientConn) error {
		_, rerr := hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, &hyperionv1.SubmitOrderRequest{})
		return rerr
	})
	if !errors.Is(err, grpcclient.ErrCircuitOpen) || mock.submitCalls.Load() != beforeCalls {
		t.Fatalf("expected fast-fail ErrCircuitOpen with 0 network calls, got err=%v", err)
	}

	// Heal upstream + advance clock past OpenCooldown (3s) -> 3 successful Half-Open probes -> StateClosed!
	mock.failAll.Store(false)
	curTime = curTime.Add(3 * time.Second)
	for i := 0; i < 3; i++ {
		if err := pool.InvokeUnary(context.Background(), false, func(cctx context.Context, cc *grpc.ClientConn) error {
			_, rerr := hyperionv1.NewOrderbookServiceClient(cc).SubmitOrder(cctx, &hyperionv1.SubmitOrderRequest{
				Order: &hyperionv1.SubmitOrder{ClientOrderId: "recover", AccountId: 1, Symbol: "BTC-USDT"},
			})
			return rerr
		}); err != nil {
			t.Fatalf("half-open probe %d failed: %v", i, err)
		}
	}
	if cb.State() != grpcclient.StateClosed {
		t.Fatalf("expected breaker recovered to StateClosed, got %s", cb.State())
	}
}
