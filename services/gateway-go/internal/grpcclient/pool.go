package grpcclient

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/gateway-go/internal/telemetry"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/keepalive"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

type pooledConn struct {
	target   string
	conn     *grpc.ClientConn
	inFlight atomic.Int64
}

// PoolConfig configures a multiplexed gRPC connection pool for an upstream microservice.
type PoolConfig struct {
	ServiceName          string
	Targets              []string
	ConnsPerTarget       int
	DefaultTimeout       time.Duration
	MaxIdempotentRetries int
	RetryBaseBackoff     time.Duration
	Breaker              *AdaptiveCircuitBreaker
	DialOptions          []grpc.DialOption
	Metrics              *telemetry.Registry
}

// ConnPool multiplexes RPCs across multiple warm HTTP/2 gRPC connections per upstream target
// using least-inflight + round-robin balancing, deadline propagation, W3C trace metadata injection,
// idempotent-only exponential backoff retries, and real-time adaptive circuit breaking.
type ConnPool struct {
	serviceName          string
	conns                []*pooledConn
	rrCursor             atomic.Uint64
	defaultTimeout       time.Duration
	maxIdempotentRetries int
	retryBaseBackoff     time.Duration
	breaker              *AdaptiveCircuitBreaker
	metrics              *telemetry.Registry
	closeOnce            sync.Once
}

// NewConnPool dials (lazily/non-blocking via grpc.NewClient) N HTTP/2 channels per upstream target.
func NewConnPool(cfg PoolConfig) (*ConnPool, error) {
	if len(cfg.Targets) == 0 {
		return nil, fmt.Errorf("grpc pool %q requires at least one target", cfg.ServiceName)
	}
	if cfg.ConnsPerTarget <= 0 {
		cfg.ConnsPerTarget = 4
	}
	if cfg.DefaultTimeout <= 0 {
		cfg.DefaultTimeout = 1500 * time.Millisecond
	}
	if cfg.MaxIdempotentRetries < 0 {
		cfg.MaxIdempotentRetries = 0
	}
	if cfg.RetryBaseBackoff <= 0 {
		cfg.RetryBaseBackoff = 15 * time.Millisecond
	}
	if cfg.Metrics == nil {
		cfg.Metrics = telemetry.Global
	}
	if cfg.Breaker == nil {
		cfg.Breaker = NewAdaptiveCircuitBreaker(BreakerConfig{
			Name:    cfg.ServiceName,
			Metrics: cfg.Metrics,
		})
	}

	baseOpts := []grpc.DialOption{
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithDefaultCallOptions(
			grpc.MaxCallRecvMsgSize(16<<20),
			grpc.MaxCallSendMsgSize(16<<20),
		),
		grpc.WithKeepaliveParams(keepalive.ClientParameters{
			Time:                30 * time.Second,
			Timeout:             10 * time.Second,
			PermitWithoutStream: true,
		}),
	}
	baseOpts = append(baseOpts, cfg.DialOptions...)

	conns := make([]*pooledConn, 0, len(cfg.Targets)*cfg.ConnsPerTarget)
	for _, target := range cfg.Targets {
		for i := 0; i < cfg.ConnsPerTarget; i++ {
			cc, err := grpc.NewClient(target, baseOpts...)
			if err != nil {
				for _, prev := range conns {
					_ = prev.conn.Close()
				}
				return nil, fmt.Errorf("grpc.NewClient(%s): %w", target, err)
			}
			conns = append(conns, &pooledConn{
				target: target,
				conn:   cc,
			})
		}
	}

	return &ConnPool{
		serviceName:          cfg.ServiceName,
		conns:                conns,
		defaultTimeout:       cfg.DefaultTimeout,
		maxIdempotentRetries: cfg.MaxIdempotentRetries,
		retryBaseBackoff:     cfg.RetryBaseBackoff,
		breaker:              cfg.Breaker,
		metrics:              cfg.Metrics,
	}, nil
}

// Breaker exposes the pool's AdaptiveCircuitBreaker for health inspection and testing.
func (p *ConnPool) Breaker() *AdaptiveCircuitBreaker {
	return p.breaker
}

// Size returns total multiplexed HTTP/2 connections managed by the pool.
func (p *ConnPool) Size() int {
	return len(p.conns)
}

// Pick selects the connection with the lowest in-flight count across a 2-choice window
// starting from an atomic round-robin cursor (Power of Two Choices load balancing).
func (p *ConnPool) pick() *pooledConn {
	n := uint64(len(p.conns))
	if n == 1 {
		return p.conns[0]
	}
	idx1 := p.rrCursor.Add(1) % n
	idx2 := (idx1 + 1) % n
	c1 := p.conns[idx1]
	c2 := p.conns[idx2]
	if c2.inFlight.Load() < c1.inFlight.Load() {
		return c2
	}
	return c1
}

type traceCtxKey struct{}

// WithTraceHeaders attaches W3C traceparent and correlation ID to outgoing gRPC context.
func WithTraceHeaders(ctx context.Context, traceparent, correlationID string) context.Context {
	return context.WithValue(ctx, traceCtxKey{}, [2]string{traceparent, correlationID})
}

func injectOutgoingMetadata(ctx context.Context) context.Context {
	if vals, ok := ctx.Value(traceCtxKey{}).([2]string); ok {
		return metadata.AppendToOutgoingContext(ctx,
			"traceparent", vals[0],
			"x-correlation-id", vals[1],
			"x-source-service", "gateway-go",
		)
	}
	return metadata.AppendToOutgoingContext(ctx, "x-source-service", "gateway-go")
}

// isRetryableCode returns true only for transient transport/overload errors.
func isRetryableCode(code codes.Code) bool {
	switch code {
	case codes.Unavailable, codes.ResourceExhausted, codes.Aborted:
		return true
	default:
		return false
	}
}

// isSystemFailure determines whether an RPC error reflects upstream health degradation
// (5xx-equivalent) vs normal client validation rejection (InvalidArgument, NotFound, AlreadyExists).
func isSystemFailure(err error) bool {
	if err == nil {
		return false
	}
	switch status.Code(err) {
	case codes.OK, codes.InvalidArgument, codes.NotFound, codes.AlreadyExists,
		codes.PermissionDenied, codes.Unauthenticated, codes.FailedPrecondition, codes.OutOfRange:
		return false
	default:
		return true
	}
}

// InvokeUnary executes fn against a balanced grpc.ClientConn with deadline propagation,
// adaptive circuit-breaker protection, and automatic retry strictly limited to idempotent RPCs.
func (p *ConnPool) InvokeUnary(ctx context.Context, idempotent bool, fn func(callCtx context.Context, cc *grpc.ClientConn) error) error {
	maxAttempts := 1
	if idempotent && p.maxIdempotentRetries > 0 {
		maxAttempts = 1 + p.maxIdempotentRetries
	}

	// Propagate caller deadline if stricter than defaultTimeout; otherwise enforce defaultTimeout.
	baseCtx := injectOutgoingMetadata(ctx)
	if _, hasDeadline := baseCtx.Deadline(); !hasDeadline {
		var cancel context.CancelFunc
		baseCtx, cancel = context.WithTimeout(baseCtx, p.defaultTimeout)
		defer cancel()
	}

	var lastErr error
	for attempt := 0; attempt < maxAttempts; attempt++ {
		if attempt > 0 {
			p.metrics.GRPCRetriesTotal.Add(1)
			backoff := p.retryBaseBackoff * time.Duration(1<<(attempt-1))
			timer := time.NewTimer(backoff)
			select {
			case <-baseCtx.Done():
				timer.Stop()
				return baseCtx.Err()
			case <-timer.C:
			}
		}

		isProbe, berr := p.breaker.Allow()
		if berr != nil {
			p.metrics.GRPCFailuresTotal.Add(1)
			return berr
		}

		pc := p.pick()
		pc.inFlight.Add(1)
		p.metrics.GRPCRequestsTotal.Add(1)
		err := fn(baseCtx, pc.conn)
		pc.inFlight.Add(-1)

		sysFail := isSystemFailure(err)
		p.breaker.Record(isProbe, !sysFail)
		if err == nil {
			return nil
		}
		if sysFail {
			p.metrics.GRPCFailuresTotal.Add(1)
		}
		lastErr = err

		if !idempotent || isProbe || !isRetryableCode(status.Code(err)) {
			return lastErr
		}
	}
	return lastErr
}

// AcquireStreamConn returns a balanced *grpc.ClientConn for server-streaming RPCs (e.g., Chronos VCR replay).
func (p *ConnPool) AcquireStreamConn() (*grpc.ClientConn, func(), error) {
	isProbe, err := p.breaker.Allow()
	if err != nil {
		return nil, nil, err
	}
	pc := p.pick()
	pc.inFlight.Add(1)
	released := atomic.Bool{}
	release := func() {
		if released.CompareAndSwap(false, true) {
			pc.inFlight.Add(-1)
			if isProbe {
				p.breaker.Record(true, true)
			}
		}
	}
	return pc.conn, release, nil
}

// Close gracefully closes all pooled HTTP/2 connections.
func (p *ConnPool) Close() error {
	var firstErr error
	p.closeOnce.Do(func() {
		for _, pc := range p.conns {
			if err := pc.conn.Close(); err != nil && firstErr == nil {
				firstErr = err
			}
		}
	})
	return firstErr
}
