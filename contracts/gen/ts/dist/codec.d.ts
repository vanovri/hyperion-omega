import type { I128, U128 } from "./generated/common";
/** Global fixed-point scale factor (1.00000000 = 100_000_000n atomic units). */
export declare const FP_SCALE = 100000000n;
export declare const PPM_SCALE = 1000000n;
/** Binary WebSocket little-endian wire framing constants (ws-binary-protocol.md). */
export declare const WS_FRAME_MAGIC = 18521;
export declare const WS_FRAME_VERSION = 1;
export declare const WS_HEADER_BYTES = 32;
export declare const WS_CACHE_LINE_BYTES = 64;
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
export declare function checkedFeeFp(notionalFp: bigint, feePpm: bigint): bigint;
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
export declare function encodeWsHeader(h: Omit<WsBinaryHeader, "magic" | "version">): Uint8Array;
/** Pass 2 (Zero-Copy Refactor): encodes directly into caller-owned buffer at `offset`. */
export declare function encodeWsHeaderInto(out: Uint8Array, offset: number, h: Omit<WsBinaryHeader, "magic" | "version">, dv?: DataView): void;
/** Pass 3 (Cache-Line Aligned 64B Slab): allocates 64B-aligned byte/u32 views for batch WS framing. */
export declare function createAlignedWsHeaderSlab64(capacity: number): AlignedWsHeaderSlab64;
/**
 * Pass 3 (Cache-Line Aligned Unboxed Uint32Array Final):
 * Writes a 32-byte little-endian HYPERION WS header directly into a Uint32Array
 * at 32-bit word offset `wordOffset` (stride 16 words = 64B cache line per slot)
 * with zero ArrayBuffer, zero DataView, and zero BigInt heap allocations.
 */
export declare function encodeWsHeaderIntoU32(u32: Uint32Array, wordOffset: number, msgType: number, flags: number, instrumentId: number, payloadLen: number, crc32cDigest: number, seqLo: number, seqHi: number, tsLo: number, tsHi: number): void;
export declare function decodeWsHeader(frame: Uint8Array): WsBinaryHeader;
export declare function decodeWsHeaderInto(frame: Uint8Array, offset: number, out: WsBinaryHeader, dv?: DataView): WsBinaryHeader;
//# sourceMappingURL=codec.d.ts.map