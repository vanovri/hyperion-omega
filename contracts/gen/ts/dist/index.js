"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __exportStar = (this && this.__exportStar) || function(m, exports) {
    for (var p in m) if (p !== "default" && !Object.prototype.hasOwnProperty.call(exports, p)) __createBinding(exports, m, p);
};
Object.defineProperty(exports, "__esModule", { value: true });
exports.WS_MSG_TYPE = exports.WS_HEADER_BYTES = exports.WS_FRAME_VERSION = exports.WS_FRAME_MAGIC = exports.PPM_SCALE = exports.FP_SCALE = void 0;
exports.bigIntToU128 = bigIntToU128;
exports.u128ToBigInt = u128ToBigInt;
exports.bigIntToI128 = bigIntToI128;
exports.i128ToBigInt = i128ToBigInt;
exports.computeNotionalFp = computeNotionalFp;
exports.encodeWsHeader = encodeWsHeader;
exports.decodeWsHeader = decodeWsHeader;
__exportStar(require("./generated/common"), exports);
__exportStar(require("./generated/orderbook"), exports);
__exportStar(require("./generated/telemetry_fraud"), exports);
__exportStar(require("./generated/chronos"), exports);
__exportStar(require("./generated/stream_analytics"), exports);
/** Global fixed-point scale factor (1.00000000 = 100_000_000n atomic units). */
exports.FP_SCALE = 100000000n;
exports.PPM_SCALE = 1000000n;
/** Binary WebSocket little-endian wire framing constants (ws-binary-protocol.md). */
exports.WS_FRAME_MAGIC = 0x4859;
exports.WS_FRAME_VERSION = 0x01;
exports.WS_HEADER_BYTES = 32;
exports.WS_MSG_TYPE = {
    HEARTBEAT: 0x01,
    L2_SNAPSHOT: 0x02,
    L3_DELTA_BATCH: 0x03,
    EXECUTION_FILL: 0x04,
    CANDLE_MICRO: 0x05,
    FRAUD_ALERT: 0x06,
    ORDER_ACK_PRIVATE: 0x07,
    GAP_RESYNC_NOTICE: 0x08,
    CMD_SUBSCRIBE: 0x81,
    CMD_UNSUBSCRIBE: 0x82,
    CMD_RESYNC_REQUEST: 0x83,
};
const MASK_64 = (1n << 64n) - 1n;
const TWO_POW_127 = 1n << 127n;
const TWO_POW_128 = 1n << 128n;
function bigIntToU128(value) {
    if (value < 0n || value >= TWO_POW_128) {
        throw new RangeError(`U128 out of range: ${value.toString()}`);
    }
    return {
        hi: (value >> 64n) & MASK_64,
        lo: value & MASK_64,
    };
}
function u128ToBigInt(msg) {
    if (!msg)
        return 0n;
    return ((msg.hi & MASK_64) << 64n) | (msg.lo & MASK_64);
}
function bigIntToI128(value) {
    if (value < -TWO_POW_127 || value >= TWO_POW_127) {
        throw new RangeError(`I128 out of range: ${value.toString()}`);
    }
    const twos = value < 0n ? value + TWO_POW_128 : value;
    const hiUnsigned = (twos >> 64n) & MASK_64;
    const hiSigned = BigInt.asIntN(64, hiUnsigned);
    return {
        hi: hiSigned,
        lo: twos & MASK_64,
    };
}
function i128ToBigInt(msg) {
    if (!msg)
        return 0n;
    const hiSigned = BigInt.asIntN(64, msg.hi);
    return (hiSigned << 64n) + (msg.lo & MASK_64);
}
function computeNotionalFp(priceFp, qtyFp) {
    if (priceFp < 0n || qtyFp < 0n) {
        throw new RangeError("priceFp and qtyFp must be non-negative fixed-point bigints");
    }
    return (priceFp * qtyFp) / exports.FP_SCALE;
}
function encodeWsHeader(h) {
    const buf = new ArrayBuffer(exports.WS_HEADER_BYTES);
    const dv = new DataView(buf);
    dv.setUint16(0, exports.WS_FRAME_MAGIC, true);
    dv.setUint8(2, exports.WS_FRAME_VERSION);
    dv.setUint8(3, h.msgType);
    dv.setUint8(4, h.flags);
    dv.setUint8(5, 0);
    dv.setUint16(6, h.instrumentId, true);
    dv.setUint32(8, h.payloadLen, true);
    dv.setUint32(12, h.crc32cDigest, true);
    dv.setBigUint64(16, h.engineSeq, true);
    dv.setBigUint64(24, h.timestampNs, true);
    return new Uint8Array(buf);
}
function decodeWsHeader(frame) {
    if (frame.byteLength < exports.WS_HEADER_BYTES) {
        throw new RangeError(`WS frame shorter than 32B header: ${frame.byteLength}`);
    }
    const dv = new DataView(frame.buffer, frame.byteOffset, exports.WS_HEADER_BYTES);
    const magic = dv.getUint16(0, true);
    const version = dv.getUint8(2);
    if (magic !== exports.WS_FRAME_MAGIC || version !== exports.WS_FRAME_VERSION) {
        throw new Error(`Invalid HYPERION WS header magic=0x${magic.toString(16)} version=${version}`);
    }
    return {
        magic,
        version,
        msgType: dv.getUint8(3),
        flags: dv.getUint8(4),
        instrumentId: dv.getUint16(6, true),
        payloadLen: dv.getUint32(8, true),
        crc32cDigest: dv.getUint32(12, true),
        engineSeq: dv.getBigUint64(16, true),
        timestampNs: dv.getBigUint64(24, true),
    };
}
//# sourceMappingURL=index.js.map