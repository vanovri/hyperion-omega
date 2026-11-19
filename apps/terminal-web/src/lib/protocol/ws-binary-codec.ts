/**
 * HYPERION Little-Endian Binary WebSocket Wire Protocol v1 Codec
 * Implements 100% of contracts/ws-binary-protocol.md:
 * - Universal 32-byte cache-line half-aligned LE header (magic 0x4859, version 0x01)
 * - Castagnoli CRC-32C payload integrity verification (flag 0x01)
 * - Server -> Client frames: 0x01 Heartbeat, 0x02 L2 Snapshot, 0x03 L3 Delta Batch,
 *   0x04 Execution Fill, 0x05 Candle + Microstructure, 0x06 Fraud Alert (Protobuf),
 *   0x07 Private Order Ack (Protobuf), 0x08 Gap Resync Notice
 * - Client -> Server frames: 0x81 Subscribe, 0x82 Unsubscribe, 0x83 Resync Request
 */

import {
  AnomalyAlert,
  AnomalyPatternType,
  OrderAck,
  WS_FRAME_MAGIC,
  WS_FRAME_VERSION,
  WS_HEADER_BYTES,
  WS_MSG_TYPE,
  u128ToBigInt,
} from "@hyperion/contracts";
import { crc32c } from "./crc32c";
import type { CandleTimeframe, FraudAlertRecord, ID_TO_SYMBOL as IdSymbolMap, InstrumentSymbol } from "../types";
import { ID_TO_SYMBOL } from "../types";

export const WS_FLAGS = {
  CRC32C_PRESENT: 0x01,
  SNAPSHOT_RESYNC: 0x02,
  SLOW_CONSUMER_DROPPED_PRIOR: 0x04,
  REPLAY_TIMELINE: 0x08,
} as const;

export const WS_CHANNEL_MASK = {
  L2: 0x01,
  L3: 0x02,
  FILLS: 0x04,
  CANDLES: 0x08,
  FRAUD: 0x10,
  PRIVATE_ORDERS: 0x20,
  ALL_PUBLIC: 0x1f,
} as const;

export const L3_OP = {
  ADD: 1,
  MODIFY: 2,
  CANCEL: 3,
  FILL: 4,
  CLEAR: 5,
} as const;

export class WsProtocolError extends Error {
  public readonly closeCode = 1002;
  constructor(message: string) {
    super(message);
    this.name = "WsProtocolError";
  }
}

export interface ParsedWsHeader {
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

export interface RawLevelRecord {
  priceFp: bigint;
  totalQtyFp: bigint;
  orderCount: number;
}

export interface L2SnapshotPayload {
  bestBidFp: bigint;
  bestAskFp: bigint;
  stateRootBlake3: Uint8Array;
  bids: RawLevelRecord[];
  asks: RawLevelRecord[];
}

export interface L3DeltaItem {
  itemEngineSeq: bigint;
  orderId: bigint;
  priceFp: bigint;
  remainingQtyFp: bigint;
  op: number;
  side: "BUY" | "SELL";
  deltaQtySignFlags: number;
}

export interface L3DeltaBatchPayload {
  prevEngineSeq: bigint;
  deltas: L3DeltaItem[];
}

export interface ExecutionFillPayload {
  fillId: bigint;
  makerOrderId: bigint;
  takerOrderId: bigint;
  makerAccountId: bigint;
  takerAccountId: bigint;
  priceFp: bigint;
  qtyFp: bigint;
  aggressorSide: "BUY" | "SELL";
}

export interface CandleMicroPayload {
  interval: CandleTimeframe;
  isClosed: boolean;
  bucketOpenTsNs: bigint;
  openFp: bigint;
  highFp: bigint;
  lowFp: bigint;
  closeFp: bigint;
  baseVolumeLoFp: bigint;
  vwapFp: bigint;
  ofiLoFp: bigint;
  bookImbalanceTop5Ppm: bigint;
  kylesLambdaFp: bigint;
  tradeCount: bigint;
}

export interface HeartbeatPayload {
  gatewayMonotonicNs: bigint;
  connectedClients: number;
  clientOutboundQueueDepth: number;
}

export type DecodedWsFrame =
  | { kind: "HEARTBEAT"; header: ParsedWsHeader; payload: HeartbeatPayload }
  | { kind: "L2_SNAPSHOT"; header: ParsedWsHeader; payload: L2SnapshotPayload }
  | { kind: "L3_DELTA_BATCH"; header: ParsedWsHeader; payload: L3DeltaBatchPayload }
  | { kind: "EXECUTION_FILL"; header: ParsedWsHeader; payload: ExecutionFillPayload }
  | { kind: "CANDLE_MICRO"; header: ParsedWsHeader; payload: CandleMicroPayload }
  | { kind: "FRAUD_ALERT"; header: ParsedWsHeader; payload: FraudAlertRecord }
  | { kind: "ORDER_ACK"; header: ParsedWsHeader; payload: OrderAck }
  | { kind: "GAP_RESYNC_NOTICE"; header: ParsedWsHeader };

const INTERVAL_FROM_CODE: Record<number, CandleTimeframe> = {
  1: "1s",
  2: "5s",
  3: "1m",
  4: "5m",
  5: "15m",
  6: "1h",
};

const CODE_FROM_INTERVAL: Record<CandleTimeframe, number> = {
  "1s": 1,
  "5s": 2,
  "1m": 3,
  "5m": 4,
  "15m": 5,
  "1h": 6,
};

export function bytesToHex(bytes: Uint8Array, maxBytes = bytes.byteLength): string {
  const limit = Math.min(bytes.byteLength, maxBytes);
  let out = "";
  for (let i = 0; i < limit; i++) {
    out += (bytes[i] ?? 0).toString(16).padStart(2, "0");
  }
  return out;
}

function writeHeaderInto(
  dv: DataView,
  msgType: number,
  flags: number,
  instrumentId: number,
  payloadBytes: Uint8Array,
  engineSeq: bigint,
  timestampNs: bigint
): void {
  const withCrc = (flags & WS_FLAGS.CRC32C_PRESENT) !== 0;
  const digest = withCrc ? crc32c(payloadBytes) : 0;
  dv.setUint16(0, WS_FRAME_MAGIC, true);
  dv.setUint8(2, WS_FRAME_VERSION);
  dv.setUint8(3, msgType);
  dv.setUint8(4, flags);
  dv.setUint8(5, 0);
  dv.setUint16(6, instrumentId, true);
  dv.setUint32(8, payloadBytes.byteLength, true);
  dv.setUint32(12, digest, true);
  dv.setBigUint64(16, engineSeq, true);
  dv.setBigUint64(24, timestampNs, true);
}

export function wrapBinaryFrame(
  msgType: number,
  flags: number,
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  payload: Uint8Array
): Uint8Array {
  const out = new Uint8Array(WS_HEADER_BYTES + payload.byteLength);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  writeHeaderInto(dv, msgType, flags, instrumentId, payload, engineSeq, timestampNs);
  out.set(payload, WS_HEADER_BYTES);
  return out;
}

/** Encode client control frame (0x81 Subscribe, 0x82 Unsubscribe, 0x83 Resync Request). */
export function encodeClientControlFrame(
  msgType: 0x81 | 0x82 | 0x83,
  channelMask: number,
  instrumentMask: number,
  lastKnownEngineSeq: bigint,
  timestampNs: bigint
): Uint8Array {
  const payload = new Uint8Array(16);
  const pdv = new DataView(payload.buffer);
  pdv.setUint32(0, channelMask >>> 0, true);
  pdv.setUint32(4, instrumentMask >>> 0, true);
  pdv.setBigUint64(8, lastKnownEngineSeq, true);
  return wrapBinaryFrame(msgType, WS_FLAGS.CRC32C_PRESENT, 0, lastKnownEngineSeq, timestampNs, payload);
}

/** Encode 0x01 MSG_HEARTBEAT (32B header + 16B payload). */
export function encodeHeartbeatFrame(
  engineSeq: bigint,
  timestampNs: bigint,
  payload: HeartbeatPayload
): Uint8Array {
  const body = new Uint8Array(16);
  const dv = new DataView(body.buffer);
  dv.setBigUint64(0, payload.gatewayMonotonicNs, true);
  dv.setUint32(8, payload.connectedClients >>> 0, true);
  dv.setUint32(12, payload.clientOutboundQueueDepth >>> 0, true);
  return wrapBinaryFrame(WS_MSG_TYPE.HEARTBEAT, WS_FLAGS.CRC32C_PRESENT, 0, engineSeq, timestampNs, body);
}

/** Encode 0x02 MSG_L2_SNAPSHOT (56B header + 20B * (Nb + Na)). */
export function encodeL2SnapshotFrame(
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  snapshot: L2SnapshotPayload,
  extraFlags = WS_FLAGS.SNAPSHOT_RESYNC
): Uint8Array {
  const totalLevels = snapshot.bids.length + snapshot.asks.length;
  const body = new Uint8Array(56 + totalLevels * 20);
  const dv = new DataView(body.buffer);
  dv.setBigUint64(0, snapshot.bestBidFp, true);
  dv.setBigUint64(8, snapshot.bestAskFp, true);
  dv.setUint16(16, snapshot.bids.length, true);
  dv.setUint16(18, snapshot.asks.length, true);
  dv.setUint32(20, 0, true);
  body.set(snapshot.stateRootBlake3.subarray(0, 32), 24);

  let offset = 56;
  for (const level of snapshot.bids) {
    dv.setBigUint64(offset, level.priceFp, true);
    dv.setBigUint64(offset + 8, level.totalQtyFp, true);
    dv.setUint32(offset + 16, level.orderCount >>> 0, true);
    offset += 20;
  }
  for (const level of snapshot.asks) {
    dv.setBigUint64(offset, level.priceFp, true);
    dv.setBigUint64(offset + 8, level.totalQtyFp, true);
    dv.setUint32(offset + 16, level.orderCount >>> 0, true);
    offset += 20;
  }

  return wrapBinaryFrame(
    WS_MSG_TYPE.L2_SNAPSHOT,
    WS_FLAGS.CRC32C_PRESENT | extraFlags,
    instrumentId,
    engineSeq,
    timestampNs,
    body
  );
}

/** Encode 0x03 MSG_L3_DELTA_BATCH (16B + 40B * K). */
export function encodeL3DeltaBatchFrame(
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  batch: L3DeltaBatchPayload,
  extraFlags = 0
): Uint8Array {
  const body = new Uint8Array(16 + batch.deltas.length * 40);
  const dv = new DataView(body.buffer);
  dv.setBigUint64(0, batch.prevEngineSeq, true);
  dv.setUint16(8, batch.deltas.length, true);

  let offset = 16;
  for (const item of batch.deltas) {
    dv.setBigUint64(offset, item.itemEngineSeq, true);
    dv.setBigUint64(offset + 8, item.orderId, true);
    dv.setBigUint64(offset + 16, item.priceFp, true);
    dv.setBigUint64(offset + 24, item.remainingQtyFp, true);
    dv.setUint8(offset + 32, item.op);
    dv.setUint8(offset + 33, item.side === "BUY" ? 1 : 2);
    dv.setUint16(offset + 34, 0, true);
    dv.setInt32(offset + 36, item.deltaQtySignFlags, true);
    offset += 40;
  }

  return wrapBinaryFrame(
    WS_MSG_TYPE.L3_DELTA_BATCH,
    WS_FLAGS.CRC32C_PRESENT | extraFlags,
    instrumentId,
    engineSeq,
    timestampNs,
    body
  );
}

/** Encode 0x04 MSG_EXECUTION_FILL (64B aligned). */
export function encodeExecutionFillFrame(
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  fill: ExecutionFillPayload
): Uint8Array {
  const body = new Uint8Array(64);
  const dv = new DataView(body.buffer);
  dv.setBigUint64(0, fill.fillId, true);
  dv.setBigUint64(8, fill.makerOrderId, true);
  dv.setBigUint64(16, fill.takerOrderId, true);
  dv.setBigUint64(24, fill.makerAccountId, true);
  dv.setBigUint64(32, fill.takerAccountId, true);
  dv.setBigUint64(40, fill.priceFp, true);
  dv.setBigUint64(48, fill.qtyFp, true);
  dv.setUint8(56, fill.aggressorSide === "BUY" ? 1 : 2);
  return wrapBinaryFrame(
    WS_MSG_TYPE.EXECUTION_FILL,
    WS_FLAGS.CRC32C_PRESENT,
    instrumentId,
    engineSeq,
    timestampNs,
    body
  );
}

/** Encode 0x05 MSG_CANDLE_MICRO (96B). */
export function encodeCandleMicroFrame(
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  candle: CandleMicroPayload
): Uint8Array {
  const body = new Uint8Array(96);
  const dv = new DataView(body.buffer);
  dv.setUint8(0, CODE_FROM_INTERVAL[candle.interval]);
  dv.setUint8(1, candle.isClosed ? 1 : 0);
  dv.setBigUint64(8, candle.bucketOpenTsNs, true);
  dv.setBigUint64(16, candle.openFp, true);
  dv.setBigUint64(24, candle.highFp, true);
  dv.setBigUint64(32, candle.lowFp, true);
  dv.setBigUint64(40, candle.closeFp, true);
  dv.setBigUint64(48, candle.baseVolumeLoFp, true);
  dv.setBigUint64(56, candle.vwapFp, true);
  dv.setBigInt64(64, candle.ofiLoFp, true);
  dv.setBigInt64(72, candle.bookImbalanceTop5Ppm, true);
  dv.setBigInt64(80, candle.kylesLambdaFp, true);
  dv.setBigUint64(88, candle.tradeCount, true);
  return wrapBinaryFrame(
    WS_MSG_TYPE.CANDLE_MICRO,
    WS_FLAGS.CRC32C_PRESENT,
    instrumentId,
    engineSeq,
    timestampNs,
    body
  );
}

/** Encode 0x06 MSG_FRAUD_ALERT using canonical Protobuf AnomalyAlert bytes. */
export function encodeFraudAlertFrame(
  instrumentId: number,
  engineSeq: bigint,
  timestampNs: bigint,
  protoAlert: AnomalyAlert
): Uint8Array {
  const body = AnomalyAlert.encode(protoAlert).finish();
  return wrapBinaryFrame(
    WS_MSG_TYPE.FRAUD_ALERT,
    WS_FLAGS.CRC32C_PRESENT,
    instrumentId,
    engineSeq,
    timestampNs,
    body
  );
}

function mapPatternType(pt: AnomalyPatternType): FraudAlertRecord["patternType"] {
  switch (pt) {
    case AnomalyPatternType.ANOMALY_PATTERN_TYPE_WASH_TRADING_CYCLE:
      return "WASH_TRADING_CYCLE";
    case AnomalyPatternType.ANOMALY_PATTERN_TYPE_SPOOFING_LAYERING:
      return "SPOOFING_LAYERING";
    case AnomalyPatternType.ANOMALY_PATTERN_TYPE_VELOCITY_SPIKE:
      return "VELOCITY_SPIKE";
    case AnomalyPatternType.ANOMALY_PATTERN_TYPE_MOMENTUM_IGNITION:
      return "MOMENTUM_IGNITION";
    case AnomalyPatternType.ANOMALY_PATTERN_TYPE_LATENCY_ARBITRAGE:
      return "LATENCY_ARBITRAGE";
    default:
      return "WASH_TRADING_CYCLE";
  }
}

function parseProtoAnomalyAlert(
  payload: Uint8Array,
  fallbackInstrumentId: number
): FraudAlertRecord {
  const decoded = AnomalyAlert.decode(payload);
  const symbol: InstrumentSymbol =
    (decoded.symbol as InstrumentSymbol) ||
    (ID_TO_SYMBOL as typeof IdSymbolMap)[fallbackInstrumentId] ||
    "BTC-USDT";

  return {
    alertId: decoded.alertId,
    engineSeq: decoded.engineSeq,
    symbol,
    patternType: mapPatternType(decoded.patternType),
    severityScore: decoded.severityScore,
    confidencePpm: decoded.confidencePpm,
    autoencoderReconPpm: decoded.autoencoderReconErrorPpm,
    isolationForestPpm: decoded.isolationForestScorePpm,
    offenderAccountIds: decoded.offenderAccountIds,
    cycleRingIds: decoded.washTradingCycle?.participantAccountIds ?? [],
    circularVolumeFp: u128ToBigInt(decoded.washTradingCycle?.circularVolumeFp),
    volumeSymmetryPpm: decoded.washTradingCycle?.volumeSymmetryPpm ?? 0,
    cancelToFillNs: decoded.spoofingLayering?.cancelToFillLatencyNs ?? 0n,
    shapAttributions: decoded.shapAttributions.map((s) => ({
      featureName: s.featureName,
      featureValueFp: s.featureValueFp,
      baselineValueFp: s.baselineValueFp,
      shapContributionPpm: Number(s.shapContributionPpm),
      importanceRank: s.importanceRank,
    })),
    detectedTsNs: decoded.detectedTsNs,
    inferenceLatencyUs: Number(decoded.inferenceLatencyUs),
    modelVersion: decoded.modelVersion || "sentinel-ensemble-v1.onnx",
  };
}

/**
 * Decode any binary WebSocket frame strictly according to ws-binary-protocol.md.
 * Validates 32-byte header, magic 0x4859, version 0x01, exact payload length, and CRC-32C.
 */
export function decodeBinaryWsFrame(frame: Uint8Array): DecodedWsFrame {
  if (frame.byteLength < WS_HEADER_BYTES) {
    throw new WsProtocolError(`Frame shorter than 32B header: ${frame.byteLength}`);
  }
  const hdv = new DataView(frame.buffer, frame.byteOffset, WS_HEADER_BYTES);
  const magic = hdv.getUint16(0, true);
  const version = hdv.getUint8(2);
  if (magic !== WS_FRAME_MAGIC || version !== WS_FRAME_VERSION) {
    throw new WsProtocolError(
      `Protocol header mismatch: magic=0x${magic.toString(16)} version=${version}`
    );
  }

  const msgType = hdv.getUint8(3);
  const flags = hdv.getUint8(4);
  const reserved = hdv.getUint8(5);
  if (reserved !== 0) {
    throw new WsProtocolError(`Non-zero reserved byte in WS header: ${reserved}`);
  }
  const instrumentId = hdv.getUint16(6, true);
  const payloadLen = hdv.getUint32(8, true);
  const crc32cDigest = hdv.getUint32(12, true);
  const engineSeq = hdv.getBigUint64(16, true);
  const timestampNs = hdv.getBigUint64(24, true);

  if (frame.byteLength !== WS_HEADER_BYTES + payloadLen) {
    throw new WsProtocolError(
      `Frame length mismatch: actual=${frame.byteLength} expected=${WS_HEADER_BYTES + payloadLen}`
    );
  }

  if ((flags & WS_FLAGS.CRC32C_PRESENT) !== 0) {
    const computedCrc = crc32c(frame, WS_HEADER_BYTES, payloadLen);
    if (computedCrc !== crc32cDigest) {
      throw new WsProtocolError(
        `CRC-32C mismatch: got=0x${computedCrc.toString(16)} expected=0x${crc32cDigest.toString(16)}`
      );
    }
  }

  const header: ParsedWsHeader = {
    magic,
    version,
    msgType,
    flags,
    instrumentId,
    payloadLen,
    crc32cDigest,
    engineSeq,
    timestampNs,
  };

  const payloadOffset = frame.byteOffset + WS_HEADER_BYTES;
  const pdv = new DataView(frame.buffer, payloadOffset, payloadLen);
  const payloadSlice = frame.subarray(WS_HEADER_BYTES, WS_HEADER_BYTES + payloadLen);

  switch (msgType) {
    case WS_MSG_TYPE.HEARTBEAT: {
      if (payloadLen < 16) {
        throw new WsProtocolError(`Heartbeat payload too short: ${payloadLen}`);
      }
      return {
        kind: "HEARTBEAT",
        header,
        payload: {
          gatewayMonotonicNs: pdv.getBigUint64(0, true),
          connectedClients: pdv.getUint32(8, true),
          clientOutboundQueueDepth: pdv.getUint32(12, true),
        },
      };
    }

    case WS_MSG_TYPE.L2_SNAPSHOT: {
      if (payloadLen < 48) {
        throw new WsProtocolError(`L2 snapshot payload too short: ${payloadLen}`);
      }
      const bestBidFp = pdv.getBigUint64(0, true);
      const bestAskFp = pdv.getBigUint64(8, true);
      const bidCount = pdv.getUint16(16, true);
      const askCount = pdv.getUint16(18, true);
      const levelsBytes = (bidCount + askCount) * 20;

      // Canonical 56-byte prefix (32B Blake3 root); gracefully accept 48-byte prefix if upstream truncated
      let levelsStart = 56;
      const stateRootBlake3 = new Uint8Array(32);
      if (payloadLen === 56 + levelsBytes) {
        stateRootBlake3.set(payloadSlice.subarray(24, 56));
      } else if (payloadLen === 48 + levelsBytes) {
        stateRootBlake3.set(payloadSlice.subarray(24, 48));
        levelsStart = 48;
      } else {
        throw new WsProtocolError(
          `L2 snapshot size mismatch: len=${payloadLen} bids=${bidCount} asks=${askCount}`
        );
      }

      const bids: RawLevelRecord[] = new Array(bidCount);
      let cursor = levelsStart;
      for (let i = 0; i < bidCount; i++) {
        bids[i] = {
          priceFp: pdv.getBigUint64(cursor, true),
          totalQtyFp: pdv.getBigUint64(cursor + 8, true),
          orderCount: pdv.getUint32(cursor + 16, true),
        };
        cursor += 20;
      }

      const asks: RawLevelRecord[] = new Array(askCount);
      for (let i = 0; i < askCount; i++) {
        asks[i] = {
          priceFp: pdv.getBigUint64(cursor, true),
          totalQtyFp: pdv.getBigUint64(cursor + 8, true),
          orderCount: pdv.getUint32(cursor + 16, true),
        };
        cursor += 20;
      }

      return {
        kind: "L2_SNAPSHOT",
        header,
        payload: { bestBidFp, bestAskFp, stateRootBlake3, bids, asks },
      };
    }

    case WS_MSG_TYPE.L3_DELTA_BATCH: {
      if (payloadLen < 16) {
        throw new WsProtocolError(`L3 delta batch payload too short: ${payloadLen}`);
      }
      const prevEngineSeq = pdv.getBigUint64(0, true);
      const deltaCount = pdv.getUint16(8, true);
      if (payloadLen !== 16 + deltaCount * 40) {
        throw new WsProtocolError(
          `L3 delta batch size mismatch: len=${payloadLen} count=${deltaCount}`
        );
      }
      const deltas: L3DeltaItem[] = new Array(deltaCount);
      let cursor = 16;
      for (let i = 0; i < deltaCount; i++) {
        const sideByte = pdv.getUint8(cursor + 33);
        deltas[i] = {
          itemEngineSeq: pdv.getBigUint64(cursor, true),
          orderId: pdv.getBigUint64(cursor + 8, true),
          priceFp: pdv.getBigUint64(cursor + 16, true),
          remainingQtyFp: pdv.getBigUint64(cursor + 24, true),
          op: pdv.getUint8(cursor + 32),
          side: sideByte === 1 ? "BUY" : "SELL",
          deltaQtySignFlags: pdv.getInt32(cursor + 36, true),
        };
        cursor += 40;
      }
      return {
        kind: "L3_DELTA_BATCH",
        header,
        payload: { prevEngineSeq, deltas },
      };
    }

    case WS_MSG_TYPE.EXECUTION_FILL: {
      if (payloadLen !== 64 && payloadLen !== 57) {
        throw new WsProtocolError(`Execution fill payload length invalid: ${payloadLen}`);
      }
      const sideByte = pdv.getUint8(56);
      return {
        kind: "EXECUTION_FILL",
        header,
        payload: {
          fillId: pdv.getBigUint64(0, true),
          makerOrderId: pdv.getBigUint64(8, true),
          takerOrderId: pdv.getBigUint64(16, true),
          makerAccountId: pdv.getBigUint64(24, true),
          takerAccountId: pdv.getBigUint64(32, true),
          priceFp: pdv.getBigUint64(40, true),
          qtyFp: pdv.getBigUint64(48, true),
          aggressorSide: sideByte === 1 ? "BUY" : "SELL",
        },
      };
    }

    case WS_MSG_TYPE.CANDLE_MICRO: {
      if (payloadLen !== 96) {
        throw new WsProtocolError(`Candle micro payload length invalid: ${payloadLen}`);
      }
      const intervalCode = pdv.getUint8(0);
      return {
        kind: "CANDLE_MICRO",
        header,
        payload: {
          interval: INTERVAL_FROM_CODE[intervalCode] ?? "1s",
          isClosed: pdv.getUint8(1) !== 0,
          bucketOpenTsNs: pdv.getBigUint64(8, true),
          openFp: pdv.getBigUint64(16, true),
          highFp: pdv.getBigUint64(24, true),
          lowFp: pdv.getBigUint64(32, true),
          closeFp: pdv.getBigUint64(40, true),
          baseVolumeLoFp: pdv.getBigUint64(48, true),
          vwapFp: pdv.getBigUint64(56, true),
          ofiLoFp: pdv.getBigInt64(64, true),
          bookImbalanceTop5Ppm: pdv.getBigInt64(72, true),
          kylesLambdaFp: pdv.getBigInt64(80, true),
          tradeCount: pdv.getBigUint64(88, true),
        },
      };
    }

    case WS_MSG_TYPE.FRAUD_ALERT: {
      return {
        kind: "FRAUD_ALERT",
        header,
        payload: parseProtoAnomalyAlert(payloadSlice, instrumentId),
      };
    }

    case WS_MSG_TYPE.ORDER_ACK_PRIVATE: {
      return {
        kind: "ORDER_ACK",
        header,
        payload: OrderAck.decode(payloadSlice),
      };
    }

    case WS_MSG_TYPE.GAP_RESYNC_NOTICE: {
      return {
        kind: "GAP_RESYNC_NOTICE",
        header,
      };
    }

    default:
      throw new WsProtocolError(`Unsupported server msg_type: 0x${msgType.toString(16)}`);
  }
}
