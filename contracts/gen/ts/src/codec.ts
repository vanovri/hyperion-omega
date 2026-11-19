import type { I128, U128 } from "./generated/common";

/** Global fixed-point scale factor (1.00000000 = 100_000_000n atomic units). */
export const FP_SCALE = 100_000_000n;
export const PPM_SCALE = 1_000_000n;

/** Binary WebSocket little-endian wire framing constants (ws-binary-protocol.md). */
export const WS_FRAME_MAGIC = 0x4859;
export const WS_FRAME_VERSION = 0x01;
export const WS_HEADER_BYTES = 32;
export const WS_CACHE_LINE_BYTES = 64;

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
  return ((msg.hi & MASK_64) << 64n) + (msg.lo & MASK_64);
}

export function bigIntToI128(value: bigint): I128 {
  if (value < -TWO_POW_127 || value >= TWO_POW_127) {
    throw new RangeError(`I128 out of range: ${value.toString()}`);
  }
  return {
    hi: BigInt.asIntN(64, value >> 64n),
    lo: value & MASK_64,
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

export function checkedFeeFp(notionalFp: bigint, feePpm: bigint): bigint {
  if (notionalFp < 0n) {
    throw new RangeError("notionalFp must be non-negative fixed-point bigint");
  }
  return (notionalFp * feePpm) / PPM_SCALE;
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

export interface AlignedWsHeaderSlab64 {
  readonly buffer: ArrayBuffer;
  readonly bytes: Uint8Array;
  readonly u32: Uint32Array;
  readonly capacity: number;
}

/** Pass 1 (Baseline): allocates a fresh ArrayBuffer + DataView + Uint8Array per header. */
export function encodeWsHeader(h: Omit<WsBinaryHeader, "magic" | "version">): Uint8Array {
  const buf = new ArrayBuffer(WS_HEADER_BYTES);
  const out = new Uint8Array(buf);
  encodeWsHeaderInto(out, 0, h);
  return out;
}

/** Pass 2 (Zero-Copy Refactor): encodes directly into caller-owned buffer at `offset`. */
export function encodeWsHeaderInto(
  out: Uint8Array,
  offset: number,
  h: Omit<WsBinaryHeader, "magic" | "version">,
  dv?: DataView,
): void {
  if (offset < 0 || offset + WS_HEADER_BYTES > out.byteLength) {
    throw new RangeError(`WS header encode out of bounds: offset=${offset} len=${out.byteLength}`);
  }
  const view = dv ?? new DataView(out.buffer, out.byteOffset, out.byteLength);
  view.setUint16(offset, WS_FRAME_MAGIC, true);
  view.setUint8(offset + 2, WS_FRAME_VERSION);
  view.setUint8(offset + 3, h.msgType);
  view.setUint8(offset + 4, h.flags);
  view.setUint8(offset + 5, 0);
  view.setUint16(offset + 6, h.instrumentId, true);
  view.setUint32(offset + 8, h.payloadLen, true);
  view.setUint32(offset + 12, h.crc32cDigest, true);
  view.setBigUint64(offset + 16, h.engineSeq, true);
  view.setBigUint64(offset + 24, h.timestampNs, true);
}

/** Pass 3 (Cache-Line Aligned 64B Slab): allocates 64B-aligned byte/u32 views for batch WS framing. */
export function createAlignedWsHeaderSlab64(capacity: number): AlignedWsHeaderSlab64 {
  if (capacity <= 0) {
    throw new RangeError(`Slab capacity must be positive: ${capacity}`);
  }
  const totalBytes = capacity * WS_CACHE_LINE_BYTES;
  const buffer = new ArrayBuffer(totalBytes + WS_CACHE_LINE_BYTES);
  const startOffset = WS_CACHE_LINE_BYTES;
  const bytes = new Uint8Array(buffer, startOffset, totalBytes);
  const u32 = new Uint32Array(buffer, startOffset, totalBytes >> 2);
  return { buffer, bytes, u32, capacity };
}

/**
 * Pass 3 (Cache-Line Aligned Unboxed Uint32Array Final):
 * Writes a 32-byte little-endian HYPERION WS header directly into a Uint32Array
 * at 32-bit word offset `wordOffset` (stride 16 words = 64B cache line per slot)
 * with zero ArrayBuffer, zero DataView, and zero BigInt heap allocations.
 */
export function encodeWsHeaderIntoU32(
  u32: Uint32Array,
  wordOffset: number,
  msgType: number,
  flags: number,
  instrumentId: number,
  payloadLen: number,
  crc32cDigest: number,
  seqLo: number,
  seqHi: number,
  tsLo: number,
  tsHi: number,
): void {
  if (wordOffset < 0 || wordOffset + 8 > u32.length) {
    throw new RangeError(`WS u32 encode out of bounds: wordOffset=${wordOffset} len=${u32.length}`);
  }
  u32[wordOffset] = (WS_FRAME_MAGIC | (WS_FRAME_VERSION << 16) | ((msgType & 0xff) << 24)) >>> 0;
  u32[wordOffset + 1] = ((flags & 0xff) | ((instrumentId & 0xffff) << 16)) >>> 0;
  u32[wordOffset + 2] = payloadLen >>> 0;
  u32[wordOffset + 3] = crc32cDigest >>> 0;
  u32[wordOffset + 4] = seqLo >>> 0;
  u32[wordOffset + 5] = seqHi >>> 0;
  u32[wordOffset + 6] = tsLo >>> 0;
  u32[wordOffset + 7] = tsHi >>> 0;
}

export function decodeWsHeader(frame: Uint8Array): WsBinaryHeader {
  const out: WsBinaryHeader = {
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
  return decodeWsHeaderInto(frame, 0, out);
}

export function decodeWsHeaderInto(
  frame: Uint8Array,
  offset: number,
  out: WsBinaryHeader,
  dv?: DataView,
): WsBinaryHeader {
  if (offset < 0 || offset + WS_HEADER_BYTES > frame.byteLength) {
    throw new RangeError(`WS frame shorter than 32B header at offset=${offset}: ${frame.byteLength}`);
  }
  const view = dv ?? new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
  const magic = view.getUint16(offset, true);
  const version = view.getUint8(offset + 2);
  if (magic !== WS_FRAME_MAGIC || version !== WS_FRAME_VERSION) {
    throw new Error(`Invalid HYPERION WS header magic=0x${magic.toString(16)} version=${version}`);
  }
  out.magic = magic;
  out.version = version;
  out.msgType = view.getUint8(offset + 3);
  out.flags = view.getUint8(offset + 4);
  out.instrumentId = view.getUint16(offset + 6, true);
  out.payloadLen = view.getUint32(offset + 8, true);
  out.crc32cDigest = view.getUint32(offset + 12, true);
  out.engineSeq = view.getBigUint64(offset + 16, true);
  out.timestampNs = view.getBigUint64(offset + 24, true);
  return out;
}
