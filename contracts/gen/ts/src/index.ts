export * from "./generated/common";
export * from "./generated/orderbook";
export * from "./generated/telemetry_fraud";
export * from "./generated/chronos";
export * from "./generated/stream_analytics";

import { I128, U128 } from "./generated/common";

/** Global fixed-point scale factor (1.00000000 = 100_000_000n atomic units). */
export const FP_SCALE = 100_000_000n;
export const PPM_SCALE = 1_000_000n;

/** Binary WebSocket little-endian wire framing constants (ws-binary-protocol.md). */
export const WS_FRAME_MAGIC = 0x4859;
export const WS_FRAME_VERSION = 0x01;
export const WS_HEADER_BYTES = 32;

export const WS_MSG_TYPE = {
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
} as const;

const MASK_64 = (1n << 64n) - 1n;
const TWO_POW_127 = 1n << 127n;
const TWO_POW_128 = 1n << 128n;

export function bigIntToU128(value: bigint): U128 {
  if (value < 0n || value >= TWO_POW_128) {
    throw new RangeError(`U128 out of range: ${value.toString()}`);
  }
  return {
    hi: (value >> 64n) & MASK_64,
    lo: value & MASK_64,
  };
}

export function u128ToBigInt(msg: U128 | undefined): bigint {
  if (!msg) return 0n;
  return ((msg.hi & MASK_64) << 64n) | (msg.lo & MASK_64);
}

export function bigIntToI128(value: bigint): I128 {
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

export function i128ToBigInt(msg: I128 | undefined): bigint {
  if (!msg) return 0n;
  const hiSigned = BigInt.asIntN(64, msg.hi);
  return (hiSigned << 64n) + (msg.lo & MASK_64);
}

export function computeNotionalFp(priceFp: bigint, qtyFp: bigint): bigint {
  if (priceFp < 0n || qtyFp < 0n) {
    throw new RangeError("priceFp and qtyFp must be non-negative fixed-point bigints");
  }
  return (priceFp * qtyFp) / FP_SCALE;
}

export interface WsBinaryHeader {
  magic: number;
  version: number;
  msgType: number;
  flags: number;
  instrumentId: number;
  payloadLen: number;
  crc32cDigest: number;
  engineSeq: bigint;
  timestampNs: bigint;
}

export function encodeWsHeader(h: Omit<WsBinaryHeader, "magic" | "version">): Uint8Array {
  const buf = new ArrayBuffer(WS_HEADER_BYTES);
  const dv = new DataView(buf);
  dv.setUint16(0, WS_FRAME_MAGIC, true);
  dv.setUint8(2, WS_FRAME_VERSION);
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

export function decodeWsHeader(frame: Uint8Array): WsBinaryHeader {
  if (frame.byteLength < WS_HEADER_BYTES) {
    throw new RangeError(`WS frame shorter than 32B header: ${frame.byteLength}`);
  }
  const dv = new DataView(frame.buffer, frame.byteOffset, WS_HEADER_BYTES);
  const magic = dv.getUint16(0, true);
  const version = dv.getUint8(2);
  if (magic !== WS_FRAME_MAGIC || version !== WS_FRAME_VERSION) {
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
