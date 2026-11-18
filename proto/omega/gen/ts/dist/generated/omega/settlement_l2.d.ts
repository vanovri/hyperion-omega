import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { TraceMetadata, U128 } from "../common";
import { InclusionSumProof } from "./zk_solvency";
/** Confirmation lifecycle of an L2 vault deposit tracked by chain-relayer-go. */
export declare enum DepositFinalityStatus {
    DEPOSIT_FINALITY_STATUS_UNSPECIFIED = 0,
    DEPOSIT_FINALITY_STATUS_MEMPOOL_SEEN = 1,
    DEPOSIT_FINALITY_STATUS_SOFT_CONFIRMED = 2,
    DEPOSIT_FINALITY_STATUS_FINALIZED_CREDITED = 3,
    DEPOSIT_FINALITY_STATUS_REORG_ORPHANED_REVOKED = 4,
    UNRECOGNIZED = -1
}
export declare function depositFinalityStatusFromJSON(object: any): DepositFinalityStatus;
export declare function depositFinalityStatusToJSON(object: DepositFinalityStatus): string;
/** Lifecycle status of an EIP-712 structured withdrawal or Censorship Escape-Hatch exit. */
export declare enum WithdrawalSettlementStatus {
    WITHDRAWAL_SETTLEMENT_STATUS_UNSPECIFIED = 0,
    WITHDRAWAL_SETTLEMENT_STATUS_QUEUED_OUTBOX = 1,
    WITHDRAWAL_SETTLEMENT_STATUS_SUBMITTED_ONCHAIN = 2,
    WITHDRAWAL_SETTLEMENT_STATUS_FINALIZED_SETTLED = 3,
    WITHDRAWAL_SETTLEMENT_STATUS_REJECTED_REPLAY_OR_EXPIRED = 4,
    WITHDRAWAL_SETTLEMENT_STATUS_FORCED_ESCAPE_HATCH_SETTLED = 5,
    UNRECOGNIZED = -1
}
export declare function withdrawalSettlementStatusFromJSON(object: any): WithdrawalSettlementStatus;
export declare function withdrawalSettlementStatusToJSON(object: WithdrawalSettlementStatus): string;
/** Finalized on-chain deposit event emitted ONLY after confirmations >= min_finality_blocks. */
export interface DepositFinalized {
    depositId: string;
    chainId: bigint;
    /** 20-byte EVM contract address of HyperionVault.sol. */
    vaultContractAddress: Uint8Array;
    /** Monotonic deposit sequence assigned by HyperionVault.sol on-chain. */
    onchainDepositNonce: bigint;
    accountId: bigint;
    depositorEvmAddress: Uint8Array;
    /** 20-byte ERC-20 token address (0x00..00 for native ETH). */
    tokenContractAddress: Uint8Array;
    assetSymbol: string;
    /** Exact credited amount converted to HYPERION fixed-point (scale 1e8). */
    amountFp: U128 | undefined;
    l2TxHash: Uint8Array;
    logIndex: number;
    blockNumber: bigint;
    blockHash: Uint8Array;
    confirmations: number;
    finalityStatus: DepositFinalityStatus;
    finalizedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** EIP-712 typed structured withdrawal intent immune to cross-chain, cross-epoch, and reorg replay. */
export interface WithdrawalEIP712Intent {
    withdrawalId: string;
    /** EIP-712 Domain chainId binding this signature to one exact L2 network. */
    chainId: bigint;
    /** 20-byte verifyingContract address of HyperionVault.sol. */
    verifyingVaultAddress: Uint8Array;
    accountId: bigint;
    recipientEvmAddress: Uint8Array;
    tokenContractAddress: Uint8Array;
    assetSymbol: string;
    /** Exact withdrawal amount in fixed-point (scale 1e8). */
    amountFp: U128 | undefined;
    /** Bridge fee deducted in fixed-point (scale 1e8). */
    bridgeFeeFp: U128 | undefined;
    /** Strict per-account withdrawal replay nonce committed in HyperionVault.sol. */
    withdrawalNonce: bigint;
    /** Solvency epoch binding this withdrawal to a committed Merkle-Sum state anchor. */
    epochId: bigint;
    /** Unix timestamp (seconds) after which HyperionVault.sol rejects the intent. */
    deadlineUnixSec: bigint;
    /** 32-byte keccak256 EIP-712 struct digest (\x19\x01 || domainSeparator || structHash). */
    eip712Digest: Uint8Array;
    /** 32-byte unique nullifier keccak256(chain_id || vault || account_id || withdrawal_nonce || epoch_id). */
    nullifierHash: Uint8Array;
    /** 65-byte secp256k1 ECDSA signature (r || s || v, with lower-half s malleability check). */
    secp256k1Signature: Uint8Array;
    ledgerDebitEngineSeq: bigint;
    status: WithdrawalSettlementStatus;
    createdTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Periodic L2 batch anchor committing off-chain Ledger + Merkle-Sum + ZK-Solvency root to SolvencyStateAnchor.sol. */
export interface StateRootBatchCommit {
    batchId: bigint;
    epochId: bigint;
    chainId: bigint;
    stateAnchorContract: Uint8Array;
    startEngineSeq: bigint;
    endEngineSeq: bigint;
    raftCommitIndex: bigint;
    /** Previous epoch's Merkle-Sum root hash (enforces unbroken on-chain state continuity). */
    prevMerkleSumRoot: Uint8Array;
    newMerkleSumRoot: Uint8Array;
    declaredLiabilitiesFp: U128 | undefined;
    verifiedVaultReservesFp: U128 | undefined;
    zkSolvencyProofBytes: Uint8Array;
    l2CommitTxHash: Uint8Array;
    l2BlockNumber: bigint;
    l2BlockHash: Uint8Array;
    maxFeePerGasWei: bigint;
    maxPriorityFeePerGasWei: bigint;
    gasUsed: bigint;
    committedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** High-priority alert emitted by chain-relayer-go when parentHash sliding-window detects an L2 chain reorg. */
export interface ChainReorgAlert {
    reorgId: string;
    chainId: bigint;
    /** Highest block number shared between old orphaned branch and new canonical branch. */
    commonAncestorBlockNumber: bigint;
    commonAncestorBlockHash: Uint8Array;
    orphanedTipBlockNumber: bigint;
    orphanedTipBlockHash: Uint8Array;
    newCanonicalTipBlockNumber: bigint;
    newCanonicalTipBlockHash: Uint8Array;
    /** Number of reverted blocks (1..12+); triggers automatic rollback of unfinalized staging deposits. */
    reorgDepth: number;
    revokedStagedDepositIds: string[];
    requeuedOutboxTxIds: string[];
    detectedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Trustless Censorship Escape-Hatch payload submitted directly to CensorshipEscapeHatch.sol on sequencer liveness failure. */
export interface EscapeHatchForcedExit {
    exitId: string;
    chainId: bigint;
    accountId: bigint;
    claimantEvmAddress: Uint8Array;
    anchoredEpochId: bigint;
    sequencerInactivityBlocks: bigint;
    merkleInclusionProof: InclusionSumProof | undefined;
    l2ExitTxHash: Uint8Array;
    settledTsNs: bigint;
}
export interface SubmitWithdrawalIntentResponse {
    withdrawalId: string;
    nullifierHash: Uint8Array;
    status: WithdrawalSettlementStatus;
    rejectReason: string;
}
export interface AnchorStateRootBatchResponse {
    batchId: bigint;
    epochId: bigint;
    anchored: boolean;
    l2CommitTxHash: Uint8Array;
    l2BlockNumber: bigint;
    committedBatch: StateRootBatchCommit | undefined;
}
export interface GetRelayerStatusRequest {
    chainId: bigint;
}
export interface RelayerStatusResponse {
    chainId: bigint;
    latestObservedBlock: bigint;
    latestFinalizedBlock: bigint;
    latestAnchoredEpochId: bigint;
    pendingOutboxCount: number;
    totalReorgsHandled: bigint;
    escapeHatchFrozenMode: boolean;
}
export interface StreamSettlementEventsRequest {
    chainId: bigint;
}
export interface SettlementEventEnvelope {
    chainId: bigint;
    tsNs: bigint;
    depositFinalized?: DepositFinalized | undefined;
    withdrawalIntent?: WithdrawalEIP712Intent | undefined;
    batchCommit?: StateRootBatchCommit | undefined;
    reorgAlert?: ChainReorgAlert | undefined;
    forcedExit?: EscapeHatchForcedExit | undefined;
}
export declare const DepositFinalized: MessageFns<DepositFinalized>;
export declare const WithdrawalEIP712Intent: MessageFns<WithdrawalEIP712Intent>;
export declare const StateRootBatchCommit: MessageFns<StateRootBatchCommit>;
export declare const ChainReorgAlert: MessageFns<ChainReorgAlert>;
export declare const EscapeHatchForcedExit: MessageFns<EscapeHatchForcedExit>;
export declare const SubmitWithdrawalIntentResponse: MessageFns<SubmitWithdrawalIntentResponse>;
export declare const AnchorStateRootBatchResponse: MessageFns<AnchorStateRootBatchResponse>;
export declare const GetRelayerStatusRequest: MessageFns<GetRelayerStatusRequest>;
export declare const RelayerStatusResponse: MessageFns<RelayerStatusResponse>;
export declare const StreamSettlementEventsRequest: MessageFns<StreamSettlementEventsRequest>;
export declare const SettlementEventEnvelope: MessageFns<SettlementEventEnvelope>;
/** Primary gRPC interface exposed by services/chain-relayer-go on port 50167. */
export type L2SettlementServiceDefinition = typeof L2SettlementServiceDefinition;
export declare const L2SettlementServiceDefinition: {
    readonly name: "L2SettlementService";
    readonly fullName: "hyperion.omega.v1.L2SettlementService";
    readonly methods: {
        readonly submitWithdrawalIntent: {
            readonly name: "SubmitWithdrawalIntent";
            readonly requestType: typeof WithdrawalEIP712Intent;
            readonly requestStream: false;
            readonly responseType: typeof SubmitWithdrawalIntentResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly anchorStateRootBatch: {
            readonly name: "AnchorStateRootBatch";
            readonly requestType: typeof StateRootBatchCommit;
            readonly requestStream: false;
            readonly responseType: typeof AnchorStateRootBatchResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getRelayerStatus: {
            readonly name: "GetRelayerStatus";
            readonly requestType: typeof GetRelayerStatusRequest;
            readonly requestStream: false;
            readonly responseType: typeof RelayerStatusResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly streamSettlementEvents: {
            readonly name: "StreamSettlementEvents";
            readonly requestType: typeof StreamSettlementEventsRequest;
            readonly requestStream: false;
            readonly responseType: typeof SettlementEventEnvelope;
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
//# sourceMappingURL=settlement_l2.d.ts.map