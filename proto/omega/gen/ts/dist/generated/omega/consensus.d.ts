import { BinaryReader, BinaryWriter } from "@bufbuild/protobuf/wire";
import { TraceMetadata } from "../common";
import { MarketEventEnvelope } from "../orderbook";
/** Role of a node inside the 5-node deterministic Raft consensus cluster. */
export declare enum RaftNodeRole {
    RAFT_NODE_ROLE_UNSPECIFIED = 0,
    RAFT_NODE_ROLE_FOLLOWER = 1,
    RAFT_NODE_ROLE_PRE_CANDIDATE = 2,
    RAFT_NODE_ROLE_CANDIDATE = 3,
    RAFT_NODE_ROLE_LEADER = 4,
    RAFT_NODE_ROLE_LEARNER = 5,
    UNRECOGNIZED = -1
}
export declare function raftNodeRoleFromJSON(object: any): RaftNodeRole;
export declare function raftNodeRoleToJSON(object: RaftNodeRole): string;
/** Payload classification for replicated Raft WAL entries. */
export declare enum RaftEntryKind {
    RAFT_ENTRY_KIND_UNSPECIFIED = 0,
    RAFT_ENTRY_KIND_NOOP_BARRIER = 1,
    RAFT_ENTRY_KIND_LEDGER_EVENT = 2,
    RAFT_ENTRY_KIND_JOINT_CONFIG_ENTER = 3,
    RAFT_ENTRY_KIND_JOINT_CONFIG_COMMIT = 4,
    UNRECOGNIZED = -1
}
export declare function raftEntryKindFromJSON(object: any): RaftEntryKind;
export declare function raftEntryKindToJSON(object: RaftEntryKind): string;
/** Peer descriptor used in Joint Consensus (C_old,new) dynamic membership changes. */
export interface RaftNodePeer {
    nodeId: bigint;
    peerAddr: string;
    isVotingVoter: boolean;
}
/** Joint Consensus membership configuration (supports atomic C_old -> C_old,new -> C_new). */
export interface JointConsensusConfig {
    configIndex: bigint;
    currentVoters: RaftNodePeer[];
    /** Non-empty only while transitioning inside joint consensus C_{old,new}. */
    previousVoters: RaftNodePeer[];
    learners: RaftNodePeer[];
}
/** Single replicated log entry protected by CRC32C bit-rot checksum & Blake3 hash chain. */
export interface RaftLogEntry {
    term: bigint;
    index: bigint;
    engineSeq: bigint;
    tsNs: bigint;
    kind: RaftEntryKind;
    /** Castagnoli CRC-32C checksum for silent disk corruption (bit-rot) detection. */
    crc32cChecksum: number;
    /** 32-byte Blake3 hash of (index - 1) frame, aligning with HYPWAL01 chain. */
    prevBlake3Hash: Uint8Array;
    /** 32-byte Blake3 hash committing this entry: blake3(term || index || prev_blake3 || payload). */
    entryBlake3Hash: Uint8Array;
    ledgerEnvelope?: MarketEventEnvelope | undefined;
    membershipChange?: JointConsensusConfig | undefined;
    rawCommandBytes?: Uint8Array | undefined;
}
/** Pipelined & batched Raft log replication + leader heartbeat RPC message. */
export interface AppendEntries {
    clusterId: bigint;
    term: bigint;
    leaderId: bigint;
    prevLogIndex: bigint;
    prevLogTerm: bigint;
    /** 32-byte Blake3 digest at prev_log_index to enforce cryptographic log matching. */
    prevLogBlake3: Uint8Array;
    entries: RaftLogEntry[];
    leaderCommitIndex: bigint;
    /** Monotonic pipeline batch sequence number for out-of-order/duplicate transport deduplication. */
    pipelineSeq: bigint;
    /** Leader monotonic lease expiration horizon in nanoseconds (bounded clock-skew safe). */
    leaderLeaseUntilNs: bigint;
    trace: TraceMetadata | undefined;
}
export interface AppendEntriesResponse {
    term: bigint;
    followerId: bigint;
    success: boolean;
    matchIndex: bigint;
    /** Fast log-backtracking hint when success == false (Ongaro conflict optimization). */
    conflictIndex: bigint;
    conflictTerm: bigint;
    pipelineSeq: bigint;
    bitRotDetected: boolean;
}
/** Standard Raft election vote request (invoked ONLY after PreVote quorum succeeds). */
export interface RequestVote {
    clusterId: bigint;
    term: bigint;
    candidateId: bigint;
    lastLogIndex: bigint;
    lastLogTerm: bigint;
    lastLogBlake3: Uint8Array;
    trace: TraceMetadata | undefined;
}
export interface RequestVoteResponse {
    term: bigint;
    voterId: bigint;
    voteGranted: boolean;
    rejectReason: string;
}
/** Pre-Vote RPC (§9.6 Raft): prevents partitioned nodes from inflating cluster term upon rejoin. */
export interface PreVote {
    clusterId: bigint;
    /** Hypothetical next_term = follower.current_term + 1 (not yet persisted to disk). */
    prospectiveTerm: bigint;
    candidateId: bigint;
    lastLogIndex: bigint;
    lastLogTerm: bigint;
    lastLogBlake3: Uint8Array;
    trace: TraceMetadata | undefined;
}
export interface PreVoteResponse {
    currentTerm: bigint;
    voterId: bigint;
    voteGranted: boolean;
    leaderLeaseActive: boolean;
}
/** Streaming log-compaction snapshot chunk aligned with HYPSNP01 Blake3 state roots. */
export interface InstallSnapshot {
    clusterId: bigint;
    term: bigint;
    leaderId: bigint;
    lastIncludedIndex: bigint;
    lastIncludedTerm: bigint;
    lastIncludedEngineSeq: bigint;
    stateRootBlake3: Uint8Array;
    clusterConfig: JointConsensusConfig | undefined;
    byteOffset: bigint;
    chunkData: Uint8Array;
    chunkCrc32c: number;
    done: boolean;
    trace: TraceMetadata | undefined;
}
export interface InstallSnapshotResponse {
    term: bigint;
    followerId: bigint;
    bytesStored: bigint;
    checksumValid: boolean;
}
/** Linearizable quorum read barrier (§6.4 ReadIndex) avoiding disk log writes on read path. */
export interface ReadIndexBarrier {
    readRequestId: string;
    term: bigint;
    leaderId: bigint;
    /** Committed Raft log index at the instant the ReadIndex request arrived at Leader. */
    requiredReadIndex: bigint;
    quorumAckMask: bigint;
    servedViaLeaderLease: boolean;
    barrierLatencyNs: bigint;
    committedStateRootBlake3: Uint8Array;
    trace: TraceMetadata | undefined;
}
export interface ReadIndexBarrierRequest {
    readRequestId: string;
    allowLeaderLeaseFastPath: boolean;
    trace: TraceMetadata | undefined;
}
export interface ProposeLedgerEntryRequest {
    idempotencyKey: string;
    envelope: MarketEventEnvelope | undefined;
    trace: TraceMetadata | undefined;
}
export interface ProposeLedgerEntryResponse {
    committed: boolean;
    term: bigint;
    logIndex: bigint;
    leaderId: bigint;
    entryBlake3Hash: Uint8Array;
    errorDetail: string;
}
export interface GetRaftClusterStatusRequest {
    nodeId: bigint;
}
export interface RaftClusterStatus {
    clusterId: bigint;
    nodeId: bigint;
    role: RaftNodeRole;
    currentTerm: bigint;
    votedFor: bigint;
    leaderId: bigint;
    commitIndex: bigint;
    lastAppliedIndex: bigint;
    lastLogIndex: bigint;
    lastLogTerm: bigint;
    lastCommittedBlake3: Uint8Array;
    activeConfig: JointConsensusConfig | undefined;
}
export declare const RaftNodePeer: MessageFns<RaftNodePeer>;
export declare const JointConsensusConfig: MessageFns<JointConsensusConfig>;
export declare const RaftLogEntry: MessageFns<RaftLogEntry>;
export declare const AppendEntries: MessageFns<AppendEntries>;
export declare const AppendEntriesResponse: MessageFns<AppendEntriesResponse>;
export declare const RequestVote: MessageFns<RequestVote>;
export declare const RequestVoteResponse: MessageFns<RequestVoteResponse>;
export declare const PreVote: MessageFns<PreVote>;
export declare const PreVoteResponse: MessageFns<PreVoteResponse>;
export declare const InstallSnapshot: MessageFns<InstallSnapshot>;
export declare const InstallSnapshotResponse: MessageFns<InstallSnapshotResponse>;
export declare const ReadIndexBarrier: MessageFns<ReadIndexBarrier>;
export declare const ReadIndexBarrierRequest: MessageFns<ReadIndexBarrierRequest>;
export declare const ProposeLedgerEntryRequest: MessageFns<ProposeLedgerEntryRequest>;
export declare const ProposeLedgerEntryResponse: MessageFns<ProposeLedgerEntryResponse>;
export declare const GetRaftClusterStatusRequest: MessageFns<GetRaftClusterStatusRequest>;
export declare const RaftClusterStatus: MessageFns<RaftClusterStatus>;
/** Primary gRPC interface exposed by services/consensus-raft-rs on port 50161 (peers 50201-50205). */
export type RaftConsensusServiceDefinition = typeof RaftConsensusServiceDefinition;
export declare const RaftConsensusServiceDefinition: {
    readonly name: "RaftConsensusService";
    readonly fullName: "hyperion.omega.v1.RaftConsensusService";
    readonly methods: {
        readonly appendEntriesRpc: {
            readonly name: "AppendEntriesRpc";
            readonly requestType: typeof AppendEntries;
            readonly requestStream: false;
            readonly responseType: typeof AppendEntriesResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly requestVoteRpc: {
            readonly name: "RequestVoteRpc";
            readonly requestType: typeof RequestVote;
            readonly requestStream: false;
            readonly responseType: typeof RequestVoteResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly preVoteRpc: {
            readonly name: "PreVoteRpc";
            readonly requestType: typeof PreVote;
            readonly requestStream: false;
            readonly responseType: typeof PreVoteResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly installSnapshotRpc: {
            readonly name: "InstallSnapshotRpc";
            readonly requestType: typeof InstallSnapshot;
            readonly requestStream: true;
            readonly responseType: typeof InstallSnapshotResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly waitReadIndexBarrier: {
            readonly name: "WaitReadIndexBarrier";
            readonly requestType: typeof ReadIndexBarrierRequest;
            readonly requestStream: false;
            readonly responseType: typeof ReadIndexBarrier;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly proposeLedgerEntry: {
            readonly name: "ProposeLedgerEntry";
            readonly requestType: typeof ProposeLedgerEntryRequest;
            readonly requestStream: false;
            readonly responseType: typeof ProposeLedgerEntryResponse;
            readonly responseStream: false;
            readonly options: {};
        };
        readonly getClusterStatus: {
            readonly name: "GetClusterStatus";
            readonly requestType: typeof GetRaftClusterStatusRequest;
            readonly requestStream: false;
            readonly responseType: typeof RaftClusterStatus;
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
//# sourceMappingURL=consensus.d.ts.map