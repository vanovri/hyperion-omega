import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { AccountStatus, I128, MarketRegime, OrderSide, OrderType, SelfTradePreventionMode, TraceMetadata, U128 } from "./common";
/** Lifecycle state of an order inside the LMAX single-writer matching engine. */
export declare enum OrderStatus {
    ORDER_STATUS_UNSPECIFIED = 0,
    ORDER_STATUS_ACCEPTED = 1,
    ORDER_STATUS_PARTIALLY_FILLED = 2,
    ORDER_STATUS_FILLED = 3,
    ORDER_STATUS_CANCELED = 4,
    ORDER_STATUS_MODIFIED = 5,
    ORDER_STATUS_REJECTED = 6,
    ORDER_STATUS_EXPIRED = 7,
    UNRECOGNIZED = -1
}
export declare function orderStatusFromJSON(object: any): OrderStatus;
export declare function orderStatusToJSON(object: OrderStatus): string;
/** Deterministic rejection code when an order or mutation cannot be applied. */
export declare enum RejectReason {
    REJECT_REASON_UNSPECIFIED = 0,
    REJECT_REASON_NONE = 1,
    REJECT_REASON_INSUFFICIENT_BALANCE = 2,
    REJECT_REASON_POST_ONLY_WOULD_CROSS = 3,
    REJECT_REASON_FOK_INSUFFICIENT_LIQUIDITY = 4,
    REJECT_REASON_SELF_TRADE_PREVENTED = 5,
    REJECT_REASON_ACCOUNT_FROZEN = 6,
    REJECT_REASON_MARKET_HALTED = 7,
    REJECT_REASON_INVALID_PRICE_OR_QTY = 8,
    REJECT_REASON_DUPLICATE_IDEMPOTENCY_KEY = 9,
    REJECT_REASON_ORDER_NOT_FOUND = 10,
    REJECT_REASON_MAX_ORDER_SIZE_EXCEEDED = 11,
    REJECT_REASON_RATE_LIMITED = 12,
    REJECT_REASON_INVALID_SIGNATURE_OR_NONCE = 13,
    UNRECOGNIZED = -1
}
export declare function rejectReasonFromJSON(object: any): RejectReason;
export declare function rejectReasonToJSON(object: RejectReason): string;
/** Level-3 incremental order book delta operation type. */
export declare enum L3DeltaOp {
    L3_DELTA_OP_UNSPECIFIED = 0,
    L3_DELTA_OP_ADD = 1,
    L3_DELTA_OP_MODIFY = 2,
    L3_DELTA_OP_CANCEL = 3,
    L3_DELTA_OP_FILL = 4,
    L3_DELTA_OP_CLEAR = 5,
    UNRECOGNIZED = -1
}
export declare function l3DeltaOpFromJSON(object: any): L3DeltaOp;
export declare function l3DeltaOpToJSON(object: L3DeltaOp): string;
/** Command to place a new order into the deterministic Limit Order Book. */
export interface SubmitOrder {
    clientOrderId: string;
    accountId: bigint;
    symbol: string;
    instrumentId: number;
    side: OrderSide;
    orderType: OrderType;
    stpMode: SelfTradePreventionMode;
    /** Fixed-point price scaled by 1e8 (0 only for pure MARKET orders). */
    priceFp: bigint;
    /** Fixed-point base asset quantity scaled by 1e8 (must be > 0). */
    qtyFp: bigint;
    /** Maximum acceptable slippage in integer basis points (1 bps = 0.01%). */
    maxSlippageBps: number;
    clientTsNs: bigint;
    nonce: bigint;
    signature: Uint8Array;
    idempotencyKey: string;
    trace: TraceMetadata | undefined;
}
/** Command to cancel a resting order in the Limit Order Book. */
export interface CancelOrder {
    orderId: bigint;
    clientOrderId: string;
    accountId: bigint;
    symbol: string;
    instrumentId: number;
    clientTsNs: bigint;
    nonce: bigint;
    signature: Uint8Array;
    idempotencyKey: string;
    trace: TraceMetadata | undefined;
}
/**
 * Command to atomically amend price and/or quantity of an active resting order.
 * Price changes or size increases lose time priority; size reductions preserve priority.
 */
export interface ModifyOrder {
    orderId: bigint;
    clientOrderId: string;
    accountId: bigint;
    symbol: string;
    instrumentId: number;
    /** New fixed-point limit price scaled by 1e8. */
    newPriceFp: bigint;
    /** New total order quantity scaled by 1e8. */
    newQtyFp: bigint;
    clientTsNs: bigint;
    nonce: bigint;
    signature: Uint8Array;
    idempotencyKey: string;
    trace: TraceMetadata | undefined;
}
/** Immutable record of a matched trade execution between a resting maker and aggressing taker. */
export interface ExecutionFill {
    fillId: bigint;
    engineSeq: bigint;
    symbol: string;
    instrumentId: number;
    makerOrderId: bigint;
    takerOrderId: bigint;
    makerAccountId: bigint;
    takerAccountId: bigint;
    aggressorSide: OrderSide;
    /** Matched execution price in fixed-point (scale 1e8). */
    priceFp: bigint;
    /** Matched base asset quantity in fixed-point (scale 1e8). */
    qtyFp: bigint;
    /** Matched quote asset notional = (price_fp * qty_fp) / 1e8 in fixed-point (scale 1e8). */
    notionalFp: U128 | undefined;
    /** Signed maker fee in quote asset fixed-point (scale 1e8; negative = maker rebate). */
    makerFeeFp: bigint;
    /** Signed taker fee in quote asset fixed-point (scale 1e8). */
    takerFeeFp: bigint;
    execTsNs: bigint;
    /** 32-byte Blake3 chained WAL digest committing this execution. */
    walBlake3Hash: Uint8Array;
    trace: TraceMetadata | undefined;
}
/** Synchronous engine acknowledgment returned for SubmitOrder, CancelOrder, and ModifyOrder. */
export interface OrderAck {
    engineSeq: bigint;
    orderId: bigint;
    clientOrderId: string;
    accountId: bigint;
    symbol: string;
    instrumentId: number;
    status: OrderStatus;
    rejectReason: RejectReason;
    rejectDetail: string;
    /** Cumulative executed base quantity (scale 1e8). */
    filledQtyFp: bigint;
    /** Remaining open base quantity on book (scale 1e8). */
    remainingQtyFp: bigint;
    /** Volume-weighted average fill price for this command (scale 1e8; 0 if unfilled). */
    avgFillPriceFp: bigint;
    engineTsNs: bigint;
    /** 32-byte Blake3 hash of the WAL frame that persisted this transition. */
    walBlake3Hash: Uint8Array;
    fills: ExecutionFill[];
    trace: TraceMetadata | undefined;
}
/** Aggregated liquidity at a single fixed-point price tick. */
export interface PriceLevel {
    priceFp: bigint;
    totalQtyFp: bigint;
    orderCount: number;
}
/** Full or top-N Level-2 order book snapshot at a deterministic engine sequence number. */
export interface L2BookSnapshot {
    engineSeq: bigint;
    symbol: string;
    instrumentId: number;
    /** Sorted descending by price_fp (best bid at index 0). */
    bids: PriceLevel[];
    /** Sorted ascending by price_fp (best ask at index 0). */
    asks: PriceLevel[];
    bestBidFp: bigint;
    bestAskFp: bigint;
    midPriceFp: bigint;
    spreadFp: bigint;
    snapshotTsNs: bigint;
    /** 32-byte Blake3 state root committing LOB + double-entry ledger at engine_seq. */
    stateRootBlake3: Uint8Array;
}
/** Granular Level-3 order-by-order book mutation event for sub-millisecond state replication. */
export interface L3BookDelta {
    engineSeq: bigint;
    prevEngineSeq: bigint;
    symbol: string;
    instrumentId: number;
    op: L3DeltaOp;
    side: OrderSide;
    orderId: bigint;
    accountId: bigint;
    priceFp: bigint;
    /** Post-mutation open quantity for this order (scale 1e8; 0 on CANCEL/full FILL). */
    remainingQtyFp: bigint;
    /** Signed liquidity change at price_fp in fixed-point (scale 1e8). */
    deltaQtyFp: bigint;
    eventTsNs: bigint;
    stateRootBlake3: Uint8Array;
    trace: TraceMetadata | undefined;
}
/** Individual debit or credit leg inside a balanced double-entry accounting journal. */
export interface LedgerEntryLine {
    accountId: bigint;
    asset: string;
    /** Canonical sub-ledger bucket: "AVAILABLE", "LOCKED_MARGIN", or "TREASURY_FEE". */
    subAccount: string;
    debitFp: I128 | undefined;
    creditFp: I128 | undefined;
    balanceAfterFp: I128 | undefined;
}
/** Balanced double-entry accounting journal (Sum(debit_fp) == Sum(credit_fp) invariant). */
export interface LedgerJournalEntry {
    journalSeq: bigint;
    engineSeq: bigint;
    referenceType: string;
    referenceId: bigint;
    lines: LedgerEntryLine[];
    tsNs: bigint;
    prevJournalBlake3: Uint8Array;
    journalBlake3: Uint8Array;
}
/** Sibling hash step in a binary Blake3 Merkle inclusion proof. */
export interface MerkleProofNode {
    siblingHash: Uint8Array;
    isLeftSibling: boolean;
    depth: number;
}
/** Per-asset fixed-point balance breakdown for an account leaf. */
export interface AccountAssetBalance {
    asset: string;
    availableFp: I128 | undefined;
    lockedFp: I128 | undefined;
    totalFp: I128 | undefined;
}
/** Cryptographic Blake3 Merkle inclusion proof & global conservation-of-funds attestation. */
export interface MerkleLedgerProof {
    engineSeq: bigint;
    accountId: bigint;
    balances: AccountAssetBalance[];
    leafBlake3Hash: Uint8Array;
    stateRootBlake3: Uint8Array;
    proofPath: MerkleProofNode[];
    systemTotalDepositsFp: U128 | undefined;
    systemTotalUserEquityFp: U128 | undefined;
    treasuryFeePoolFp: I128 | undefined;
    /** True iff Sum(Available + Locked across all accounts) + TreasuryFeePool == TotalDeposits. */
    doubleEntryBalanced: boolean;
    proofTsNs: bigint;
}
/** gRPC wrappers for OrderbookService. */
export interface SubmitOrderRequest {
    order: SubmitOrder | undefined;
}
export interface SubmitOrderResponse {
    ack: OrderAck | undefined;
}
export interface CancelOrderRequest {
    cancel: CancelOrder | undefined;
}
export interface CancelOrderResponse {
    ack: OrderAck | undefined;
}
export interface ModifyOrderRequest {
    modify: ModifyOrder | undefined;
}
export interface ModifyOrderResponse {
    ack: OrderAck | undefined;
}
export interface CreditDepositRequest {
    depositId: string;
    accountId: bigint;
    asset: string;
    /** Fixed-point deposit amount scaled by 1e8 (must be > 0). */
    amountFp: U128 | undefined;
    tsNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface CreditDepositResponse {
    engineSeq: bigint;
    journalSeq: bigint;
    updatedBalance: AccountAssetBalance | undefined;
    stateRootBlake3: Uint8Array;
}
export interface GetL2BookSnapshotRequest {
    symbol: string;
    depth: number;
}
export interface GetL2BookSnapshotResponse {
    snapshot: L2BookSnapshot | undefined;
}
export interface GetMerkleLedgerProofRequest {
    accountId: bigint;
    /** Optional historical sequence; 0 queries latest committed state. */
    atEngineSeq: bigint;
}
export interface GetMerkleLedgerProofResponse {
    proof: MerkleLedgerProof | undefined;
}
export interface VerifySystemLedgerInvariantRequest {
    asset: string;
}
export interface VerifySystemLedgerInvariantResponse {
    engineSeq: bigint;
    asset: string;
    totalDepositsFp: U128 | undefined;
    totalAvailableFp: U128 | undefined;
    totalLockedFp: U128 | undefined;
    treasuryFeePoolFp: I128 | undefined;
    invariantHolds: boolean;
    crossedBookFree: boolean;
    stateRootBlake3: Uint8Array;
    verifiedTsNs: bigint;
}
export interface ApplyAccountRiskActionRequest {
    accountId: bigint;
    newStatus: AccountStatus;
    symbolRegimeOverride: MarketRegime;
    targetSymbol: string;
    cancelAllOpenOrders: boolean;
    commandId: string;
    reason: string;
    trace: TraceMetadata | undefined;
}
export interface ApplyAccountRiskActionResponse {
    applied: boolean;
    engineSeq: bigint;
    canceledOrdersCount: number;
    effectiveStatus: AccountStatus;
    stateRootBlake3: Uint8Array;
}
export interface StreamMarketEventsRequest {
    symbols: string[];
    fromEngineSeq: bigint;
    includeL3Deltas: boolean;
    includeFills: boolean;
}
export interface MarketEventEnvelope {
    engineSeq: bigint;
    symbol: string;
    tsNs: bigint;
    l3Delta?: L3BookDelta | undefined;
    fill?: ExecutionFill | undefined;
    l2Snapshot?: L2BookSnapshot | undefined;
    orderAck?: OrderAck | undefined;
    journalEntry?: LedgerJournalEntry | undefined;
}
export declare const SubmitOrder: MessageFns<SubmitOrder>;
export declare const CancelOrder: MessageFns<CancelOrder>;
export declare const ModifyOrder: MessageFns<ModifyOrder>;
export declare const ExecutionFill: MessageFns<ExecutionFill>;
export declare const OrderAck: MessageFns<OrderAck>;
export declare const PriceLevel: MessageFns<PriceLevel>;
export declare const L2BookSnapshot: MessageFns<L2BookSnapshot>;
export declare const L3BookDelta: MessageFns<L3BookDelta>;
export declare const LedgerEntryLine: MessageFns<LedgerEntryLine>;
export declare const LedgerJournalEntry: MessageFns<LedgerJournalEntry>;
export declare const MerkleProofNode: MessageFns<MerkleProofNode>;
export declare const AccountAssetBalance: MessageFns<AccountAssetBalance>;
export declare const MerkleLedgerProof: MessageFns<MerkleLedgerProof>;
export declare const SubmitOrderRequest: MessageFns<SubmitOrderRequest>;
export declare const SubmitOrderResponse: MessageFns<SubmitOrderResponse>;
export declare const CancelOrderRequest: MessageFns<CancelOrderRequest>;
export declare const CancelOrderResponse: MessageFns<CancelOrderResponse>;
export declare const ModifyOrderRequest: MessageFns<ModifyOrderRequest>;
export declare const ModifyOrderResponse: MessageFns<ModifyOrderResponse>;
export declare const CreditDepositRequest: MessageFns<CreditDepositRequest>;
export declare const CreditDepositResponse: MessageFns<CreditDepositResponse>;
export declare const GetL2BookSnapshotRequest: MessageFns<GetL2BookSnapshotRequest>;
export declare const GetL2BookSnapshotResponse: MessageFns<GetL2BookSnapshotResponse>;
export declare const GetMerkleLedgerProofRequest: MessageFns<GetMerkleLedgerProofRequest>;
export declare const GetMerkleLedgerProofResponse: MessageFns<GetMerkleLedgerProofResponse>;
export declare const VerifySystemLedgerInvariantRequest: MessageFns<VerifySystemLedgerInvariantRequest>;
export declare const VerifySystemLedgerInvariantResponse: MessageFns<VerifySystemLedgerInvariantResponse>;
export declare const ApplyAccountRiskActionRequest: MessageFns<ApplyAccountRiskActionRequest>;
export declare const ApplyAccountRiskActionResponse: MessageFns<ApplyAccountRiskActionResponse>;
export declare const StreamMarketEventsRequest: MessageFns<StreamMarketEventsRequest>;
export declare const MarketEventEnvelope: MessageFns<MarketEventEnvelope>;
/** Primary gRPC interface exposed by services/ledger-rs on port 50151. */
export type OrderbookServiceDefinition = typeof OrderbookServiceDefinition;
export declare const OrderbookServiceDefinition: {
    readonly name: "OrderbookService";
    readonly fullName: "hyperion.v1.OrderbookService";
    readonly methods: {
        readonly submitOrder: {
            readonly name: "SubmitOrder";
            readonly requestType: typeof SubmitOrderRequest;
            readonly requestStream: false;
            readonly responseType: typeof SubmitOrderResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly cancelOrder: {
            readonly name: "CancelOrder";
            readonly requestType: typeof CancelOrderRequest;
            readonly requestStream: false;
            readonly responseType: typeof CancelOrderResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly modifyOrder: {
            readonly name: "ModifyOrder";
            readonly requestType: typeof ModifyOrderRequest;
            readonly requestStream: false;
            readonly responseType: typeof ModifyOrderResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly creditDeposit: {
            readonly name: "CreditDeposit";
            readonly requestType: typeof CreditDepositRequest;
            readonly requestStream: false;
            readonly responseType: typeof CreditDepositResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getL2BookSnapshot: {
            readonly name: "GetL2BookSnapshot";
            readonly requestType: typeof GetL2BookSnapshotRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetL2BookSnapshotResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getMerkleLedgerProof: {
            readonly name: "GetMerkleLedgerProof";
            readonly requestType: typeof GetMerkleLedgerProofRequest;
            readonly requestStream: false;
            readonly responseType: typeof GetMerkleLedgerProofResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly verifySystemLedgerInvariant: {
            readonly name: "VerifySystemLedgerInvariant";
            readonly requestType: typeof VerifySystemLedgerInvariantRequest;
            readonly requestStream: false;
            readonly responseType: typeof VerifySystemLedgerInvariantResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly applyAccountRiskAction: {
            readonly name: "ApplyAccountRiskAction";
            readonly requestType: typeof ApplyAccountRiskActionRequest;
            readonly requestStream: false;
            readonly responseType: typeof ApplyAccountRiskActionResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamMarketEvents: {
            readonly name: "StreamMarketEvents";
            readonly requestType: typeof StreamMarketEventsRequest;
            readonly requestStream: false;
            readonly responseType: typeof MarketEventEnvelope;
            readonly responseStream: true;
            readonly options: {};
        };
    };
};
type Builtin = Date | Function | Uint8Array | string | number | boolean | bigint | undefined;
type DeepPartial<T> = T extends bigint ? string | number | bigint : T extends Builtin ? T : T extends globalThis.Array<infer U> ? globalThis.Array<DeepPartial<U>> : T extends ReadonlyArray<infer U> ? ReadonlyArray<DeepPartial<U>> : T extends {} ? {
    [K in keyof T]?: DeepPartial<T[K]>;
} : Partial<T>;
type KeysOfUnion<T> = T extends T ? keyof T : never;
type Exact<P, I> = P extends Builtin ? P : P & {
    [K in keyof P as Extract<K, keyof I>]: Exact<P[K], I[K & keyof I]>;
} & {
    [K in Exclude<keyof I, KeysOfUnion<P>>]: never;
};
interface MessageFns<T> {
    encode(message: T, writer?: BinaryWriter): BinaryWriter;
    decode(input: BinaryReader | Uint8Array, length?: number): T;
    fromJSON(object: any): T;
    toJSON(message: T): unknown;
    create<I extends Exact<DeepPartial<T>, I>>(base?: I): T;
    fromPartial<I extends Exact<DeepPartial<T>, I>>(object: I): T;
}
export {};
//# sourceMappingURL=orderbook.d.ts.map