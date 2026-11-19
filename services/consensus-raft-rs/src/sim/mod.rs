//! TigerBeetle VOPR / FoundationDB-Style Deterministic Simulation Testing (`DST`) Engine.
//!
//! Controls 5+ Raft nodes, `SimulatedClock`, `SimulatedNetworkSwitch`, `SimulatedFaultyDisk`,
//! `NemesisEngine`, concurrent multi-client workloads, and continuous safety invariant monitors
//! from a **single 64-bit PRNG seed** with bit-identical trace reproducibility.

pub mod clock;
pub mod disk;
pub mod nemesis;
pub mod network;

use crate::checker::{
    ClientOpReturn, ClientOpSpec, ConcurrentHistoryOp, LedgerKey, LinearizabilityChecker,
    LinearizabilityVerdict, ObservedTransaction, SerializabilityVerdict,
    StrictSerializabilityVerifier,
};
use crate::prng::DeterministicPrng;
use crate::raft_core::{RaftCore, RaftEffect, RaftTuningConfig};
use crate::sim::clock::SimulatedClock;
use crate::sim::disk::SimulatedFaultyDisk;
use crate::sim::nemesis::{NemesisEngine, NemesisFaultEvent, NemesisProfile};
use crate::sim::network::SimulatedNetworkSwitch;
use crate::traits::{Clock, DiskStorage, NetworkTransport};
use crate::types::{
    ClusterConfig, LedgerCommand, LedgerReadQuery, LedgerReadResult, LogIndex, NodeId, RaftRole,
    ReadConsistencyMode, Term, FP_SCALE_U128,
};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

#[derive(Clone, Debug)]
struct ScheduledDiskSync {
    complete_at_ns: u64,
    seq: u64,
    node_id: NodeId,
    io_id: u64,
     disk_epoch: u64,
}

impl PartialEq for ScheduledDiskSync {
    fn eq(&self, other: &Self) -> bool {
        self.complete_at_ns == other.complete_at_ns && self.seq == other.seq
    }
}
impl Eq for ScheduledDiskSync {}
impl PartialOrd for ScheduledDiskSync {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ScheduledDiskSync {
    fn cmp(&self, other: &Self) -> Ordering {
        self.complete_at_ns
            .cmp(&other.complete_at_ns)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

#[derive(Clone, Debug)]
struct PendingClientTracker {
    history_idx: usize,
    is_write: bool,
    write_keys: BTreeSet<LedgerKey>,
    read_key: Option<LedgerKey>,
}

pub struct SimNodeSlot {
    pub core: RaftCore,
    pub disk: SimulatedFaultyDisk,
    pub crashed_until_ns: Option<u64>,
    pub disk_boot_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoprRunSummary {
    pub seed: u64,
    pub steps_executed: u64,
    pub final_master_ns: u64,
    pub final_leader: Option<NodeId>,
    pub final_term: Term,
    pub final_commit_index: LogIndex,
    pub converged_state_root_blake3: [u8; 32],
    pub trace_digest_blake3: [u8; 32],
    pub total_client_ops: usize,
    pub committed_writes: usize,
    pub linearizable_reads: usize,
    pub indeterminate_ops: usize,
    pub nemesis_faults_injected: usize,
    pub torn_writes_recovered: usize,
    pub linearizability_explored_states: usize,
    pub strict_serializability_edges: usize,
}

pub struct VoprClusterSimulator {
    pub seed: u64,
    pub clock: SimulatedClock,
    pub net: SimulatedNetworkSwitch,
    pub nemesis: NemesisEngine,
    pub nodes: BTreeMap<NodeId, SimNodeSlot>,
    tuning: RaftTuningConfig,
    prng: DeterministicPrng,
    disk_sync_heap: BinaryHeap<Reverse<ScheduledDiskSync>>,
    next_disk_seq: u64,

    /// Continuous safety invariant trackers across ALL terms & steps!
    leaders_by_term: BTreeMap<Term, NodeId>,
    committed_entries_oracle: BTreeMap<LogIndex, (Term, [u8; 32])>,

    /// Concurrent client history for WGL Linearizability & Strict-1SR Serializability checkers!
    pub history: Vec<ConcurrentHistoryOp>,
    pub serial_txs: Vec<ObservedTransaction>,
    pending_proposals: BTreeMap<(NodeId, u64), PendingClientTracker>,
    pending_reads: BTreeMap<(NodeId, u64), PendingClientTracker>,
    next_client_req_id: u64,
    pub nemesis_active: bool,
    pub torn_writes_recovered: usize,
    trace_hasher: blake3::Hasher,
}

impl VoprClusterSimulator {
    pub fn new_five_node(
        seed: u64,
        tuning: RaftTuningConfig,
        nemesis_profile: NemesisProfile,
    ) -> Self {
        let node_ids: Vec<NodeId> = vec![1, 2, 3, 4, 5];
        Self::with_nodes(seed, &node_ids, tuning, nemesis_profile)
    }

    pub fn with_nodes(
        seed: u64,
        node_ids: &[NodeId],
        tuning: RaftTuningConfig,
        nemesis_profile: NemesisProfile,
    ) -> Self {
        let start_ns = 1_000_000_000;
        let clock = SimulatedClock::new(start_ns);
        let mut prng = DeterministicPrng::new(seed);

        // Assign bounded realistic quartz oscillator skews per node (`[-12_000, +12_000]` ppm <= `20_000` max_clock_skew_ppm).
        for &nid in node_ids {
            let skew = (prng.range_u64(0, 24_000) as i64) - 12_000;
            clock.set_node_skew(nid, skew, 0);
        }

        let mut net = SimulatedNetworkSwitch::new(prng.next_u64());
        net.heal_all(node_ids);

        let cfg = ClusterConfig::uniform(node_ids.iter().copied());
        let mut nodes = BTreeMap::new();
        for &nid in node_ids {
            let node_now = clock.now_ns(nid);
            let core = RaftCore::new(
                nid,
                cfg.clone(),
                tuning.clone(),
                prng.next_u64(),
                node_now,
                None,
            );
            let disk = SimulatedFaultyDisk::new(nid);
            nodes.insert(
                nid,
                SimNodeSlot {
                    core,
                    disk,
                    crashed_until_ns: None,
                    disk_boot_epoch: 1,
                },
            );
        }

        let nemesis = NemesisEngine::new(prng.next_u64(), nemesis_profile, start_ns);
        let mut trace_hasher = blake3::Hasher::new();
        trace_hasher.update(&seed.to_le_bytes());

        Self {
            seed,
            clock,
            net,
            nemesis,
            nodes,
            tuning,
            prng,
            disk_sync_heap: BinaryHeap::new(),
            next_disk_seq: 1,
            leaders_by_term: BTreeMap::new(),
            committed_entries_oracle: BTreeMap::new(),
            history: Vec::new(),
            serial_txs: Vec::new(),
            pending_proposals: BTreeMap::new(),
            pending_reads: BTreeMap::new(),
            next_client_req_id: 1,
            nemesis_active: false,
            torn_writes_recovered: 0,
            trace_hasher,
        }
    }

    pub fn add_node_dynamically(&mut self, node_id: NodeId, as_learner_in: ClusterConfig) {
        if self.nodes.contains_key(&node_id) {
            return;
        }
        let node_now = self.clock.now_ns(node_id);
        let core = RaftCore::new(
            node_id,
            as_learner_in,
            self.tuning.clone(),
            self.prng.next_u64(),
            node_now,
            None,
        );
        let disk = SimulatedFaultyDisk::new(node_id);
        self.nodes.insert(
            node_id,
            SimNodeSlot {
                core,
                disk,
                crashed_until_ns: None,
                disk_boot_epoch: 1,
            },
        );
        let all_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        self.net.heal_all(&all_ids);
    }

    pub fn active_leader(&self) -> Option<NodeId> {
        let mut best: Option<(Term, NodeId)> = None;
        for (&nid, slot) in &self.nodes {
            if slot.crashed_until_ns.is_none() && slot.core.role == RaftRole::Leader {
                let t = slot.core.hard_state.current_term;
                if best.is_none_or(|(bt, _)| t > bt) {
                    best = Some((t, nid));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    pub fn down_nodes_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|s| s.crashed_until_ns.is_some() || s.core.role == RaftRole::Quarantined)
            .count()
    }

    /// Advance simulation by `step_ns` (e.g., `5_000_000` = 5ms), processing all due network packets,
    /// disk `fsync` completions, node crash recoveries, Raft ticks, and continuous invariant checks!
    pub fn step_simulation(&mut self, step_ns: u64, allow_nemesis: bool) -> Result<(), String> {
        self.nemesis_active = allow_nemesis;
        let target_master_ns = self.clock.master_ns().saturating_add(step_ns);

        while self.clock.master_ns() < target_master_ns {
            let next_pkt = self.net.peek_next_delivery_ns().unwrap_or(u64::MAX);
            let next_io = self
                .disk_sync_heap
                .peek()
                .map(|Reverse(s)| s.complete_at_ns)
                .unwrap_or(u64::MAX);
            let next_event = next_pkt.min(next_io).min(target_master_ns);
            self.clock.advance_to(next_event);
            let now_master = self.clock.master_ns();

            // 1. Reboot any crashed nodes whose downtime has elapsed!
            let reboot_ids: Vec<NodeId> = self
                .nodes
                .iter()
                .filter_map(|(&nid, s)| match s.crashed_until_ns {
                    Some(until) if now_master >= until => Some(nid),
                    _ => None,
                })
                .collect();
            for nid in reboot_ids {
                self.reboot_crashed_node(nid)?;
            }

            // 2. Process due `DiskStorage::fsync()` completions!
            while let Some(Reverse( peeked )) = self.disk_sync_heap.peek() {
                if peeked.complete_at_ns > now_master {
                    break;
                }
                let Reverse(sync_ev) = self.disk_sync_heap.pop().expect("peeked");
                self.complete_disk_sync(sync_ev)?;
            }

            // 3. Deliver due network packets!
            while let Some(pkt) = self.net.pop_due_packet(now_master) {
                let to = pkt.envelope.to;
                self.trace_hasher.update(b"PKT:");
                self.trace_hasher.update(&pkt.deliver_at_ns.to_le_bytes());
                self.trace_hasher.update(&pkt.envelope.from.to_le_bytes());
                self.trace_hasher.update(&to.to_le_bytes());

                let is_up = self
                    .nodes
                    .get(&to)
                    .map(|s| s.crashed_until_ns.is_none())
                    .unwrap_or(false);
                if is_up {
                    let node_now = self.clock.now_ns(to);
                    if let Some(slot) = self.nodes.get_mut(&to) {
                        slot.core.step_network(pkt.envelope, node_now);
                    }
                    self.harvest_node_effects(to, allow_nemesis)?;
                }
            }

            if next_event == target_master_ns {
                break;
            }
        }

        // 4. Scheduled Nemesis fault injection!
        let now_master = self.clock.master_ns();
        if allow_nemesis && self.nemesis.should_fire_scheduled(now_master) {
            let all_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
            let leader = self.active_leader();
            let down_cnt = self.down_nodes_count();
            if let Some(ev) = self.nemesis.choose_scheduled_fault(
                now_master,
                &all_ids,
                leader,
                down_cnt,
                &mut self.net,
            ) {
                self.apply_nemesis_side_effect(now_master, ev)?;
            }
        }

        // 5. Tick every live node!
        let node_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        for nid in node_ids {
            let is_up = self
                .nodes
                .get(&nid)
                .map(|s| s.crashed_until_ns.is_none())
                .unwrap_or(false);
            if is_up {
                let node_now = self.clock.now_ns(nid);
                if let Some(slot) = self.nodes.get_mut(&nid) {
                    slot.core.tick(node_now);
                }
                self.harvest_node_effects(nid, allow_nemesis)?;
            }
        }

        // 6. Enforce Global Consensus & Financial Safety Invariants!
        self.assert_step_safety_invariants()?;
        Ok(())
    }

    fn apply_nemesis_side_effect(
        &mut self,
        now_master: u64,
        ev: NemesisFaultEvent,
    ) -> Result<(), String> {
        self.trace_hasher.update(b"NEMESIS:");
        self.trace_hasher.update(&now_master.to_le_bytes());

        match ev {
            NemesisFaultEvent::LeaderFsyncStall { node_id, stall_ns } => {
                if let Some(slot) = self.nodes.get_mut(&node_id) {
                    slot.disk.extra_fsync_stall_ns = stall_ns;
                }
            }
            NemesisFaultEvent::CrashNodeTornWrite { node_id, .. } => {
                if self.down_nodes_count() < self.nemesis.profile.max_simultaneous_down_nodes {
                    self.crash_node_now(node_id, now_master, self.nemesis.profile.enable_torn_writes);
                }
            }
            NemesisFaultEvent::HealNetwork => {
                for slot in self.nodes.values_mut() {
                    slot.disk.extra_fsync_stall_ns = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn crash_node_now(&mut self, node_id: NodeId, now_master: u64, inject_torn_write: bool) {
        let downtime = self.prng.range_u64(160_000_000, 380_000_000);
        if let Some(slot) = self.nodes.get_mut(&node_id) {
            if slot.crashed_until_ns.is_some() {
                return;
            }
            slot.disk
                .simulate_power_loss_crash(&mut self.prng, inject_torn_write);
            slot.crashed_until_ns = Some(now_master.saturating_add(downtime));
            slot.disk_boot_epoch = slot.disk_boot_epoch.wrapping_add(1);
        }
        self.net.drop_all_inflight_for_node(node_id);

        // Any client ops awaiting response from `node_id` become indeterminate (`return_ts_ns = None`)!
        self.pending_proposals.retain(|&(nid, _), _| nid != node_id);
        self.pending_reads.retain(|&(nid, _), _| nid != node_id);
    }

    fn reboot_crashed_node(&mut self, node_id: NodeId) -> Result<(), String> {
        let now_master = self.clock.master_ns();
        let node_now = self.clock.now_ns(node_id);
        let next_seed = self.prng.next_u64();

        if let Some(slot) = self.nodes.get_mut(&node_id) {
            slot.crashed_until_ns = None;
            match slot.disk.recover_after_restart() {
                Ok(rec) => {
                    self.torn_writes_recovered += usize::from(rec.truncated_torn_tail_bytes > 0);
                    let init_cfg = rec
                        .snapshot
                        .as_ref()
                        .map(|s| s.meta.config.clone())
                        .unwrap_or_else(|| slot.core.active_config().clone());
                    slot.core = RaftCore::new(
                        node_id,
                        init_cfg,
                        self.tuning.clone(),
                        next_seed,
                        node_now,
                        Some(rec),
                    );
                    self.nemesis.record_reactive_fault(
                        now_master,
                        NemesisFaultEvent::RestartNode { node_id },
                    );
                }
                Err(bit_rot_err) => {
                    // Unrecoverable disk bit-rot on reboot -> quarantine node safely!
                    slot.core.enter_quarantine(bit_rot_err);
                }
            }
        }
        Ok(())
    }

    fn complete_disk_sync(&mut self, sync_ev: ScheduledDiskSync) -> Result<(), String> {
        let nid = sync_ev.node_id;
        let should_run = self
            .nodes
            .get(&nid)
            .map(|s| s.crashed_until_ns.is_none() && s.disk_boot_epoch == sync_ev.disk_epoch)
            .unwrap_or(false);
        if !should_run {
            return Ok(());
        }

        let node_now = self.clock.now_ns(nid);
        if let Some(slot) = self.nodes.get_mut(&nid) {
            match slot.disk.fsync() {
                Ok(()) => {
                    let sealed_idx = slot.disk.sealed_max_index();
                    slot.core.on_disk_synced(sync_ev.io_id, sealed_idx, node_now);
                }
                Err(err) => {
                    slot.core.enter_quarantine(err);
                }
            }
        }
        let allow_nem = self.nemesis_active;
        self.harvest_node_effects(nid, allow_nem)
    }

    fn harvest_node_effects(&mut self, nid: NodeId, allow_nemesis: bool) -> Result<(), String> {
        let effects = match self.nodes.get_mut(&nid) {
            Some(s) => s.core.drain_effects(),
            None => return Ok(()),
        };

        let now_master = self.clock.master_ns();
        let mut trigger_crash_before_commit = false;
        let mut trigger_crash_after_commit = false;

        for eff in effects {
            match eff {
                RaftEffect::SendRpc(env) => {
                    self.net.send(now_master, env);
                }
                RaftEffect::Persist(batch) => {
                    let io_id = batch.io_id;
                    let has_journal = batch
                        .entries
                        .iter()
                        .any(|e| matches!(e.kind, crate::types::RaftEntryKind::JournalBatch(_)));

                    if let Some(slot) = self.nodes.get_mut(&nid) {
                        if let Err(e) = slot.disk.stage_batch(&batch) {
                            slot.core.enter_quarantine(e);
                            continue;
                        }
                        let delay = slot.disk.effective_fsync_delay_ns(&mut self.prng);
                        let seq = self.next_disk_seq;
                        self.next_disk_seq = self.next_disk_seq.wrapping_add(1);
                        self.disk_sync_heap.push(Reverse(ScheduledDiskSync {
                            complete_at_ns: now_master.saturating_add(delay),
                            seq,
                            node_id: nid,
                            io_id,
                            disk_epoch: slot.disk_boot_epoch,
                        }));

                        // Surgical Nemesis Hook 1: Crash Leader right BEFORE commit (while batch is still dirty in page cache!)
                        if allow_nemesis
                            && has_journal
                            && slot.core.role == RaftRole::Leader
                            && self.nemesis.arm_crash_leader_before_commit
                            && self.down_nodes_count()
                                < self.nemesis.profile.max_simultaneous_down_nodes
                        {
                            self.nemesis.arm_crash_leader_before_commit = false;
                            trigger_crash_before_commit = true;
                        }
                    }
                }
                RaftEffect::ProposalCommitted {
                    client_proposal_id,
                    log_index,
                    term,
                    entry_blake3,
                    receipts,
                } => {
                    self.trace_hasher.update(b"COMMIT:");
                    self.trace_hasher.update(&log_index.to_le_bytes());
                    self.trace_hasher.update(&term.to_le_bytes());
                    self.trace_hasher.update(&entry_blake3);

                    if let Some(tracker) =
                        self.pending_proposals.remove(&(nid, client_proposal_id))
                    {
                        let applied = receipts.first().map(|r| r.applied).unwrap_or(false);
                        if let Some(h_op) = self.history.get_mut(tracker.history_idx) {
                            h_op.return_ts_ns = Some(now_master);
                            h_op.observed = Some(ClientOpReturn::WriteAck { applied });
                            if applied && tracker.is_write {
                                self.serial_txs.push(ObservedTransaction {
                                    tx_id: h_op.op_id as u64,
                                    invoke_ts_ns: h_op.invoke_ts_ns,
                                    return_ts_ns: now_master,
                                    write_version: Some(log_index),
                                    writes: tracker.write_keys,
                                    reads: BTreeMap::new(),
                                });
                            }
                        }
                    }

                    // Surgical Nemesis Hook 2: Crash Leader at the EXACT instant right AFTER commit!
                    if allow_nemesis
                        && self.nemesis.arm_crash_leader_after_commit
                        && self.down_nodes_count() < self.nemesis.profile.max_simultaneous_down_nodes
                    {
                        self.nemesis.arm_crash_leader_after_commit = false;
                        trigger_crash_after_commit = true;
                    }
                }
                RaftEffect::ProposalRejected {
                    client_proposal_id, ..
                } => {
                    // Rejected/stepped-down proposal remains `return_ts_ns = None` (indeterminate if already appended, or never executed if pre-append!).
                    self.pending_proposals.remove(&(nid, client_proposal_id));
                }
                RaftEffect::LinearizableReadReady {
                    client_read_id,
                    result,
                    ..
                } => {
                    if let Some(tracker) = self.pending_reads.remove(&(nid, client_read_id)) {
                        if let LedgerReadResult::AccountBalance {
                            total_fp, at_index, ..
                        } = result
                        {
                            if let Some(h_op) = self.history.get_mut(tracker.history_idx) {
                                h_op.return_ts_ns = Some(now_master);
                                h_op.observed = Some(ClientOpReturn::BalanceVal { total_fp });
                                if let Some(rk) = tracker.read_key {
                                    let mut reads = BTreeMap::new();
                                    reads.insert(rk, at_index);
                                    self.serial_txs.push(ObservedTransaction {
                                        tx_id: h_op.op_id as u64,
                                        invoke_ts_ns: h_op.invoke_ts_ns,
                                        return_ts_ns: now_master,
                                        write_version: None,
                                        writes: BTreeSet::new(),
                                        reads,
                                    });
                                }
                            }
                        }
                    }
                }
                RaftEffect::LinearizableReadRejected { client_read_id, .. } => {
                    self.pending_reads.remove(&(nid, client_read_id));
                }
                RaftEffect::MembershipChanged { .. } | RaftEffect::NodeQuarantined { .. } => {}
            }

            if trigger_crash_before_commit {
                self.nemesis.record_reactive_fault(
                    now_master,
                    NemesisFaultEvent::CrashNodeTornWrite {
                        node_id: nid,
                        reason: "surgical reactive CrashLeaderBeforeCommit + TornWrite".to_string(),
                    },
                );
                self.crash_node_now(nid, now_master, true);
                break;
            }
            if trigger_crash_after_commit {
                self.nemesis.record_reactive_fault(
                    now_master,
                    NemesisFaultEvent::CrashNodeTornWrite {
                        node_id: nid,
                        reason: "surgical reactive CrashLeaderJustAfterCommit".to_string(),
                    },
                );
                self.crash_node_now(nid, now_master, true);
                break;
            }
        }
        Ok(())
    }

    /// Submit a client `CreditDeposit` or `Transfer` to the current Leader (if any).
    pub fn submit_client_write(&mut self, client_id: u64, spec: ClientOpSpec) -> Option<usize> {
        let leader_id = self.active_leader()?;
        let invoke_ns = self.clock.master_ns();
        let node_now = self.clock.now_ns(leader_id);
        let req_id = self.next_client_req_id;
        self.next_client_req_id = self.next_client_req_id.wrapping_add(1);

        let mut write_keys = BTreeSet::new();
        let cmd = match &spec {
            ClientOpSpec::CreditDeposit {
                idempotency_key,
                account_id,
                asset,
                amount_fp,
            } => {
                write_keys.insert((*account_id, asset.clone()));
                LedgerCommand::CreditDeposit {
                    idempotency_key: idempotency_key.clone(),
                    account_id: *account_id,
                    asset: asset.clone(),
                    amount_fp: *amount_fp,
                    ts_ns: invoke_ns,
                }
            }
            ClientOpSpec::Transfer {
                idempotency_key,
                from_account,
                to_account,
                asset,
                amount_fp,
            } => {
                write_keys.insert((*from_account, asset.clone()));
                write_keys.insert((*to_account, asset.clone()));
                LedgerCommand::Transfer {
                    idempotency_key: idempotency_key.clone(),
                    from_account: *from_account,
                    to_account: *to_account,
                    asset: asset.clone(),
                    amount_fp: *amount_fp,
                    ts_ns: invoke_ns,
                }
            }
            ClientOpSpec::ReadBalance { .. } => return None,
        };

        let op_id = self.history.len();
        self.history.push(ConcurrentHistoryOp {
            op_id,
            client_id,
            invoke_ts_ns: invoke_ns,
            return_ts_ns: None,
            spec,
            observed: None,
        });
        self.pending_proposals.insert(
            (leader_id, req_id),
            PendingClientTracker {
                history_idx: op_id,
                is_write: true,
                write_keys,
                read_key: None,
            },
        );

        if let Some(slot) = self.nodes.get_mut(&leader_id) {
            slot.core
                .propose_ledger_commands(req_id, vec![cmd], node_now, true);
        }
        let allow_nem = self.nemesis_active;
        let _ = self.harvest_node_effects(leader_id, allow_nem);
        Some(op_id)
    }

    /// Submit a linearizable `ReadBalance` query to `target_node` (typically Leader, or even a stale partitioned old Leader to prove it NEVER returns stale data!).
    pub fn submit_client_read(
        &mut self,
        client_id: u64,
        target_node: NodeId,
        account_id: u64,
        asset: &str,
        mode: ReadConsistencyMode,
    ) -> Option<usize> {
        let is_up = self
            .nodes
            .get(&target_node)
            .map(|s| s.crashed_until_ns.is_none())
            .unwrap_or(false);
        if !is_up {
            return None;
        }
        let invoke_ns = self.clock.master_ns();
        let node_now = self.clock.now_ns(target_node);
        let req_id = self.next_client_req_id;
        self.next_client_req_id = self.next_client_req_id.wrapping_add(1);

        let op_id = self.history.len();
        self.history.push(ConcurrentHistoryOp {
            op_id,
            client_id,
            invoke_ts_ns: invoke_ns,
            return_ts_ns: None,
            spec: ClientOpSpec::ReadBalance {
                account_id,
                asset: asset.to_string(),
            },
            observed: None,
        });
        self.pending_reads.insert(
            (target_node, req_id),
            PendingClientTracker {
                history_idx: op_id,
                is_write: false,
                write_keys: BTreeSet::new(),
                read_key: Some((account_id, asset.to_string())),
            },
        );

        if let Some(slot) = self.nodes.get_mut(&target_node) {
            slot.core.request_linearizable_read(
                req_id,
                LedgerReadQuery::GetAccountBalance {
                    account_id,
                    asset: asset.to_string(),
                },
                mode,
                node_now,
            );
        }
        let allow_nem = self.nemesis_active;
        let _ = self.harvest_node_effects(target_node, allow_nem);
        Some(op_id)
    }

    /// Continuous step-by-step verification of Raft Safety + Double-Entry Financial Conservation!
    pub fn assert_step_safety_invariants(&mut self) -> Result<(), String> {
        for (&nid, slot) in &self.nodes {
            if slot.crashed_until_ns.is_some() || slot.core.role == RaftRole::Quarantined {
                continue;
            }

            // 1. Election Safety: at most ONE Leader per Term!
            if slot.core.role == RaftRole::Leader {
                let term = slot.core.hard_state.current_term;
                if let Some(&existing_leader) = self.leaders_by_term.get(&term) {
                    if existing_leader != nid {
                        return Err(format!(
                            "ELECTION SAFETY VIOLATION in term {term}: both node {existing_leader} and node {nid} became Leader!"
                        ));
                    }
                } else {
                    self.leaders_by_term.insert(term, nid);
                }
            }

            // 2. Durability Invariant: `last_applied <= commit_index <= persisted_index <= last_log_index`
            // (except when a node just stepped down after committing its own removal in C_new).
            if slot.core.state_machine.last_applied_index > slot.core.commit_index {
                return Err(format!(
                    "node {nid}: last_applied_index ({}) > commit_index ({})",
                    slot.core.state_machine.last_applied_index, slot.core.commit_index
                ));
            }
            if slot.core.active_config().is_voter(nid)
                && slot.core.commit_index > slot.core.persisted_index
            {
                return Err(format!(
                    "node {nid}: commit_index ({}) > persisted_index ({}) — un-fsynced commit!",
                    slot.core.commit_index, slot.core.persisted_index
                ));
            }

            // 3. Committed Log Immutability & Log Matching against Global Oracle!
            let start_check = slot.core.first_log_index();
            let end_check = slot.core.commit_index.min(slot.core.last_log_index());
            for idx in start_check..=end_check {
                if let Some(entry) = slot.core.entry_at(idx) {
                    if let Some(&(expected_term, expected_hash)) =
                        self.committed_entries_oracle.get(&idx)
                    {
                        if entry.term != expected_term || entry.wal_blake3 != expected_hash {
                            return Err(format!(
                                "STATE MACHINE / LOG MATCHING VIOLATION at committed index {idx} on node {nid}: expected term={expected_term}, got term={}",
                                entry.term
                            ));
                        }
                    } else {
                        self.committed_entries_oracle
                            .insert(idx, (entry.term, entry.wal_blake3));
                    }
                }
            }

            // 4. Double-Entry Financial Conservation Law (`00-hyperion-contract.md` §5.2)!
            if !slot.core.state_machine.verify_all_assets_conserved() {
                return Err(format!(
                    "CRITICAL DOUBLE-ENTRY CONSERVATION BREACH on node {nid}!"
                ));
            }
        }
        Ok(())
    }

    /// Execute a full adversarial VOPR workload + Nemesis chaos phase + Healing Liveness convergence
    /// + WGL Linearizability + Adya Strict-1SR Serializability verification!
    pub fn run_full_vopr_campaign(
        &mut self,
        chaos_steps: u64,
        recovery_steps: u64,
    ) -> Result<VoprRunSummary, String> {
        let step_ns = 10_000_000; // 10 ms simulation quanta

        // Warmup 40 steps (400 ms) to elect initial Leader & fund accounts.
        for _ in 0..40 {
            self.step_simulation(step_ns, false)?;
        }
        for acct in 1..=4u64 {
            let idem = format!("seed-{}-init-dep-{acct}", self.seed);
            let _ = self.submit_client_write(
                acct,
                ClientOpSpec::CreditDeposit {
                    idempotency_key: idem,
                    account_id: acct,
                    asset: "USDT".to_string(),
                    amount_fp: 1_000 * FP_SCALE_U128,
                },
            );
            self.step_simulation(step_ns, false)?;
        }

        // Chaos Phase: concurrent writes, transfers, linearizable reads (including probing minority/stale leaders!),
        // asymmetric partitions, packet storms, fsync stalls, torn writes, and leader crashes before/after commit!
        let mut idem_seq = 1u64;
        for step_i in 0..chaos_steps {
            self.step_simulation(step_ns, true)?;

            if step_i % 3 == 0 {
                let acct = self.prng.range_u64(1, 4);
                let client_id = self.prng.range_u64(1, 8);
                let op_roll = self.prng.range_u64(0, 99);
                if op_roll < 35 {
                    let amt = u128::from(self.prng.range_u64(1, 25)) * FP_SCALE_U128;
                    let idem = format!("s{}-dep-{idem_seq}", self.seed);
                    idem_seq += 1;
                    let _ = self.submit_client_write(
                        client_id,
                        ClientOpSpec::CreditDeposit {
                            idempotency_key: idem,
                            account_id: acct,
                            asset: "USDT".to_string(),
                            amount_fp: amt,
                        },
                    );
                } else if op_roll < 70 {
                    let mut to_acct = self.prng.range_u64(1, 4);
                    if to_acct == acct {
                        to_acct = (acct % 4) + 1;
                    }
                    let amt = u128::from(self.prng.range_u64(1, 15)) * FP_SCALE_U128;
                    let idem = format!("s{}-xfer-{idem_seq}", self.seed);
                    idem_seq += 1;
                    let _ = self.submit_client_write(
                        client_id,
                        ClientOpSpec::Transfer {
                            idempotency_key: idem,
                            from_account: acct,
                            to_account: to_acct,
                            asset: "USDT".to_string(),
                            amount_fp: amt,
                        },
                    );
                } else {
                    // Deliberately route 30% of reads to random nodes (including potentially partitioned stale leaders!)
                    // and 70% to current active_leader, alternating `ReadIndex` and `LeaderLeaseOrReadIndex`!
                    let all_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
                    let target = if self.prng.chance_ppm(300_000) {
                        all_ids[self.prng.range_usize(all_ids.len())]
                    } else {
                        self.active_leader().unwrap_or(all_ids[0])
                    };
                    let mode = if self.prng.chance_ppm(500_000) {
                        ReadConsistencyMode::ReadIndex
                    } else {
                        ReadConsistencyMode::LeaderLeaseOrReadIndex
                    };
                    let _ = self.submit_client_read(client_id, target, acct, "USDT", mode);
                }
            }
        }

        // Recovery & Quiescence Phase: heal partitions, clear fsync stalls, reboot all crashed nodes,
        // submit one final barrier write + linearizable reads, and prove 100% Liveness & State Convergence!
        self.nemesis_active = false;
        self.nemesis.arm_crash_leader_before_commit = false;
        self.nemesis.arm_crash_leader_after_commit = false;
        let all_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
        self.net
            .set_default_policy(crate::sim::network::DirectedLinkPolicy::default());
        self.net.heal_all(&all_ids);
        for slot in self.nodes.values_mut() {
            slot.disk.extra_fsync_stall_ns = 0;
        }

        for r_step in 0..recovery_steps {
            self.step_simulation(step_ns, false)?;
            if r_step == recovery_steps / 2 {
                let _ = self.submit_client_write(
                    99,
                    ClientOpSpec::CreditDeposit {
                        idempotency_key: format!("s{}-final-liveness-barrier", self.seed),
                        account_id: 1,
                        asset: "USDT".to_string(),
                        amount_fp: 7 * FP_SCALE_U128,
                    },
                );
            }
        }

        // Final linearizable read across all 4 accounts after healing.
        if let Some(ldr) = self.active_leader() {
            for acct in 1..=4u64 {
                let _ = self.submit_client_read(
                    100 + acct,
                    ldr,
                    acct,
                    "USDT",
                    ReadConsistencyMode::ReadIndex,
                );
            }
            for _ in 0..15 {
                self.step_simulation(step_ns, false)?;
            }
        }

        // Verify Liveness & Exact Replica Convergence across all non-quarantined voting nodes!
        let leader = self
            .active_leader()
            .ok_or_else(|| format!("LIVENESS FAILURE (seed={}): no Leader elected after healing!", self.seed))?;

        let leader_slot = &self.nodes[&leader];
        let expected_commit = leader_slot.core.commit_index;
        let expected_root = leader_slot.core.state_machine.state_root_blake3();
        let final_term = leader_slot.core.hard_state.current_term;

        for (&nid, slot) in &self.nodes {
            if slot.core.role == RaftRole::Quarantined {
                continue;
            }
            if !slot.core.active_config().contains(nid) {
                continue;
            }
            if slot.core.commit_index != expected_commit {
                return Err(format!(
                    "REPLICA CONVERGENCE FAILURE (seed={}): node {nid} commit_index={} != leader {leader} commit_index={expected_commit}",
                    self.seed, slot.core.commit_index
                ));
            }
            let node_root = slot.core.state_machine.state_root_blake3();
            if node_root != expected_root {
                return Err(format!(
                    "BLAKE3 STATE ROOT DIVERGENCE (seed={}): node {nid} state_root != leader {leader} state_root!",
                    self.seed
                ));
            }
            // Scrub physical simulated disk on every healthy node to prove zero undetected corruption!
            slot.disk
                .scrub_and_verify()
                .map_err(|e| format!("post-run disk scrub failed on node {nid}: {e}"))?;
        }

        // Verify Concurrent Execution History Linearizability (Wing & Gong / Porcupine WGL)!
        let lin_checker = LinearizabilityChecker::default();
        let lin_explored = match lin_checker.verify(&self.history) {
            LinearizabilityVerdict::Linearizable {
                explored_states, ..
            } => explored_states,
            LinearizabilityVerdict::Violation { reason, .. } => {
                return Err(format!("WGL LINEARIZABILITY VIOLATION (seed={}): {reason}", self.seed));
            }
        };

        // Verify Multi-Account Strict Serializability (`Strict-1SR` Adya DSG + Real-Time Tarjan SCC)!
        let ssr_edges = match StrictSerializabilityVerifier::verify(&self.serial_txs) {
            SerializabilityVerdict::StrictlySerializable { edge_count, .. } => edge_count,
            SerializabilityVerdict::AnomalyDetected {
                reason,
                cycle_tx_ids,
            } => {
                return Err(format!(
                    "STRICT SERIALIZABILITY VIOLATION (seed={}): {reason} cycle={cycle_tx_ids:?}",
                    self.seed
                ));
            }
        };

        let committed_writes = self
            .history
            .iter()
            .filter(|op| matches!(op.observed, Some(ClientOpReturn::WriteAck { .. })))
            .count();
        let linearizable_reads = self
            .history
            .iter()
            .filter(|op| matches!(op.observed, Some(ClientOpReturn::BalanceVal { .. })))
            .count();
        let indeterminate_ops = self.history.iter().filter(|op| !op.is_completed()).count();

        let trace_digest_blake3 = *self.trace_hasher.finalize().as_bytes();

        Ok(VoprRunSummary {
            seed: self.seed,
            steps_executed: 40 + chaos_steps + recovery_steps + 19,
            final_master_ns: self.clock.master_ns(),
            final_leader: Some(leader),
            final_term,
            final_commit_index: expected_commit,
            converged_state_root_blake3: expected_root,
            trace_digest_blake3,
            total_client_ops: self.history.len(),
            committed_writes,
            linearizable_reads,
            indeterminate_ops,
            nemesis_faults_injected: self.nemesis.injected_log.len(),
            torn_writes_recovered: self.torn_writes_recovered,
            linearizability_explored_states: lin_explored,
            strict_serializability_edges: ssr_edges,
        })
    }
}
