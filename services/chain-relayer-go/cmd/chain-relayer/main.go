package main

import (
	"context"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	omegav1 "github.com/vanovri/hyperion/proto/omega/gen/go/omegav1"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/relayer"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/rpcsim"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/server"
	"google.golang.org/grpc"
)

func main() {
	grpcAddr := envOrDefault("HYPERION_RELAYER_GRPC_ADDR", ":50167")
	httpAddr := envOrDefault("HYPERION_RELAYER_HTTP_ADDR", ":8097")
	chainID := envUint64("HYPERION_L2_CHAIN_ID", 31337)

	simNode := rpcsim.NewSimulatedL2Node(chainID, 1_000_000_000, 100_000_000)
	bus := relayer.NewMemoryEventBus()

	cfg := relayer.DefaultConfig(chainID)
	daemon, err := relayer.NewDaemon(cfg, simNode, bus)
	if err != nil {
		log.Fatalf("failed to initialize chain-relayer-go daemon: %v", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	lis, err := net.Listen("tcp", grpcAddr)
	if err != nil {
		log.Fatalf("failed to bind gRPC %s: %v", grpcAddr, err)
	}
	grpcSrv := grpc.NewServer()
	omegav1.RegisterL2SettlementServiceServer(grpcSrv, server.NewGRPCServer(daemon, bus))

	httpSrv := &http.Server{
		Addr:              httpAddr,
		Handler:           server.NewHTTPMux(daemon, bus),
		ReadHeaderTimeout: 5 * time.Second,
	}

	go func() {
		log.Printf("[chain-relayer-go] gRPC listening on %s", grpcAddr)
		if err := grpcSrv.Serve(lis); err != nil {
			log.Printf("[chain-relayer-go] gRPC server stopped: %v", err)
		}
	}()

	go func() {
		log.Printf("[chain-relayer-go] HTTP /healthz & /metrics listening on %s", httpAddr)
		if err := httpSrv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Printf("[chain-relayer-go] HTTP server stopped: %v", err)
		}
	}()

	go func() {
		if err := daemon.Run(ctx); err != nil {
			log.Printf("[chain-relayer-go] daemon exited: %v", err)
		}
	}()

	<-ctx.Done()
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	grpcSrv.GracefulStop()
	_ = httpSrv.Shutdown(shutdownCtx)
}

func envOrDefault(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envUint64(key string, fallback uint64) uint64 {
	if v := os.Getenv(key); v != "" {
		if parsed, err := strconv.ParseUint(v, 10, 64); err == nil {
			return parsed
		}
	}
	return fallback
}
