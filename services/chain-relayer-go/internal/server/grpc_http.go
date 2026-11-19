package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"

	omegav1 "github.com/vanovri/hyperion/proto/omega/gen/go/omegav1"
	"github.com/vanovri/hyperion/services/chain-relayer-go/internal/relayer"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// GRPCServer implements omegav1.L2SettlementServiceServer on port 50167 (01-hyperion-omega.md).
type GRPCServer struct {
	omegav1.UnimplementedL2SettlementServiceServer
	daemon *relayer.Daemon
	bus    *relayer.MemoryEventBus
}

func NewGRPCServer(d *relayer.Daemon, bus *relayer.MemoryEventBus) *GRPCServer {
	return &GRPCServer{daemon: d, bus: bus}
}

func (s *GRPCServer) SubmitWithdrawalIntent(
	_ context.Context,
	req *omegav1.WithdrawalEIP712Intent,
) (*omegav1.SubmitWithdrawalIntentResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "nil WithdrawalEIP712Intent")
	}
	return s.daemon.SubmitWithdrawal(req)
}

func (s *GRPCServer) AnchorStateRootBatch(
	ctx context.Context,
	req *omegav1.StateRootBatchCommit,
) (*omegav1.AnchorStateRootBatchResponse, error) {
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "nil StateRootBatchCommit")
	}
	if err := s.daemon.EnqueueStateRootBatch(req); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	_ = s.daemon.StepOnce(ctx)
	txID := fmt.Sprintf("anchor:epoch:%d:batch:%d", req.EpochId, req.BatchId)
	otx, ok := s.daemon.OutboxStore().GetTx(txID)
	resp := &omegav1.AnchorStateRootBatchResponse{
		BatchId:        req.BatchId,
		EpochId:        req.EpochId,
		Anchored:       ok,
		CommittedBatch: req,
	}
	if ok {
		resp.L2CommitTxHash = otx.LastTxHash[:]
		resp.L2BlockNumber = otx.MinedBlockNumber
	}
	return resp, nil
}

func (s *GRPCServer) GetRelayerStatus(
	_ context.Context,
	_ *omegav1.GetRelayerStatusRequest,
) (*omegav1.RelayerStatusResponse, error) {
	return s.daemon.Status(), nil
}

func (s *GRPCServer) StreamSettlementEvents(
	_ *omegav1.StreamSettlementEventsRequest,
	stream omegav1.L2SettlementService_StreamSettlementEventsServer,
) error {
	if s.bus == nil {
		return status.Error(codes.Unavailable, "event stream bus not configured")
	}
	_, ch, cancel := s.bus.Subscribe()
	defer cancel()

	for {
		select {
		case <-stream.Context().Done():
			return nil
		case env, ok := <-ch:
			if !ok {
				return nil
			}
			if err := stream.Send(env); err != nil {
				return err
			}
		}
	}
}

// NewHTTPMux builds the canonical HTTP observability & settlement status router on port 8097.
func NewHTTPMux(d *relayer.Daemon, bus *relayer.MemoryEventBus) *http.ServeMux {
	mux := http.NewServeMux()

	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		m := d.Metrics()
		_ = json.NewEncoder(w).Encode(map[string]any{
			"status":                "ok",
			"service":               "chain-relayer-go",
			"latest_observed_block": m.LatestObservedBlock,
			"latest_finalized_block": m.LatestFinalizedBlock,
		})
	})

	mux.HandleFunc("/metrics", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; version=0.0.4")
		m := d.Metrics()
		_, _ = fmt.Fprintf(
			w,
			"# HELP hyperion_relayer_observed_block Latest canonical L2 block observed\n"+
				"# TYPE hyperion_relayer_observed_block gauge\n"+
				"hyperion_relayer_observed_block %d\n"+
				"# HELP hyperion_relayer_finalized_block Latest finalized L2 block\n"+
				"# TYPE hyperion_relayer_finalized_block gauge\n"+
				"hyperion_relayer_finalized_block %d\n"+
				"# HELP hyperion_relayer_reorgs_total Total chain reorganizations handled\n"+
				"# TYPE hyperion_relayer_reorgs_total counter\n"+
				"hyperion_relayer_reorgs_total %d\n"+
				"# HELP hyperion_relayer_deposits_finalized_total Finalized vault deposits credited\n"+
				"# TYPE hyperion_relayer_deposits_finalized_total counter\n"+
				"hyperion_relayer_deposits_finalized_total %d\n"+
				"# HELP hyperion_relayer_deposits_revoked_total Staged deposits revoked due to chain reorgs\n"+
				"# TYPE hyperion_relayer_deposits_revoked_total counter\n"+
				"hyperion_relayer_deposits_revoked_total %d\n"+
				"# HELP hyperion_relayer_fee_bumps_total Dynamic EIP-1559 replacement fee bumps\n"+
				"# TYPE hyperion_relayer_fee_bumps_total counter\n"+
				"hyperion_relayer_fee_bumps_total %d\n"+
				"# HELP hyperion_relayer_rpc_errors_total Transient L2 RPC errors recovered\n"+
				"# TYPE hyperion_relayer_rpc_errors_total counter\n"+
				"hyperion_relayer_rpc_errors_total %d\n",
			m.LatestObservedBlock,
			m.LatestFinalizedBlock,
			m.TotalReorgsHandled,
			m.DepositsFinalizedTotal,
			m.DepositsRevokedTotal,
			m.FeeBumpsTotal,
			m.RPCErrorsTotal,
		)
	})

	mux.HandleFunc("/api/v1/settlement/status", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(d.Metrics())
	})

	mux.HandleFunc("/api/v1/settlement/reorgs", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if bus == nil {
			_ = json.NewEncoder(w).Encode([]any{})
			return
		}
		_ = json.NewEncoder(w).Encode(bus.EventsForTopic(relayer.TopicSettlementReorgs))
	})

	return mux
}
