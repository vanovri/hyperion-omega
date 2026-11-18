import assert from "node:assert/strict";
import test from "node:test";
import {
  AnomalyPatternType,
  MitigationActionType,
  OrderSide,
  OrderType,
  RiskMitigationCommand,
  SelfTradePreventionMode,
  SubmitOrder,
  TimelineForkRequest,
  WS_FRAME_MAGIC,
  WS_MSG_TYPE,
  bigIntToI128,
  bigIntToU128,
  computeNotionalFp,
  decodeWsHeader,
  encodeWsHeader,
  i128ToBigInt,
  u128ToBigInt,
} from "./index";

test("bigint fixed-point 128-bit roundtrip and notional arithmetic", () => {
  for (const sample of [0n, 1n, -1n, (1n << 90n) + 77n, -((1n << 102n) + 19n)]) {
    assert.equal(i128ToBigInt(bigIntToI128(sample)), sample);
  }
  for (const sample of [0n, 1n, (1n << 115n) + 13n]) {
    assert.equal(u128ToBigInt(bigIntToU128(sample)), sample);
  }
  assert.equal(computeNotionalFp(6_842_550_000_000n, 125_000_000n), 8_553_187_500_000n);
});

test("SubmitOrder, RiskMitigationCommand, and TimelineForkRequest binary Protobuf roundtrip", () => {
  const order = SubmitOrder.create({
    clientOrderId: "ts-ord-01",
    accountId: 42001n,
    symbol: "BTC-USDT",
    instrumentId: 1,
    side: OrderSide.ORDER_SIDE_BUY,
    orderType: OrderType.ORDER_TYPE_LIMIT,
    stpMode: SelfTradePreventionMode.SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING,
    priceFp: 6_842_550_000_000n,
    qtyFp: 250_000_000n,
    maxSlippageBps: 5,
    clientTsNs: 1_795_013_000_000_000_000n,
    nonce: 99n,
    signature: new Uint8Array(64).fill(0x5a),
    idempotencyKey: "idem-ts-01",
  });
  const bytes = SubmitOrder.encode(order).finish();
  const decoded = SubmitOrder.decode(bytes);
  assert.equal(decoded.priceFp, 6_842_550_000_000n);
  assert.equal(decoded.qtyFp, 250_000_000n);

  const cmd = RiskMitigationCommand.create({
    commandId: "mit-001",
    sourceAlertId: "alert-99",
    incidentId: "inc-2026-01",
    action: MitigationActionType.MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT,
    targetAccountIds: [9001n, 9002n],
    targetSymbol: "BTC-USDT",
    cooldownDurationMs: 60_000n,
    dryRun: false,
    requiresHumanApproval: false,
    rollbackToken: "rbk-abc123",
    reason: `Automated quarantine for ${AnomalyPatternType[AnomalyPatternType.ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE]}`,
    issuedBy: "aegis-agent",
    issuedTsNs: 1_795_013_005_000_000_000n,
  });
  const cmdDecoded = RiskMitigationCommand.decode(RiskMitigationCommand.encode(cmd).finish());
  assert.deepEqual(cmdDecoded.targetAccountIds, [9001n, 9002n]);

  const fork = TimelineForkRequest.create({
    forkName: "early-freeze-sim",
    symbol: "BTC-USDT",
    startEngineSeq: 100n,
    endEngineSeq: 500n,
    policyOverride: {
      makerFeePpmOverride: -25n,
      takerFeePpmOverride: 100n,
      accountOverrides: [],
      stpModeOverride: SelfTradePreventionMode.SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH,
      regimeOverride: 0,
    },
  });
  const forkDecoded = TimelineForkRequest.decode(TimelineForkRequest.encode(fork).finish());
  assert.equal(forkDecoded.policyOverride?.makerFeePpmOverride, -25n);
});

test("Binary WebSocket 32-byte little-endian header codec", () => {
  const rawHeader = encodeWsHeader({
    msgType: WS_MSG_TYPE.L3_DELTA_BATCH,
    flags: 0x01,
    instrumentId: 1,
    payloadLen: 96,
    crc32cDigest: 0xdeadbeef,
    engineSeq: 987_654_321n,
    timestampNs: 1_795_013_111_222_333_444n,
  });
  assert.equal(rawHeader.byteLength, 32);
  const parsed = decodeWsHeader(rawHeader);
  assert.equal(parsed.magic, WS_FRAME_MAGIC);
  assert.equal(parsed.msgType, WS_MSG_TYPE.L3_DELTA_BATCH);
  assert.equal(parsed.engineSeq, 987_654_321n);
  assert.equal(parsed.timestampNs, 1_795_013_111_222_333_444n);
});
