import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { TraceMetadata, U128 } from "../common";
/** Elliptic-curve zero-knowledge proving backend used to attest solvency. */
export declare enum ZkProofSystem {
    ZK_PROOF_SYSTEM_UNSPECIFIED = 0,
    ZK_PROOF_SYSTEM_GROTH16_BN254 = 1,
    ZK_PROOF_SYSTEM_PLONK_BN254 = 2,
    ZK_PROOF_SYSTEM_HALO2_PASTA = 3,
    ZK_PROOF_SYSTEM_BULLETPROOFS_RISTRETTO = 4,
    UNRECOGNIZED = -1
}
export declare function zkProofSystemFromJSON(object: any): ZkProofSystem;
export declare function zkProofSystemToJSON(object: ZkProofSystem): string;
/** Hash function binding each internal Merkle-Sum Sparse Tree (MSST) node. */
export declare enum MsstHashScheme {
    MSST_HASH_SCHEME_UNSPECIFIED = 0,
    MSST_HASH_SCHEME_POSEIDON_BN254 = 1,
    MSST_HASH_SCHEME_POSEIDON_BLS12_381 = 2,
    MSST_HASH_SCHEME_HYBRID_POSEIDON_BLAKE3 = 3,
    UNRECOGNIZED = -1
}
export declare function msstHashSchemeFromJSON(object: any): MsstHashScheme;
export declare function msstHashSchemeToJSON(object: MsstHashScheme): string;
/** Per-asset reserve vs liability attestation summary in fixed-point (scale 1e8). */
export interface AssetSolvencySummary {
    asset: string;
    /** Total user liabilities committed at the Merkle-Sum root (scale 1e8, non-negative). */
    totalLiabilitiesFp: U128 | undefined;
    /** Verified on-chain vault reserves at anchor block height (scale 1e8). */
    verifiedReservesFp: U128 | undefined;
    /** Net solvency surplus = verified_reserves_fp - total_liabilities_fp >= 0 (scale 1e8). */
    surplusEquityFp: U128 | undefined;
    /** Collateralization ratio in parts-per-million (>= 1_000_000 = >= 100.0000%). */
    reserveRatioPpm: bigint;
}
/** Periodic cryptographic announcement of the Merkle-Sum Sparse Tree root commitment. */
export interface MerkleSumRootAnnouncement {
    epochId: bigint;
    snapshotEngineSeq: bigint;
    snapshotRaftIndex: bigint;
    asset: string;
    treeHeight: number;
    activeAccountCount: bigint;
    hashScheme: MsstHashScheme;
    /** 32-byte Poseidon/Blake3 root hash committing all leaf hashes and sum commitments. */
    rootHash: Uint8Array;
    /** Root committed sum of all customer liabilities in atomic fixed-point (scale 1e8). */
    rootLiabilitySumFp: U128 | undefined;
    onchainReserveSumFp: U128 | undefined;
    l2ReferenceBlockNumber: bigint;
    l2ReferenceBlockHash: Uint8Array;
    announcedTsNs: bigint;
    trace: TraceMetadata | undefined;
}
/** Single sibling step along an authenticated Merkle-Sum Sparse Tree path. */
export interface MerkleSumSiblingNode {
    level: number;
    siblingIsLeft: boolean;
    siblingHash: Uint8Array;
    /** Exact non-negative balance sum carried by the sibling subtree (scale 1e8). */
    siblingSumFp: U128 | undefined;
}
/** Individual user cryptographic inclusion & non-negative sum proof verifiable in Wasm/Browser. */
export interface InclusionSumProof {
    epochId: bigint;
    snapshotEngineSeq: bigint;
    asset: string;
    /** Blinded leaf identifier H(account_id || user_salt) so sibling users learn zero identity info. */
    blindedAccountLeafId: Uint8Array;
    /** User's exact non-negative balance in 64-bit fixed-point [0, 2^64 - 1] (scale 1e8). */
    userBalanceFp: bigint;
    leafSaltCommitment: Uint8Array;
    leafHash: Uint8Array;
    siblings: MerkleSumSiblingNode[];
    expectedRootHash: Uint8Array;
    expectedRootSumFp: U128 | undefined;
    hashScheme: MsstHashScheme;
    generatedTsNs: bigint;
}
/**
 * Zero-knowledge succinct receipt proving:
 *   (a) For all leaves i: 0 <= balance_i < 2^64 (no negative phantom accounts),
 *   (b) Every internal binary sum matches left_sum + right_sum without finite-field overflow,
 *   (c) root_liability_sum_fp <= verified_onchain_reserves_fp (100%+ solvency).
 */
export interface ZKSolvencyReceipt {
    receiptId: string;
    epochId: bigint;
    snapshotEngineSeq: bigint;
    proofSystem: ZkProofSystem;
    hashScheme: MsstHashScheme;
    rootAnnouncement: MerkleSumRootAnnouncement | undefined;
    assetSummaries: AssetSolvencySummary[];
    /** Serialized succinct elliptic-curve ZK proof bytes (Groth16/Plonk/Halo2). */
    zkProofBytes: Uint8Array;
    /** Canonical field elements of the public inputs vector serialized in little-endian 32B chunks. */
    publicInputsLe: Uint8Array[];
    /** 32-byte digest of the circuit verifying key (VK) anchored in SolvencyStateAnchor.sol. */
    verifyingKeyDigest: Uint8Array;
    provingLatencyMs: bigint;
    proofSizeBytes: number;
    selfVerifiedOk: boolean;
    createdTsNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface GenerateEpochSolvencyRequest {
    epochId: bigint;
    snapshotEngineSeq: bigint;
    asset: string;
    verifiedOnchainReservesFp: U128 | undefined;
    l2BlockNumber: bigint;
    l2BlockHash: Uint8Array;
    trace: TraceMetadata | undefined;
}
export interface GetInclusionSumProofRequest {
    epochId: bigint;
    accountId: bigint;
    asset: string;
}
export interface VerifyZKSolvencyReceiptRequest {
    receipt: ZKSolvencyReceipt | undefined;
}
export interface VerifyZKSolvencyReceiptResponse {
    valid: boolean;
    verificationLatencyUs: bigint;
    failureReason: string;
}
export declare const AssetSolvencySummary: MessageFns<AssetSolvencySummary>;
export declare const MerkleSumRootAnnouncement: MessageFns<MerkleSumRootAnnouncement>;
export declare const MerkleSumSiblingNode: MessageFns<MerkleSumSiblingNode>;
export declare const InclusionSumProof: MessageFns<InclusionSumProof>;
export declare const ZKSolvencyReceipt: MessageFns<ZKSolvencyReceipt>;
export declare const GenerateEpochSolvencyRequest: MessageFns<GenerateEpochSolvencyRequest>;
export declare const GetInclusionSumProofRequest: MessageFns<GetInclusionSumProofRequest>;
export declare const VerifyZKSolvencyReceiptRequest: MessageFns<VerifyZKSolvencyReceiptRequest>;
export declare const VerifyZKSolvencyReceiptResponse: MessageFns<VerifyZKSolvencyReceiptResponse>;
/** Primary gRPC interface exposed by services/zk-solvency-rs on port 50162. */
export type ZkSolvencyServiceDefinition = typeof ZkSolvencyServiceDefinition;
export declare const ZkSolvencyServiceDefinition: {
    readonly name: "ZkSolvencyService";
    readonly fullName: "hyperion.omega.v1.ZkSolvencyService";
    readonly methods: {
        readonly generateEpochSolvencyProof: {
            readonly name: "GenerateEpochSolvencyProof";
            readonly requestType: typeof GenerateEpochSolvencyRequest;
            readonly requestStream: false;
            readonly responseType: typeof ZKSolvencyReceipt;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getInclusionSumProof: {
            readonly name: "GetInclusionSumProof";
            readonly requestType: typeof GetInclusionSumProofRequest;
            readonly requestStream: false;
            readonly responseType: typeof InclusionSumProof;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly verifyZKSolvencyReceipt: {
            readonly name: "VerifyZKSolvencyReceipt";
            readonly requestType: typeof VerifyZKSolvencyReceiptRequest;
            readonly requestStream: false;
            readonly responseType: typeof VerifyZKSolvencyReceiptResponse;
            readonly responseStream: false;
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
//# sourceMappingURL=zk_solvency.d.ts.map