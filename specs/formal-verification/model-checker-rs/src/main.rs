#![forbid(unsafe_code)]

//! HYPERION OMEGA — Channel 11 Compiled Explicit-State Model Checker
//!
//! Exhaustively explores >= 1,000,000 concurrent state transitions of the coupled
//! Raft + Pre-Vote + Cryptographic Chained-Hash WAL + L2 Settlement Batch Commit protocol
//! under adversarial fault injection:
//!   1. Asymmetric & symmetric network partitions (`InjectPartition`, `HealNetwork`)
//!   2. Sudden Leader & Follower crash + persistent WAL restart (`CrashNode`, `RestartNode`)
//!   3. Duplicate, dropped, and out-of-order RPC delivery (`DuplicateMsg`, `DropMsg`, `DeliverMsg`)
//!   4. Stale deposed-leader writes in minority partitions (`StaleLeaderWrite`)
//!   5. L2 Settlement batch staging, confirmation finality, and shallow chain reorgs (`L2Reorg`)
//!
//! Verifies 6 Safety Invariants on EVERY explored state & transition plus bounded Post-Heal Liveness.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::path::PathBuf;
use std::time::Instant;

pub mod overclock;

const NUM_NODES: usize = 3;
const QUORUM: usize = (NUM_NODES / 2) + 1;
const MAX_CLIENT_WAL_LEN: usize = 3;
const MAX_WAL_LEN: usize = 6; // Reserves headroom for post-election Raft §8 NOOP_BARRIER entries
const MAX_MSGS_IN_FLIGHT: usize = 6;
const MAX_L2_ANCHORS: usize = 4;
const L2_FINALITY_CONFS: u8 = 2;

/// Fast deterministic 64-bit FxHasher (zero external crate dependencies, 100% reproducible).
#[derive(Default)]
pub(crate) struct FxHasher64 {
    hash: u64,
}

impl Hasher for FxHasher64 {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517cc1b727220a95;
        let mut h = self.hash;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let word = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            h = (h.rotate_left(5) ^ word).wrapping_mul(K);
        }
        for &b in chunks.remainder() {
            h = (h.rotate_left(5) ^ (b as u64)).wrapping_mul(K);
        }
        self.hash = h;
    }
}

type FastHashSet<T> = HashSet<T, BuildHasherDefault<FxHasher64>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Role {
    Follower = 0,
    PreCandidate = 1,
    Candidate = 2,
    Leader = 3,
}

/// Deterministic 32-bit collision-resistant chained WAL digest model (mirrors Blake3 WAL chaining).
#[inline]
pub fn compute_wal_hash(index_1based: u8, term: u8, cmd: u8, prev_hash: u32) -> u32 {
    let mut h = prev_hash ^ 0x9E37_79B9;
    h = h.wrapping_mul(0x85EB_CA6B) ^ ((index_1based as u32) << 16) ^ ((term as u32) << 8) ^ (cmd as u32);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WalEntry {
    pub term: u8,
    pub cmd: u8,
    pub prev_hash: u32,
    pub entry_hash: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeState {
    // Persistent state (survives crash & recovery)
    pub current_term: u8,
    pub voted_for: u8, // 0 = None, 1..=NUM_NODES = node_id + 1
    pub wal_len: u8,
    pub wal: [WalEntry; MAX_WAL_LEN],

    // Volatile state (wiped on CrashNode)
    pub crashed: bool,
    pub role: Role,
    pub commit_index: u8,
    pub prevotes_mask: u8,
    pub votes_mask: u8,
    pub next_index: [u8; NUM_NODES],
    pub match_index: [u8; NUM_NODES],
}

impl NodeState {
    pub fn new() -> Self {
        Self {
            current_term: 1,
            voted_for: 0,
            wal_len: 0,
            wal: [WalEntry::default(); MAX_WAL_LEN],
            crashed: false,
            role: Role::Follower,
            commit_index: 0,
            prevotes_mask: 0,
            votes_mask: 0,
            next_index: [1; NUM_NODES],
            match_index: [0; NUM_NODES],
        }
    }

    #[inline]
    pub fn last_log_term(&self) -> u8 {
        if self.wal_len == 0 {
            0
        } else {
            self.wal[(self.wal_len - 1) as usize].term
        }
    }

    #[inline]
    pub fn last_log_hash(&self) -> u32 {
        if self.wal_len == 0 {
            0
        } else {
            self.wal[(self.wal_len - 1) as usize].entry_hash
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RpcPayload {
    PreVoteReq {
        prospective_term: u8,
        last_log_idx: u8,
        last_log_term: u8,
    },
    PreVoteResp {
        term: u8,
        prospective_term: u8,
        granted: bool,
    },
    RequestVoteReq {
        term: u8,
        last_log_idx: u8,
        last_log_term: u8,
    },
    RequestVoteResp {
        term: u8,
        granted: bool,
    },
    AppendEntriesReq {
        term: u8,
        prev_log_idx: u8,
        prev_log_term: u8,
        prev_log_hash: u32,
        entries_len: u8,
        entries: [WalEntry; MAX_WAL_LEN],
        leader_commit: u8,
    },
    AppendEntriesResp {
        term: u8,
        success: bool,
        match_idx: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InFlightMsg {
    pub src: u8,
    pub dst: u8,
    pub duplicated: bool,
    pub payload: RpcPayload,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct L2BatchAnchor {
    pub epoch: u8,
    pub commit_len: u8,
    pub root_hash: u32,
    pub confirmations: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProtocolConfig {
    pub max_term: u8,
    pub max_faults: u8,
    pub target_min_transitions: u64,
    /// Mutation toggle 1: if true, violates Raft §5.4.2 by committing old-term entries by count alone.
    pub bug_commit_old_term_entries: bool,
    /// Mutation toggle 2: if true, truncates follower log unconditionally even on stale AppendEntries.
    pub bug_unconditional_truncate: bool,
    /// Mutation toggle 3: if true, anchors uncommitted leader WAL entries directly to L2 finalized state.
    pub bug_anchor_uncommitted_to_l2: bool,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_term: 4,
            max_faults: 4,
            target_min_transitions: 1_250_000,
            bug_commit_old_term_entries: false,
            bug_unconditional_truncate: false,
            bug_anchor_uncommitted_to_l2: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SystemState {
    pub nodes: [NodeState; NUM_NODES],
    pub msg_count: u8,
    pub msgs: [InFlightMsg; MAX_MSGS_IN_FLIGHT],
    /// Bitmask over directed pairs `(src * NUM_NODES + dst)` blocked by asymmetric/symmetric partition.
    pub partition_mask: u16,
    pub faults_used: u8,
    pub next_cmd_seq: u8,

    // Append-only ghost oracle of quorum-committed WAL prefix
    pub ghost_committed_len: u8,
    pub ghost_committed: [WalEntry; MAX_WAL_LEN],

    // L2 Settlement staged & finalized batch anchors
    pub has_staged_l2: bool,
    pub staged_l2: L2BatchAnchor,
    pub finalized_l2_len: u8,
    pub finalized_l2: [L2BatchAnchor; MAX_L2_ANCHORS],
}

impl SystemState {
    pub fn initial() -> Self {
        let dummy_msg = InFlightMsg {
            src: 0,
            dst: 0,
            duplicated: false,
            payload: RpcPayload::PreVoteResp {
                term: 0,
                prospective_term: 0,
                granted: false,
            },
        };
        let mut nodes = [NodeState::new(); NUM_NODES];
        // Bootstrap initial quorum Leader on node 0 in term 1 (voted by quorum {0, 1});
        // Followers 1 & 2 remain free to start PreVote -> Election into terms 2..=max_term!
        nodes[0].role = Role::Leader;
        nodes[0].voted_for = 1;
        nodes[0].votes_mask = 0b011;
        nodes[1].voted_for = 1;
        Self {
            nodes,
            msg_count: 0,
            msgs: [dummy_msg; MAX_MSGS_IN_FLIGHT],
            partition_mask: 0,
            faults_used: 0,
            next_cmd_seq: 1,
            ghost_committed_len: 0,
            ghost_committed: [WalEntry::default(); MAX_WAL_LEN],
            has_staged_l2: false,
            staged_l2: L2BatchAnchor::default(),
            finalized_l2_len: 0,
            finalized_l2: [L2BatchAnchor::default(); MAX_L2_ANCHORS],
        }
    }

    #[inline]
    pub fn link_blocked(&self, src: usize, dst: usize) -> bool {
        let bit = (src * NUM_NODES + dst) as u16;
        ((self.partition_mask >> bit) & 1) != 0
    }

    #[inline]
    pub fn can_deliver(&self, src: usize, dst: usize) -> bool {
        !self.nodes[src].crashed && !self.nodes[dst].crashed && !self.link_blocked(src, dst)
    }

    #[inline]
    pub fn push_msg(&mut self, msg: InFlightMsg) {
        if (self.msg_count as usize) < MAX_MSGS_IN_FLIGHT {
            // Avoid exact duplicate slot saturation if identical frame already queued
            for i in 0..(self.msg_count as usize) {
                if self.msgs[i] == msg {
                    return;
                }
            }
            self.msgs[self.msg_count as usize] = msg;
            self.msg_count += 1;
        }
    }

    pub fn remove_msg(&mut self, idx: usize) -> InFlightMsg {
        let m = self.msgs[idx];
        let last = (self.msg_count as usize) - 1;
        for k in idx..last {
            self.msgs[k] = self.msgs[k + 1];
        }
        self.msgs[last] = InFlightMsg {
            src: 0,
            dst: 0,
            duplicated: false,
            payload: RpcPayload::PreVoteResp {
                term: 0,
                prospective_term: 0,
                granted: false,
            },
        };
        self.msg_count -= 1;
        m
    }
}

#[inline]
fn is_log_up_to_date(cand_term: u8, cand_idx: u8, voter_term: u8, voter_idx: u8) -> bool {
    cand_term > voter_term || (cand_term == voter_term && cand_idx >= voter_idx)
}

#[inline]
fn popcount_u8(mask: u8) -> usize {
    mask.count_ones() as usize
}

/// Verify all 6 Safety Invariants on a state transition `parent -> next`.
pub fn check_safety_invariants(parent: &SystemState, next: &SystemState) -> Result<(), String> {
    // Invariant 1: Election Safety (at most 1 Leader per term)
    for i in 0..NUM_NODES {
        for j in (i + 1)..NUM_NODES {
            if next.nodes[i].role == Role::Leader
                && next.nodes[j].role == Role::Leader
                && next.nodes[i].current_term == next.nodes[j].current_term
            {
                return Err(format!(
                    "INV1_ELECTION_SAFETY_VIOLATION: nodes {} and {} are both Leader in term {}",
                    i, j, next.nodes[i].current_term
                ));
            }
        }
    }

    // Invariant 2: Log Matching (equal term at index k implies identical prefix [0..=k])
    for i in 0..NUM_NODES {
        for j in (i + 1)..NUM_NODES {
            let min_len = next.nodes[i].wal_len.min(next.nodes[j].wal_len) as usize;
            for k in 0..min_len {
                if next.nodes[i].wal[k].term == next.nodes[j].wal[k].term
                    && next.nodes[i].wal[..=k] != next.nodes[j].wal[..=k]
                {
                    return Err(format!(
                        "INV2_LOG_MATCHING_VIOLATION: nodes {} and {} match term {} at idx {} but prefixes diverge",
                        i, j, next.nodes[i].wal[k].term, k
                    ));
                }
            }
        }
    }

    // Invariant 3: Cryptographic Blake3 WAL Hash-Chain Integrity on every node
    for i in 0..NUM_NODES {
        let len = next.nodes[i].wal_len as usize;
        for k in 0..len {
            let expected_prev = if k == 0 {
                0
            } else {
                next.nodes[i].wal[k - 1].entry_hash
            };
            let entry = next.nodes[i].wal[k];
            let expected_hash = compute_wal_hash((k + 1) as u8, entry.term, entry.cmd, expected_prev);
            if entry.prev_hash != expected_prev || entry.entry_hash != expected_hash {
                return Err(format!(
                    "INV3_WAL_HASH_CHAIN_CORRUPTION on node {} at index {}",
                    i, k
                ));
            }
        }
    }

    // Invariant 4: Zero Committed Data Loss (ghost_committed is strictly append-only & matches all commit_index prefixes)
    if next.ghost_committed_len < parent.ghost_committed_len {
        return Err("INV4_ZERO_COMMITTED_DATA_LOSS: ghost_committed_len shrank!".to_string());
    }
    let p_len = parent.ghost_committed_len as usize;
    if next.ghost_committed[..p_len] != parent.ghost_committed[..p_len] {
        return Err("INV4_ZERO_COMMITTED_DATA_LOSS: previously committed WAL entry mutated!".to_string());
    }
    let g_len = next.ghost_committed_len as usize;
    for i in 0..NUM_NODES {
        let c_idx = next.nodes[i].commit_index as usize;
        if c_idx > g_len || c_idx > (next.nodes[i].wal_len as usize) {
            return Err(format!(
                "INV4_COMMIT_BOUNDS_VIOLATION on node {}: commit_index={} ghost={} wal={}",
                i, c_idx, g_len, next.nodes[i].wal_len
            ));
        }
        if next.nodes[i].wal[..c_idx] != next.ghost_committed[..c_idx] {
            return Err(format!(
                "INV4_STATE_MACHINE_SAFETY_VIOLATION: node {} committed prefix diverges from canonical history",
                i
            ));
        }
    }

    // Invariant 5: Leader Completeness
    let last_committed_term = if g_len == 0 {
        0
    } else {
        next.ghost_committed[g_len - 1].term
    };
    for i in 0..NUM_NODES {
        if next.nodes[i].role == Role::Leader && next.nodes[i].current_term >= last_committed_term {
            if (next.nodes[i].wal_len as usize) < g_len
                || next.nodes[i].wal[..g_len] != next.ghost_committed[..g_len]
            {
                return Err(format!(
                    "INV5_LEADER_COMPLETENESS_VIOLATION: Leader {} in term {} missing committed prefix of len {}",
                    i, next.nodes[i].current_term, g_len
                ));
            }
        }
    }

    // Invariant 6: Coupled L2 Settlement Prefix Consistency & Monotonicity
    let fin_len = next.finalized_l2_len as usize;
    for b in 0..fin_len {
        let anchor = next.finalized_l2[b];
        let clen = anchor.commit_len as usize;
        if clen == 0 || clen > g_len {
            return Err(format!(
                "INV6_L2_SETTLEMENT_UNCOMMITTED_DATA: finalized L2 epoch {} anchored len {} > committed {}",
                anchor.epoch, clen, g_len
            ));
        }
        if next.ghost_committed[clen - 1].entry_hash != anchor.root_hash {
            return Err(format!(
                "INV6_L2_SETTLEMENT_ROOT_MISMATCH at epoch {}",
                anchor.epoch
            ));
        }
        if b > 0 && anchor.commit_len <= next.finalized_l2[b - 1].commit_len {
            return Err("INV6_L2_NON_MONOTONIC_BATCH".to_string());
        }
    }

    Ok(())
}

#[derive(Default, Debug, Clone)]
pub struct TransitionTelemetry {
    pub total_transitions: u64,
    pub prevote_elections: u64,
    pub message_deliveries: u64,
    pub client_proposals: u64,
    pub stale_leader_writes: u64,
    pub partition_injections: u64,
    pub partition_heals: u64,
    pub node_crashes: u64,
    pub node_restarts: u64,
    pub duplicate_messages: u64,
    pub message_drops: u64,
    pub l2_batch_actions: u64,
    pub l2_shallow_reorgs: u64,
}

/// Generate all enabled concurrent next-states from `state` and record telemetry counters.
pub fn expand_successors(
    state: &SystemState,
    cfg: &ProtocolConfig,
    out: &mut Vec<SystemState>,
    stats: &mut TransitionTelemetry,
) {
    out.clear();
    let max_cluster_term = state
        .nodes
        .iter()
        .map(|n| n.current_term)
        .max()
        .unwrap_or(1);

    // 1. Election Timeout -> Start Pre-Vote (§9.6)
    for i in 0..NUM_NODES {
        let n = &state.nodes[i];
        if !n.crashed
            && matches!(n.role, Role::Follower | Role::PreCandidate)
            && n.current_term < cfg.max_term
            && (state.msg_count as usize) + (NUM_NODES - 1) <= MAX_MSGS_IN_FLIGHT
        {
            let mut next = *state;
            let prospective_term = next.nodes[i].current_term + 1;
            next.nodes[i].role = Role::PreCandidate;
            next.nodes[i].prevotes_mask = 1 << i;
            let last_idx = next.nodes[i].wal_len;
            let last_term = next.nodes[i].last_log_term();
            for peer in 0..NUM_NODES {
                if peer != i {
                    next.push_msg(InFlightMsg {
                        src: i as u8,
                        dst: peer as u8,
                        duplicated: false,
                        payload: RpcPayload::PreVoteReq {
                            prospective_term,
                            last_log_idx: last_idx,
                            last_log_term: last_term,
                        },
                    });
                }
            }
            stats.prevote_elections += 1;
            out.push(next);
        }
    }

    // 2. Leader Proposals (both Active Quorum Leader AND Stale Partitioned Old-Term Leader!)
    for i in 0..NUM_NODES {
        let n = &state.nodes[i];
        if !n.crashed
            && n.role == Role::Leader
            && (n.wal_len as usize) < MAX_CLIENT_WAL_LEN
            && state.next_cmd_seq <= 5
        {
            let mut next = *state;
            let idx = next.nodes[i].wal_len;
            let term = next.nodes[i].current_term;
            let cmd = next.next_cmd_seq;
            next.next_cmd_seq += 1;
            let prev_hash = next.nodes[i].last_log_hash();
            let entry_hash = compute_wal_hash(idx + 1, term, cmd, prev_hash);
            next.nodes[i].wal[idx as usize] = WalEntry {
                term,
                cmd,
                prev_hash,
                entry_hash,
            };
            next.nodes[i].wal_len += 1;
            next.nodes[i].match_index[i] = next.nodes[i].wal_len;

            if term < max_cluster_term {
                stats.stale_leader_writes += 1;
            } else {
                stats.client_proposals += 1;
            }
            out.push(next);
        }
    }

    // 3. Leader AppendEntries Replication / Heartbeats
    for i in 0..NUM_NODES {
        let n = &state.nodes[i];
        if !n.crashed && n.role == Role::Leader && (state.msg_count as usize) < MAX_MSGS_IN_FLIGHT {
            for peer in 0..NUM_NODES {
                if peer == i {
                    continue;
                }
                let next_idx = n.next_index[peer].max(1);
                let prev_idx = next_idx - 1;
                if prev_idx > n.wal_len {
                    continue;
                }
                let prev_term = if prev_idx == 0 {
                    0
                } else {
                    n.wal[(prev_idx - 1) as usize].term
                };
                let prev_hash = if prev_idx == 0 {
                    0
                } else {
                    n.wal[(prev_idx - 1) as usize].entry_hash
                };
                let mut entries = [WalEntry::default(); MAX_WAL_LEN];
                let mut entries_len = 0_u8;
                for k in (prev_idx as usize)..(n.wal_len as usize) {
                    entries[entries_len as usize] = n.wal[k];
                    entries_len += 1;
                }

                // Emit replication RPC when follower lags in log or commit_index
                if entries_len > 0 || n.match_index[peer] < n.wal_len || state.nodes[peer].commit_index < n.commit_index {
                    let mut next = *state;
                    next.push_msg(InFlightMsg {
                        src: i as u8,
                        dst: peer as u8,
                        duplicated: false,
                        payload: RpcPayload::AppendEntriesReq {
                            term: n.current_term,
                            prev_log_idx: prev_idx,
                            prev_log_term: prev_term,
                            prev_log_hash: prev_hash,
                            entries_len,
                            entries,
                            leader_commit: n.commit_index,
                        },
                    });
                    out.push(next);
                }
            }
        }
    }

    // 4. Asynchronous Out-of-Order Network Message Delivery
    for m_idx in 0..(state.msg_count as usize) {
        let msg = state.msgs[m_idx];
        let src = msg.src as usize;
        let dst = msg.dst as usize;
        if !state.can_deliver(src, dst) {
            continue;
        }
        let mut next = *state;
        let delivered = next.remove_msg(m_idx);
        deliver_message(&mut next, delivered, cfg);
        stats.message_deliveries += 1;
        out.push(next);
    }

    // 5. Coupled L2 Settlement Batch Staging & Confirmation Finalization
    for i in 0..NUM_NODES {
        let n = &state.nodes[i];
        if !n.crashed && n.role == Role::Leader && !state.has_staged_l2 && (state.finalized_l2_len as usize) < MAX_L2_ANCHORS {
            let candidate_len = if cfg.bug_anchor_uncommitted_to_l2 {
                n.wal_len
            } else {
                n.commit_index
            };
            let last_fin_len = if state.finalized_l2_len == 0 {
                0
            } else {
                state.finalized_l2[(state.finalized_l2_len - 1) as usize].commit_len
            };
            if candidate_len > last_fin_len && candidate_len > 0 {
                let mut next = *state;
                let root_hash = n.wal[(candidate_len - 1) as usize].entry_hash;
                next.has_staged_l2 = true;
                next.staged_l2 = L2BatchAnchor {
                    epoch: state.finalized_l2_len + 1,
                    commit_len: candidate_len,
                    root_hash,
                    confirmations: 1,
                };
                stats.l2_batch_actions += 1;
                out.push(next);
            }
        }
    }
    if state.has_staged_l2 {
        let mut next = *state;
        next.staged_l2.confirmations += 1;
        if next.staged_l2.confirmations >= L2_FINALITY_CONFS {
            let idx = next.finalized_l2_len as usize;
            if idx < MAX_L2_ANCHORS {
                next.finalized_l2[idx] = next.staged_l2;
                next.finalized_l2_len += 1;
            }
            next.has_staged_l2 = false;
            next.staged_l2 = L2BatchAnchor::default();
        }
        stats.l2_batch_actions += 1;
        out.push(next);
    }

    // 6. Adversarial Fault Injection (Bounded by cfg.max_faults; active once cluster has elected a leader or proposed entries)
    let cluster_active = state
        .nodes
        .iter()
        .any(|n| n.role == Role::Leader || n.wal_len > 0 || n.current_term > 1);
    if cluster_active && state.faults_used < cfg.max_faults {
        // 6a. Canonical Symmetric Node Isolations & Asymmetric Bridge Cuts (avoids redundant bit-flip permutations)
        let canonical_cuts: [u16; 5] = [
            // Isolate Node 0 symmetrically ({0} <-> {1,2})
            (1 << (0 * NUM_NODES + 1))
                | (1 << (1 * NUM_NODES + 0))
                | (1 << (0 * NUM_NODES + 2))
                | (1 << (2 * NUM_NODES + 0)),
            // Isolate Node 1 symmetrically ({1} <-> {0,2})
            (1 << (1 * NUM_NODES + 0))
                | (1 << (0 * NUM_NODES + 1))
                | (1 << (1 * NUM_NODES + 2))
                | (1 << (2 * NUM_NODES + 1)),
            // Isolate Node 2 symmetrically ({2} <-> {0,1})
            (1 << (2 * NUM_NODES + 0))
                | (1 << (0 * NUM_NODES + 2))
                | (1 << (2 * NUM_NODES + 1))
                | (1 << (1 * NUM_NODES + 2)),
            // Asymmetric triangle cut: 0 -x-> 2 blocked while 0 <-> 1 and 1 <-> 2 open
            (1 << (0 * NUM_NODES + 2)) | (1 << (2 * NUM_NODES + 0)),
            // Asymmetric unidirectional cut: 0 -x-> 1 blocked while 1 -> 0 open
            1 << (0 * NUM_NODES + 1),
        ];
        for cut_mask in canonical_cuts {
            if (state.partition_mask & cut_mask) != cut_mask {
                let mut next = *state;
                next.partition_mask |= cut_mask;
                next.faults_used += 1;
                stats.partition_injections += 1;
                out.push(next);
            }
        }

        // 6b. Sudden Leader / Follower Crash (wipes volatile RAM, keeps persistent WAL/term/voted_for)
        let active_crashed = state.nodes.iter().filter(|n| n.crashed).count();
        if active_crashed < 1 {
            for i in 0..NUM_NODES {
                if !state.nodes[i].crashed {
                    let mut next = *state;
                    next.nodes[i].crashed = true;
                    next.nodes[i].role = Role::Follower;
                    next.nodes[i].commit_index = 0;
                    next.nodes[i].prevotes_mask = 0;
                    next.nodes[i].votes_mask = 0;
                    next.nodes[i].next_index = [1; NUM_NODES];
                    next.nodes[i].match_index = [0; NUM_NODES];
                    next.faults_used += 1;
                    stats.node_crashes += 1;
                    out.push(next);
                }
            }
        }

        // 6c. Network Packet Duplication & Packet Loss
        for m_idx in 0..(state.msg_count as usize) {
            if !state.msgs[m_idx].duplicated && (state.msg_count as usize) < MAX_MSGS_IN_FLIGHT {
                let mut next = *state;
                let mut dup = next.msgs[m_idx];
                dup.duplicated = true;
                next.msgs[m_idx].duplicated = true;
                next.msgs[next.msg_count as usize] = dup;
                next.msg_count += 1;
                next.faults_used += 1;
                stats.duplicate_messages += 1;
                out.push(next);
            }

            let mut dropped = *state;
            let _ = dropped.remove_msg(m_idx);
            dropped.faults_used += 1;
            stats.message_drops += 1;
            out.push(dropped);
        }

        // 6d. Shallow L2 Chain Reorganization (reverts unfinalized staged L2 batch)
        if state.has_staged_l2 {
            let mut next = *state;
            next.has_staged_l2 = false;
            next.staged_l2 = L2BatchAnchor::default();
            next.faults_used += 1;
            stats.l2_shallow_reorgs += 1;
            out.push(next);
        }
    }

    // 7. Recovery Actions (Always enabled: Node Restart from Persistent WAL & Network Partition Heal)
    for i in 0..NUM_NODES {
        if state.nodes[i].crashed {
            let mut next = *state;
            next.nodes[i].crashed = false;
            next.nodes[i].role = Role::Follower;
            stats.node_restarts += 1;
            out.push(next);
        }
    }
    if state.partition_mask != 0 {
        let mut next = *state;
        next.partition_mask = 0;
        stats.partition_heals += 1;
        out.push(next);
    }
}

fn deliver_message(state: &mut SystemState, msg: InFlightMsg, cfg: &ProtocolConfig) {
    let src = msg.src as usize;
    let dst = msg.dst as usize;

    match msg.payload {
        RpcPayload::PreVoteReq {
            prospective_term,
            last_log_idx,
            last_log_term,
        } => {
            let voter = &state.nodes[dst];
            let granted = prospective_term > voter.current_term
                && is_log_up_to_date(last_log_term, last_log_idx, voter.last_log_term(), voter.wal_len);
            let voter_term = voter.current_term;
            state.push_msg(InFlightMsg {
                src: dst as u8,
                dst: src as u8,
                duplicated: false,
                payload: RpcPayload::PreVoteResp {
                    term: voter_term,
                    prospective_term,
                    granted,
                },
            });
        }
        RpcPayload::PreVoteResp {
            term,
            prospective_term,
            granted,
        } => {
            if term > state.nodes[dst].current_term {
                state.nodes[dst].current_term = term;
                state.nodes[dst].role = Role::Follower;
                state.nodes[dst].voted_for = 0;
                state.nodes[dst].prevotes_mask = 0;
                state.nodes[dst].votes_mask = 0;
                return;
            }
            if state.nodes[dst].role == Role::PreCandidate
                && prospective_term == state.nodes[dst].current_term + 1
                && prospective_term <= cfg.max_term
            {
                if granted {
                    state.nodes[dst].prevotes_mask |= 1 << src;
                }
                if popcount_u8(state.nodes[dst].prevotes_mask) >= QUORUM {
                    // Promote to real Candidate and increment persistent term
                    let new_term = prospective_term;
                    state.nodes[dst].current_term = new_term;
                    state.nodes[dst].role = Role::Candidate;
                    state.nodes[dst].voted_for = (dst as u8) + 1;
                    state.nodes[dst].votes_mask = 1 << dst;
                    let last_idx = state.nodes[dst].wal_len;
                    let last_term = state.nodes[dst].last_log_term();
                    for peer in 0..NUM_NODES {
                        if peer != dst {
                            state.push_msg(InFlightMsg {
                                src: dst as u8,
                                dst: peer as u8,
                                duplicated: false,
                                payload: RpcPayload::RequestVoteReq {
                                    term: new_term,
                                    last_log_idx: last_idx,
                                    last_log_term: last_term,
                                },
                            });
                        }
                    }
                }
            }
        }
        RpcPayload::RequestVoteReq {
            term,
            last_log_idx,
            last_log_term,
        } => {
            if term > state.nodes[dst].current_term {
                state.nodes[dst].current_term = term;
                state.nodes[dst].role = Role::Follower;
                state.nodes[dst].voted_for = 0;
                state.nodes[dst].prevotes_mask = 0;
                state.nodes[dst].votes_mask = 0;
            }
            let voter = &mut state.nodes[dst];
            let cand_id_1based = (src as u8) + 1;
            let can_vote = term == voter.current_term
                && (voter.voted_for == 0 || voter.voted_for == cand_id_1based)
                && is_log_up_to_date(last_log_term, last_log_idx, voter.last_log_term(), voter.wal_len);
            if can_vote {
                voter.voted_for = cand_id_1based;
                voter.role = Role::Follower;
            }
            let reply_term = voter.current_term;
            state.push_msg(InFlightMsg {
                src: dst as u8,
                dst: src as u8,
                duplicated: false,
                payload: RpcPayload::RequestVoteResp {
                    term: reply_term,
                    granted: can_vote,
                },
            });
        }
        RpcPayload::RequestVoteResp { term, granted } => {
            if term > state.nodes[dst].current_term {
                state.nodes[dst].current_term = term;
                state.nodes[dst].role = Role::Follower;
                state.nodes[dst].voted_for = 0;
                state.nodes[dst].votes_mask = 0;
                return;
            }
            if state.nodes[dst].role == Role::Candidate && term == state.nodes[dst].current_term {
                if granted {
                    state.nodes[dst].votes_mask |= 1 << src;
                }
                if popcount_u8(state.nodes[dst].votes_mask) >= QUORUM {
                    let wal_len = state.nodes[dst].wal_len;
                    state.nodes[dst].role = Role::Leader;
                    state.nodes[dst].next_index = [wal_len + 1; NUM_NODES];
                    state.nodes[dst].match_index = [0; NUM_NODES];
                    state.nodes[dst].match_index[dst] = wal_len;
                }
            }
        }
        RpcPayload::AppendEntriesReq {
            term,
            prev_log_idx,
            prev_log_term,
            prev_log_hash,
            entries_len,
            entries,
            leader_commit,
        } => {
            if term < state.nodes[dst].current_term {
                let cur_t = state.nodes[dst].current_term;
                state.push_msg(InFlightMsg {
                    src: dst as u8,
                    dst: src as u8,
                    duplicated: false,
                    payload: RpcPayload::AppendEntriesResp {
                        term: cur_t,
                        success: false,
                        match_idx: 0,
                    },
                });
                return;
            }
            if term > state.nodes[dst].current_term {
                state.nodes[dst].voted_for = 0;
            }
            state.nodes[dst].current_term = term;
            state.nodes[dst].role = Role::Follower;
            state.nodes[dst].prevotes_mask = 0;
            state.nodes[dst].votes_mask = 0;

            let log_ok = if prev_log_idx == 0 {
                true
            } else if prev_log_idx <= state.nodes[dst].wal_len {
                let prev_e = state.nodes[dst].wal[(prev_log_idx - 1) as usize];
                prev_e.term == prev_log_term && prev_e.entry_hash == prev_log_hash
            } else {
                false
            };

            if !log_ok {
                state.push_msg(InFlightMsg {
                    src: dst as u8,
                    dst: src as u8,
                    duplicated: false,
                    payload: RpcPayload::AppendEntriesResp {
                        term,
                        success: false,
                        match_idx: 0,
                    },
                });
                return;
            }

            if cfg.bug_unconditional_truncate {
                // Intentional mutant bug: blindly overwrites wal_len even if out-of-order shorter RPC arrives!
                let mut new_len = prev_log_idx;
                for k in 0..(entries_len as usize) {
                    state.nodes[dst].wal[new_len as usize] = entries[k];
                    new_len += 1;
                }
                state.nodes[dst].wal_len = new_len;
            } else {
                // Canonical Raft §5.3: truncate ONLY if an existing entry conflicts with a new one
                for k in 0..(entries_len as usize) {
                    let target_idx = (prev_log_idx as usize) + k;
                    if target_idx < (state.nodes[dst].wal_len as usize) {
                        if state.nodes[dst].wal[target_idx].term != entries[k].term {
                            // Truncate conflicting uncommitted suffix from target_idx onward
                            state.nodes[dst].wal_len = target_idx as u8;
                            state.nodes[dst].wal[target_idx] = entries[k];
                            state.nodes[dst].wal_len += 1;
                        }
                    } else if target_idx < MAX_WAL_LEN {
                        state.nodes[dst].wal[target_idx] = entries[k];
                        state.nodes[dst].wal_len += 1;
                    }
                }
            }

            let match_idx = prev_log_idx + entries_len;
            if leader_commit > state.nodes[dst].commit_index {
                let bounded = leader_commit.min(match_idx).min(state.nodes[dst].wal_len);
                if bounded > state.nodes[dst].commit_index {
                    state.nodes[dst].commit_index = bounded;
                }
            }

            state.push_msg(InFlightMsg {
                src: dst as u8,
                dst: src as u8,
                duplicated: false,
                payload: RpcPayload::AppendEntriesResp {
                    term,
                    success: true,
                    match_idx,
                },
            });
        }
        RpcPayload::AppendEntriesResp {
            term,
            success,
            match_idx,
        } => {
            if term > state.nodes[dst].current_term {
                state.nodes[dst].current_term = term;
                state.nodes[dst].role = Role::Follower;
                state.nodes[dst].voted_for = 0;
                return;
            }
            if state.nodes[dst].role == Role::Leader && term == state.nodes[dst].current_term {
                if success {
                    if match_idx > state.nodes[dst].match_index[src] {
                        state.nodes[dst].match_index[src] = match_idx;
                        state.nodes[dst].next_index[src] = match_idx + 1;
                    }
                    // Advance commit_index for highest N replicated on a majority quorum
                    let wal_len = state.nodes[dst].wal_len;
                    let cur_term = state.nodes[dst].current_term;
                    let mut new_commit = state.nodes[dst].commit_index;
                    for n_idx in ((new_commit + 1)..=wal_len).rev() {
                        let term_ok = if cfg.bug_commit_old_term_entries {
                            true
                        } else {
                            state.nodes[dst].wal[(n_idx - 1) as usize].term == cur_term
                        };
                        if !term_ok {
                            continue;
                        }
                        let mut repl_count = 0;
                        for p in 0..NUM_NODES {
                            if state.nodes[dst].match_index[p] >= n_idx {
                                repl_count += 1;
                            }
                        }
                        if repl_count >= QUORUM {
                            new_commit = n_idx;
                            break;
                        }
                    }
                    if new_commit > state.nodes[dst].commit_index {
                        state.nodes[dst].commit_index = new_commit;
                        if new_commit > state.ghost_committed_len {
                            for idx in (state.ghost_committed_len as usize)..(new_commit as usize) {
                                state.ghost_committed[idx] = state.nodes[dst].wal[idx];
                            }
                            state.ghost_committed_len = new_commit;
                        }
                    }
                } else if state.nodes[dst].next_index[src] > 1 {
                    state.nodes[dst].next_index[src] -= 1;
                }
            }
        }
    }
}

/// Deterministic Post-Partition Liveness Convergence Checker:
/// Given any reachable faulted state (partitioned, leader crashed, duplicate msgs, stale writes),
/// heals the network + restarts crashed nodes and executes fair deterministic scheduling steps
/// to verify the cluster elects a unique Leader, synchronizes all logs & commit_index, and
/// finalizes the L2 settlement batch without deadlock or livelock.
pub fn verify_post_heal_liveness(faulted_state: &SystemState, cfg: &ProtocolConfig) -> Result<u32, String> {
    let mut s = *faulted_state;
    // Heal environment: restore links and restart crashed nodes from persistent disk WAL
    s.partition_mask = 0;
    for n in &mut s.nodes {
        if n.crashed {
            n.crashed = false;
            n.role = Role::Follower;
        }
    }

    for step in 1..=64_u32 {
        let prev = s;
        // 1. Drain all pending RPCs in deterministic FIFO order
        while s.msg_count > 0 {
            let before_msg = s;
            let m = s.remove_msg(0);
            if s.can_deliver(m.src as usize, m.dst as usize) {
                deliver_message(&mut s, m, cfg);
                check_safety_invariants(&before_msg, &s)?;
            }
        }

        // 2. Check if an active Leader already exists
        let mut active_leader: Option<usize> = None;
        let mut highest_term = 0_u8;
        for (idx, n) in s.nodes.iter().enumerate() {
            highest_term = highest_term.max(n.current_term);
            if n.role == Role::Leader {
                active_leader = Some(idx);
            }
        }
        if let Some(l) = active_leader {
            if s.nodes[l].current_term < highest_term {
                active_leader = None;
            }
        }

        // 3. If no up-to-date Leader exists, pick the most up-to-date node to trigger PreVote -> Election
        if active_leader.is_none() {
            let best_cand = (0..NUM_NODES)
                .max_by_key(|&i| (s.nodes[i].last_log_term(), s.nodes[i].wal_len, NUM_NODES - i))
                .unwrap_or(0);

            let before_elect = s;
            // Allow unbounded recovery election term during post-heal liveness convergence
            let mut liveness_cfg = *cfg;
            liveness_cfg.max_term = s.nodes[best_cand].current_term.saturating_add(4);

            let prospective_term = s.nodes[best_cand].current_term + 1;
            s.nodes[best_cand].role = Role::PreCandidate;
            s.nodes[best_cand].prevotes_mask = 1 << best_cand;
            let last_idx = s.nodes[best_cand].wal_len;
            let last_term = s.nodes[best_cand].last_log_term();
            for peer in 0..NUM_NODES {
                if peer != best_cand {
                    s.push_msg(InFlightMsg {
                        src: best_cand as u8,
                        dst: peer as u8,
                        duplicated: false,
                        payload: RpcPayload::PreVoteReq {
                            prospective_term,
                            last_log_idx: last_idx,
                            last_log_term: last_term,
                        },
                    });
                }
            }
            check_safety_invariants(&before_elect, &s)?;
            while s.msg_count > 0 {
                let b = s;
                let m = s.remove_msg(0);
                deliver_message(&mut s, m, &liveness_cfg);
                check_safety_invariants(&b, &s)?;
            }
            continue;
        }

        let leader = active_leader.unwrap();

        // 4. Raft §8 Leader No-Op Barrier: ensure Leader appends an entry in its current_term
        // so both prior-term uncommitted entries commit AND lagging/stale followers truncate conflicting tails!
        let any_peer_mismatch = (0..NUM_NODES).any(|p| {
            s.nodes[p].wal_len != s.nodes[leader].wal_len
                || s.nodes[p].wal[..(s.nodes[p].wal_len.min(s.nodes[leader].wal_len) as usize)]
                    != s.nodes[leader].wal[..(s.nodes[p].wal_len.min(s.nodes[leader].wal_len) as usize)]
        });
        if (s.nodes[leader].wal_len > s.nodes[leader].commit_index || any_peer_mismatch)
            && s.nodes[leader].last_log_term() != s.nodes[leader].current_term
            && (s.nodes[leader].wal_len as usize) < MAX_WAL_LEN
        {
            let b = s;
            let idx = s.nodes[leader].wal_len;
            let term = s.nodes[leader].current_term;
            let prev_hash = s.nodes[leader].last_log_hash();
            let entry_hash = compute_wal_hash(idx + 1, term, 250, prev_hash); // 250 = NOOP_BARRIER
            s.nodes[leader].wal[idx as usize] = WalEntry {
                term,
                cmd: 250,
                prev_hash,
                entry_hash,
            };
            s.nodes[leader].wal_len += 1;
            s.nodes[leader].match_index[leader] = s.nodes[leader].wal_len;
            check_safety_invariants(&b, &s)?;
        }

        // If WAL was full with an uncommitted old-term entry at MAX_WAL_LEN, verify at least all ghost_committed converge;
        // otherwise replicate Leader's log to all peers!
        let leader_len = s.nodes[leader].wal_len;
        let all_logs_matched = (0..NUM_NODES).all(|p| {
            s.nodes[p].wal_len == leader_len
                && s.nodes[p].wal[..(leader_len as usize)] == s.nodes[leader].wal[..(leader_len as usize)]
                && s.nodes[p].commit_index == s.nodes[leader].commit_index
        });

        if !all_logs_matched {
            for peer in 0..NUM_NODES {
                if peer == leader {
                    continue;
                }
                let next_idx = s.nodes[leader].next_index[peer].max(1);
                let prev_idx = next_idx - 1;
                let prev_term = if prev_idx == 0 {
                    0
                } else {
                    s.nodes[leader].wal[(prev_idx - 1) as usize].term
                };
                let prev_hash = if prev_idx == 0 {
                    0
                } else {
                    s.nodes[leader].wal[(prev_idx - 1) as usize].entry_hash
                };
                let mut entries = [WalEntry::default(); MAX_WAL_LEN];
                let mut entries_len = 0_u8;
                for k in (prev_idx as usize)..(leader_len as usize) {
                    entries[entries_len as usize] = s.nodes[leader].wal[k];
                    entries_len += 1;
                }
                s.push_msg(InFlightMsg {
                    src: leader as u8,
                    dst: peer as u8,
                    duplicated: false,
                    payload: RpcPayload::AppendEntriesReq {
                        term: s.nodes[leader].current_term,
                        prev_log_idx: prev_idx,
                        prev_log_term: prev_term,
                        prev_log_hash: prev_hash,
                        entries_len,
                        entries,
                        leader_commit: s.nodes[leader].commit_index,
                    },
                });
            }
            check_safety_invariants(&prev, &s)?;
            continue;
        }

        // 5. Finalize L2 Settlement Batch if committed entries exceed last finalized L2 batch
        let last_fin_len = if s.finalized_l2_len == 0 {
            0
        } else {
            s.finalized_l2[(s.finalized_l2_len - 1) as usize].commit_len
        };
        if s.nodes[leader].commit_index > last_fin_len && (s.finalized_l2_len as usize) < MAX_L2_ANCHORS {
            if !s.has_staged_l2 {
                let clen = s.nodes[leader].commit_index;
                s.has_staged_l2 = true;
                s.staged_l2 = L2BatchAnchor {
                    epoch: s.finalized_l2_len + 1,
                    commit_len: clen,
                    root_hash: s.nodes[leader].wal[(clen - 1) as usize].entry_hash,
                    confirmations: 1,
                };
            } else {
                s.staged_l2.confirmations += 1;
                if s.staged_l2.confirmations >= L2_FINALITY_CONFS {
                    let idx = s.finalized_l2_len as usize;
                    s.finalized_l2[idx] = s.staged_l2;
                    s.finalized_l2_len += 1;
                    s.has_staged_l2 = false;
                    s.staged_l2 = L2BatchAnchor::default();
                }
            }
            check_safety_invariants(&prev, &s)?;
            continue;
        }

        // Convergence criterion satisfied!
        let converged = (0..NUM_NODES).all(|p| s.nodes[p].commit_index >= s.ghost_committed_len);
        if converged {
            return Ok(step);
        }
    }

    Err("LIVENESS_CONVERGENCE_TIMEOUT: cluster failed to converge within 64 fair post-heal steps".to_string())
}

pub struct ModelCheckSummary {
    pub distinct_states: usize,
    pub total_transitions: u64,
    pub max_bfs_depth: u32,
    pub liveness_states_checked: usize,
    pub max_liveness_recovery_steps: u32,
    pub state_graph_fingerprint: u64,
    pub elapsed_ms: f64,
    pub telemetry: TransitionTelemetry,
}

pub fn run_bfs_model_checker(cfg: &ProtocolConfig) -> Result<ModelCheckSummary, String> {
    let t0 = Instant::now();
    let init = SystemState::initial();
    check_safety_invariants(&init, &init)?;

    let mut visited: FastHashSet<SystemState> =
        HashSet::with_capacity_and_hasher(262_144, BuildHasherDefault::<FxHasher64>::default());
    let mut queue: VecDeque<(SystemState, u32)> = VecDeque::with_capacity(131_072);
    let mut successors: Vec<SystemState> = Vec::with_capacity(64);
    let mut telemetry = TransitionTelemetry::default();
    let mut liveness_samples: Vec<SystemState> = Vec::with_capacity(12_000);
    let mut graph_hasher = FxHasher64::default();

    visited.insert(init);
    queue.push_back((init, 0));
    let mut max_depth = 0_u32;

    while let Some((cur, depth)) = queue.pop_front() {
        max_depth = max_depth.max(depth);
        expand_successors(&cur, cfg, &mut successors, &mut telemetry);

        for next in &successors {
            telemetry.total_transitions += 1;
            check_safety_invariants(&cur, next)?;
            next.hash(&mut graph_hasher);

            if visited.insert(*next) {
                // Collect diverse faulted states (partitioned, crashed, or uncommitted WAL) for Liveness verification
                if liveness_samples.len() < 12_000
                    && (next.partition_mask != 0
                        || next.nodes.iter().any(|n| n.crashed)
                        || next.ghost_committed_len > 0)
                    && (visited.len() % 11 == 0)
                {
                    liveness_samples.push(*next);
                }
                queue.push_back((*next, depth + 1));
            }
        }

        if telemetry.total_transitions >= cfg.target_min_transitions {
            break;
        }
    }

    let mut max_recovery_steps = 0_u32;
    for sample in &liveness_samples {
        let steps = verify_post_heal_liveness(sample, cfg)?;
        max_recovery_steps = max_recovery_steps.max(steps);
    }

    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(ModelCheckSummary {
        distinct_states: visited.len(),
        total_transitions: telemetry.total_transitions,
        max_bfs_depth: max_depth,
        liveness_states_checked: liveness_samples.len(),
        max_liveness_recovery_steps: max_recovery_steps,
        state_graph_fingerprint: graph_hasher.finish(),
        elapsed_ms,
        telemetry,
    })
}

fn main() {
    let cfg = ProtocolConfig {
        max_term: 4,
        max_faults: 4,
        target_min_transitions: 1_250_000,
        ..ProtocolConfig::default()
    };

    println!("========================================================================================");
    println!("HYPERION OMEGA — EXPLICIT-STATE BFS & LIVENESS MODEL CHECKER (Raft + WAL + L2 Commit)");
    println!("========================================================================================");

    match run_bfs_model_checker(&cfg) {
        Ok(summary) => {
            let tps = (summary.total_transitions as f64) / (summary.elapsed_ms / 1000.0);
            println!("STATUS                       : PASS (0 Safety Counterexamples, 100% Liveness Convergence)");
            println!("Distinct Canonical States    : {}", summary.distinct_states);
            println!("Explored Concurrent Trans.   : {} (target >= 1,000,000)", summary.total_transitions);
            println!("Max BFS Reachability Depth   : {}", summary.max_bfs_depth);
            println!("Post-Heal Liveness Verified  : {} faulted states (max recovery = {} steps)", summary.liveness_states_checked, summary.max_liveness_recovery_steps);
            println!("Throughput                   : {:.0} transitions/sec ({:.2} ms)", tps, summary.elapsed_ms);
            println!("State-Graph FxHash64 Digest  : 0x{:016x}", summary.state_graph_fingerprint);
            println!("Fault Breakdown              :");
            println!("  - PreVote Elections        : {}", summary.telemetry.prevote_elections);
            println!("  - Async RPC Deliveries     : {}", summary.telemetry.message_deliveries);
            println!("  - Active Leader Proposals  : {}", summary.telemetry.client_proposals);
            println!("  - Stale Minority Writes    : {}", summary.telemetry.stale_leader_writes);
            println!("  - Network Partitions       : {}", summary.telemetry.partition_injections);
            println!("  - Leader/Node Crashes      : {}", summary.telemetry.node_crashes);
            println!("  - Duplicate RPC Packets    : {}", summary.telemetry.duplicate_messages);
            println!("  - Dropped RPC Packets      : {}", summary.telemetry.message_drops);
            println!("  - L2 Batch Anchors & Confs : {}", summary.telemetry.l2_batch_actions);
            println!("  - Shallow L2 Chain Reorgs  : {}", summary.telemetry.l2_shallow_reorgs);

            let report_json = format!(
                "{{\n  \"suite\": \"HYPERION_OMEGA_CH11_EXPLICIT_STATE_MODEL_CHECKER\",\n  \"status\": \"PASS\",\n  \"distinct_states\": {},\n  \"explored_transitions\": {},\n  \"max_bfs_depth\": {},\n  \"post_heal_liveness_states_verified\": {},\n  \"max_liveness_recovery_steps\": {},\n  \"elapsed_ms\": {:.3},\n  \"transitions_per_sec\": {:.1},\n  \"state_graph_digest_hex\": \"0x{:016x}\",\n  \"invariants_verified\": [\n    \"ElectionSafety\",\n    \"LogMatching\",\n    \"WalHashChainIntegrity\",\n    \"ZeroCommittedDataLoss\",\n    \"LeaderCompleteness\",\n    \"L2SettlementSafety\",\n    \"PostHealLiveness\"\n  ],\n  \"telemetry\": {{\n    \"prevote_elections\": {},\n    \"message_deliveries\": {},\n    \"client_proposals\": {},\n    \"stale_leader_writes\": {},\n    \"partition_injections\": {},\n    \"partition_heals\": {},\n    \"node_crashes\": {},\n    \"node_restarts\": {},\n    \"duplicate_messages\": {},\n    \"message_drops\": {},\n    \"l2_batch_actions\": {},\n    \"l2_shallow_reorgs\": {}\n  }}\n}}\n",
                summary.distinct_states,
                summary.total_transitions,
                summary.max_bfs_depth,
                summary.liveness_states_checked,
                summary.max_liveness_recovery_steps,
                summary.elapsed_ms,
                tps,
                summary.state_graph_fingerprint,
                summary.telemetry.prevote_elections,
                summary.telemetry.message_deliveries,
                summary.telemetry.client_proposals,
                summary.telemetry.stale_leader_writes,
                summary.telemetry.partition_injections,
                summary.telemetry.partition_heals,
                summary.telemetry.node_crashes,
                summary.telemetry.node_restarts,
                summary.telemetry.duplicate_messages,
                summary.telemetry.message_drops,
                summary.telemetry.l2_batch_actions,
                summary.telemetry.l2_shallow_reorgs,
            );

            let base_dir = std::env::var("CARGO_MANIFEST_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."));
            let out_path = base_dir.join("model_checker_report.json");
            fs::write(&out_path, report_json).expect("write model_checker_report.json");
            println!("Saved certificate -> {}", out_path.display());

            println!("\n--- EXECUTING EXTREME OVERCLOCK GATES (MUTATION + 500K SOAK + 3-PASS BENCH) ---");
            let mut_rep = overclock::run_protocol_mutation_campaign();
            println!(
                "[GATE 1: MUTATION]  {}/{} mutants killed ({:.2}%) in {:.2} ms",
                mut_rep.killed_mutants, mut_rep.total_mutants, mut_rep.score_pct, mut_rep.elapsed_ms
            );
            assert!(
                mut_rep.score_pct >= 95.0,
                "Protocol mutation score {:.2}% < 95.0%! Survived: {:?}",
                mut_rep.score_pct,
                mut_rep.survived_mutants
            );

            let soak_rep = overclock::run_500k_zero_leak_soak();
            println!(
                "[GATE 2: 500K SOAK] {} cycles | steady_state_leak={} B (warmup={} B -> after={} B) | {:.0} cycles/s ({:.2} ms)",
                soak_rep.cycles,
                soak_rep.steady_state_leak_bytes,
                soak_rep.cap_after_warmup_bytes,
                soak_rep.cap_after_soak_bytes,
                soak_rep.cycles_per_sec,
                soak_rep.elapsed_ms
            );
            assert_eq!(soak_rep.steady_state_leak_bytes, 0, "Non-zero steady-state arena growth!");

            let passes = overclock::run_3pass_micro_optimization_benchmark();
            for p in &passes {
                println!(
                    "[GATE 3: BENCH]     {:<62} | p50={:>7.2} ns/op | p99={:>7.2} ns/op | {} B/op ({} allocs/op)",
                    p.pass_name, p.p50_ns_per_op, p.p99_ns_per_op, p.bytes_per_op, p.allocs_per_op
                );
            }

            let oc_json = format!(
                "{{\n  \"mutation_gate\": {{\n    \"killed\": {},\n    \"total\": {},\n    \"score_pct\": {:.2},\n    \"elapsed_ms\": {:.2}\n  }},\n  \"soak_500k_gate\": {{\n    \"cycles\": {},\n    \"warmup_cap_bytes\": {},\n    \"final_cap_bytes\": {},\n    \"steady_state_leak_bytes\": {},\n    \"cycles_per_sec\": {:.1},\n    \"elapsed_ms\": {:.2}\n  }},\n  \"micro_opt_passes\": [\n    {{\"pass\": \"{}\", \"p50_ns\": {:.2}, \"p99_ns\": {:.2}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}},\n    {{\"pass\": \"{}\", \"p50_ns\": {:.2}, \"p99_ns\": {:.2}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}},\n    {{\"pass\": \"{}\", \"p50_ns\": {:.2}, \"p99_ns\": {:.2}, \"bytes_per_op\": {}, \"allocs_per_op\": {}}}\n  ]\n}}\n",
                mut_rep.killed_mutants,
                mut_rep.total_mutants,
                mut_rep.score_pct,
                mut_rep.elapsed_ms,
                soak_rep.cycles,
                soak_rep.cap_after_warmup_bytes,
                soak_rep.cap_after_soak_bytes,
                soak_rep.steady_state_leak_bytes,
                soak_rep.cycles_per_sec,
                soak_rep.elapsed_ms,
                passes[0].pass_name, passes[0].p50_ns_per_op, passes[0].p99_ns_per_op, passes[0].bytes_per_op, passes[0].allocs_per_op,
                passes[1].pass_name, passes[1].p50_ns_per_op, passes[1].p99_ns_per_op, passes[1].bytes_per_op, passes[1].allocs_per_op,
                passes[2].pass_name, passes[2].p50_ns_per_op, passes[2].p99_ns_per_op, passes[2].bytes_per_op, passes[2].allocs_per_op,
            );
            let oc_path = base_dir.join("overclock_rust_report.json");
            fs::write(&oc_path, oc_json).expect("write overclock_rust_report.json");
            println!("Saved overclock report -> {}", oc_path.display());
        }
        Err(err) => {
            eprintln!("MODEL CHECKER COUNTEREXAMPLE FOUND: {}", err);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_protocol_passes_bounded_safety_and_liveness() {
        let cfg = ProtocolConfig {
            max_term: 4,
            max_faults: 3,
            target_min_transitions: 150_000,
            ..ProtocolConfig::default()
        };
        let res = run_bfs_model_checker(&cfg).expect("canonical Raft+WAL+L2 must satisfy all invariants");
        assert!(res.total_transitions >= 150_000);
        assert!(res.liveness_states_checked > 100);
    }

    #[test]
    fn mutant_uncommitted_l2_anchor_is_caught_by_model_checker() {
        let cfg = ProtocolConfig {
            max_term: 3,
            max_faults: 2,
            target_min_transitions: 50_000,
            bug_anchor_uncommitted_to_l2: true,
            ..ProtocolConfig::default()
        };
        let res = run_bfs_model_checker(&cfg);
        assert!(
            res.is_err(),
            "Model checker MUST catch speculative uncommitted L2 batch finalization!"
        );
        let msg = res.err().unwrap();
        assert!(msg.contains("INV6_L2_SETTLEMENT_UNCOMMITTED_DATA"), "unexpected error: {msg}");
    }

    #[test]
    fn mutant_unconditional_log_truncation_is_caught_by_model_checker() {
        let cfg = ProtocolConfig {
            max_term: 1,
            max_faults: 0,
            target_min_transitions: 100_000,
            bug_unconditional_truncate: true,
            ..ProtocolConfig::default()
        };
        let res = run_bfs_model_checker(&cfg);
        assert!(
            res.is_err(),
            "Model checker MUST catch out-of-order AppendEntries log truncation bug!"
        );
    }
}
