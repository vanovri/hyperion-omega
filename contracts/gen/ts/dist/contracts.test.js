"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = __importDefault(require("node:test"));
const index_1 = require("./index");
(0, node_test_1.default)("bigint fixed-point 128-bit roundtrip, boundaries, and fee/notional arithmetic", () => {
    const TWO_POW_64 = 1n << 64n;
    const TWO_POW_127 = 1n << 127n;
    const TWO_POW_128 = 1n << 128n;
    strict_1.default.equal((0, index_1.i128ToBigInt)(undefined), 0n);
    strict_1.default.equal((0, index_1.u128ToBigInt)(undefined), 0n);
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
        const enc = (0, index_1.bigIntToI128)(sample);
        strict_1.default.equal((0, index_1.i128ToBigInt)(enc), sample);
    }
    strict_1.default.throws(() => (0, index_1.bigIntToI128)(TWO_POW_127), RangeError);
    strict_1.default.throws(() => (0, index_1.bigIntToI128)(-TWO_POW_127 - 1n), RangeError);
    // Masking check on high/low bits
    strict_1.default.equal((0, index_1.i128ToBigInt)({ hi: 1n, lo: (1n << 64n) + 5n }), (1n << 64n) + 5n);
    for (const sample of [0n, 1n, TWO_POW_64 - 1n, TWO_POW_64, (1n << 115n) + 13n, TWO_POW_128 - 1n]) {
        const enc = (0, index_1.bigIntToU128)(sample);
        strict_1.default.equal((0, index_1.u128ToBigInt)(enc), sample);
    }
    strict_1.default.throws(() => (0, index_1.bigIntToU128)(-1n), RangeError);
    strict_1.default.throws(() => (0, index_1.bigIntToU128)(TWO_POW_128), RangeError);
    strict_1.default.equal((0, index_1.u128ToBigInt)({ hi: (1n << 64n) + 2n, lo: (1n << 64n) + 7n }), (2n << 64n) + 7n);
    strict_1.default.equal((0, index_1.computeNotionalFp)(0n, 125000000n), 0n);
    strict_1.default.equal((0, index_1.computeNotionalFp)(125000000n, 0n), 0n);
    strict_1.default.equal((0, index_1.computeNotionalFp)(6842550000000n, 125000000n), 8553187500000n);
    strict_1.default.equal((0, index_1.computeNotionalFp)(199999999n, 1n), 1n);
    strict_1.default.throws(() => (0, index_1.computeNotionalFp)(-1n, 1n), RangeError);
    strict_1.default.throws(() => (0, index_1.computeNotionalFp)(1n, -1n), RangeError);
    strict_1.default.equal((0, index_1.checkedFeeFp)(0n, 100n), 0n);
    strict_1.default.equal((0, index_1.checkedFeeFp)(10000000000n, 250n), 2500000n);
    strict_1.default.equal((0, index_1.checkedFeeFp)(10000000000n, -25n), -250000n);
    strict_1.default.equal((0, index_1.checkedFeeFp)(1999999n, -1n), -1n);
    strict_1.default.throws(() => (0, index_1.checkedFeeFp)(-1n, 10n), RangeError);
});
(0, node_test_1.default)("SubmitOrder, RiskMitigationCommand, and TimelineForkRequest binary Protobuf roundtrip", () => {
    const order = index_1.SubmitOrder.create({
        clientOrderId: "ts-ord-01",
        accountId: 42001n,
        symbol: "BTC-USDT",
        instrumentId: 1,
        side: index_1.OrderSide.ORDER_SIDE_BUY,
        orderType: index_1.OrderType.ORDER_TYPE_LIMIT,
        stpMode: index_1.SelfTradePreventionMode.SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING,
        priceFp: 6842550000000n,
        qtyFp: 250000000n,
        maxSlippageBps: 5,
        clientTsNs: 1795013000000000000n,
        nonce: 99n,
        signature: new Uint8Array(64).fill(0x5a),
        idempotencyKey: "idem-ts-01",
    });
    const bytes = index_1.SubmitOrder.encode(order).finish();
    const decoded = index_1.SubmitOrder.decode(bytes);
    strict_1.default.equal(decoded.priceFp, 6842550000000n);
    strict_1.default.equal(decoded.qtyFp, 250000000n);
    const cmd = index_1.RiskMitigationCommand.create({
        commandId: "mit-001",
        sourceAlertId: "alert-99",
        incidentId: "inc-2026-01",
        action: index_1.MitigationActionType.MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT,
        targetAccountIds: [9001n, 9002n],
        targetSymbol: "BTC-USDT",
        cooldownDurationMs: 60000n,
        dryRun: false,
        requiresHumanApproval: false,
        rollbackToken: "rbk-abc123",
        reason: `Automated quarantine for ${index_1.AnomalyPatternType[index_1.AnomalyPatternType.ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE]}`,
        issuedBy: "aegis-agent",
        issuedTsNs: 1795013005000000000n,
    });
    const cmdDecoded = index_1.RiskMitigationCommand.decode(index_1.RiskMitigationCommand.encode(cmd).finish());
    strict_1.default.deepEqual(cmdDecoded.targetAccountIds, [9001n, 9002n]);
    const fork = index_1.TimelineForkRequest.create({
        forkName: "early-freeze-sim",
        symbol: "BTC-USDT",
        startEngineSeq: 100n,
        endEngineSeq: 500n,
        policyOverride: {
            makerFeePpmOverride: -25n,
            takerFeePpmOverride: 100n,
            accountOverrides: [],
            stpModeOverride: index_1.SelfTradePreventionMode.SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH,
            regimeOverride: 0,
        },
    });
    const forkDecoded = index_1.TimelineForkRequest.decode(index_1.TimelineForkRequest.encode(fork).finish());
    strict_1.default.equal(forkDecoded.policyOverride?.makerFeePpmOverride, -25n);
});
(0, node_test_1.default)("Binary WebSocket 32-byte little-endian header codec (Pass 1/2/3 bit-exact + boundaries)", () => {
    const sample = {
        msgType: index_1.WS_MSG_TYPE.L3_DELTA_BATCH,
        flags: 0x05,
        instrumentId: 0x1234,
        payloadLen: 0x0001_0060,
        crc32cDigest: 0xdeadbeef,
        engineSeq: 0x000000073adbf531n,
        timestampNs: 1795013111222333444n,
    };
    const rawHeader = (0, index_1.encodeWsHeader)(sample);
    strict_1.default.equal(rawHeader.byteLength, index_1.WS_HEADER_BYTES);
    strict_1.default.equal(rawHeader[5], 0); // reserved byte must stay 0
    const parsed = (0, index_1.decodeWsHeader)(rawHeader);
    strict_1.default.equal(parsed.magic, index_1.WS_FRAME_MAGIC);
    strict_1.default.equal(parsed.version, index_1.WS_FRAME_VERSION);
    strict_1.default.equal(parsed.msgType, sample.msgType);
    strict_1.default.equal(parsed.flags, sample.flags);
    strict_1.default.equal(parsed.instrumentId, sample.instrumentId);
    strict_1.default.equal(parsed.payloadLen, sample.payloadLen);
    strict_1.default.equal(parsed.crc32cDigest, sample.crc32cDigest);
    strict_1.default.equal(parsed.engineSeq, sample.engineSeq);
    strict_1.default.equal(parsed.timestampNs, sample.timestampNs);
    // Pass 2 zero-copy into non-zero offset + Pass 3 aligned 64B slab bit-exact parity
    const slab = (0, index_1.createAlignedWsHeaderSlab64)(4);
    strict_1.default.equal(slab.bytes.byteOffset % index_1.WS_CACHE_LINE_BYTES, 0);
    strict_1.default.equal(slab.u32.byteOffset % index_1.WS_CACHE_LINE_BYTES, 0);
    strict_1.default.equal(slab.capacity, 4);
    strict_1.default.throws(() => (0, index_1.createAlignedWsHeaderSlab64)(0), RangeError);
    strict_1.default.throws(() => (0, index_1.createAlignedWsHeaderSlab64)(-1), RangeError);
    const dv = new DataView(slab.buffer, slab.bytes.byteOffset, slab.bytes.byteLength);
    (0, index_1.encodeWsHeaderInto)(slab.bytes, 64, sample, dv);
    strict_1.default.deepEqual(slab.bytes.subarray(64, 96), rawHeader);
    const seqLo = Number(sample.engineSeq & 0xffffffffn);
    const seqHi = Number((sample.engineSeq >> 32n) & 0xffffffffn);
    const tsLo = Number(sample.timestampNs & 0xffffffffn);
    const tsHi = Number((sample.timestampNs >> 32n) & 0xffffffffn);
    (0, index_1.encodeWsHeaderIntoU32)(slab.u32, 32, // slot 2 = byte offset 128
    sample.msgType, sample.flags, sample.instrumentId, sample.payloadLen, sample.crc32cDigest, seqLo, seqHi, tsLo, tsHi);
    strict_1.default.deepEqual(slab.bytes.subarray(128, 160), rawHeader);
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
    (0, index_1.decodeWsHeaderInto)(slab.bytes, 128, reused, dv);
    strict_1.default.deepEqual(reused, parsed);
    // Boundary guards
    strict_1.default.throws(() => (0, index_1.encodeWsHeaderInto)(slab.bytes, -1, sample), /WS header encode out of bounds/);
    strict_1.default.throws(() => (0, index_1.encodeWsHeaderInto)(slab.bytes, slab.bytes.byteLength - 31, sample), /WS header encode out of bounds/);
    strict_1.default.throws(() => (0, index_1.encodeWsHeaderIntoU32)(slab.u32, -1, 1, 0, 1, 0, 0, 0, 0, 0, 0), /WS u32 encode out of bounds/);
    strict_1.default.throws(() => (0, index_1.encodeWsHeaderIntoU32)(slab.u32, slab.u32.length - 7, 1, 0, 1, 0, 0, 0, 0, 0, 0), /WS u32 encode out of bounds/);
    strict_1.default.throws(() => (0, index_1.decodeWsHeader)(new Uint8Array(31)), RangeError);
    strict_1.default.throws(() => (0, index_1.decodeWsHeaderInto)(slab.bytes, -1, reused), RangeError);
    strict_1.default.throws(() => (0, index_1.decodeWsHeaderInto)(slab.bytes, slab.bytes.byteLength - 31, reused), RangeError);
    const badMagic = new Uint8Array(rawHeader);
    badMagic[0] = (badMagic[0] ?? 0) ^ 0xff;
    strict_1.default.throws(() => (0, index_1.decodeWsHeader)(badMagic), /Invalid HYPERION WS header/);
    const badVersion = new Uint8Array(rawHeader);
    badVersion[2] = 0x02;
    strict_1.default.throws(() => (0, index_1.decodeWsHeader)(badVersion), /Invalid HYPERION WS header/);
});
//# sourceMappingURL=contracts.test.js.map