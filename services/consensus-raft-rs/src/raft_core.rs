//! Hardened Pure-Rust Raft Consensus State Machine (`RaftCore`) — Written 100% From Scratch.
//!
//! Implements Ongaro & Ousterhout Raft (§5–§8) + Production Optimizations:
//! 1. Pre-Vote Protocol (§9.6) + Leader Stickiness + CheckQuorum (prevents term inflation under asymmetric partitions)
//! 2. Figure-8 Term Commitment Safety + Immediate Election `NoOp` Barrier
//! 3. Fast Log-Conflict Backtracking (`conflict_term` & `conflict_index`, 1-RTT per conflicting term)
//! 4. Proposal Batching + Optimistic In-Flight Pipelining (`Probe` / `Replicate` / `Snapshot` peer FSM)
//! 5. Linearizable Zero-Log-Write Reads: Drift-Bounded `LeaderLease` (0-RTT) + Quorum `ReadIndexBarrier` (1-RTT)
//! 6. Dynamic Membership Reconfiguration via 2-Phase Joint Consensus (`C_old,new` -> `C_new`) + Learner Catch-Up
//! 7. Streaming Chunked `InstallSnapshot` (`HYPSNP01`) + Chained Blake3 WAL (`HYPWAL01`) + Bit-Rot Quarantine

use crate::prng::DeterministicPrng;
use crate::quorum::AlignedQuorumTracker;
use crate::state_machine::ReplicatedLedgerStateMachine;
use crate::traits::{PersistBatch, RecoveredDiskState, StorageError};
use crate::types::{
    ClusterConfig, CommandApplyReceipt, HardState, LedgerCommand, LedgerReadQuery,
    LedgerReadResult, LogEntry, LogIndex, NetworkEnvelope, NodeId, RaftEntryKind, RaftRole,
    RaftRpc, ReadConsistencyMode, SnapshotArtifact, SnapshotMetadata, Term,
};
use crate::wal_codec::{
    build_chained_log_entry, decode_and_verify_hypsnp01_snapshot, encode_hypsnp01_snapshot,
    verify_entry_integrity,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug)]
pub struct RaftTuningConfig {
    pub cluster_id: u64,
    pub election_timeout_min_ns: u64,
    pub election_timeout_max_ns: u64,
    pub heartbeat_interval_ns: u64,
    /// Maximum tolerated relative clock rate drift in parts-per-million (`1e6`).
    pub max_clock_skew_ppm: u64,
    pub lease_safety_margin_ns: u64,
    pub max_batch_entries: usize,
    pub max_inflight_pipelines: usize,
    pub compaction_threshold_entries: usize,
    pub snapshot_chunk_bytes: usize,
    pub enable_prevote: bool,
    pub enable_leader_stickiness: bool,
    pub enable_check_quorum: bool,
    /// Optional mutation flags used exclusively by `mutation_suite` to verify 100% test kill score.
    pub mutant_flags: ConsensusMutantFlags,
}

impl Default for RaftTuningConfig {
    fn default() -> Self {
        Self {
            cluster_id: 1,
            election_timeout_min_ns: 150_000_000, // 150 ms
            election_timeout_max_ns: 300_000_000, // 300 ms
            heartbeat_interval_ns: 40_000_000,    // 40 ms
            max_clock_skew_ppm: 20_000,           // 2.0% max drift
            lease_safety_margin_ns: 5_000_000,    // 5 ms guard
            max_batch_entries: 64,
            max_inflight_pipelines: 8,
            compaction_threshold_entries: 256,
            snapshot_chunk_bytes: 4096,
            enable_prevote: true,
            enable_leader_stickiness: true,
            enable_check_quorum: true,
            mutant_flags: ConsensusMutantFlags::default(),
        }
    }
}

impl RaftTuningConfig {
    /// Drift-bounded safe Leader Lease duration in Leader nanoseconds:
    /// `floor(election_timeout_min_ns * (1e6 - skew) / (1e6 + skew)) - lease_safety_margin_ns`.
    #[inline]
    pub fn safe_lease_duration_ns(&self) -> u64 {
        let num = u128::from(self.election_timeout_min_ns)
            * u128::from(1_000_000u64.saturating_sub(self.max_clock_skew_ppm));
        let den = u128::from(1_000_000u64.saturating_add(self.max_clock_skew_ppm)).max(1);
        let scaled = (num / den) as u64;
        scaled.saturating_sub(self.lease_safety_margin_ns)
    }
}

/// Fault-injection toggles for `mutation_suite` (all `false` in normal production/simulation!).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsensusMutantFlags {
    /// Mutant 1: Commit entries from prior terms by counting replicas (violates Ongaro Figure 8!).
    pub bypass_figure8_term_check: bool,
    /// Mutant 2: Weaken up-to-date log check from `>=` to `>` or ignore term priority.
    pub invert_log_up_to_date_tiebreak: bool,
    /// Mutant 3: Weaken quorum from `n/2 + 1` to `n/2` (allows split-brain in even/partitioned sets!).
    pub weaken_quorum_threshold: bool,
    /// Mutant 4: Joint consensus checks `old_quorum || new_quorum` instead of `&&`.
    pub joint_quorum_disjunction_bug: bool,
    /// Mutant 5: Follower advances `commit_index` to `leader_commit` without `.min(req_last_idx)`.
    pub follower_commit_unverified_suffix_bug: bool,
    /// Mutant 6: Skip CRC32C bit-rot verification on incoming/stored entries.
    pub bypass_crc32c_verification: bool,
    /// Mutant 7: Skip `prev_wal_blake3` hash chain check in `AppendEntries`.
    pub bypass_blake3_chain_check: bool,
    /// Mutant 8: Serve `ReadIndex` immediately without waiting for quorum heartbeat acks.
    pub bypass_read_index_quorum: bool,
    /// Mutant 9: Grant `RequestVote` even when already `voted_for` a different candidate in same term.
    pub allow_double_vote_same_term: bool,
    /// Mutant 10: Forget to roll back uncommitted `ClusterConfig` when log suffix is truncated.
    pub skip_config_rollback_on_truncate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerReplicationMode {
    Probe { probe_inflight: bool },
    Replicate { inflight_batches: usize },
    Snapshot { next_chunk_index: u32 },
}

#[derive(Clone, Debug)]
pub struct PeerProgress {
    pub mode: PeerReplicationMode,
    pub next_index: LogIndex,
    pub match_index: LogIndex,
    pub last_ack_ts_ns: u64,
    pub acked_lease_sent_ts_ns: u64,
    pub next_pipeline_seq: u64,
}

/// Deterministic side-effect actions emitted by `RaftCore` to the caller/simulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaftEffect {
    SendRpc(NetworkEnvelope),
    Persist(PersistBatch),
    ProposalCommitted {
        client_proposal_id: u64,
        log_index: LogIndex,
        term: Term,
        entry_blake3: [u8; 32],
        receipts: Vec<CommandApplyReceipt>,
    },
    ProposalRejected {
        client_proposal_id: u64,
        leader_hint: Option<NodeId>,
        reason: String,
    },
    LinearizableReadReady {
        client_read_id: u64,
        term: Term,
        leader_id: NodeId,
        required_read_index: LogIndex,
        served_via_leader_lease: bool,
        quorum_ack_mask: u64,
        latency_ns: u64,
        result: LedgerReadResult,
    },
    LinearizableReadRejected {
        client_read_id: u64,
        leader_hint: Option<NodeId>,
        reason: String,
    },
    MembershipChanged {
        log_index: LogIndex,
        config: ClusterConfig,
        committed: bool,
    },
    NodeQuarantined {
        node_id: NodeId,
        error: StorageError,
    },
}

#[derive(Clone, Debug)]
struct PendingReadBarrier {
    read_seq: u64,
    client_read_id: u64,
    query: LedgerReadQuery,
    required_read_index: LogIndex,
    acked_voters: BTreeSet<NodeId>,
    created_ts_ns: u64,
}

#[derive(Clone, Debug)]
struct StagingSnapshotBuffer {
    meta: SnapshotMetadata,
    assembled_bytes: Vec<u8>,
}

pub struct RaftCore {
    pub id: NodeId,
    pub tuning: RaftTuningConfig,
    pub role: RaftRole,
    pub hard_state: HardState,
    pub leader_id: Option<NodeId>,

    /// Compacted prefix boundary (`index == 0` when no snapshot exists yet).
    pub snapshot_meta: SnapshotMetadata,
    pub latest_snapshot: Option<SnapshotArtifact>,

    /// Suffix log starting strictly at `snapshot_meta.last_included_index + 1`.
    log: Vec<LogEntry>,

    /// Highest `LogIndex` confirmed `fsync`-durable on local `DiskStorage`.
    pub persisted_index: LogIndex,
    pub commit_index: LogIndex,

    /// Follower-side ceiling of `min(leader_commit, req_last_idx)` waiting for `persisted_index` catch-up.
    pending_follower_commit_cap: LogIndex,

    /// Stack of `(log_index, ClusterConfig)` for instant O(1) rollback when uncommitted entries truncate.
    config_history: Vec<(LogIndex, ClusterConfig)>,

    /// Replicated deterministic Double-Entry Ledger State Machine.
    pub state_machine: ReplicatedLedgerStateMachine,

    /// Election & heartbeat timers (monotonic virtual/physical nanoseconds).
    election_deadline_ns: u64,
    last_heartbeat_sent_ns: u64,
    last_valid_leader_contact_ns: Option<u64>,

    /// Votes collected in current `PreCandidate` or `Candidate` round.
    votes_granted: BTreeSet<NodeId>,

    /// Leader-only replication state.
    pub peers: BTreeMap<NodeId, PeerProgress>,
    quorum_tracker: AlignedQuorumTracker,
    term_noop_index: LogIndex,
    has_committed_in_current_term: bool,
    auto_joint_finalize_appended: bool,

    /// Coalesced client proposals waiting to flush as a single batched `LogEntry`.
    staged_proposals: VecDeque<(u64, Vec<LedgerCommand>)>,
    inflight_proposals: BTreeMap<LogIndex, Vec<u64>>,

    /// Linearizable `ReadIndex` & `LeaderLease` state.
    next_read_ctx_seq: u64,
    pending_reads: VecDeque<PendingReadBarrier>,
    leader_lease_until_ns: u64,

    /// Follower chunked `InstallSnapshot` reassembly buffer.
    staging_snapshot: Option<StagingSnapshotBuffer>,

    /// Deterministic PRNG for election jitter, message IDs, and I/O correlation.
    prng: DeterministicPrng,
    next_msg_id: u64,
    next_io_id: u64,
    log_epoch: u64,
    pending_io_targets: BTreeMap<u64, (u64, LogIndex)>,
    deferred_post_sync: BTreeMap<u64, (u64, Vec<NetworkEnvelope>)>,
    outbox: Vec<RaftEffect>,
}

impl RaftCore {
    pub fn new(
        id: NodeId,
        initial_config: ClusterConfig,
        tuning: RaftTuningConfig,
        prng_seed: u64,
        now_ns: u64,
        recovered: Option<RecoveredDiskState>,
    ) -> Self {
        let mut prng = DeterministicPrng::new(prng_seed ^ id.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let base_meta = SnapshotMetadata {
            last_included_index: 0,
            last_included_term: 0,
            ts_ns: 0,
            last_wal_blake3: [0u8; 32],
            state_root_blake3: [0u8; 32],
            config: initial_config.clone(),
        };

        let init_role = if initial_config.is_learner(id) {
            RaftRole::Learner
        } else {
            RaftRole::Follower
        };

        let mut core = Self {
            id,
            tuning,
            role: init_role,
            hard_state: HardState::default(),
            leader_id: None,
            snapshot_meta: base_meta,
            latest_snapshot: None,
            log: Vec::new(),
            persisted_index: 0,
            commit_index: 0,
            pending_follower_commit_cap: 0,
            config_history: vec![(0, initial_config)],
            state_machine: ReplicatedLedgerStateMachine::new(),
            election_deadline_ns: 0,
            last_heartbeat_sent_ns: now_ns,
            last_valid_leader_contact_ns: None,
            votes_granted: BTreeSet::new(),
            peers: BTreeMap::new(),
            quorum_tracker: AlignedQuorumTracker::new(),
            term_noop_index: 0,
            has_committed_in_current_term: false,
            auto_joint_finalize_appended: false,
            staged_proposals: VecDeque::new(),
            inflight_proposals: BTreeMap::new(),
            next_read_ctx_seq: 1,
            pending_reads: VecDeque::new(),
            leader_lease_until_ns: 0,
            staging_snapshot: None,
            prng: prng.fork_stream(0xA11C_E001),
            next_msg_id: 1,
            next_io_id: 1,
            log_epoch: 1,
            pending_io_targets: BTreeMap::new(),
            deferred_post_sync: BTreeMap::new(),
            outbox: Vec::new(),
        };
        core.snapshot_meta.state_root_blake3 = core.state_machine.state_root_blake3();
        core.reset_election_timer(now_ns);

        if let Some(rec) = recovered {
            core.restore_recovered_state(rec);
        }
        core
    }

    fn restore_recovered_state(&mut self, rec: RecoveredDiskState) {
        self.hard_state = rec.hard_state;
        if let Some(snap) = rec.snapshot {
            if let Ok((meta, sm_bytes)) = decode_and_verify_hypsnp01_snapshot(&snap.raw_hypsnp_bytes)
            {
                if let Ok(sm) = ReplicatedLedgerStateMachine::deserialize_deterministic(&sm_bytes) {
                    self.state_machine = sm;
                    self.snapshot_meta = meta.clone();
                    self.config_history = vec![(meta.last_included_index, meta.config)];
                    self.commit_index = meta.last_included_index;
                    self.persisted_index = meta.last_included_index;
                    self.latest_snapshot = Some(snap);
                }
            }
        }
        for entry in rec.entries {
            if entry.index == self.last_log_index() + 1 {
                if let RaftEntryKind::ConfigChange(ref cfg) = entry.kind {
                    self.config_history.push((entry.index, cfg.clone()));
                }
                self.persisted_index = entry.index;
                self.log.push(entry);
            }
        }
        self.role = if self.active_config().is_learner(self.id) {
            RaftRole::Learner
        } else {
            RaftRole::Follower
        };
    }

    #[inline]
    pub fn drain_effects(&mut self) -> Vec<RaftEffect> {
        std::mem::take(&mut self.outbox)
    }

    #[inline]
    pub fn active_config(&self) -> &ClusterConfig {
        &self
            .config_history
            .last()
            .expect("config_history always holds at least index 0")
            .1
    }

    #[inline]
    pub fn first_log_index(&self) -> LogIndex {
        self.snapshot_meta.last_included_index + 1
    }

    #[inline]
    pub fn last_log_index(&self) -> LogIndex {
        self.snapshot_meta.last_included_index + (self.log.len() as u64)
    }

    #[inline]
    pub fn last_log_term(&self) -> Term {
        self.log
            .last()
            .map(|e| e.term)
            .unwrap_or(self.snapshot_meta.last_included_term)
    }

    #[inline]
    pub fn last_wal_blake3(&self) -> [u8; 32] {
        self.log
            .last()
            .map(|e| e.wal_blake3)
            .unwrap_or(self.snapshot_meta.last_wal_blake3)
    }

    pub fn term_at(&self, index: LogIndex) -> Option<Term> {
        if index == 0 {
            return Some(0);
        }
        if index == self.snapshot_meta.last_included_index {
            return Some(self.snapshot_meta.last_included_term);
        }
        if index < self.snapshot_meta.last_included_index || index > self.last_log_index() {
            return None;
        }
        let pos = (index - self.snapshot_meta.last_included_index - 1) as usize;
        self.log.get(pos).map(|e| e.term)
    }

    pub fn blake3_at(&self, index: LogIndex) -> Option<[u8; 32]> {
        if index == 0 {
            return Some([0u8; 32]);
        }
        if index == self.snapshot_meta.last_included_index {
            return Some(self.snapshot_meta.last_wal_blake3);
        }
        if index < self.snapshot_meta.last_included_index || index > self.last_log_index() {
            return None;
        }
        let pos = (index - self.snapshot_meta.last_included_index - 1) as usize;
        self.log.get(pos).map(|e| e.wal_blake3)
    }

    pub fn entry_at(&self, index: LogIndex) -> Option<&LogEntry> {
        if index <= self.snapshot_meta.last_included_index || index > self.last_log_index() {
            return None;
        }
        let pos = (index - self.snapshot_meta.last_included_index - 1) as usize;
        self.log.get(pos)
    }

    /// Injects silent in-memory/cached entry bit-rot for testing Follower/Leader scrub quarantine.
    pub fn corrupt_cached_entry_byte_for_test(&mut self, index: LogIndex) -> bool {
        if index <= self.snapshot_meta.last_included_index || index > self.last_log_index() {
            return false;
        }
        let pos = (index - self.snapshot_meta.last_included_index - 1) as usize;
        if let Some(entry) = self.log.get_mut(pos) {
            if let Some(b) = entry.payload_bytes.first_mut() {
                *b ^= 0xFF;
                return true;
            }
        }
        false
    }

    fn verify_local_entry_not_corrupted(&mut self, index: LogIndex) -> bool {
        if self.tuning.mutant_flags.bypass_crc32c_verification {
            return true;
        }
        if index <= self.snapshot_meta.last_included_index || index > self.last_log_index() {
            return true;
        }
        let prev_hash = match self.blake3_at(index - 1) {
            Some(h) => h,
            None => return false,
        };
        let pos = (index - self.snapshot_meta.last_included_index - 1) as usize;
        if let Some(entry) = self.log.get(pos) {
            if let Err(err) = verify_entry_integrity(entry, &prev_hash, pos) {
                self.enter_quarantine(err);
                return false;
            }
        }
        true
    }

    pub fn enter_quarantine(&mut self, err: StorageError) {
        self.role = RaftRole::Quarantined;
        self.leader_id = None;
        self.fail_inflight_proposals_and_reads("node quarantined due to storage bit-rot");
        self.outbox.push(RaftEffect::NodeQuarantined {
            node_id: self.id,
            error: err,
        });
    }

    fn reset_election_timer(&mut self, now_ns: u64) {
        let timeout = self.prng.range_u64(
            self.tuning.election_timeout_min_ns,
            self.tuning.election_timeout_max_ns,
        );
        self.election_deadline_ns = now_ns.saturating_add(timeout);
    }

    /// Periodic deterministic tick driven by `Clock::now_ns`.
    pub fn tick(&mut self, now_ns: u64) {
        if self.role == RaftRole::Quarantined {
            return;
        }

        // Flush any staged client proposals if Leader.
        if self.role == RaftRole::Leader && !self.staged_proposals.is_empty() {
            self.flush_staged_proposals(now_ns);
        }

        match self.role {
            RaftRole::Leader => {
                // CheckQuorum: if Leader hasn't heard from a quorum within election_timeout_max_ns, step down!
                if self.tuning.enable_check_quorum && !self.check_leader_quorum_alive(now_ns) {
                    let term = self.hard_state.current_term;
                    self.step_down_to_follower(term, None, now_ns, false);
                    return;
                }
                if now_ns.saturating_sub(self.last_heartbeat_sent_ns)
                    >= self.tuning.heartbeat_interval_ns
                {
                    self.broadcast_append_entries(now_ns, true, 0);
                }
            }
            RaftRole::Follower | RaftRole::PreCandidate | RaftRole::Candidate => {
                if !self.active_config().is_voter(self.id) {
                    self.role = RaftRole::Learner;
                    return;
                }
                if now_ns >= self.election_deadline_ns {
                    self.start_election_flow(now_ns);
                }
            }
            RaftRole::Learner | RaftRole::Quarantined => {}
        }
    }

    fn check_leader_quorum_alive(&self, now_ns: u64) -> bool {
        let horizon = self.tuning.election_timeout_max_ns;
        let mut active = BTreeSet::new();
        if self.active_config().is_voter(self.id) {
            active.insert(self.id);
        }
        for (&peer_id, prog) in &self.peers {
            if now_ns.saturating_sub(prog.last_ack_ts_ns) <= horizon {
                active.insert(peer_id);
            }
        }
        self.has_quorum_with_mutants(&active)
    }

    fn has_quorum_with_mutants(&self, acked: &BTreeSet<NodeId>) -> bool {
        let cfg = self.active_config();
        if self.tuning.mutant_flags.weaken_quorum_threshold {
            let voters = cfg.all_voters();
            let needed = (voters.len() / 2).max(1);
            return voters.iter().filter(|id| acked.contains(id)).count() >= needed;
        }
        if self.tuning.mutant_flags.joint_quorum_disjunction_bug {
            if let ClusterConfig::Joint {
                old_voters,
                new_voters,
                ..
            } = cfg
            {
                let old_ok =
                    old_voters.iter().filter(|id| acked.contains(id)).count() > old_voters.len() / 2;
                let new_ok =
                    new_voters.iter().filter(|id| acked.contains(id)).count() > new_voters.len() / 2;
                return old_ok || new_ok;
            }
        }
        cfg.has_quorum(acked)
    }

    /// Start Pre-Vote (§9.6) or direct RequestVote if Pre-Vote disabled.
    fn start_election_flow(&mut self, now_ns: u64) {
        self.reset_election_timer(now_ns);
        self.leader_id = None;

        if self.tuning.enable_prevote && self.role != RaftRole::PreCandidate {
            self.role = RaftRole::PreCandidate;
            self.votes_granted.clear();
            if self.active_config().is_voter(self.id) {
                self.votes_granted.insert(self.id);
            }

            // Single-node cluster fast path
            if self.has_quorum_with_mutants(&self.votes_granted) {
                self.promote_to_real_candidate(now_ns);
                return;
            }

            let prospective_term = self.hard_state.current_term + 1;
            let last_idx = self.last_log_index();
            let last_term = self.last_log_term();
            let last_blake3 = self.last_wal_blake3();
            let voters: Vec<NodeId> = self
                .active_config()
                .all_voters()
                .into_iter()
                .filter(|&p| p != self.id)
                .collect();

            for peer in voters {
                self.emit_rpc(
                    peer,
                    RaftRpc::PreVoteRequest {
                        term: prospective_term,
                        candidate_id: self.id,
                        last_log_index: last_idx,
                        last_log_term: last_term,
                        last_wal_blake3: last_blake3,
                    },
                );
            }
        } else if self.tuning.enable_prevote && self.role == RaftRole::PreCandidate {
            // Re-run PreVote WITHOUT inflating current_term!
            self.votes_granted.clear();
            if self.active_config().is_voter(self.id) {
                self.votes_granted.insert(self.id);
            }
            if self.has_quorum_with_mutants(&self.votes_granted) {
                self.promote_to_real_candidate(now_ns);
                return;
            }
            let prospective_term = self.hard_state.current_term + 1;
            let last_idx = self.last_log_index();
            let last_term = self.last_log_term();
            let last_blake3 = self.last_wal_blake3();
            let voters: Vec<NodeId> = self
                .active_config()
                .all_voters()
                .into_iter()
                .filter(|&p| p != self.id)
                .collect();
            for peer in voters {
                self.emit_rpc(
                    peer,
                    RaftRpc::PreVoteRequest {
                        term: prospective_term,
                        candidate_id: self.id,
                        last_log_index: last_idx,
                        last_log_term: last_term,
                        last_wal_blake3: last_blake3,
                    },
                );
            }
        } else {
            self.promote_to_real_candidate(now_ns);
        }
    }

    fn promote_to_real_candidate(&mut self, now_ns: u64) {
        self.role = RaftRole::Candidate;
        self.hard_state.current_term += 1;
        self.hard_state.voted_for = Some(self.id);
        self.leader_id = None;
        self.reset_election_timer(now_ns);

        self.votes_granted.clear();
        if self.active_config().is_voter(self.id) {
            self.votes_granted.insert(self.id);
        }

        let term = self.hard_state.current_term;
        let last_idx = self.last_log_index();
        let last_term = self.last_log_term();
        let last_blake3 = self.last_wal_blake3();

        let voters: Vec<NodeId> = self
            .active_config()
            .all_voters()
            .into_iter()
            .filter(|&p| p != self.id)
            .collect();

        let mut post_sync_msgs = Vec::with_capacity(voters.len());
        for peer in voters {
            post_sync_msgs.push(self.build_envelope(
                peer,
                RaftRpc::RequestVoteRequest {
                    term,
                    candidate_id: self.id,
                    last_log_index: last_idx,
                    last_log_term: last_term,
                    last_wal_blake3: last_blake3,
                },
            ));
        }

        let io_id = self.emit_persist(
            Some(self.hard_state),
            None,
            Vec::new(),
            None,
            post_sync_msgs,
        );

        // If single-node cluster, become leader once disk syncs (handled when on_disk_synced runs or if voters empty)
        let _ = io_id;
        if self.has_quorum_with_mutants(&self.votes_granted) {
            self.become_leader(now_ns);
        }
    }

    fn become_leader(&mut self, now_ns: u64) {
        if self.role == RaftRole::Leader {
            return;
        }
        self.role = RaftRole::Leader;
        self.leader_id = Some(self.id);
        self.has_committed_in_current_term = false;
        self.auto_joint_finalize_appended = false;
        self.leader_lease_until_ns = 0;

        let next_idx = self.last_log_index() + 1;
        self.peers.clear();
        for member in self.active_config().all_members() {
            if member == self.id {
                continue;
            }
            self.peers.insert(
                member,
                PeerProgress {
                    mode: PeerReplicationMode::Probe {
                        probe_inflight: false,
                    },
                    next_index: next_idx,
                    match_index: 0,
                    last_ack_ts_ns: now_ns,
                    acked_lease_sent_ts_ns: 0,
                    next_pipeline_seq: 1,
                },
            );
        }

        // Immediately append a term-sealing `NoOp` entry (Ongaro §5.4.2 Figure 8 + §6.4 ReadIndex)!
        let noop_entry = self.append_local_entry(now_ns, RaftEntryKind::NoOp);
        self.term_noop_index = noop_entry.index;
        self.rebuild_quorum_tracker();

        self.emit_persist(None, None, vec![noop_entry], None, Vec::new());
        self.broadcast_append_entries(now_ns, true, 0);
    }

    fn step_down_to_follower(
        &mut self,
        new_term: Term,
        leader_id: Option<NodeId>,
        now_ns: u64,
        persist_hard_state: bool,
    ) {
        let was_leader = self.role == RaftRole::Leader;
        if new_term > self.hard_state.current_term {
            self.hard_state.current_term = new_term;
            self.hard_state.voted_for = None;
        }
        self.role = if self.active_config().is_learner(self.id) {
            RaftRole::Learner
        } else {
            RaftRole::Follower
        };
        self.leader_id = leader_id;
        self.votes_granted.clear();
        self.leader_lease_until_ns = 0;
        if leader_id.is_some() {
            self.reset_election_timer(now_ns);
        }
        if was_leader {
            self.fail_inflight_proposals_and_reads("leader stepped down");
        }
        if persist_hard_state {
            self.emit_persist(Some(self.hard_state), None, Vec::new(), None, Vec::new());
        }
    }

    fn fail_inflight_proposals_and_reads(&mut self, reason: &str) {
        while let Some((prop_id, _)) = self.staged_proposals.pop_front() {
            self.outbox.push(RaftEffect::ProposalRejected {
                client_proposal_id: prop_id,
                leader_hint: self.leader_id,
                reason: reason.to_string(),
            });
        }
        let inflight = std::mem::take(&mut self.inflight_proposals);
        for (_idx, ids) in inflight {
            for prop_id in ids {
                self.outbox.push(RaftEffect::ProposalRejected {
                    client_proposal_id: prop_id,
                    leader_hint: self.leader_id,
                    reason: format!("{reason} (outcome indeterminate until committed)"),
                });
            }
        }
        while let Some(rb) = self.pending_reads.pop_front() {
            self.outbox.push(RaftEffect::LinearizableReadRejected {
                client_read_id: rb.client_read_id,
                leader_hint: self.leader_id,
                reason: reason.to_string(),
            });
        }
    }

    /// Stage one or more deterministic `LedgerCommand` items for replication.
    /// Automatically coalesces bursts up to `max_batch_entries`.
    pub fn propose_ledger_commands(
        &mut self,
        client_proposal_id: u64,
        commands: Vec<LedgerCommand>,
        now_ns: u64,
        flush_immediately: bool,
    ) {
        if self.role == RaftRole::Quarantined {
            self.outbox.push(RaftEffect::ProposalRejected {
                client_proposal_id,
                leader_hint: None,
                reason: "node is quarantined due to disk bit-rot".to_string(),
            });
            return;
        }
        if self.role != RaftRole::Leader {
            self.outbox.push(RaftEffect::ProposalRejected {
                client_proposal_id,
                leader_hint: self.leader_id,
                reason: "not leader".to_string(),
            });
            return;
        }
        if commands.is_empty() {
            self.outbox.push(RaftEffect::ProposalRejected {
                client_proposal_id,
                leader_hint: Some(self.id),
                reason: "empty command batch".to_string(),
            });
            return;
        }

        self.staged_proposals
            .push_back((client_proposal_id, commands));
        if flush_immediately || self.staged_proposals.len() >= self.tuning.max_batch_entries {
            self.flush_staged_proposals(now_ns);
        }
    }

    pub fn flush_staged_proposals(&mut self, now_ns: u64) {
        if self.role != RaftRole::Leader || self.staged_proposals.is_empty() {
            return;
        }

        let mut new_entries = Vec::new();
        while !self.staged_proposals.is_empty() {
            let mut merged_cmds = Vec::new();
            let mut prop_ids = Vec::new();
            while merged_cmds.len() < self.tuning.max_batch_entries {
                let Some((pid, cmds)) = self.staged_proposals.pop_front() else {
                    break;
                };
                prop_ids.push(pid);
                merged_cmds.extend(cmds);
            }
            let entry = self.append_local_entry(now_ns, RaftEntryKind::JournalBatch(merged_cmds));
            self.inflight_proposals.insert(entry.index, prop_ids);
            new_entries.push(entry);
        }

        if !new_entries.is_empty() {
            self.emit_persist(None, None, new_entries, None, Vec::new());
            self.broadcast_append_entries(now_ns, false, 0);
        }
    }

    /// Initiate dynamic membership reconfiguration via 2-Phase Joint Consensus (`C_old,new` -> `C_new`).
    pub fn propose_membership_change(
        &mut self,
        new_voters: BTreeSet<NodeId>,
        learners: BTreeSet<NodeId>,
        now_ns: u64,
    ) -> Result<LogIndex, String> {
        if self.role != RaftRole::Leader {
            return Err("not leader".to_string());
        }
        if new_voters.is_empty() {
            return Err("new_voters cannot be empty".to_string());
        }
        if !self.has_committed_in_current_term {
            return Err("leader has not yet committed current-term barrier".to_string());
        }
        let uncommitted_cfg = self
            .config_history
            .last()
            .map(|(idx, _)| *idx > self.commit_index)
            .unwrap_or(false);
        if uncommitted_cfg || self.active_config().is_joint() {
            return Err("another membership reconfiguration is already in progress".to_string());
        }

        let old_voters = self.active_config().all_voters();
        let clean_learners: BTreeSet<NodeId> = learners
            .into_iter()
            .filter(|id| !old_voters.contains(id) && !new_voters.contains(id))
            .collect();

        let joint_cfg = if old_voters == new_voters {
            ClusterConfig::Uniform {
                voters: new_voters,
                learners: clean_learners,
            }
        } else {
            ClusterConfig::Joint {
                old_voters,
                new_voters,
                learners: clean_learners,
            }
        };

        let entry = self.append_local_entry(now_ns, RaftEntryKind::ConfigChange(joint_cfg.clone()));
        let idx = entry.index;
        self.sync_leader_peers_with_active_config(now_ns);
        self.outbox.push(RaftEffect::MembershipChanged {
            log_index: idx,
            config: joint_cfg,
            committed: false,
        });
        self.emit_persist(None, None, vec![entry], None, Vec::new());
        self.broadcast_append_entries(now_ns, true, 0);
        Ok(idx)
    }

    /// Serve a linearizable read via 0-RTT `LeaderLease` (when valid) or 1-RTT `ReadIndexBarrier`!
    pub fn request_linearizable_read(
        &mut self,
        client_read_id: u64,
        query: LedgerReadQuery,
        mode: ReadConsistencyMode,
        now_ns: u64,
    ) {
        if self.role == RaftRole::Quarantined {
            self.outbox.push(RaftEffect::LinearizableReadRejected {
                client_read_id,
                leader_hint: None,
                reason: "node quarantined".to_string(),
            });
            return;
        }
        if self.role != RaftRole::Leader {
            self.outbox.push(RaftEffect::LinearizableReadRejected {
                client_read_id,
                leader_hint: self.leader_id,
                reason: "not leader".to_string(),
            });
            return;
        }

        // Fast Path: 0-RTT Drift-Bounded Leader Lease!
        if mode == ReadConsistencyMode::LeaderLeaseOrReadIndex
            && self.has_committed_in_current_term
            && now_ns < self.leader_lease_until_ns
            && self.state_machine.last_applied_index >= self.commit_index
        {
            let result = self.state_machine.execute_read(&query);
            self.outbox.push(RaftEffect::LinearizableReadReady {
                client_read_id,
                term: self.hard_state.current_term,
                leader_id: self.id,
                required_read_index: self.commit_index,
                served_via_leader_lease: true,
                quorum_ack_mask: 1u64 << (self.id.min(63)),
                latency_ns: 0,
                result,
            });
            return;
        }

        // Mutant 8 check (used only by mutation test suite to verify test suite catches stale reads!):
        if self.tuning.mutant_flags.bypass_read_index_quorum {
            let result = self.state_machine.execute_read(&query);
            self.outbox.push(RaftEffect::LinearizableReadReady {
                client_read_id,
                term: self.hard_state.current_term,
                leader_id: self.id,
                required_read_index: self.commit_index,
                served_via_leader_lease: false,
                quorum_ack_mask: 1u64 << (self.id.min(63)),
                latency_ns: 0,
                result,
            });
            return;
        }

        // Strict Quorum `ReadIndexBarrier` (1-RTT heartbeat confirmation, 0 log writes!):
        let read_seq = self.next_read_ctx_seq;
        self.next_read_ctx_seq = self.next_read_ctx_seq.saturating_add(1);
        let required_read_index = self.commit_index.max(self.term_noop_index);
        let mut acked_voters = BTreeSet::new();
        if self.active_config().is_voter(self.id) {
            acked_voters.insert(self.id);
        }

        self.pending_reads.push_back(PendingReadBarrier {
            read_seq,
            client_read_id,
            query,
            required_read_index,
            acked_voters,
            created_ts_ns: now_ns,
        });

        self.try_satisfy_pending_reads(now_ns);
        if self.pending_reads.iter().any(|r| r.read_seq == read_seq) {
            self.broadcast_append_entries(now_ns, true, read_seq);
        }
    }

    /// Callback invoked when `DiskStorage::fsync()` completes for `io_id`.
    pub fn on_disk_synced(&mut self, io_id: u64, synced_up_to_index: LogIndex, now_ns: u64) {
        if self.role == RaftRole::Quarantined {
            return;
        }

        let io_meta = self.pending_io_targets.remove(&io_id);
        // POSIX fsync is a sequential barrier: flushing `io_id` supersedes any un-acked older `k < io_id`.
        self.pending_io_targets.retain(|&k, _| k > io_id);
        let epoch_still_current = io_meta
            .map(|(saved_epoch, _)| saved_epoch == self.log_epoch)
            .unwrap_or(true);

        if epoch_still_current {
            let effective_idx = io_meta
                .map(|(_, target_idx)| target_idx.min(synced_up_to_index))
                .unwrap_or(synced_up_to_index);
            if effective_idx > self.persisted_index {
                self.persisted_index = effective_idx.min(self.last_log_index());
            }
        }

        if let Some((saved_epoch, envelopes)) = self.deferred_post_sync.remove(&io_id) {
            if saved_epoch == self.log_epoch {
                for env in envelopes {
                    self.outbox.push(RaftEffect::SendRpc(env));
                }
            } else {
                // Only emit non-AppendEntriesResponse (e.g. Vote responses) if log_epoch rolled forward!
                for env in envelopes {
                    if !matches!(env.rpc, RaftRpc::AppendEntriesResponse { success: true, .. }) {
                        self.outbox.push(RaftEffect::SendRpc(env));
                    }
                }
            }
        }

        if self.role == RaftRole::Leader {
            self.rebuild_quorum_tracker();
            self.advance_leader_commit_index(now_ns);
        } else {
            let target = self.pending_follower_commit_cap.min(self.persisted_index);
            if target > self.commit_index {
                self.commit_index = target;
                self.apply_committed_entries(now_ns);
            }
        }
    }

    /// Process an incoming `NetworkEnvelope` deterministically.
    pub fn step_network(&mut self, env: NetworkEnvelope, now_ns: u64) {
        if self.role == RaftRole::Quarantined {
            return;
        }

        match env.rpc {
            RaftRpc::PreVoteRequest {
                term,
                candidate_id,
                last_log_index,
                last_log_term,
                ..
            } => self.handle_prevote_request(
                env.from,
                term,
                candidate_id,
                last_log_index,
                last_log_term,
                now_ns,
            ),
            RaftRpc::PreVoteResponse {
                term,
                vote_granted,
                rejected_by_leader_lease,
            } => self.handle_prevote_response(
                env.from,
                term,
                vote_granted,
                rejected_by_leader_lease,
                now_ns,
            ),
            RaftRpc::RequestVoteRequest {
                term,
                candidate_id,
                last_log_index,
                last_log_term,
                ..
            } => self.handle_request_vote(
                env.from,
                term,
                candidate_id,
                last_log_index,
                last_log_term,
                now_ns,
            ),
            RaftRpc::RequestVoteResponse { term, vote_granted } => {
                self.handle_request_vote_response(env.from, term, vote_granted, now_ns)
            }
            RaftRpc::AppendEntriesRequest {
                term,
                leader_id,
                prev_log_index,
                prev_log_term,
                prev_wal_blake3,
                entries,
                leader_commit,
                read_ctx_seq,
                leader_sent_ts_ns,
            } => self.handle_append_entries(
                env.from,
                term,
                leader_id,
                prev_log_index,
                prev_log_term,
                prev_wal_blake3,
                entries,
                leader_commit,
                read_ctx_seq,
                leader_sent_ts_ns,
                now_ns,
            ),
            RaftRpc::AppendEntriesResponse {
                term,
                success,
                match_index,
                conflict_term,
                conflict_index,
                read_ctx_seq,
                leader_sent_ts_ns,
            } => self.handle_append_entries_response(
                env.from,
                term,
                success,
                match_index,
                conflict_term,
                conflict_index,
                read_ctx_seq,
                leader_sent_ts_ns,
                now_ns,
            ),
            RaftRpc::InstallSnapshotRequest {
                term,
                leader_id,
                meta,
                offset,
                chunk_index,
                total_chunks,
                data,
                chunk_crc32c,
                done,
            } => self.handle_install_snapshot(
                env.from,
                term,
                leader_id,
                meta,
                offset,
                chunk_index,
                total_chunks,
                data,
                chunk_crc32c,
                done,
                now_ns,
            ),
            RaftRpc::InstallSnapshotResponse {
                term,
                accepted,
                next_expected_offset,
                stored_up_to_index,
            } => self.handle_install_snapshot_response(
                env.from,
                term,
                accepted,
                next_expected_offset,
                stored_up_to_index,
                now_ns,
            ),
            RaftRpc::ReadIndexRequest { req_id, query } => {
                self.request_linearizable_read(
                    req_id,
                    query,
                    ReadConsistencyMode::ReadIndex,
                    now_ns,
                );
            }
            RaftRpc::ReadIndexResponse { .. } => {}
        }
    }

    fn is_candidate_log_up_to_date(&self, cand_last_term: Term, cand_last_index: LogIndex) -> bool {
        let my_last_term = self.last_log_term();
        let my_last_index = self.last_log_index();
        if self.tuning.mutant_flags.invert_log_up_to_date_tiebreak {
            return cand_last_index >= my_last_index;
        }
        cand_last_term > my_last_term
            || (cand_last_term == my_last_term && cand_last_index >= my_last_index)
    }

    fn has_recent_leader_stickiness(&self, now_ns: u64) -> bool {
        if !self.tuning.enable_leader_stickiness {
            return false;
        }
        if self.role == RaftRole::Leader {
            return self.check_leader_quorum_alive(now_ns);
        }
        if let Some(last_contact) = self.last_valid_leader_contact_ns {
            now_ns.saturating_sub(last_contact) < self.tuning.election_timeout_min_ns
        } else {
            false
        }
    }

    fn handle_prevote_request(
        &mut self,
        from: NodeId,
        prospective_term: Term,
        _candidate_id: NodeId,
        cand_last_index: LogIndex,
        cand_last_term: Term,
        now_ns: u64,
    ) {
        // PreVote NEVER mutates current_term or voted_for!
        if prospective_term < self.hard_state.current_term {
            self.emit_rpc(
                from,
                RaftRpc::PreVoteResponse {
                    term: self.hard_state.current_term,
                    vote_granted: false,
                    rejected_by_leader_lease: false,
                },
            );
            return;
        }

        if self.has_recent_leader_stickiness(now_ns) {
            self.emit_rpc(
                from,
                RaftRpc::PreVoteResponse {
                    term: self.hard_state.current_term,
                    vote_granted: false,
                    rejected_by_leader_lease: true,
                },
            );
            return;
        }

        let up_to_date = self.is_candidate_log_up_to_date(cand_last_term, cand_last_index);
        self.emit_rpc(
            from,
            RaftRpc::PreVoteResponse {
                term: self.hard_state.current_term,
                vote_granted: up_to_date,
                rejected_by_leader_lease: false,
            },
        );
    }

    fn handle_prevote_response(
        &mut self,
        from: NodeId,
        resp_term: Term,
        vote_granted: bool,
        _rejected_by_lease: bool,
        now_ns: u64,
    ) {
        if resp_term > self.hard_state.current_term {
            self.step_down_to_follower(resp_term, None, now_ns, true);
            return;
        }
        if self.role != RaftRole::PreCandidate {
            return;
        }
        if vote_granted {
            self.votes_granted.insert(from);
            if self.has_quorum_with_mutants(&self.votes_granted) {
                self.promote_to_real_candidate(now_ns);
            }
        }
    }

    fn handle_request_vote(
        &mut self,
        from: NodeId,
        term: Term,
        candidate_id: NodeId,
        cand_last_index: LogIndex,
        cand_last_term: Term,
        now_ns: u64,
    ) {
        let mut hs_changed = false;
        if term > self.hard_state.current_term {
            self.hard_state.current_term = term;
            self.hard_state.voted_for = None;
            let was_leader = self.role == RaftRole::Leader;
            self.role = if self.active_config().is_learner(self.id) {
                RaftRole::Learner
            } else {
                RaftRole::Follower
            };
            self.leader_id = None;
            self.votes_granted.clear();
            self.leader_lease_until_ns = 0;
            if was_leader {
                self.fail_inflight_proposals_and_reads("higher term in RequestVote");
            }
            hs_changed = true;
        }

        if term < self.hard_state.current_term {
            let reply = self.build_envelope(
                from,
                RaftRpc::RequestVoteResponse {
                    term: self.hard_state.current_term,
                    vote_granted: false,
                },
            );
            if hs_changed {
                self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
            } else {
                self.outbox.push(RaftEffect::SendRpc(reply));
            }
            return;
        }

        let can_vote_for_candidate = if self.tuning.mutant_flags.allow_double_vote_same_term {
            true
        } else {
            self.hard_state.voted_for.is_none() || self.hard_state.voted_for == Some(candidate_id)
        };
        let log_ok = self.is_candidate_log_up_to_date(cand_last_term, cand_last_index);
        let grant = can_vote_for_candidate && log_ok && self.active_config().is_voter(self.id);

        if grant && self.hard_state.voted_for != Some(candidate_id) {
            self.hard_state.voted_for = Some(candidate_id);
            hs_changed = true;
        }
        if grant {
            self.reset_election_timer(now_ns);
        }

        let reply = self.build_envelope(
            from,
            RaftRpc::RequestVoteResponse {
                term: self.hard_state.current_term,
                vote_granted: grant,
            },
        );
        if hs_changed {
            self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
        } else {
            self.outbox.push(RaftEffect::SendRpc(reply));
        }
    }

    fn handle_request_vote_response(
        &mut self,
        from: NodeId,
        term: Term,
        vote_granted: bool,
        now_ns: u64,
    ) {
        if term > self.hard_state.current_term {
            self.step_down_to_follower(term, None, now_ns, true);
            return;
        }
        if self.role != RaftRole::Candidate || term != self.hard_state.current_term {
            return;
        }
        if vote_granted {
            self.votes_granted.insert(from);
            if self.has_quorum_with_mutants(&self.votes_granted) {
                self.become_leader(now_ns);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_append_entries(
        &mut self,
        from: NodeId,
        term: Term,
        leader_id: NodeId,
        prev_log_index: LogIndex,
        prev_log_term: Term,
        prev_wal_blake3: [u8; 32],
        entries: Vec<LogEntry>,
        leader_commit: LogIndex,
        read_ctx_seq: u64,
        leader_sent_ts_ns: u64,
        now_ns: u64,
    ) {
        if term < self.hard_state.current_term {
            self.emit_rpc(
                from,
                RaftRpc::AppendEntriesResponse {
                    term: self.hard_state.current_term,
                    success: false,
                    match_index: 0,
                    conflict_term: None,
                    conflict_index: None,
                    read_ctx_seq,
                    leader_sent_ts_ns,
                },
            );
            return;
        }

        let mut hs_changed = false;
        if term > self.hard_state.current_term
            || self.role == RaftRole::PreCandidate
            || self.role == RaftRole::Candidate
            || (self.role == RaftRole::Leader && term > self.hard_state.current_term)
        {
            let was_leader = self.role == RaftRole::Leader;
            if term > self.hard_state.current_term {
                self.hard_state.current_term = term;
                self.hard_state.voted_for = None;
                hs_changed = true;
            }
            self.role = if self.active_config().is_learner(self.id) {
                RaftRole::Learner
            } else {
                RaftRole::Follower
            };
            self.votes_granted.clear();
            self.leader_lease_until_ns = 0;
            if was_leader {
                self.fail_inflight_proposals_and_reads("stepped down on AppendEntries");
            }
        }

        self.leader_id = Some(leader_id);
        self.last_valid_leader_contact_ns = Some(now_ns);
        self.reset_election_timer(now_ns);

        // Case 1: `prev_log_index` already compacted inside our local snapshot!
        if prev_log_index < self.snapshot_meta.last_included_index {
            // Shift slice forward so we only inspect entries strictly after `snapshot_meta.last_included_index`.
            let snap_idx = self.snapshot_meta.last_included_index;
            let req_last = prev_log_index + (entries.len() as u64);
            if req_last <= snap_idx {
                let reply = self.build_envelope(
                    from,
                    RaftRpc::AppendEntriesResponse {
                        term: self.hard_state.current_term,
                        success: true,
                        match_index: self.persisted_index,
                        conflict_term: None,
                        conflict_index: None,
                        read_ctx_seq,
                        leader_sent_ts_ns,
                    },
                );
                if hs_changed {
                    self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
                } else {
                    self.outbox.push(RaftEffect::SendRpc(reply));
                }
                return;
            }
            let skip = (snap_idx - prev_log_index) as usize;
            let trimmed: Vec<LogEntry> = entries.into_iter().skip(skip).collect();
            self.handle_append_entries(
                from,
                term,
                leader_id,
                snap_idx,
                self.snapshot_meta.last_included_term,
                self.snapshot_meta.last_wal_blake3,
                trimmed,
                leader_commit,
                read_ctx_seq,
                leader_sent_ts_ns,
                now_ns,
            );
            return;
        }

        // Case 2: Follower log is shorter than `prev_log_index` -> Fast Backtrack hint!
        if prev_log_index > self.last_log_index() {
            let reply = self.build_envelope(
                from,
                RaftRpc::AppendEntriesResponse {
                    term: self.hard_state.current_term,
                    success: false,
                    match_index: self.persisted_index,
                    conflict_term: None,
                    conflict_index: Some(self.last_log_index() + 1),
                    read_ctx_seq,
                    leader_sent_ts_ns,
                },
            );
            if hs_changed {
                self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
            } else {
                self.outbox.push(RaftEffect::SendRpc(reply));
            }
            return;
        }

        // Verify local `prev_log_index` hasn't suffered silent disk bit-rot!
        if !self.verify_local_entry_not_corrupted(prev_log_index) {
            return;
        }

        // Case 3: Term or Blake3 mismatch at `prev_log_index` -> Fast Term Conflict Backtracking!
        let local_prev_term = self.term_at(prev_log_index).unwrap_or(0);
        let local_prev_blake3 = self.blake3_at(prev_log_index).unwrap_or([0u8; 32]);
        let blake3_matches = self.tuning.mutant_flags.bypass_blake3_chain_check
            || (local_prev_blake3 == prev_wal_blake3);

        if local_prev_term != prev_log_term || !blake3_matches {
            // Find the FIRST index in our log with `term == local_prev_term` (1-RTT full-term skip!).
            let mut first_idx = prev_log_index;
            while first_idx > self.first_log_index() {
                if self.term_at(first_idx - 1) == Some(local_prev_term) {
                    first_idx -= 1;
                } else {
                    break;
                }
            }
            let reply = self.build_envelope(
                from,
                RaftRpc::AppendEntriesResponse {
                    term: self.hard_state.current_term,
                    success: false,
                    match_index: 0,
                    conflict_term: Some(local_prev_term),
                    conflict_index: Some(first_idx),
                    read_ctx_seq,
                    leader_sent_ts_ns,
                },
            );
            if hs_changed {
                self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
            } else {
                self.outbox.push(RaftEffect::SendRpc(reply));
            }
            return;
        }

        // Verify incoming entries' CRC32C & Blake3 chain BEFORE accepting!
        let mut expected_prev_hash = local_prev_blake3;
        for incoming in &entries {
            if !self.tuning.mutant_flags.bypass_crc32c_verification
                && verify_entry_integrity(incoming, &expected_prev_hash, 0).is_err()
            {
                // Corrupted packet over wire / bit-rot -> reject without corrupting Follower log!
                let reply = self.build_envelope(
                    from,
                    RaftRpc::AppendEntriesResponse {
                        term: self.hard_state.current_term,
                        success: false,
                        match_index: self.persisted_index,
                        conflict_term: None,
                        conflict_index: Some(incoming.index),
                        read_ctx_seq,
                        leader_sent_ts_ns,
                    },
                );
                if hs_changed {
                    self.emit_persist(
                        Some(self.hard_state),
                        None,
                        Vec::new(),
                        None,
                        vec![reply],
                    );
                } else {
                    self.outbox.push(RaftEffect::SendRpc(reply));
                }
                return;
            }
            expected_prev_hash = incoming.wal_blake3;
        }

        // Reconcile entries (NEVER truncate matching prefix/suffix!).
        let req_last_idx = prev_log_index + (entries.len() as u64);
        let mut truncate_from: Option<LogIndex> = None;
        let mut newly_appended: Vec<LogEntry> = Vec::new();

        for entry in entries {
            if entry.index <= self.snapshot_meta.last_included_index {
                continue;
            }
            if entry.index <= self.last_log_index() {
                if !self.verify_local_entry_not_corrupted(entry.index) {
                    return;
                }
                let existing_matches = self
                    .entry_at(entry.index)
                    .map(|ex| ex.term == entry.term && ex.wal_blake3 == entry.wal_blake3)
                    .unwrap_or(false);
                if existing_matches {
                    continue;
                }
                // Conflict at `entry.index`: truncate `entry.index..=last_log_index`!
                self.truncate_log_suffix_from(entry.index);
                truncate_from = Some(truncate_from.map_or(entry.index, |t| t.min(entry.index)));
            }

            if entry.index == self.last_log_index() + 1 {
                if let RaftEntryKind::ConfigChange(ref cfg) = entry.kind {
                    self.config_history.push((entry.index, cfg.clone()));
                    self.outbox.push(RaftEffect::MembershipChanged {
                        log_index: entry.index,
                        config: cfg.clone(),
                        committed: false,
                    });
                }
                self.log.push(entry.clone());
                newly_appended.push(entry);
            }
        }

        // Ongaro Figure 2 Receiver Rule 5:
        // `commitIndex = min(leaderCommit, index of last new entry)`
        let verified_commit_cap = if self.tuning.mutant_flags.follower_commit_unverified_suffix_bug
        {
            leader_commit
        } else {
            leader_commit.min(req_last_idx)
        };
        if verified_commit_cap > self.pending_follower_commit_cap {
            self.pending_follower_commit_cap = verified_commit_cap;
        }

        let need_disk_sync = hs_changed || truncate_from.is_some() || !newly_appended.is_empty();
        if need_disk_sync {
            if let Some(trunc_idx) = truncate_from {
                self.persisted_index = self.persisted_index.min(trunc_idx - 1);
            }
            let ack_match_idx = req_last_idx.min(self.last_log_index());
            let reply = self.build_envelope(
                from,
                RaftRpc::AppendEntriesResponse {
                    term: self.hard_state.current_term,
                    success: true,
                    match_index: ack_match_idx,
                    conflict_term: None,
                    conflict_index: None,
                    read_ctx_seq,
                    leader_sent_ts_ns,
                },
            );
            self.emit_persist(
                if hs_changed {
                    Some(self.hard_state)
                } else {
                    None
                },
                truncate_from,
                newly_appended,
                None,
                vec![reply],
            );
        } else {
            // Pure heartbeat or duplicate already-persisted batch: zero disk I/O needed!
            let target = self.pending_follower_commit_cap.min(self.persisted_index);
            if target > self.commit_index {
                self.commit_index = target;
                self.apply_committed_entries(now_ns);
            }
            self.emit_rpc(
                from,
                RaftRpc::AppendEntriesResponse {
                    term: self.hard_state.current_term,
                    success: true,
                    match_index: req_last_idx.min(self.persisted_index),
                    conflict_term: None,
                    conflict_index: None,
                    read_ctx_seq,
                    leader_sent_ts_ns,
                },
            );
        }
    }

    fn truncate_log_suffix_from(&mut self, from_index: LogIndex) {
        if from_index <= self.snapshot_meta.last_included_index || from_index > self.last_log_index()
        {
            return;
        }
        let keep_len = (from_index - self.snapshot_meta.last_included_index - 1) as usize;
        self.log.truncate(keep_len);
        self.log_epoch = self.log_epoch.wrapping_add(1);

        if !self.tuning.mutant_flags.skip_config_rollback_on_truncate {
            while self
                .config_history
                .last()
                .map(|(idx, _)| *idx >= from_index)
                .unwrap_or(false)
                && self.config_history.len() > 1
            {
                self.config_history.pop();
            }
            if self.role != RaftRole::Leader {
                self.role = if self.active_config().is_learner(self.id) {
                    RaftRole::Learner
                } else {
                    RaftRole::Follower
                };
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_append_entries_response(
        &mut self,
        from: NodeId,
        term: Term,
        success: bool,
        match_index: LogIndex,
        conflict_term: Option<Term>,
        conflict_index: Option<LogIndex>,
        read_ctx_seq: u64,
        leader_sent_ts_ns: u64,
        now_ns: u64,
    ) {
        if term > self.hard_state.current_term {
            self.step_down_to_follower(term, None, now_ns, true);
            return;
        }
        if self.role != RaftRole::Leader || term != self.hard_state.current_term {
            return;
        }

        // Update peer liveness, lease start horizon, and ReadIndex acks even if log lagged!
        if let Some(prog) = self.peers.get_mut(&from) {
            prog.last_ack_ts_ns = now_ns;
            if leader_sent_ts_ns > prog.acked_lease_sent_ts_ns {
                prog.acked_lease_sent_ts_ns = leader_sent_ts_ns;
            }
        }
        self.recompute_leader_lease_horizon();

        if read_ctx_seq > 0 {
            for rb in &mut self.pending_reads {
                if read_ctx_seq >= rb.read_seq {
                    rb.acked_voters.insert(from);
                }
            }
        }

        if !self.peers.contains_key(&from) {
            self.try_satisfy_pending_reads(now_ns);
            return;
        }

        if success {
            let snap_last_included = self.snapshot_meta.last_included_index;
            let (updated_match, updated_next) = {
                let prog = self.peers.get_mut(&from).expect("peer checked above");
                if match_index > prog.match_index {
                    prog.match_index = match_index;
                }
                if prog.next_index < prog.match_index + 1 {
                    prog.next_index = prog.match_index + 1;
                }
                match prog.mode {
                    PeerReplicationMode::Probe { .. } => {
                        prog.mode = PeerReplicationMode::Replicate {
                            inflight_batches: 0,
                        };
                    }
                    PeerReplicationMode::Replicate {
                        ref mut inflight_batches,
                    } => {
                        *inflight_batches = inflight_batches.saturating_sub(1);
                    }
                    PeerReplicationMode::Snapshot { .. } => {
                        if prog.match_index >= snap_last_included {
                            prog.mode = PeerReplicationMode::Replicate {
                                inflight_batches: 0,
                            };
                        }
                    }
                }
                (prog.match_index, prog.next_index)
            };

            self.quorum_tracker.update_match_index(from, updated_match);
            self.advance_leader_commit_index(now_ns);
            // If peer still lags behind `last_log_index()`, pipeline next batches immediately!
            if self.role == RaftRole::Leader && updated_next <= self.last_log_index() {
                self.send_append_to_peer(from, now_ns, false, 0);
            }
        } else {
            // Fast 1-RTT per-term conflict backtracking (Ongaro §5.3)!
            let prev_next = self.peers[&from].next_index;
            let backtrack_next = if let Some(c_term) = conflict_term {
                // Search Leader's log for the LAST entry with `term == c_term`.
                let mut last_of_cterm = None;
                for e in self.log.iter().rev() {
                    if e.term == c_term {
                        last_of_cterm = Some(e.index);
                        break;
                    }
                    if e.term < c_term {
                        break;
                    }
                }
                if let Some(idx) = last_of_cterm {
                    idx + 1
                } else {
                    conflict_index.unwrap_or_else(|| prev_next.saturating_sub(1))
                }
            } else {
                conflict_index.unwrap_or_else(|| prev_next.saturating_sub(1))
            };

            let first_idx = self.first_log_index();
            let needs_snapshot = {
                let prog = self.peers.get_mut(&from).expect("peer checked above");
                let clamped = backtrack_next.max(1).min(prog.next_index.saturating_sub(1).max(1));
                prog.next_index = clamped.max(prog.match_index + 1);
                if prog.next_index < first_idx {
                    prog.mode = PeerReplicationMode::Snapshot {
                        next_chunk_index: 0,
                    };
                    true
                } else {
                    prog.mode = PeerReplicationMode::Probe {
                        probe_inflight: false,
                    };
                    false
                }
            };

            if needs_snapshot {
                // Peer needs compacted prefix -> transition to streaming `InstallSnapshot`!
                self.send_snapshot_chunk_to_peer(from, 0);
            } else {
                self.send_append_to_peer(from, now_ns, true, 0);
            }
        }

        self.try_satisfy_pending_reads(now_ns);
    }

    fn recompute_leader_lease_horizon(&mut self) {
        let mut candidate_timestamps: Vec<u64> = self
            .peers
            .values()
            .filter(|p| p.acked_lease_sent_ts_ns > 0)
            .map(|p| p.acked_lease_sent_ts_ns)
            .collect();
        // Single-node cluster case
        if self.active_config().all_voters().len() == 1 && self.active_config().is_voter(self.id) {
            candidate_timestamps.push(self.last_heartbeat_sent_ns);
        }
        candidate_timestamps.sort_unstable_by(|a, b| b.cmp(a));
        candidate_timestamps.dedup();

        let safe_dur = self.tuning.safe_lease_duration_ns();
        for ts in candidate_timestamps {
            let mut acked = BTreeSet::new();
            if self.active_config().is_voter(self.id) {
                acked.insert(self.id);
            }
            for (&pid, prog) in &self.peers {
                if prog.acked_lease_sent_ts_ns >= ts {
                    acked.insert(pid);
                }
            }
            if self.has_quorum_with_mutants(&acked) {
                let horizon = ts.saturating_add(safe_dur);
                if horizon > self.leader_lease_until_ns {
                    self.leader_lease_until_ns = horizon;
                }
                break;
            }
        }
    }

    fn rebuild_quorum_tracker(&mut self) {
        let mut map = BTreeMap::new();
        map.insert(self.id, self.persisted_index);
        for (&pid, prog) in &self.peers {
            map.insert(pid, prog.match_index);
        }
        let cfg = self.active_config().clone();
        self.quorum_tracker.rebuild_from_config(&cfg, &map);
    }

    fn sync_leader_peers_with_active_config(&mut self, now_ns: u64) {
        let members = self.active_config().all_members();
        let next_idx = self.last_log_index() + 1;
        for m in &members {
            if *m == self.id {
                continue;
            }
            self.peers.entry(*m).or_insert(PeerProgress {
                mode: PeerReplicationMode::Probe {
                    probe_inflight: false,
                },
                next_index: next_idx,
                match_index: 0,
                last_ack_ts_ns: now_ns,
                acked_lease_sent_ts_ns: 0,
                next_pipeline_seq: 1,
            });
        }
        self.peers.retain(|k, _| members.contains(k));
        self.rebuild_quorum_tracker();
    }

    fn advance_leader_commit_index(&mut self, now_ns: u64) {
        if self.role != RaftRole::Leader {
            return;
        }
        self.quorum_tracker
            .update_match_index(self.id, self.persisted_index);
        let cfg = self.active_config().clone();
        let quorum_idx = self.quorum_tracker.committed_quorum_index(&cfg);

        // Leader must NEVER advance commit_index beyond its own durable `persisted_index`
        // (unless Leader itself is no longer in `C_new` during final step-down commit!).
        let bounded_idx = if cfg.is_voter(self.id) {
            quorum_idx.min(self.persisted_index)
        } else {
            quorum_idx
        };

        if bounded_idx > self.commit_index {
            // CRITICAL Ongaro §5.4.2 Figure 8 Rule:
            // Only commit `bounded_idx` if `log[bounded_idx].term == current_term`!
            let term_ok = if self.tuning.mutant_flags.bypass_figure8_term_check {
                true
            } else {
                self.term_at(bounded_idx) == Some(self.hard_state.current_term)
            };

            if term_ok {
                self.commit_index = bounded_idx;
                if self.commit_index >= self.term_noop_index {
                    self.has_committed_in_current_term = true;
                }
                self.apply_committed_entries(now_ns);
                // Broadcast updated `leader_commit` immediately so Followers apply in low latency!
                if self.role == RaftRole::Leader {
                    self.broadcast_append_entries(now_ns, true, 0);
                }
            }
        }

        self.try_satisfy_pending_reads(now_ns);
    }

    fn apply_committed_entries(&mut self, now_ns: u64) {
        while self.state_machine.last_applied_index < self.commit_index {
            let next_apply = self.state_machine.last_applied_index + 1;
            if !self.verify_local_entry_not_corrupted(next_apply) {
                return;
            }
            let Some(entry) = self.entry_at(next_apply).cloned() else {
                break;
            };

            match entry.kind {
                RaftEntryKind::NoOp => {
                    self.state_machine.advance_noop_or_config(next_apply);
                }
                RaftEntryKind::JournalBatch(ref cmds) => {
                    let receipts = self.state_machine.apply_journal_batch(next_apply, cmds);
                    if let Some(prop_ids) = self.inflight_proposals.remove(&next_apply) {
                        for pid in prop_ids {
                            self.outbox.push(RaftEffect::ProposalCommitted {
                                client_proposal_id: pid,
                                log_index: next_apply,
                                term: entry.term,
                                entry_blake3: entry.wal_blake3,
                                receipts: receipts.clone(),
                            });
                        }
                    }
                }
                RaftEntryKind::ConfigChange(ref cfg) => {
                    self.state_machine.advance_noop_or_config(next_apply);
                    self.outbox.push(RaftEffect::MembershipChanged {
                        log_index: next_apply,
                        config: cfg.clone(),
                        committed: true,
                    });

                    // Phase 2 of Joint Consensus: if Leader just committed `C_old,new`,
                    // automatically append `C_new`!
                    if self.role == RaftRole::Leader && cfg.is_joint() {
                        let final_cfg = cfg.into_final_uniform();
                        let final_entry = self.append_local_entry(
                            now_ns,
                            RaftEntryKind::ConfigChange(final_cfg.clone()),
                        );
                        self.sync_leader_peers_with_active_config(now_ns);
                        self.outbox.push(RaftEffect::MembershipChanged {
                            log_index: final_entry.index,
                            config: final_cfg,
                            committed: false,
                        });
                        self.emit_persist(None, None, vec![final_entry], None, Vec::new());
                    } else if !cfg.is_joint() && !cfg.is_voter(self.id) {
                        // Uniform `C_new` committed and we are no longer a voter -> graceful step down!
                        let cur_term = self.hard_state.current_term;
                        self.step_down_to_follower(cur_term, None, now_ns, false);
                    }
                }
            }
        }

        // Automatic log compaction check (`HYPSNP01`) when log grows past threshold!
        let uncompacted_committed = self
            .state_machine
            .last_applied_index
            .saturating_sub(self.snapshot_meta.last_included_index) as usize;
        if self.tuning.compaction_threshold_entries > 0
            && uncompacted_committed >= self.tuning.compaction_threshold_entries
        {
            let _ = self.compact_log_up_to(self.state_machine.last_applied_index, now_ns);
        }
    }

    /// Create a cryptographic `HYPSNP01` snapshot at `compact_to_index` and compact log prefix!
    pub fn compact_log_up_to(
        &mut self,
        compact_to_index: LogIndex,
        now_ns: u64,
    ) -> Result<SnapshotMetadata, String> {
        if compact_to_index <= self.snapshot_meta.last_included_index {
            return Err("compact_to_index already compacted".to_string());
        }
        if compact_to_index > self.state_machine.last_applied_index {
            return Err("cannot compact unapplied entries".to_string());
        }

        let Some(compact_term) = self.term_at(compact_to_index) else {
            return Err("missing term at compact_to_index".to_string());
        };
        let Some(compact_blake3) = self.blake3_at(compact_to_index) else {
            return Err("missing blake3 at compact_to_index".to_string());
        };

        // Determine effective config at `compact_to_index`.
        let cfg_at_snap = self
            .config_history
            .iter()
            .rev()
            .find(|(idx, _)| *idx <= compact_to_index)
            .map(|(_, c)| c.clone())
            .unwrap_or_else(|| self.active_config().clone());

        let meta = SnapshotMetadata {
            last_included_index: compact_to_index,
            last_included_term: compact_term,
            ts_ns: now_ns,
            last_wal_blake3: compact_blake3,
            state_root_blake3: self.state_machine.state_root_blake3(),
            config: cfg_at_snap.clone(),
        };
        let sm_bytes = self.state_machine.serialize_deterministic();
        let artifact = encode_hypsnp01_snapshot(&meta, sm_bytes);

        let drain_count = (compact_to_index - self.snapshot_meta.last_included_index) as usize;
        self.log.drain(0..drain_count);
        self.snapshot_meta = meta.clone();
        self.hard_state.compacted_index = meta.last_included_index;
        self.hard_state.compacted_term = meta.last_included_term;

        // Prune config_history entries superseded by the snapshot.
        self.config_history
            .retain(|(idx, _)| *idx > compact_to_index);
        self.config_history.insert(0, (compact_to_index, cfg_at_snap));

        self.latest_snapshot = Some(artifact.clone());
        self.emit_persist(
            Some(self.hard_state),
            None,
            Vec::new(),
            Some(artifact),
            Vec::new(),
        );
        Ok(meta)
    }

    fn try_satisfy_pending_reads(&mut self, now_ns: u64) {
        if self.role != RaftRole::Leader || !self.has_committed_in_current_term {
            return;
        }
        let mut remaining = VecDeque::new();
        while let Some(rb) = self.pending_reads.pop_front() {
            let quorum_ok = self.has_quorum_with_mutants(&rb.acked_voters);
            let applied_ok = self.state_machine.last_applied_index >= rb.required_read_index;
            if quorum_ok && applied_ok {
                let mut mask = 0u64;
                for &v in &rb.acked_voters {
                    if v < 64 {
                        mask |= 1u64 << v;
                    }
                }
                let result = self.state_machine.execute_read(&rb.query);
                self.outbox.push(RaftEffect::LinearizableReadReady {
                    client_read_id: rb.client_read_id,
                    term: self.hard_state.current_term,
                    leader_id: self.id,
                    required_read_index: rb.required_read_index,
                    served_via_leader_lease: false,
                    quorum_ack_mask: mask,
                    latency_ns: now_ns.saturating_sub(rb.created_ts_ns),
                    result,
                });
            } else {
                remaining.push_back(rb);
            }
        }
        self.pending_reads = remaining;
    }

    fn broadcast_append_entries(&mut self, now_ns: u64, force_heartbeat: bool, read_ctx_seq: u64) {
        self.last_heartbeat_sent_ns = now_ns;
        if self.active_config().all_voters().len() == 1 && self.active_config().is_voter(self.id) {
            self.recompute_leader_lease_horizon();
        }
        let peer_ids: Vec<NodeId> = self.peers.keys().copied().collect();
        for peer_id in peer_ids {
            self.send_append_to_peer(peer_id, now_ns, force_heartbeat, read_ctx_seq);
        }
    }

    fn send_append_to_peer(
        &mut self,
        peer_id: NodeId,
        now_ns: u64,
        force_heartbeat: bool,
        read_ctx_seq: u64,
    ) {
        let Some(prog) = self.peers.get(&peer_id).cloned() else {
            return;
        };

        // If peer needs entries prior to `first_log_index()`, stream `InstallSnapshot`!
        if prog.next_index < self.first_log_index() {
            let chunk_idx = match prog.mode {
                PeerReplicationMode::Snapshot { next_chunk_index } => next_chunk_index,
                _ => 0,
            };
            if let Some(p) = self.peers.get_mut(&peer_id) {
                p.mode = PeerReplicationMode::Snapshot {
                    next_chunk_index: chunk_idx,
                };
            }
            self.send_snapshot_chunk_to_peer(peer_id, chunk_idx);
            // If read_ctx_seq > 0, also send an empty heartbeat or let snapshot response ack leadership
            if read_ctx_seq > 0 {
                let prev_idx = self.snapshot_meta.last_included_index;
                let prev_term = self.snapshot_meta.last_included_term;
                let prev_blake3 = self.snapshot_meta.last_wal_blake3;
                self.emit_rpc(
                    peer_id,
                    RaftRpc::AppendEntriesRequest {
                        term: self.hard_state.current_term,
                        leader_id: self.id,
                        prev_log_index: prev_idx,
                        prev_log_term: prev_term,
                        prev_wal_blake3: prev_blake3,
                        entries: Vec::new(),
                        leader_commit: self.commit_index,
                        read_ctx_seq,
                        leader_sent_ts_ns: now_ns,
                    },
                );
            }
            return;
        }

        // Optimistic Pipelining Loop in `Replicate` mode; single-packet in `Probe` mode!
        let max_loops = match prog.mode {
            PeerReplicationMode::Probe { .. } => 1,
            PeerReplicationMode::Replicate { inflight_batches } => self
                .tuning
                .max_inflight_pipelines
                .saturating_sub(inflight_batches)
                .max(usize::from(force_heartbeat)),
            PeerReplicationMode::Snapshot { .. } => usize::from(force_heartbeat),
        };

        for step in 0..max_loops {
            let Some(cur_prog) = self.peers.get(&peer_id).cloned() else {
                break;
            };
            if cur_prog.next_index < self.first_log_index() {
                break;
            }
            let has_unsent = cur_prog.next_index <= self.last_log_index();
            if !has_unsent && !(force_heartbeat && step == 0) {
                break;
            }

            let prev_idx = cur_prog.next_index - 1;
            if !self.verify_local_entry_not_corrupted(prev_idx) {
                return;
            }
            let prev_term = self.term_at(prev_idx).unwrap_or(0);
            let prev_blake3 = self.blake3_at(prev_idx).unwrap_or([0u8; 32]);

            let mut batch = Vec::new();
            if has_unsent {
                let end_idx = (cur_prog.next_index + self.tuning.max_batch_entries as u64 - 1)
                    .min(self.last_log_index());
                for idx in cur_prog.next_index..=end_idx {
                    if !self.verify_local_entry_not_corrupted(idx) {
                        return;
                    }
                    if let Some(e) = self.entry_at(idx) {
                        batch.push(e.clone());
                    }
                }
            }

            let batch_len = batch.len() as u64;
            if let Some(p) = self.peers.get_mut(&peer_id) {
                p.next_pipeline_seq = p.next_pipeline_seq.saturating_add(1);
                match p.mode {
                    PeerReplicationMode::Probe {
                        ref mut probe_inflight,
                    } => {
                        *probe_inflight = true;
                    }
                    PeerReplicationMode::Replicate {
                        ref mut inflight_batches,
                    } => {
                        if batch_len > 0 {
                            p.next_index += batch_len;
                            *inflight_batches += 1;
                        }
                    }
                    PeerReplicationMode::Snapshot { .. } => {}
                }
            }

            self.emit_rpc(
                peer_id,
                RaftRpc::AppendEntriesRequest {
                    term: self.hard_state.current_term,
                    leader_id: self.id,
                    prev_log_index: prev_idx,
                    prev_log_term: prev_term,
                    prev_wal_blake3: prev_blake3,
                    entries: batch,
                    leader_commit: self.commit_index,
                    read_ctx_seq: if step == 0 { read_ctx_seq } else { 0 },
                    leader_sent_ts_ns: now_ns,
                },
            );

            if !has_unsent || matches!(cur_prog.mode, PeerReplicationMode::Probe { .. }) {
                break;
            }
        }
    }

    fn send_snapshot_chunk_to_peer(&mut self, peer_id: NodeId, chunk_index: u32) {
        let Some(ref snap) = self.latest_snapshot else {
            return;
        };
        let raw = &snap.raw_hypsnp_bytes;
        let chunk_sz = self.tuning.snapshot_chunk_bytes.max(64);
        let total_chunks = raw.len().div_ceil(chunk_sz).max(1) as u32;
        let idx = chunk_index.min(total_chunks - 1);
        let start = (idx as usize) * chunk_sz;
        let end = (start + chunk_sz).min(raw.len());
        let slice = raw[start..end].to_vec();
        let chunk_crc = crc32c::crc32c(&slice);
        let done = (idx + 1) >= total_chunks;

        self.emit_rpc(
            peer_id,
            RaftRpc::InstallSnapshotRequest {
                term: self.hard_state.current_term,
                leader_id: self.id,
                meta: snap.meta.clone(),
                offset: start as u64,
                chunk_index: idx,
                total_chunks,
                data: slice,
                chunk_crc32c: chunk_crc,
                done,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_install_snapshot(
        &mut self,
        from: NodeId,
        term: Term,
        leader_id: NodeId,
        meta: SnapshotMetadata,
        offset: u64,
        _chunk_index: u32,
        _total_chunks: u32,
        data: Vec<u8>,
        chunk_crc32c: u32,
        done: bool,
        now_ns: u64,
    ) {
        if term < self.hard_state.current_term {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: 0,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        }

        let mut hs_changed = false;
        if term > self.hard_state.current_term
            || self.role == RaftRole::PreCandidate
            || self.role == RaftRole::Candidate
        {
            if term > self.hard_state.current_term {
                self.hard_state.current_term = term;
                self.hard_state.voted_for = None;
                hs_changed = true;
            }
            self.role = if self.active_config().is_learner(self.id) {
                RaftRole::Learner
            } else {
                RaftRole::Follower
            };
        }

        self.leader_id = Some(leader_id);
        self.last_valid_leader_contact_ns = Some(now_ns);
        self.reset_election_timer(now_ns);

        // Obsolete snapshot older than our commit_index -> ack current commit_index!
        if meta.last_included_index <= self.commit_index {
            let reply = self.build_envelope(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: true,
                    next_expected_offset: 0,
                    stored_up_to_index: self.commit_index,
                },
            );
            if hs_changed {
                self.emit_persist(Some(self.hard_state), None, Vec::new(), None, vec![reply]);
            } else {
                self.outbox.push(RaftEffect::SendRpc(reply));
            }
            return;
        }

        // Verify per-chunk Castagnoli CRC-32C!
        if !self.tuning.mutant_flags.bypass_crc32c_verification
            && crc32c::crc32c(&data) != chunk_crc32c
        {
            let expected_off = self
                .staging_snapshot
                .as_ref()
                .map(|s| s.assembled_bytes.len() as u64)
                .unwrap_or(0);
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: expected_off,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        }

        if offset == 0 {
            self.staging_snapshot = Some(StagingSnapshotBuffer {
                meta: meta.clone(),
                assembled_bytes: Vec::new(),
            });
        }

        let Some(ref mut staging) = self.staging_snapshot else {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: 0,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        };

        if staging.meta.last_included_index != meta.last_included_index
            || (staging.assembled_bytes.len() as u64) != offset
        {
            let expected_off = if staging.meta.last_included_index == meta.last_included_index {
                staging.assembled_bytes.len() as u64
            } else {
                0
            };
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: expected_off,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        }

        staging.assembled_bytes.extend_from_slice(&data);
        let next_off = staging.assembled_bytes.len() as u64;

        if !done {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: true,
                    next_expected_offset: next_off,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        }

        // Full snapshot received! Verify `HYPSNP01` header, CRC32C, and `state_root_blake3`!
        let assembled = self.staging_snapshot.take().expect("staging present");
        let Ok((verified_meta, sm_bytes)) =
            decode_and_verify_hypsnp01_snapshot(&assembled.assembled_bytes)
        else {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: 0,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        };
        let Ok(restored_sm) = ReplicatedLedgerStateMachine::deserialize_deterministic(&sm_bytes)
        else {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: 0,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        };
        if restored_sm.state_root_blake3() != verified_meta.state_root_blake3 {
            self.emit_rpc(
                from,
                RaftRpc::InstallSnapshotResponse {
                    term: self.hard_state.current_term,
                    accepted: false,
                    next_expected_offset: 0,
                    stored_up_to_index: self.persisted_index,
                },
            );
            return;
        }

        // Preserve any matching suffix after `verified_meta.last_included_index`, otherwise clear log!
        let snap_idx = verified_meta.last_included_index;
        let retain_suffix = self
            .entry_at(snap_idx)
            .map(|e| {
                e.term == verified_meta.last_included_term
                    && e.wal_blake3 == verified_meta.last_wal_blake3
            })
            .unwrap_or(false);

        if retain_suffix {
            let drain_n = (snap_idx - self.snapshot_meta.last_included_index) as usize;
            self.log.drain(0..drain_n);
        } else {
            self.log.clear();
        }

        self.snapshot_meta = verified_meta.clone();
        self.hard_state.compacted_index = snap_idx;
        self.hard_state.compacted_term = verified_meta.last_included_term;
        if self.state_machine.last_applied_index < snap_idx || !retain_suffix {
            self.state_machine = restored_sm;
        }
        self.commit_index = self.commit_index.max(snap_idx);
        self.pending_follower_commit_cap = self.pending_follower_commit_cap.max(snap_idx);
        self.persisted_index = self.persisted_index.max(snap_idx);

        self.config_history.retain(|(idx, _)| *idx > snap_idx);
        self.config_history
            .insert(0, (snap_idx, verified_meta.config.clone()));
        for e in &self.log {
            if let RaftEntryKind::ConfigChange(ref c) = e.kind {
                if !self.config_history.iter().any(|(i, _)| *i == e.index) {
                    self.config_history.push((e.index, c.clone()));
                }
            }
        }
        self.role = if self.active_config().is_learner(self.id) {
            RaftRole::Learner
        } else {
            RaftRole::Follower
        };

        let artifact = SnapshotArtifact {
            meta: verified_meta,
            raw_hypsnp_bytes: assembled.assembled_bytes,
        };
        self.latest_snapshot = Some(artifact.clone());

        let reply = self.build_envelope(
            from,
            RaftRpc::InstallSnapshotResponse {
                term: self.hard_state.current_term,
                accepted: true,
                next_expected_offset: next_off,
                stored_up_to_index: snap_idx,
            },
        );
        self.emit_persist(
            Some(self.hard_state),
            None,
            Vec::new(),
            Some(artifact),
            vec![reply],
        );
    }

    fn handle_install_snapshot_response(
        &mut self,
        from: NodeId,
        term: Term,
        accepted: bool,
        next_expected_offset: u64,
        stored_up_to_index: LogIndex,
        now_ns: u64,
    ) {
        if term > self.hard_state.current_term {
            self.step_down_to_follower(term, None, now_ns, true);
            return;
        }
        if self.role != RaftRole::Leader || term != self.hard_state.current_term {
            return;
        }
        if let Some(prog) = self.peers.get_mut(&from) {
            prog.last_ack_ts_ns = now_ns;
        }

        let Some(ref snap) = self.latest_snapshot else {
            return;
        };
        let snap_idx = snap.meta.last_included_index;
        let chunk_sz = self.tuning.snapshot_chunk_bytes.max(64) as u64;

        if accepted && stored_up_to_index >= snap_idx {
            if let Some(prog) = self.peers.get_mut(&from) {
                prog.match_index = prog.match_index.max(stored_up_to_index);
                prog.next_index = prog.match_index + 1;
                prog.mode = PeerReplicationMode::Replicate {
                    inflight_batches: 0,
                };
            }
            self.quorum_tracker
                .update_match_index(from, stored_up_to_index);
            self.advance_leader_commit_index(now_ns);
            self.send_append_to_peer(from, now_ns, true, 0);
        } else {
            let next_chunk = (next_expected_offset / chunk_sz) as u32;
            if let Some(prog) = self.peers.get_mut(&from) {
                prog.mode = PeerReplicationMode::Snapshot {
                    next_chunk_index: next_chunk,
                };
            }
            self.send_snapshot_chunk_to_peer(from, next_chunk);
        }
    }

    fn append_local_entry(&mut self, ts_ns: u64, kind: RaftEntryKind) -> LogEntry {
        let index = self.last_log_index() + 1;
        let term = self.hard_state.current_term;
        let prev_blake3 = self.last_wal_blake3();
        if let RaftEntryKind::ConfigChange(ref cfg) = kind {
            self.config_history.push((index, cfg.clone()));
        }
        let entry = build_chained_log_entry(index, term, ts_ns, prev_blake3, kind);
        self.log.push(entry.clone());
        entry
    }

    fn build_envelope(&mut self, to: NodeId, rpc: RaftRpc) -> NetworkEnvelope {
        let id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.wrapping_add(1);
        NetworkEnvelope {
            from: self.id,
            to,
            msg_id: id,
            rpc,
        }
    }

    fn emit_rpc(&mut self, to: NodeId, rpc: RaftRpc) {
        let env = self.build_envelope(to, rpc);
        self.outbox.push(RaftEffect::SendRpc(env));
    }

    fn emit_persist(
        &mut self,
        hard_state: Option<HardState>,
        truncate_from_index: Option<LogIndex>,
        entries: Vec<LogEntry>,
        install_snapshot: Option<SnapshotArtifact>,
        post_sync_rpcs: Vec<NetworkEnvelope>,
    ) -> u64 {
        let io_id = self.next_io_id;
        self.next_io_id = self.next_io_id.wrapping_add(1);
        self.pending_io_targets
            .insert(io_id, (self.log_epoch, self.last_log_index()));
        if !post_sync_rpcs.is_empty() {
            self.deferred_post_sync
                .insert(io_id, (self.log_epoch, post_sync_rpcs));
        }
        self.outbox.push(RaftEffect::Persist(PersistBatch {
            io_id,
            hard_state,
            truncate_from_index,
            entries,
            install_snapshot,
        }));
        io_id
    }
}
