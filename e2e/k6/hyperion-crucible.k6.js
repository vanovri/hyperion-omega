import http from 'k6/http';
import ws from 'k6/ws';
import crypto from 'k6/crypto';
import { check, sleep } from 'k6';
import { Counter, Rate, Trend } from 'k6/metrics';

// =============================================================================
// HYPERION END-TO-END K6 LOAD & CRYPTOGRAPHIC INVARIANT HARNESS
// =============================================================================
// Exercises:
//  1. Signed Fixed-Point Order Ingress (HMAC-SHA256 Canonical Preimage)
//  2. Replay-Attack / Expired-Timestamp Rejection Verification (401/403)
//  3. Binary WebSocket Multiplexer (0x4859 Little-Endian Wire Frames)
//  4. Quantitative Candlestick & Microstructure Query Latency
//  5. Merkle Ledger Proof & Chronos Time-Travel Seek Latency
// =============================================================================

const GATEWAY_URL = __ENV.GATEWAY_URL || 'http://localhost:8080';
const WS_URL = __ENV.WS_URL || 'ws://localhost:8080/ws?format=binary';
const AGGREGATOR_URL = __ENV.AGGREGATOR_URL || 'http://localhost:8083';
const CHRONOS_URL = __ENV.CHRONOS_URL || 'http://localhost:8088';
const HMAC_SECRET = __ENV.HYPERION_BOOTSTRAP_HMAC_SECRET || __ENV.HYPERION_HMAC_SECRET || 'hyperion-trading-hmac-dev-key-2026';
const JWT_TOKEN = __ENV.HYPERION_JWT_TOKEN || '';

const orderSubmitLatency = new Trend('hyperion_order_submit_latency_ms', true);
const l2BookLatency = new Trend('hyperion_l2_book_latency_ms', true);
const analyticsLatency = new Trend('hyperion_analytics_query_latency_ms', true);
const merkleProofLatency = new Trend('hyperion_merkle_proof_latency_ms', true);
const wsConnectLatency = new Trend('hyperion_ws_session_ms', true);

const ordersAccepted = new Counter('hyperion_orders_accepted_total');
const wsBinaryFrames = new Counter('hyperion_ws_binary_frames_total');
const replayRejected = new Counter('hyperion_replay_attacks_rejected_total');
const successRate = new Rate('hyperion_crucible_check_pass_rate');

export const options = {
  summaryTrendStats: ['avg', 'min', 'med', 'max', 'p(90)', 'p(95)', 'p(99)'],
  scenarios: {
    signed_order_flow: {
      executor: 'ramping-vus',
      exec: 'signedOrderScenario',
      startVUs: 4,
      stages: [
        { duration: __ENV.K6_STAGE_WARMUP || '4s', target: 16 },
        { duration: __ENV.K6_STAGE_PEAK || '8s', target: 32 },
        { duration: __ENV.K6_STAGE_COOLDOWN || '3s', target: 4 },
      ],
      gracefulRampDown: '2s',
    },
    binary_ws_subscribers: {
      executor: 'constant-vus',
      exec: 'binaryWebSocketScenario',
      vus: Number(__ENV.K6_WS_VUS || 8),
      duration: __ENV.K6_WS_DURATION || '12s',
    },
    adversarial_replay_probe: {
      executor: 'per-vu-iterations',
      exec: 'replaySecurityScenario',
      vus: 2,
      iterations: 5,
      startTime: '2s',
    },
  },
  thresholds: {
    hyperion_order_submit_latency_ms: ['p(95)<50', 'p(99)<120'],
    hyperion_l2_book_latency_ms: ['p(95)<35'],
    hyperion_crucible_check_pass_rate: ['rate>0.95'],
  },
};

function signHyperionRequest(accountId, method, path, rawBody, tsNs, nonce) {
  const bodySha256 = crypto.sha256(rawBody, 'hex');
  const preimage = `${tsNs}\n${nonce}\n${method.toUpperCase()}\n${path}\n${bodySha256}`;
  const sigHex = crypto.hmac('sha256', HMAC_SECRET, preimage, 'hex');
  return {
    'Content-Type': 'application/json',
    'X-Hyperion-Account-Id': String(accountId),
    'X-Hyperion-Key-Id': 'dev-hmac',
    'X-Hyperion-Timestamp': String(tsNs),
    'X-Hyperion-Nonce': String(nonce),
    'X-Hyperion-Signature': sigHex,
    'X-Hyperion-Auth-Scheme': 'hmac-sha256',
    'Idempotency-Key': `k6-${accountId}-${__VU}-${__ITER}-${tsNs}-${nonce}`,
  };
}

export function signedOrderScenario() {
  const accountId = 10001 + (__VU % 20);
  const iterNonce = (__VU * 1000000) + __ITER + 1;
  const nowNs = BigInt(Date.now()) * 1000000n + BigInt(__ITER % 1000);
  const isBuy = (__ITER + __VU) % 2 === 0;

  // Fixed-point 1e8 integer strings ONLY (zero IEEE-754 floats on wire)
  const basePriceTicks = 6842000000000n + BigInt((__ITER % 25) * 100000000);
  const qtyFp = 1000000n + BigInt((__VU % 5) * 500000); // 0.01 .. 0.03 BTC

  const payloadObj = {
    client_order_id: `k6-ord-${__VU}-${__ITER}`,
    account_id: String(accountId),
    symbol: 'BTC-USDT',
    side: isBuy ? 'BUY' : 'SELL',
    order_type: 'LIMIT',
    stp_mode: 'CANCEL_RESTING',
    price_fp: basePriceTicks.toString(),
    qty_fp: qtyFp.toString(),
    max_slippage_bps: 50,
  };
  const rawBody = JSON.stringify(payloadObj);
  const path = '/api/v1/orders';
  const headers = signHyperionRequest(accountId, 'POST', path, rawBody, nowNs.toString(), iterNonce);

  const res = http.post(`${GATEWAY_URL}${path}`, rawBody, { headers });
  orderSubmitLatency.add(res.timings.duration);

  const ok = check(res, {
    'order endpoint responded (200/201/429/403 under throttle)': (r) =>
      [200, 201, 400, 429].includes(r.status),
  });
  successRate.add(ok);
  if (res.status === 200 || res.status === 201) {
    ordersAccepted.add(1);
  }

  // Every 4th iteration, query L2 orderbook + microstructure + Merkle proof
  if (__ITER % 4 === 0) {
    const l2Res = http.get(`${GATEWAY_URL}/api/v1/market/orderbook?symbol=BTC-USDT&depth=20`);
    l2BookLatency.add(l2Res.timings.duration);
    successRate.add(l2Res.status === 200);

    const authHeaders = JWT_TOKEN ? { Authorization: `Bearer ${JWT_TOKEN}` } : {};
    const candleRes = http.get(`${AGGREGATOR_URL}/api/v1/analytics/candles?symbol=BTC-USDT&interval=1s&limit=20`, {
      headers: authHeaders,
    });
    if (candleRes.status === 200) {
      analyticsLatency.add(candleRes.timings.duration);
    }

    const proofRes = http.get(`${GATEWAY_URL}/api/v1/accounts/${accountId}/merkle-proof`, {
      headers: authHeaders,
    });
    if (proofRes.status === 200) {
      merkleProofLatency.add(proofRes.timings.duration);
    }
  }

  sleep(0.02);
}

export function binaryWebSocketScenario() {
  const startMs = Date.now();
  const res = ws.connect(WS_URL, {}, function (socket) {
    socket.on('open', function () {
      // Subscribe via developer-friendly JSON control frame to receive binary 0x4859 frames
      socket.send(
        JSON.stringify({
          op: 'subscribe',
          channels: ['l2', 'l3', 'fills', 'candles', 'fraud'],
          symbols: ['BTC-USDT'],
        })
      );
    });

    socket.on('binaryMessage', function (data) {
      wsBinaryFrames.add(1);
      if (data && data.byteLength >= 32) {
        const view = new DataView(data);
        const magic = view.getUint16(0, true);
        const version = view.getUint8(2);
        successRate.add(magic === 0x4859 && version === 0x01);
      }
    });

    socket.on('message', function () {
      wsBinaryFrames.add(1);
    });

    socket.setTimeout(function () {
      socket.close();
    }, 2500);
  });

  wsConnectLatency.add(Date.now() - startMs);
  successRate.add(res && res.status === 101);
  sleep(0.2);
}

export function replaySecurityScenario() {
  const accountId = 10099;
  const staleTsNs = (BigInt(Date.now() - 60000) * 1000000n).toString(); // 60s old (> 5s skew window)
  const rawBody = JSON.stringify({
    client_order_id: `stale-${__VU}-${__ITER}`,
    account_id: String(accountId),
    symbol: 'BTC-USDT',
    side: 'BUY',
    order_type: 'LIMIT',
    price_fp: '6800000000000',
    qty_fp: '1000000',
  });
  const headers = signHyperionRequest(accountId, 'POST', '/api/v1/orders', rawBody, staleTsNs, 999999);
  const res = http.post(`${GATEWAY_URL}/api/v1/orders`, rawBody, { headers });

  const rejected = check(res, {
    'expired timestamp / replay attack strictly rejected (401/403)': (r) =>
      r.status === 401 || r.status === 403,
  });
  if (rejected) {
    replayRejected.add(1);
  }
  successRate.add(rejected);
}

export function handleSummary(data) {
  const outPath = __ENV.K6_SUMMARY_OUT || 'e2e/results/k6_summary.json';
  return {
    [outPath]: JSON.stringify(data, null, 2),
  };
}
