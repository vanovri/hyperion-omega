"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = __importDefault(require("node:test"));
const index_1 = require("./index");
(0, node_test_1.default)("bigint fixed-point 128-bit roundtrip and notional arithmetic", () => {
    for (const sample of [0n, 1n, -1n, (1n << 90n) + 77n, -((1n << 102n) + 19n)]) {
        strict_1.default.equal((0, index_1.i128ToBigInt)((0, index_1.bigIntToI128)(sample)), sample);
    }
    for (const sample of [0n, 1n, (1n << 115n) + 13n]) {
        strict_1.default.equal((0, index_1.u128ToBigInt)((0, index_1.bigIntToU128)(sample)), sample);
    }
    strict_1.default.equal((0, index_1.computeNotionalFp)(6842550000000n, 125000000n), 8553187500000n);
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
(0, node_test_1.default)("Binary WebSocket 32-byte little-endian header codec", () => {
    const rawHeader = (0, index_1.encodeWsHeader)({
        msgType: index_1.WS_MSG_TYPE.L3_DELTA_BATCH,
        flags: 0x01,
        instrumentId: 1,
        payloadLen: 96,
        crc32cDigest: 0xdeadbeef,
        engineSeq: 987654321n,
        timestampNs: 1795013111222333444n,
    });
    strict_1.default.equal(rawHeader.byteLength, 32);
    const parsed = (0, index_1.decodeWsHeader)(rawHeader);
    strict_1.default.equal(parsed.magic, index_1.WS_FRAME_MAGIC);
    strict_1.default.equal(parsed.msgType, index_1.WS_MSG_TYPE.L3_DELTA_BATCH);
    strict_1.default.equal(parsed.engineSeq, 987654321n);
    strict_1.default.equal(parsed.timestampNs, 1795013111222333444n);
});
//# sourceMappingURL=contracts.test.js.map