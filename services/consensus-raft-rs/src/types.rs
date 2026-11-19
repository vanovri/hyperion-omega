//! Core types for HYPERION CH12 Hardened Raft Consensus Engine & Replicated Double-Entry Ledger.
//! Strictly adheres to `.agents/rules/00-hyperion-contract.md` & `01-hyperion-omega.md`:
//! - Zero floating-point (`f32`/`f64`) anywhere in ledger or consensus state.
//! - Fixed-point integer scale `1e8` (`FP_SCALE = 100_000_000`) with checked arithmetic.
//! - Chained 32-byte Blake3 WAL digests (`HYPWAL01`) + Castagnoli CRC-32C checksums.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type NodeId = u64;
pub type Term = u64;
pub type LogIndex = u64;

/// Canonical fixed-point monetary/quantity scale (`1.00000000` = `100_000_000`).
pub const FP_SCALE_U128: u128 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;

/// Maximum voters supported in stack-allocated fast quorum calculators.
pub const MAX_CLUSTER_VOTERS: usize = 16;

/// Consensus role of a Raft node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RaftRole {
    Follower,
    PreCandidate,
    Candidate,
    Leader,
    Learner,
    /// Quarantined due to unrecoverable physical bit-rot on committed storage until healed via snapshot.
    Quarantined,
}

/// Persistent Raft HardState protected on disk by dual ping-pong 4KiB superblocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardState {
    pub current_term: Term,
    pub voted_for: Option<NodeId>,
    pub compacted_index: LogIndex,
    pub compacted_term: Term,
}

/// Dynamic Cluster Membership Configuration supporting full 2-Phase Joint Consensus (`C_old,new` -> `C_new`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterConfig {
    Uniform {
        voters: BTreeSet<NodeId>,
        learners: BTreeSet<NodeId>,
    },
    Joint {
        old_voters: BTreeSet<NodeId>,
        new_voters: BTreeSet<NodeId>,
        learners: BTreeSet<NodeId>,
    },
}

impl ClusterConfig {
    pub fn uniform<I: IntoIterator<Item = NodeId>>(voters: I) -> Self {
        Self::Uniform {
            voters: voters.into_iter().collect(),
            learners: BTreeSet::new(),
        }
    }

    pub fn uniform_with_learners<V, L>(voters: V, learners: L) -> Self
    where
        V: IntoIterator<Item = NodeId>,
        L: IntoIterator<Item = NodeId>,
    {
        let voter_set: BTreeSet<NodeId> = voters.into_iter().collect();
        let learner_set: BTreeSet<NodeId> = learners
            .into_iter()
            .filter(|id| !voter_set.contains(id))
            .collect();
        Self::Uniform {
            voters: voter_set,
            learners: learner_set,
        }
    }

    pub fn is_joint(&self) -> bool {
        matches!(self, Self::Joint { .. })
    }

    pub fn is_voter(&self, node_id: NodeId) -> bool {
        match self {
            Self::Uniform { voters, .. } => voters.contains(&node_id),
            Self::Joint {
                old_voters,
                new_voters,
                ..
            } => old_voters.contains(&node_id) || new_voters.contains(&node_id),
        }
    }

    pub fn is_learner(&self, node_id: NodeId) -> bool {
        !self.is_voter(node_id) && self.learners().contains(&node_id)
    }

    pub fn contains(&self, node_id: NodeId) -> bool {
        self.is_voter(node_id) || self.learners().contains(&node_id)
    }

    pub fn learners(&self) -> &BTreeSet<NodeId> {
        match self {
            Self::Uniform { learners, .. } | Self::Joint { learners, .. } => learners,
        }
    }

    pub fn all_members(&self) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::new();
        match self {
            Self::Uniform { voters, learners } => {
                out.extend(voters.iter().copied());
                out.extend(learners.iter().copied());
            }
            Self::Joint {
                old_voters,
                new_voters,
                learners,
            } => {
                out.extend(old_voters.iter().copied());
                out.extend(new_voters.iter().copied());
                out.extend(learners.iter().copied());
            }
        }
        out
    }

    pub fn all_voters(&self) -> BTreeSet<NodeId> {
        match self {
            Self::Uniform { voters, .. } => voters.clone(),
            Self::Joint {
                old_voters,
                new_voters,
                ..
            } => old_voters.union(new_voters).copied().collect(),
        }
    }

    /// Strict quorum predicate:
    /// - Uniform `V`: `|acks ∩ V| >= floor(|V| / 2) + 1`
    /// - Joint `(V_old, V_new)`: simultaneous strict majorities in BOTH `V_old` AND `V_new`.
    #[inline]
    pub fn has_quorum(&self, acked: &BTreeSet<NodeId>) -> bool {
        fn set_majority(voters: &BTreeSet<NodeId>, acked: &BTreeSet<NodeId>) -> bool {
            if voters.is_empty() {
                return false;
            }
            let needed = (voters.len() / 2) + 1;
            let got = voters.iter().filter(|id| acked.contains(id)).count();
            got >= needed
        }

        match self {
            Self::Uniform { voters, .. } => set_majority(voters, acked),
            Self::Joint {
                old_voters,
                new_voters,
                ..
            } => set_majority(old_voters, acked) && set_majority(new_voters, acked),
        }
    }

    /// Extracts the target `Uniform` configuration when finalizing Phase 2 of Joint Consensus.
    pub fn into_final_uniform(&self) -> Self {
        match self {
            Self::Uniform { .. } => self.clone(),
            Self::Joint {
                new_voters,
                learners,
                ..
            } => Self::uniform_with_learners(new_voters.iter().copied(), learners.iter().copied()),
        }
    }
}

/// Deterministic fixed-point Double-Entry Ledger command replicated through Raft.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerCommand {
    CreditDeposit {
        idempotency_key: String,
        account_id: u64,
        asset: String,
        amount_fp: u128,
        ts_ns: u64,
    },
    Withdraw {
        idempotency_key: String,
        account_id: u64,
        asset: String,
        amount_fp: u128,
        ts_ns: u64,
    },
    Transfer {
        idempotency_key: String,
        from_account: u64,
        to_account: u64,
        asset: String,
        amount_fp: u128,
        ts_ns: u64,
    },
    LockMargin {
        idempotency_key: String,
        account_id: u64,
        asset: String,
        amount_fp: u128,
        ts_ns: u64,
    },
    ReleaseMargin {
        idempotency_key: String,
        account_id: u64,
        asset: String,
        amount_fp: u128,
        ts_ns: u64,
    },
    SettleFill {
        idempotency_key: String,
        fill_id: u64,
        symbol: String,
        base_asset: String,
        quote_asset: String,
        maker_account: u64,
        taker_account: u64,
        buyer_is_taker: bool,
        price_fp: u64,
        qty_fp: u64,
        /// Signed parts-per-million (`1e6`; negative = maker rebate).
        maker_fee_ppm: i64,
        /// Signed parts-per-million (`1e6`).
        taker_fee_ppm: i64,
        ts_ns: u64,
    },
}

impl LedgerCommand {
    pub fn idempotency_key(&self) -> &str {
        match self {
            Self::CreditDeposit {
                idempotency_key, ..
            }
            | Self::Withdraw {
                idempotency_key, ..
            }
            | Self::Transfer {
                idempotency_key, ..
            }
            | Self::LockMargin {
                idempotency_key, ..
            }
            | Self::ReleaseMargin {
                idempotency_key, ..
            }
            | Self::SettleFill {
                idempotency_key, ..
            } => idempotency_key,
        }
    }
}

/// Result of applying a single `LedgerCommand` to the deterministic state machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandApplyReceipt {
    pub idempotency_key: String,
    pub journal_seq: u64,
    pub applied: bool,
    pub duplicate_noop: bool,
    pub reject_reason: Option<String>,
    pub state_root_blake3: [u8; 32],
}

/// Payload variant stored inside a Raft `LogEntry`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RaftEntryKind {
    /// Term-sealing barrier appended immediately upon Leader election (Ongaro §5.4.2 Figure 8).
    NoOp,
    /// Coalesced batch of deterministic double-entry ledger mutations.
    JournalBatch(Vec<LedgerCommand>),
    /// Dynamic membership transition (`ClusterConfig::Joint` or `ClusterConfig::Uniform`).
    ConfigChange(ClusterConfig),
}

/// Chained cryptographic Raft log entry aligned with `HYPWAL01` (`00-hyperion-contract.md` §6.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub index: LogIndex,
    pub term: Term,
    pub ts_ns: u64,
    pub prev_wal_blake3: [u8; 32],
    pub wal_blake3: [u8; 32],
    pub payload_crc32c: u32,
    pub kind: RaftEntryKind,
    pub payload_bytes: Vec<u8>,
}

/// Metadata header of a compacted Raft + Ledger snapshot (`HYPSNP01`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub last_included_index: LogIndex,
    pub last_included_term: Term,
    pub ts_ns: u64,
    pub last_wal_blake3: [u8; 32],
    pub state_root_blake3: [u8; 32],
    pub config: ClusterConfig,
}

/// Full serialized snapshot artifact (`HYPSNP01` binary envelope).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotArtifact {
    pub meta: SnapshotMetadata,
    /// Full `HYPSNP01` wire bytes (with magic, version, crc32c, state_root_blake3, payload).
    pub raw_hypsnp_bytes: Vec<u8>,
}

/// Linearizable read query against the deterministic ledger state machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerReadQuery {
    GetAccountBalance { account_id: u64, asset: String },
    GetSystemConservation { asset: String },
    GetLastJournalSeq,
}

/// Linearizable read response returned once `ReadIndex` or `LeaderLease` + `last_applied >= read_index` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerReadResult {
    AccountBalance {
        account_id: u64,
        asset: String,
        available_fp: i128,
        locked_fp: i128,
        total_fp: i128,
        at_index: LogIndex,
        state_root_blake3: [u8; 32],
    },
    SystemConservation {
        asset: String,
        total_deposits_fp: u128,
        total_withdrawals_fp: u128,
        total_available_fp: i128,
        total_locked_fp: i128,
        treasury_fee_pool_fp: i128,
        balanced: bool,
        at_index: LogIndex,
        state_root_blake3: [u8; 32],
    },
    LastJournalSeq {
        journal_seq: u64,
        at_index: LogIndex,
        state_root_blake3: [u8; 32],
    },
}

/// Read consistency mode for linearizable queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadConsistencyMode {
    /// Strict clock-free quorum round-trip barrier (`ReadIndexBarrier`).
    ReadIndex,
    /// Drift-bounded 0-RTT Leader Lease when active, falling back seamlessly to `ReadIndex` when expired.
    LeaderLeaseOrReadIndex,
}

/// Wire RPC messages exchanged between Raft peers (`50201–50205` / `SimulatedNetworkSwitch`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RaftRpc {
    PreVoteRequest {
        /// Prospective next term (`current_term + 1`), NOT persisted.
        term: Term,
        candidate_id: NodeId,
        last_log_index: LogIndex,
        last_log_term: Term,
        last_wal_blake3: [u8; 32],
    },
    PreVoteResponse {
        term: Term,
        vote_granted: bool,
        rejected_by_leader_lease: bool,
    },
    RequestVoteRequest {
        term: Term,
        candidate_id: NodeId,
        last_log_index: LogIndex,
        last_log_term: Term,
        last_wal_blake3: [u8; 32],
    },
    RequestVoteResponse {
        term: Term,
        vote_granted: bool,
    },
    AppendEntriesRequest {
        term: Term,
        leader_id: NodeId,
        prev_log_index: LogIndex,
        prev_log_term: Term,
        prev_wal_blake3: [u8; 32],
        entries: Vec<LogEntry>,
        leader_commit: LogIndex,
        /// Monotonic heartbeat/read-barrier sequence id (`0` if regular replication without new barrier).
        read_ctx_seq: u64,
        /// Monotonic timestamp on Leader when this RPC was dispatched (used for drift-bounded lease accounting).
        leader_sent_ts_ns: u64,
    },
    AppendEntriesResponse {
        term: Term,
        success: bool,
        match_index: LogIndex,
        conflict_term: Option<Term>,
        conflict_index: Option<LogIndex>,
        read_ctx_seq: u64,
        leader_sent_ts_ns: u64,
    },
    InstallSnapshotRequest {
        term: Term,
        leader_id: NodeId,
        meta: SnapshotMetadata,
        offset: u64,
        chunk_index: u32,
        total_chunks: u32,
        data: Vec<u8>,
        chunk_crc32c: u32,
        done: bool,
    },
    InstallSnapshotResponse {
        term: Term,
        accepted: bool,
        next_expected_offset: u64,
        stored_up_to_index: LogIndex,
    },
    ReadIndexRequest {
        req_id: u64,
        query: LedgerReadQuery,
    },
    ReadIndexResponse {
        req_id: u64,
        term: Term,
        success: bool,
        leader_id: Option<NodeId>,
        read_index: LogIndex,
        result: Option<LedgerReadResult>,
        error: Option<String>,
    },
}

/// Envelope wrapping source/destination addressing over the deterministic network fabric.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkEnvelope {
    pub from: NodeId,
    pub to: NodeId,
    pub msg_id: u64,
    pub rpc: RaftRpc,
}
