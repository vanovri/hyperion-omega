package transport

import (
	"context"
	"crypto/ed25519"
	"encoding/binary"
	"math/big"
	"sync/atomic"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"

	"github.com/vanovri/hyperion/contracts/gen/go/fixedpoint"
	"github.com/vanovri/hyperion/contracts/gen/go/hyperionv1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// LedgerGRPCClient multiplexes gRPC requests across a pool of HTTP/2 connections
// to services/ledger-rs (port 50151 or Toxic TCP Proxy port 19001).
type LedgerGRPCClient struct {
	target  string
	conns   []*grpc.ClientConn
	clients []hyperionv1.OrderbookServiceClient
	rr      atomic.Uint64
}

func NewLedgerGRPCClient(target string, poolSize int) (*LedgerGRPCClient, error) {
	if poolSize <= 0 {
		poolSize = 4
	}
	c := &LedgerGRPCClient{
		target:  target,
		conns:   make([]*grpc.ClientConn, 0, poolSize),
		clients: make([]hyperionv1.OrderbookServiceClient, 0, poolSize),
	}
	for i := 0; i < poolSize; i++ {
		conn, err := grpc.NewClient(
			target,
			grpc.WithTransportCredentials(insecure.NewCredentials()),
		)
		if err != nil {
			_ = c.Close()
			return nil, err
		}
		c.conns = append(c.conns, conn)
		c.clients = append(c.clients, hyperionv1.NewOrderbookServiceClient(conn))
	}
	return c, nil
}

func (l *LedgerGRPCClient) Close() error {
	var firstErr error
	for _, conn := range l.conns {
		if err := conn.Close(); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

func (l *LedgerGRPCClient) pickClient() hyperionv1.OrderbookServiceClient {
	idx := l.rr.Add(1) % uint64(len(l.clients))
	return l.clients[idx]
}

// CreditInitialDeposit funds a simulated bot account on ledger-rs via double-entry CreditDeposit RPC.
func (l *LedgerGRPCClient) CreditInitialDeposit(ctx context.Context, depositID string, accountNum uint64, asset string, amountFP int64) error {
	if amountFP <= 0 {
		return nil
	}
	u128Amt, err := fixedpoint.ToProtoU128(big.NewInt(amountFP))
	if err != nil {
		return err
	}
	callCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	_, err = l.pickClient().CreditDeposit(callCtx, &hyperionv1.CreditDepositRequest{
		DepositId: depositID,
		AccountId: accountNum,
		Asset:     asset,
		AmountFp:  u128Amt,
		TsNs:      uint64(time.Now().UnixNano()),
		Trace: &hyperionv1.TraceMetadata{
			SourceService: "chaos-sim",
			IngestTsNs:    uint64(time.Now().UnixNano()),
		},
	})
	return err
}

// SendOrderIntent submits or cancels an order directly over gRPC, honoring chaos fault flags.
func (l *LedgerGRPCClient) SendOrderIntent(ctx context.Context, bot *agents.BotAccount, intent model.OrderIntent) model.SubmitResult {
	start := time.Now()

	var cidBuf [40]byte
	var idemBuf [40]byte
	cidStr := string(intent.FormatClientOrderID(cidBuf[:0]))
	idemStr := string(intent.FormatIdempotencyKey(idemBuf[:0]))

	effTs := intent.TimestampNs + intent.ClockSkewNs
	if effTs < 0 {
		effTs = 1
	}

	// Deterministic Ed25519 signature over compact binary preimage
	var sigPre [32]byte
	binary.LittleEndian.PutUint64(sigPre[0:8], intent.AccountNum)
	binary.LittleEndian.PutUint64(sigPre[8:16], uint64(effTs))
	binary.LittleEndian.PutUint64(sigPre[16:24], intent.Nonce)
	binary.LittleEndian.PutUint64(sigPre[24:32], intent.ClientOrderID)
	sig := ed25519.Sign(bot.PrivKey, sigPre[:])
	if intent.CorruptSignature && len(sig) > 0 {
		sig[0] ^= 0xFF
	}

	copies := int(intent.DuplicateCount)
	if copies < 1 {
		copies = 1
	}

	var firstResult model.SubmitResult
	for c := 0; c < copies; c++ {
		callCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
		client := l.pickClient()

		var ack *hyperionv1.OrderAck
		var rpcErr error

		if intent.Kind == model.ActionCancel {
			resp, err := client.CancelOrder(callCtx, &hyperionv1.CancelOrderRequest{
				Cancel: &hyperionv1.CancelOrder{
					OrderId:        intent.TargetServerID,
					ClientOrderId:  cidStr,
					AccountId:      intent.AccountNum,
					Symbol:         intent.Symbol,
					InstrumentId:   intent.InstrumentID,
					ClientTsNs:     uint64(effTs),
					Nonce:          intent.Nonce,
					Signature:      sig,
					IdempotencyKey: idemStr,
				},
			})
			if err == nil && resp != nil {
				ack = resp.Ack
			}
			rpcErr = err
		} else {
			resp, err := client.SubmitOrder(callCtx, &hyperionv1.SubmitOrderRequest{
				Order: &hyperionv1.SubmitOrder{
					ClientOrderId:  cidStr,
					AccountId:      intent.AccountNum,
					Symbol:         intent.Symbol,
					InstrumentId:   intent.InstrumentID,
					Side:           toProtoSide(intent.Side),
					OrderType:      toProtoOrderType(intent.OrderType),
					StpMode:        hyperionv1.SelfTradePreventionMode_SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING,
					PriceFp:        uint64(intent.Price),
					QtyFp:          uint64(intent.Quantity),
					ClientTsNs:     uint64(effTs),
					Nonce:          intent.Nonce,
					Signature:      sig,
					IdempotencyKey: idemStr,
				},
			})
			if err == nil && resp != nil {
				ack = resp.Ack
			}
			rpcErr = err
		}
		cancel()

		lat := time.Since(start).Nanoseconds()
		if rpcErr != nil || ack == nil {
			errStr := "EMPTY_GRPC_ACK"
			if rpcErr != nil {
				errStr = rpcErr.Error()
			}
			res := model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				TransportDrop:  true,
				RejectReason:   errStr,
				LatencyNs:      lat,
			}
			if c == 0 {
				firstResult = res
			}
			continue
		}

		parsed := mapProtoAck(intent, ack, lat)
		if c == 0 {
			firstResult = parsed
		}
	}
	return firstResult
}

func mapProtoAck(intent model.OrderIntent, ack *hyperionv1.OrderAck, latNs int64) model.SubmitResult {
	res := model.SubmitResult{
		ClientOrderID:  intent.ClientOrderID,
		ServerOrderID:  ack.OrderId,
		IdempotencyKey: intent.IdempotencyKey,
		BotID:          intent.BotID,
		AcceptedQty:    intent.Quantity,
		FilledQty:      int64(ack.FilledQtyFp),
		RemainingQty:   int64(ack.RemainingQtyFp),
		RejectReason:   ack.RejectReason.String(),
		LatencyNs:      latNs,
	}
	if ack.RejectReason == hyperionv1.RejectReason_REJECT_REASON_DUPLICATE_IDEMPOTENCY_KEY {
		res.Status = model.StatusDuplicateDeduped
		return res
	}
	switch ack.Status {
	case hyperionv1.OrderStatus_ORDER_STATUS_ACCEPTED:
		res.Status = model.StatusAccepted
	case hyperionv1.OrderStatus_ORDER_STATUS_PARTIALLY_FILLED:
		res.Status = model.StatusPartiallyFilled
	case hyperionv1.OrderStatus_ORDER_STATUS_FILLED:
		res.Status = model.StatusFilled
	case hyperionv1.OrderStatus_ORDER_STATUS_CANCELED:
		res.Status = model.StatusCanceled
	default:
		res.Status = model.StatusRejected
	}
	for _, pf := range ack.Fills {
		res.Fills = append(res.Fills, model.FillEvent{
			TradeID:        pf.FillId,
			ClientOrderID:  intent.ClientOrderID,
			ServerOrderID:  ack.OrderId,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			AccountID:      intent.AccountID,
			Symbol:         intent.Symbol,
			Side:           intent.Side,
			Price:          int64(pf.PriceFp),
			Quantity:       int64(pf.QtyFp),
			FeeQuote:       pf.TakerFeeFp,
			IsMaker:        false,
			TimestampNs:    int64(pf.ExecTsNs),
		})
	}
	return res
}

func toProtoSide(s model.Side) hyperionv1.OrderSide {
	if s == model.SideSell {
		return hyperionv1.OrderSide_ORDER_SIDE_SELL
	}
	return hyperionv1.OrderSide_ORDER_SIDE_BUY
}

func toProtoOrderType(t model.OrderType) hyperionv1.OrderType {
	switch t {
	case model.OrderTypeMarket:
		return hyperionv1.OrderType_ORDER_TYPE_MARKET
	case model.OrderTypeIOC:
		return hyperionv1.OrderType_ORDER_TYPE_IOC
	case model.OrderTypeFOK:
		return hyperionv1.OrderType_ORDER_TYPE_FOK
	case model.OrderTypePostOnly:
		return hyperionv1.OrderType_ORDER_TYPE_POST_ONLY
	default:
		return hyperionv1.OrderType_ORDER_TYPE_LIMIT
	}
}
