/**
 * HYPERION — Sub-Millisecond Binary WebSocket Wire Protocol v1 Codec
 * Implements exact 32-byte Little-Endian header (magic 0x4859) + Castagnoli CRC-32C
 * per contracts/ws-binary-protocol.md.
 */

export const WS_MAGIC_LE = 0x4859;
export const WS_VERSION = 0x01;
export const WS_HEADER_BYTES = 32;

export const WS_FLAG_CRC32C_PRESENT = 0x01;
export const WS_FLAG_SNAPSHOT_RESYNC = 0x02;
export const WS_FLAG_SLOW_CONSUMER_DROPPED = 0x04;
export const WS_FLAG_REPLAY_TIMELINE = 0x08;

export const MSG_HEARTBEAT = 0x01;
export const MSG_L2_SNAPSHOT = 0x02;
export const MSG_L3_DELTA_BATCH = 0x03;
export const MSG_EXECUTION_FILL = 0x04;
export const MSG_CANDLE_MICRO = 0x05;
export const MSG_FRAUD_ALERT = 0x06;
export const MSG_ORDER_ACK_PRIVATE = 0x07;
export const MSG_GAP_RESYNC_NOTICE = 0x08;

export const CMD_SUBSCRIBE = 0x81;
export const CMD_UNSUBSCRIBE = 0x82;
export const CMD_RESYNC_REQUEST = 0x83;

// Castagnoli CRC-32C lookup table (reflected polynomial 0x82F63B78)
const CRC32C_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let crc = i;
    for (let bit = 0; bit < 8; bit++) {
      crc = (crc & 1) !== 0 ? (crc >>> 1) ^ 0x82f63b78 : crc >>> 1;
    }
    table[i] = crc >>> 0;
  }
  return table;
})();

export function crc32c(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    const byte = bytes[i] ?? 0;
    const lookup = CRC32C_TABLE[(crc ^ byte) & 0xff] ?? 0;
    crc = (crc >>> 8) ^ lookup;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

export interface BinaryFrameHeader {
  readonly magic: number;
  readonly version: number;
  readonly msgType: number;
  readonly flags: number;
  readonly instrumentId: number;
  readonly payloadLen: number;
  readonly crc32cDigest: number;
  readonly engineSeq: bigint;
  readonly timestampNs: bigint;
}

export interface DecodedL2Level {
  readonly priceFp: bigint;
  readonly totalQtyFp: bigint;
  readonly orderCount: number;
}

export interface DecodedL2Snapshot {
  readonly header: BinaryFrameHeader;
  readonly bestBidFp: bigint;
  readonly bestAskFp: bigint;
  readonly stateRootBlake3Hex: string;
  readonly bids: ReadonlyArray<DecodedL2Level>;
  readonly asks: ReadonlyArray<DecodedL2Level>;
}

export interface DecodedExecutionFill {
  readonly header: BinaryFrameHeader;
  readonly fillId: bigint;
  readonly makerOrderId: bigint;
  readonly takerOrderId: bigint;
  readonly makerAccountId: bigint;
  readonly takerAccountId: bigint;
  readonly priceFp: bigint;
  readonly qtyFp: bigint;
  readonly aggressorSide: 1 | 2;
}

export interface SequenceCheckOutcome {
  readonly status: "APPLY" | "DUPLICATE_OVERLAP" | "GAP_DETECTED";
  readonly nextLocalSeq: bigint;
}

function bytesToHex(bytes: Uint8Array): string {
  let hex = "";
  for (let i = 0; i < bytes.length; i++) {
    hex += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return hex;
}

export function encodeBinaryFrame(
  msgType: number,
  flags: number,
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  payload: Uint8Array
): Uint8Array {
  const effectiveFlags = flags | WS_FLAG_CRC32C_PRESENT;
  const digest = crc32c(payload);
  const frame = new Uint8Array(WS_HEADER_BYTES + payload.byteLength);
  const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);

  view.setUint16(0, WS_MAGIC_LE, true);
  view.setUint8(2, WS_VERSION);
  view.setUint8(3, msgType & 0xff);
  view.setUint8(4, effectiveFlags & 0xff);
  view.setUint8(5, 0x00);
  view.setUint16(6, instrumentId & 0xffff, true);
  view.setUint32(8, payload.byteLength >>> 0, true);
  view.setUint32(12, digest >>> 0, true);
  view.setBigUint64(16, engineSeq, true);
  view.setBigUint64(24, timestampNs, true);

  frame.set(payload, WS_HEADER_BYTES);
  return frame;
}

export function decodeBinaryHeader(buffer: ArrayBuffer | Uint8Array): {
  header: BinaryFrameHeader;
  payload: Uint8Array;
} {
  const raw = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  if (raw.byteLength < WS_HEADER_BYTES) {
    throw new Error(`Protocol 1002: frame shorter than 32B (${raw.byteLength})`);
  }
  const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  const magic = view.getUint16(0, true);
  const version = view.getUint8(2);
  if (magic !== WS_MAGIC_LE || version !== WS_VERSION) {
    throw new Error(
      `Protocol 1002: invalid magic=0x${magic.toString(16)} or version=${version}`
    );
  }
  const msgType = view.getUint8(3);
  const flags = view.getUint8(4);
  const reserved = view.getUint8(5);
  if (reserved !== 0) {
    throw new Error(`Protocol 1002: non-zero reserved header byte (${reserved})`);
  }
  const instrumentId = view.getUint16(6, true);
  const payloadLen = view.getUint32(8, true);
  const crc32cDigest = view.getUint32(12, true);
  const engineSeq = view.getBigUint64(16, true);
  const timestampNs = view.getBigUint64(24, true);

  if (raw.byteLength !== WS_HEADER_BYTES + payloadLen) {
    throw new Error(
      `Protocol 1002: frame byteLength=${raw.byteLength} != 32 + payloadLen(${payloadLen})`
    );
  }
  const payload = raw.subarray(WS_HEADER_BYTES, WS_HEADER_BYTES + payloadLen);
  if ((flags & WS_FLAG_CRC32C_PRESENT) !== 0) {
    const computed = crc32c(payload);
    if (computed !== crc32cDigest) {
      throw new Error(
        `Protocol 1002: CRC32C mismatch computed=0x${computed.toString(16)} != header=0x${crc32cDigest.toString(16)}`
      );
    }
  }

  return {
    header: {
      magic,
      version,
      msgType,
      flags,
      instrumentId,
      payloadLen,
      crc32cDigest,
      engineSeq,
      timestampNs,
    },
    payload,
  };
}

export function encodeSubscribeCommand(
  channelMask: number,
  instrumentMask: number,
  lastKnownSeq = 0n
): Uint8Array {
  const payload = new Uint8Array(16);
  const view = new DataView(payload.buffer);
  view.setUint32(0, channelMask >>> 0, true);
  view.setUint32(4, instrumentMask >>> 0, true);
  view.setBigUint64(8, lastKnownSeq, true);
  return encodeBinaryFrame(CMD_SUBSCRIBE, 0, 0, 0n, BigInt(Date.now()) * 1_000_000n, payload);
}

export function decodeL2SnapshotFrame(buffer: ArrayBuffer | Uint8Array): DecodedL2Snapshot {
  const { header, payload } = decodeBinaryHeader(buffer);
  if (header.msgType !== MSG_L2_SNAPSHOT || payload.byteLength < 56) {
    throw new Error("Invalid MSG_L2_SNAPSHOT payload");
  }
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const bestBidFp = view.getBigUint64(0, true);
  const bestAskFp = view.getBigUint64(8, true);
  const bidCount = view.getUint16(16, true);
  const askCount = view.getUint16(18, true);
  const expectedLen = 56 + (bidCount + askCount) * 20;
  if (payload.byteLength !== expectedLen) {
    throw new Error(`MSG_L2_SNAPSHOT length mismatch: ${payload.byteLength} != ${expectedLen}`);
  }
  const stateRootBlake3Hex = bytesToHex(payload.subarray(24, 56));
  const bids: DecodedL2Level[] = [];
  const asks: DecodedL2Level[] = [];
  let offset = 56;
  for (let i = 0; i < bidCount; i++) {
    bids.push({
      priceFp: view.getBigUint64(offset, true),
      totalQtyFp: view.getBigUint64(offset + 8, true),
      orderCount: view.getUint32(offset + 16, true),
    });
    offset += 20;
  }
  for (let i = 0; i < askCount; i++) {
    asks.push({
      priceFp: view.getBigUint64(offset, true),
      totalQtyFp: view.getBigUint64(offset + 8, true),
      orderCount: view.getUint32(offset + 16, true),
    });
    offset += 20;
  }
  return { header, bestBidFp, bestAskFp, stateRootBlake3Hex, bids, asks };
}

export function decodeExecutionFillFrame(buffer: ArrayBuffer | Uint8Array): DecodedExecutionFill {
  const { header, payload } = decodeBinaryHeader(buffer);
  if (header.msgType !== MSG_EXECUTION_FILL || payload.byteLength !== 64) {
    throw new Error("Invalid MSG_EXECUTION_FILL payload");
  }
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const rawSide = view.getUint8(56);
  return {
    header,
    fillId: view.getBigUint64(0, true),
    makerOrderId: view.getBigUint64(8, true),
    takerOrderId: view.getBigUint64(16, true),
    makerAccountId: view.getBigUint64(24, true),
    takerAccountId: view.getBigUint64(32, true),
    priceFp: view.getBigUint64(40, true),
    qtyFp: view.getBigUint64(48, true),
    aggressorSide: rawSide === 2 ? 2 : 1,
  };
}

/**
 * Sequence Continuity Rule (ws-binary-protocol.md §3.3):
 * Validates prev_engine_seq against local_seq and slow-consumer flags.
 */
export function evaluateSequenceContinuity(
  localSeq: bigint,
  prevEngineSeq: bigint,
  lastBatchItemSeq: bigint,
  headerFlags: number
): SequenceCheckOutcome {
  if ((headerFlags & WS_FLAG_SLOW_CONSUMER_DROPPED) !== 0 || prevEngineSeq > localSeq) {
    return { status: "GAP_DETECTED", nextLocalSeq: localSeq };
  }
  if (prevEngineSeq === localSeq) {
    return {
      status: "APPLY",
      nextLocalSeq: lastBatchItemSeq > localSeq ? lastBatchItemSeq : localSeq,
    };
  }
  return {
    status: lastBatchItemSeq <= localSeq ? "DUPLICATE_OVERLAP" : "APPLY",
    nextLocalSeq: lastBatchItemSeq > localSeq ? lastBatchItemSeq : localSeq,
  };
}
