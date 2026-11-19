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
  WS_CACHE_LINE_BYTES,
  WS_FRAME_MAGIC,
  WS_FRAME_VERSION,
  WS_HEADER_BYTES,
  WS_MSG_TYPE,
  bigIntToI128,
  bigIntToU128,
  checkedFeeFp,
  computeNotionalFp,
  createAlignedWsHeaderSlab64,
  decodeWsHeader,
  decodeWsHeaderInto,
  encodeWsHeader,
  encodeWsHeaderInto,
  encodeWsHeaderIntoU32,
  i128ToBigInt,
  u128ToBigInt,
} from "./index";

test("bigint fixed-point 128-bit roundtrip, boundaries, and fee/notional arithmetic", () => {
  const TWO_POW_64 = 1n << 64n;
  const TWO_POW_127 = 1n << 127n;
  const TWO_POW_128 = 1n << 128n;

  assert.equal(i128ToBigInt(undefined), 0n);
  assert.equal(u128ToBigInt(undefined), 0n);

  for (const sample of [
    0n,
    1n,
    -1n,
    TWO_POW_64 - 1n,
    TWO_POW_64,
    -TWO_POW_64,
    (1n << 90n) + 77n,
    -((1n << 102n) + 19n),
    TWO_POW_127 - 1n,
    -TWO_POW_127,
  ]) {
    const enc = bigIntToI128(sample);
    assert.equal(i128ToBigInt(enc), sample);
  }
  assert.throws(() => bigIntToI128(TWO_POW_127), RangeError);
  assert.throws(() => bigIntToI128(-TWO_POW_127 - 1n), RangeError);
  // Masking check on high/low bits
  assert.equal(i128ToBigInt({ hi: 1n, lo: (1n << 64n) + 5n }), (1n << 64n) + 5n);

  for (const sample of [0n, 1n, TWO_POW_64 - 1n, TWO_POW_64, (1n << 115n) + 13n, TWO_POW_128 - 1n]) {
    const enc = bigIntToU128(sample);
    assert.equal(u128ToBigInt(enc), sample);
  }
  assert.throws(() => bigIntToU128(-1n), RangeError);
  assert.throws(() => bigIntToU128(TWO_POW_128), RangeError);
  assert.equal(u128ToBigInt({ hi: (1n << 64n) + 2n, lo: (1n << 64n) + 7n }), (2n << 64n) + 7n);

  assert.equal(computeNotionalFp(0n, 125_000_000n), 0n);
  assert.equal(computeNotionalFp(125_000_000n, 0n), 0n);
  assert.equal(computeNotionalFp(6_842_550_000_000n, 125_000_000n), 8_553_187_500_000n);
  assert.equal(computeNotionalFp(199_999_999n, 1n), 1n);
  assert.throws(() => computeNotionalFp(-1n, 1n), RangeError);
  assert.throws(() => computeNotionalFp(1n, -1n), RangeError);

  assert.equal(checkedFeeFp(0n, 100n), 0n);
  assert.equal(checkedFeeFp(10_000_000_000n, 250n), 2_500_000n);
  assert.equal(checkedFeeFp(10_000_000_000n, -25n), -250_000n);
  assert.equal(checkedFeeFp(1_999_999n, -1n), -1n);
  assert.throws(() => checkedFeeFp(-1n, 10n), RangeError);
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

test("Binary WebSocket 32-byte little-endian header codec (Pass 1/2/3 bit-exact + boundaries)", () => {
  const sample = {
    msgType: WS_MSG_TYPE.L3_DELTA_BATCH,
    flags: 0x05,
    instrumentId: 0x1234,
    payloadLen: 0x0001_0060,
    crc32cDigest: 0xdeadbeef,
    engineSeq: 0x0000_0007_3adb_f531n,
    timestampNs: 1_795_013_111_222_333_444n,
  };
  const rawHeader = encodeWsHeader(sample);
  assert.equal(rawHeader.byteLength, WS_HEADER_BYTES);
  assert.equal(rawHeader[5], 0); // reserved byte must stay 0

  const parsed = decodeWsHeader(rawHeader);
  assert.equal(parsed.magic, WS_FRAME_MAGIC);
  assert.equal(parsed.version, WS_FRAME_VERSION);
  assert.equal(parsed.msgType, sample.msgType);
  assert.equal(parsed.flags, sample.flags);
  assert.equal(parsed.instrumentId, sample.instrumentId);
  assert.equal(parsed.payloadLen, sample.payloadLen);
  assert.equal(parsed.crc32cDigest, sample.crc32cDigest);
  assert.equal(parsed.engineSeq, sample.engineSeq);
  assert.equal(parsed.timestampNs, sample.timestampNs);

  // Pass 2 zero-copy into non-zero offset + Pass 3 aligned 64B slab bit-exact parity
  const slab = createAlignedWsHeaderSlab64(4);
  assert.equal(slab.bytes.byteOffset % WS_CACHE_LINE_BYTES, 0);
  assert.equal(slab.u32.byteOffset % WS_CACHE_LINE_BYTES, 0);
  assert.equal(slab.capacity, 4);
  assert.throws(() => createAlignedWsHeaderSlab64(0), RangeError);
  assert.throws(() => createAlignedWsHeaderSlab64(-1), RangeError);

  const dv = new DataView(slab.buffer, slab.bytes.byteOffset, slab.bytes.byteLength);
  encodeWsHeaderInto(slab.bytes, 64, sample, dv);
  assert.deepEqual(slab.bytes.subarray(64, 96), rawHeader);

  const seqLo = Number(sample.engineSeq & 0xffff_ffffn);
  const seqHi = Number((sample.engineSeq >> 32n) & 0xffff_ffffn);
  const tsLo = Number(sample.timestampNs & 0xffff_ffffn);
  const tsHi = Number((sample.timestampNs >> 32n) & 0xffff_ffffn);
  encodeWsHeaderIntoU32(
    slab.u32,
    32, // slot 2 = byte offset 128
    sample.msgType,
    sample.flags,
    sample.instrumentId,
    sample.payloadLen,
    sample.crc32cDigest,
    seqLo,
    seqHi,
    tsLo,
    tsHi,
  );
  assert.deepEqual(slab.bytes.subarray(128, 160), rawHeader);

  const reused = {
    magic: 0,
    version: 0,
    msgType: 0,
    flags: 0,
    instrumentId: 0,
    payloadLen: 0,
    crc32cDigest: 0,
    engineSeq: 0n,
    timestampNs: 0n,
  };
  decodeWsHeaderInto(slab.bytes, 128, reused, dv);
  assert.deepEqual(reused, parsed);

  // Boundary guards
  assert.throws(() => encodeWsHeaderInto(slab.bytes, -1, sample), /WS header encode out of bounds/);
  assert.throws(() => encodeWsHeaderInto(slab.bytes, slab.bytes.byteLength - 31, sample), /WS header encode out of bounds/);
  assert.throws(() => encodeWsHeaderIntoU32(slab.u32, -1, 1, 0, 1, 0, 0, 0, 0, 0, 0), /WS u32 encode out of bounds/);
  assert.throws(
    () => encodeWsHeaderIntoU32(slab.u32, slab.u32.length - 7, 1, 0, 1, 0, 0, 0, 0, 0, 0),
    /WS u32 encode out of bounds/,
  );
  assert.throws(() => decodeWsHeader(new Uint8Array(31)), RangeError);
  assert.throws(() => decodeWsHeaderInto(slab.bytes, -1, reused), RangeError);
  assert.throws(() => decodeWsHeaderInto(slab.bytes, slab.bytes.byteLength - 31, reused), RangeError);

  const badMagic = new Uint8Array(rawHeader);
  badMagic[0] = (badMagic[0] ?? 0) ^ 0xff;
  assert.throws(() => decodeWsHeader(badMagic), /Invalid HYPERION WS header/);

  const badVersion = new Uint8Array(rawHeader);
  badVersion[2] = 0x02;
  assert.throws(() => decodeWsHeader(badVersion), /Invalid HYPERION WS header/);
});
