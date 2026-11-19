package transport

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"hash"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/vanovri/hyperion/services/chaos-sim/internal/agents"
	"github.com/vanovri/hyperion/services/chaos-sim/internal/model"
)

type AuthScheme string

const (
	AuthEd25519    AuthScheme = "ed25519"
	AuthHMACSHA256 AuthScheme = "hmac-sha256"
)

var bufPool = sync.Pool{
	New: func() any {
		b := make([]byte, 0, 512)
		return &b
	},
}

// hmacScratch bundles a reusable sha256.Hash together with all fixed-size signing
// buffers on cache-line-aligned contiguous memory, achieving 0 B/op and 0 allocs/op.
type hmacScratch struct {
	h        hash.Hash
	ipad     [64]byte
	opad     [64]byte
	bodySum  [32]byte
	innerSum [32]byte
	macSum   [32]byte
	bodyHex  [64]byte
	preimage [256]byte
	_pad     [64]byte
}

var hmacScratchPool = sync.Pool{
	New: func() any {
		return &hmacScratch{h: sha256.New()}
	},
}

// BuildCanonicalSignatureInto computes the exact §6.2 cryptographic signature & preimage
// with ZERO heap allocations on the HMAC-SHA256 fast path using cache-line-aligned pooled scratch buffers.
// Canonical Preimage:
// <X-Hyperion-Timestamp>\n<X-Hyperion-Nonce>\n<HTTP_METHOD_UPPERCASE>\n<REQUEST_PATH>\n<SHA256_HEX_LOWERCASE(RAW_BODY)>
func BuildCanonicalSignatureInto(
	sigDst []byte,
	tsDst []byte,
	nonceDst []byte,
	scheme AuthScheme,
	privKey ed25519.PrivateKey,
	hmacSecret [32]byte,
	tsNs int64,
	nonce uint64,
	method string,
	reqPath string,
	rawBody []byte,
	corrupt bool,
) (sigOut []byte, tsOut []byte, nonceOut []byte) {
	tsOut = strconv.AppendInt(tsDst[:0], tsNs, 10)
	nonceOut = strconv.AppendUint(nonceDst[:0], nonce, 10)

	sc := hmacScratchPool.Get().(*hmacScratch)
	defer hmacScratchPool.Put(sc)

	sc.h.Reset()
	_, _ = sc.h.Write(rawBody)
	sc.h.Sum(sc.bodySum[:0])
	hex.Encode(sc.bodyHex[:], sc.bodySum[:])

	preimage := sc.preimage[:0]
	preimage = append(preimage, tsOut...)
	preimage = append(preimage, '\n')
	preimage = append(preimage, nonceOut...)
	preimage = append(preimage, '\n')
	for i := 0; i < len(method); i++ {
		c := method[i]
		if c >= 'a' && c <= 'z' {
			c -= 'a' - 'A'
		}
		preimage = append(preimage, c)
	}
	preimage = append(preimage, '\n')
	preimage = append(preimage, reqPath...)
	preimage = append(preimage, '\n')
	preimage = append(preimage, sc.bodyHex[:]...)

	if scheme == AuthHMACSHA256 {
		// Zero-allocation RFC-2104 HMAC-SHA256 over pooled 64-byte block pads.
		clear(sc.ipad[:])
		clear(sc.opad[:])
		copy(sc.ipad[:], hmacSecret[:])
		copy(sc.opad[:], hmacSecret[:])
		for i := 0; i < 64; i++ {
			sc.ipad[i] ^= 0x36
			sc.opad[i] ^= 0x5c
		}
		sc.h.Reset()
		_, _ = sc.h.Write(sc.ipad[:])
		_, _ = sc.h.Write(preimage)
		sc.h.Sum(sc.innerSum[:0])

		sc.h.Reset()
		_, _ = sc.h.Write(sc.opad[:])
		_, _ = sc.h.Write(sc.innerSum[:])
		sc.h.Sum(sc.macSum[:0])

		if corrupt {
			sc.macSum[0] ^= 0xFF
		}
		if cap(sigDst) < 64 {
			sigDst = make([]byte, 64)
		} else {
			sigDst = sigDst[:64]
		}
		hex.Encode(sigDst, sc.macSum[:])
		return sigDst, tsOut, nonceOut
	}

	rawSig := ed25519.Sign(privKey, preimage)
	if corrupt && len(rawSig) > 0 {
		rawSig[0] ^= 0xFF
	}
	hexLen := hex.EncodedLen(len(rawSig))
	if cap(sigDst) < hexLen {
		sigDst = make([]byte, hexLen)
	} else {
		sigDst = sigDst[:hexLen]
	}
	hex.Encode(sigDst, rawSig)
	return sigDst, tsOut, nonceOut
}

// BuildCanonicalSignature computes the exact §6.2 cryptographic signature & preimage as strings.
func BuildCanonicalSignature(
	scheme AuthScheme,
	privKey ed25519.PrivateKey,
	hmacSecret [32]byte,
	tsNs int64,
	nonce uint64,
	method string,
	reqPath string,
	rawBody []byte,
	corrupt bool,
) (sigHex string, tsHeader string, nonceHeader string) {
	var sigBuf [128]byte
	var tsBuf [24]byte
	var nonceBuf [24]byte
	sigB, tsB, nonceB := BuildCanonicalSignatureInto(
		sigBuf[:0], tsBuf[:0], nonceBuf[:0],
		scheme, privKey, hmacSecret, tsNs, nonce, method, reqPath, rawBody, corrupt,
	)
	return string(sigB), string(tsB), string(nonceB)
}

// GatewayHTTPClient dispatches signed fixed-point REST mutations to services/gateway-go (:8080).
type GatewayHTTPClient struct {
	baseURL string
	scheme  AuthScheme
	client  *http.Client
}

func NewGatewayHTTPClient(baseURL string, scheme AuthScheme, maxConns int) *GatewayHTTPClient {
	if maxConns <= 0 {
		maxConns = 512
	}
	if scheme == "" {
		scheme = AuthEd25519
	}
	tr := &http.Transport{
		MaxIdleConns:        maxConns,
		MaxIdleConnsPerHost: maxConns,
		MaxConnsPerHost:     maxConns * 2,
		IdleConnTimeout:     90 * time.Second,
		DisableCompression:  true,
	}
	return &GatewayHTTPClient{
		baseURL: strings.TrimRight(baseURL, "/"),
		scheme:  scheme,
		client: &http.Client{
			Transport: tr,
			Timeout:   4 * time.Second,
		},
	}
}

type orderAckWire struct {
	EngineSeq      string `json:"engine_seq"`
	OrderID        string `json:"order_id"`
	ClientOrderID  string `json:"client_order_id"`
	AccountID      string `json:"account_id"`
	Symbol         string `json:"symbol"`
	Status         string `json:"status"`
	RejectReason   string `json:"reject_reason"`
	FilledQtyFP    string `json:"filled_qty_fp"`
	RemainingQtyFP string `json:"remaining_qty_fp"`
	Fills          []struct {
		FillID         string `json:"fill_id"`
		MakerOrderID   string `json:"maker_order_id"`
		TakerOrderID   string `json:"taker_order_id"`
		MakerAccountID string `json:"maker_account_id"`
		TakerAccountID string `json:"taker_account_id"`
		AggressorSide  string `json:"aggressor_side"`
		PriceFP        string `json:"price_fp"`
		QtyFP          string `json:"qty_fp"`
		MakerFeeFP     string `json:"maker_fee_fp"`
		TakerFeeFP     string `json:"taker_fee_fp"`
		ExecTsNs       string `json:"exec_ts_ns"`
	} `json:"fills"`
}

// SendOrderIntent sends a signed Submit or Cancel HTTP request to Gateway, honoring
// chaos clock-skew, signature corruption, and idempotent duplication counts.
func (g *GatewayHTTPClient) SendOrderIntent(ctx context.Context, bot *agents.BotAccount, intent model.OrderIntent) model.SubmitResult {
	start := time.Now()

	var cidBuf [40]byte
	var idemBuf [40]byte
	cidStr := string(intent.FormatClientOrderID(cidBuf[:0]))
	idemStr := string(intent.FormatIdempotencyKey(idemBuf[:0]))

	effTs := intent.TimestampNs + intent.ClockSkewNs
	if effTs <= 0 {
		effTs = time.Now().UnixNano() + intent.ClockSkewNs
	}

	var method, reqPath, fullURL string
	var bodyBytes []byte

	if intent.Kind == model.ActionCancel {
		method = http.MethodDelete
		ordIDStr := strconv.FormatUint(intent.TargetServerID, 10)
		if intent.TargetServerID == 0 {
			ordIDStr = strconv.FormatUint(intent.TargetOrderID, 10)
		}
		reqPath = "/api/v1/orders/" + ordIDStr
		fullURL = g.baseURL + reqPath + "?symbol=" + intent.Symbol
	} else {
		method = http.MethodPost
		reqPath = "/api/v1/orders"
		fullURL = g.baseURL + reqPath

		bp := bufPool.Get().(*[]byte)
		b := (*bp)[:0]
		b = append(b, `{"client_order_id":"`...)
		b = append(b, cidStr...)
		b = append(b, `","account_id":"`...)
		b = append(b, intent.AccountID...)
		b = append(b, `","symbol":"`...)
		b = append(b, intent.Symbol...)
		b = append(b, `","side":"`...)
		b = append(b, intent.Side.String()...)
		b = append(b, `","order_type":"`...)
		b = append(b, intent.OrderType.String()...)
		b = append(b, `","stp_mode":"CANCEL_RESTING","price_fp":"`...)
		b = strconv.AppendInt(b, intent.Price, 10)
		b = append(b, `","qty_fp":"`...)
		b = strconv.AppendInt(b, intent.Quantity, 10)
		b = append(b, `"}`...)
		bodyBytes = make([]byte, len(b))
		copy(bodyBytes, b)
		*bp = b[:0]
		bufPool.Put(bp)
	}

	sigHex, tsHdr, nonceHdr := BuildCanonicalSignature(
		g.scheme,
		bot.PrivKey,
		bot.HMACSecret,
		effTs,
		intent.Nonce,
		method,
		reqPath,
		bodyBytes,
		intent.CorruptSignature,
	)

	copies := int(intent.DuplicateCount)
	if copies < 1 {
		copies = 1
	}

	var firstResult model.SubmitResult
	for c := 0; c < copies; c++ {
		req, err := http.NewRequestWithContext(ctx, method, fullURL, bytes.NewReader(bodyBytes))
		if err != nil {
			return model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				Status:         model.StatusRejected,
				RejectReason:   err.Error(),
			}
		}
		if len(bodyBytes) > 0 {
			req.Header.Set("Content-Type", "application/json")
		}
		req.Header.Set("Idempotency-Key", idemStr)
		req.Header.Set("X-Hyperion-Account-Id", intent.AccountID)
		req.Header.Set("X-Hyperion-Key-Id", bot.KeyID)
		req.Header.Set("X-Hyperion-Timestamp", tsHdr)
		req.Header.Set("X-Hyperion-Nonce", nonceHdr)
		req.Header.Set("X-Hyperion-Signature", sigHex)
		req.Header.Set("X-Hyperion-Auth-Scheme", string(g.scheme))

		resp, err := g.client.Do(req)
		if err != nil {
			res := model.SubmitResult{
				ClientOrderID:  intent.ClientOrderID,
				IdempotencyKey: intent.IdempotencyKey,
				BotID:          intent.BotID,
				TransportDrop:  true,
				RejectReason:   err.Error(),
				LatencyNs:      time.Since(start).Nanoseconds(),
			}
			if c == 0 {
				firstResult = res
			}
			continue
		}

		respBody, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
		_ = resp.Body.Close()

		parsed := parseGatewayResponse(intent, resp.StatusCode, respBody, time.Since(start).Nanoseconds())
		if c == 0 {
			firstResult = parsed
		}
	}
	return firstResult
}

func parseGatewayResponse(intent model.OrderIntent, statusCode int, body []byte, latNs int64) model.SubmitResult {
	res := model.SubmitResult{
		ClientOrderID:  intent.ClientOrderID,
		IdempotencyKey: intent.IdempotencyKey,
		BotID:          intent.BotID,
		LatencyNs:      latNs,
	}
	if statusCode == http.StatusConflict {
		res.Status = model.StatusDuplicateDeduped
		return res
	}
	if statusCode < 200 || statusCode >= 300 {
		res.Status = model.StatusRejected
		res.RejectReason = fmt.Sprintf("HTTP_%d", statusCode)
		return res
	}

	var ack orderAckWire
	if err := json.Unmarshal(body, &ack); err != nil {
		res.Status = model.StatusRejected
		res.RejectReason = "INVALID_ACK_JSON"
		return res
	}
	if ack.RejectReason == "DUPLICATE_IDEMPOTENCY_KEY" || ack.RejectReason == "REJECT_REASON_DUPLICATE_IDEMPOTENCY_KEY" {
		res.Status = model.StatusDuplicateDeduped
		return res
	}

	srvID, _ := strconv.ParseUint(ack.OrderID, 10, 64)
	filledFP, _ := strconv.ParseInt(ack.FilledQtyFP, 10, 64)
	remFP, _ := strconv.ParseInt(ack.RemainingQtyFP, 10, 64)

	res.ServerOrderID = srvID
	res.AcceptedQty = intent.Quantity
	res.FilledQty = filledFP
	res.RemainingQty = remFP
	res.RejectReason = ack.RejectReason

	switch ack.Status {
	case "ACCEPTED", "ORDER_STATUS_ACCEPTED":
		res.Status = model.StatusAccepted
	case "PARTIALLY_FILLED", "ORDER_STATUS_PARTIALLY_FILLED":
		res.Status = model.StatusPartiallyFilled
	case "FILLED", "ORDER_STATUS_FILLED":
		res.Status = model.StatusFilled
	case "CANCELED", "ORDER_STATUS_CANCELED":
		res.Status = model.StatusCanceled
	default:
		res.Status = model.StatusRejected
	}

	for _, wf := range ack.Fills {
		tid, _ := strconv.ParseUint(wf.FillID, 10, 64)
		px, _ := strconv.ParseInt(wf.PriceFP, 10, 64)
		qty, _ := strconv.ParseInt(wf.QtyFP, 10, 64)
		tFee, _ := strconv.ParseInt(wf.TakerFeeFP, 10, 64)
		ts, _ := strconv.ParseInt(wf.ExecTsNs, 10, 64)
		res.Fills = append(res.Fills, model.FillEvent{
			TradeID:        tid,
			ClientOrderID:  intent.ClientOrderID,
			ServerOrderID:  srvID,
			IdempotencyKey: intent.IdempotencyKey,
			BotID:          intent.BotID,
			AccountID:      intent.AccountID,
			CounterpartyID: wf.MakerAccountID,
			Symbol:         intent.Symbol,
			Side:           intent.Side,
			Price:          px,
			Quantity:       qty,
			FeeQuote:       tFee,
			IsMaker:        false,
			TimestampNs:    ts,
		})
	}
	return res
}
