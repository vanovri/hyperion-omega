package chaos

import (
	"context"
	"errors"
	"io"
	"net"
	"sync"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/rng"
)

// ToxicConfig configures L4 TCP fault injection between microservices.
type ToxicConfig struct {
	Enabled        bool    `json:"enabled"`
	Partitioned    bool    `json:"partitioned"`       // Complete TCP blackhole / immediate RST
	BaseLatencyMs  float64 `json:"base_latency_ms"`   // Constant added latency (ms)
	ParetoJitterMs float64 `json:"pareto_jitter_ms"`  // Pareto scale xm (ms)
	ParetoAlpha    float64 `json:"pareto_alpha"`      // Pareto tail exponent alpha (>1.0)
	MaxJitterMs    float64 `json:"max_jitter_ms"`     // Hard upper cap on Pareto draw (ms)
	DropConnProb   float64 `json:"drop_conn_prob"`    // Per-chunk probability of abrupt TCP RST
	SliceChunkSize int     `json:"slice_chunk_size"`  // Fragment writes into N-byte slices (0 = default 16KB)
}

func DefaultToxicConfig() ToxicConfig {
	return ToxicConfig{
		Enabled:        false,
		Partitioned:    false,
		BaseLatencyMs:  0,
		ParetoJitterMs: 0,
		ParetoAlpha:    1.5,
		MaxJitterMs:    500.0,
		DropConnProb:   0,
		SliceChunkSize: 0,
	}
}

type ToxicProxyStats struct {
	ListenAddr       string `json:"listen_addr"`
	UpstreamAddr     string `json:"upstream_addr"`
	ActiveConns      int64  `json:"active_conns"`
	TotalConns       uint64 `json:"total_conns"`
	BytesForwarded   uint64 `json:"bytes_forwarded"`
	DelayedChunks    uint64 `json:"delayed_chunks"`
	ResetConnections uint64 `json:"reset_connections"`
	PartitionDrops   uint64 `json:"partition_drops"`
}

// ToxicProxy is a deterministic, zero-leak L4 TCP fault-injecting proxy.
type ToxicProxy struct {
	listenAddr   string
	upstreamAddr string
	masterSeed   uint64

	cfgPtr atomic.Pointer[ToxicConfig]
	ln     net.Listener

	ctx    context.Context
	cancel context.CancelFunc
	wg     sync.WaitGroup

	mu        sync.Mutex
	openConns map[net.Conn]struct{}

	connSeq          atomic.Uint64
	activeConns      atomic.Int64
	bytesForwarded   atomic.Uint64
	delayedChunks    atomic.Uint64
	resetConnections atomic.Uint64
	partitionDrops   atomic.Uint64
}

func NewToxicProxy(listenAddr, upstreamAddr string, seed uint64, initial ToxicConfig) *ToxicProxy {
	ctx, cancel := context.WithCancel(context.Background())
	p := &ToxicProxy{
		listenAddr:   listenAddr,
		upstreamAddr: upstreamAddr,
		masterSeed:   seed,
		ctx:          ctx,
		cancel:       cancel,
		openConns:    make(map[net.Conn]struct{}),
	}
	cfgCopy := initial
	p.cfgPtr.Store(&cfgCopy)
	return p
}

// Start binds the TCP listener and spawns the accept loop.
func (p *ToxicProxy) Start() error {
	ln, err := net.Listen("tcp", p.listenAddr)
	if err != nil {
		return err
	}
	p.ln = ln
	p.listenAddr = ln.Addr().String()

	p.wg.Add(1)
	go p.acceptLoop()
	return nil
}

func (p *ToxicProxy) Addr() string {
	return p.listenAddr
}

func (p *ToxicProxy) UpdateConfig(cfg ToxicConfig) {
	c := cfg
	p.cfgPtr.Store(&c)
	if c.Enabled && c.Partitioned {
		// Sever all currently established sockets immediately on partition activation.
		p.severActiveConnections()
	}
}

func (p *ToxicProxy) Config() ToxicConfig {
	c := p.cfgPtr.Load()
	if c == nil {
		return DefaultToxicConfig()
	}
	return *c
}

func (p *ToxicProxy) Stats() ToxicProxyStats {
	return ToxicProxyStats{
		ListenAddr:       p.listenAddr,
		UpstreamAddr:     p.upstreamAddr,
		ActiveConns:      p.activeConns.Load(),
		TotalConns:       p.connSeq.Load(),
		BytesForwarded:   p.bytesForwarded.Load(),
		DelayedChunks:    p.delayedChunks.Load(),
		ResetConnections: p.resetConnections.Load(),
		PartitionDrops:   p.partitionDrops.Load(),
	}
}

func (p *ToxicProxy) Stop() error {
	p.cancel()
	var err error
	if p.ln != nil {
		err = p.ln.Close()
	}
	p.severActiveConnections()
	p.wg.Wait()
	return err
}

func (p *ToxicProxy) trackConn(c net.Conn, add bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if add {
		p.openConns[c] = struct{}{}
	} else {
		delete(p.openConns, c)
	}
}

func (p *ToxicProxy) severActiveConnections() {
	p.mu.Lock()
	conns := make([]net.Conn, 0, len(p.openConns))
	for c := range p.openConns {
		conns = append(conns, c)
	}
	p.mu.Unlock()

	for _, c := range conns {
		resetTCPConn(c)
	}
}

func resetTCPConn(c net.Conn) {
	if tcp, ok := c.(*net.TCPConn); ok {
		_ = tcp.SetLinger(0) // Send immediate TCP RST
	}
	_ = c.Close()
}

func (p *ToxicProxy) acceptLoop() {
	defer p.wg.Done()
	for {
		clientConn, err := p.ln.Accept()
		if err != nil {
			if p.ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return
			}
			continue
		}

		seq := p.connSeq.Add(1)
		cfg := p.cfgPtr.Load()
		if cfg != nil && cfg.Enabled && cfg.Partitioned {
			p.partitionDrops.Add(1)
			resetTCPConn(clientConn)
			continue
		}

		p.wg.Add(1)
		go p.handleConn(clientConn, seq)
	}
}

func (p *ToxicProxy) handleConn(clientConn net.Conn, seq uint64) {
	defer p.wg.Done()
	p.activeConns.Add(1)
	defer p.activeConns.Add(-1)

	p.trackConn(clientConn, true)
	defer func() {
		p.trackConn(clientConn, false)
		_ = clientConn.Close()
	}()

	dialer := net.Dialer{Timeout: 3 * time.Second}
	upConn, err := dialer.DialContext(p.ctx, "tcp", p.upstreamAddr)
	if err != nil {
		resetTCPConn(clientConn)
		return
	}
	p.trackConn(upConn, true)
	defer func() {
		p.trackConn(upConn, false)
		_ = upConn.Close()
	}()

	var pipeWG sync.WaitGroup
	pipeWG.Add(2)
	go func() {
		defer pipeWG.Done()
		p.pump(clientConn, upConn, rng.NewStream(p.masterSeed, seq*2))
		_ = upConn.SetReadDeadline(time.Now())
	}()
	go func() {
		defer pipeWG.Done()
		p.pump(upConn, clientConn, rng.NewStream(p.masterSeed, seq*2+1))
		_ = clientConn.SetReadDeadline(time.Now())
	}()
	pipeWG.Wait()
}

func (p *ToxicProxy) pump(src, dst net.Conn, stream rng.Stream) {
	buf := make([]byte, 16*1024)
	timer := time.NewTimer(time.Hour)
	if !timer.Stop() {
		select {
		case <-timer.C:
		default:
		}
	}
	defer timer.Stop()

	for {
		if p.ctx.Err() != nil {
			return
		}
		cfg := p.cfgPtr.Load()
		readLimit := len(buf)
		if cfg != nil && cfg.Enabled && cfg.SliceChunkSize > 0 && cfg.SliceChunkSize < readLimit {
			readLimit = cfg.SliceChunkSize
		}

		n, rErr := src.Read(buf[:readLimit])
		if n > 0 {
			cfg = p.cfgPtr.Load()
			if cfg != nil && cfg.Enabled {
				if cfg.Partitioned {
					p.partitionDrops.Add(1)
					resetTCPConn(src)
					resetTCPConn(dst)
					return
				}
				if cfg.DropConnProb > 0 && stream.Float64() < cfg.DropConnProb {
					p.resetConnections.Add(1)
					resetTCPConn(src)
					resetTCPConn(dst)
					return
				}
				delayMs := cfg.BaseLatencyMs
				if cfg.ParetoJitterMs > 0 {
					maxCap := cfg.MaxJitterMs
					if maxCap < cfg.ParetoJitterMs {
						maxCap = cfg.ParetoJitterMs * 20.0
					}
					delayMs += stream.Pareto(cfg.ParetoJitterMs, cfg.ParetoAlpha, maxCap)
				}
				if delayMs > 0.05 {
					p.delayedChunks.Add(1)
					dur := time.Duration(delayMs * float64(time.Millisecond))
					timer.Reset(dur)
					select {
					case <-p.ctx.Done():
						return
					case <-timer.C:
					}
				}
			}

			written := 0
			for written < n {
				wn, wErr := dst.Write(buf[written:n])
				if wn > 0 {
					written += wn
					p.bytesForwarded.Add(uint64(wn))
				}
				if wErr != nil {
					return
				}
			}
		}
		if rErr != nil {
			if !errors.Is(rErr, io.EOF) && p.ctx.Err() == nil {
				return
			}
			return
		}
	}
}
