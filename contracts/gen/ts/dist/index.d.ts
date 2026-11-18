export * from "./generated/common";
export * from "./generated/orderbook";
export * from "./generated/telemetry_fraud";
export * from "./generated/chronos";
export * from "./generated/stream_analytics";
import { I128, U128 } from "./generated/common";
/** Global fixed-point scale factor (1.00000000 = 100_000_000n atomic units). */
export declare const FP_SCALE = 100000000n;
export declare const PPM_SCALE = 1000000n;
/** Binary WebSocket little-endian wire framing constants (ws-binary-protocol.md). */
export declare const WS_FRAME_MAGIC = 18521;
export declare const WS_FRAME_VERSION = 1;
export declare const WS_HEADER_BYTES = 32;
export declare const WS_MSG_TYPE: {
    readonly HEARTBEAT: 1;
    readonly L2_SNAPSHOT: 2;
    readonly L3_DELTA_BATCH: 3;
    readonly EXECUTION_FILL: 4;
    readonly CANDLE_MICRO: 5;
    readonly FRAUD_ALERT: 6;
    readonly ORDER_ACK_PRIVATE: 7;
    readonly GAP_RESYNC_NOTICE: 8;
    readonly CMD_SUBSCRIBE: 129;
    readonly CMD_UNSUBSCRIBE: 130;
    readonly CMD_RESYNC_REQUEST: 131;
};
export declare function bigIntToU128(value: bigint): U128;
export declare function u128ToBigInt(msg: U128 | undefined): bigint;
export declare function bigIntToI128(value: bigint): I128;
export declare function i128ToBigInt(msg: I128 | undefined): bigint;
export declare function computeNotionalFp(priceFp: bigint, qtyFp: bigint): bigint;
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
export declare function encodeWsHeader(h: Omit<WsBinaryHeader, "magic" | "version">): Uint8Array;
export declare function decodeWsHeader(frame: Uint8Array): WsBinaryHeader;
//# sourceMappingURL=index.d.ts.map