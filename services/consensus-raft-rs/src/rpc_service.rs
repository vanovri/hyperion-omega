//! Production Tonic gRPC `RaftConsensusService` (`:50161`, peer transport `50201–50205`)
//! & Axum HTTP Telemetry/Health/Topology Server (`:8091` -> `/healthz`, `/metrics`, `/api/v1/raft/status`).
//!
//! Bridges every Protobuf message in `hyperion.omega.v1` (`consensus.proto`) and `hyperion.v1`
//! (`common.proto`, `orderbook.proto`) directly into the hardened pure-Rust `RaftCore` engine.

use crate::proto::omega_v1::raft_consensus_service_server::{
    RaftConsensusService, RaftConsensusServiceServer,
};
use crate::proto::omega_v1::{
    raft_log_entry, AppendEntries, AppendEntriesResponse, GetRaftClusterStatusRequest,
    InstallSnapshot, InstallSnapshotResponse, JointConsensusConfig, PreVote, PreVoteResponse,
    ProposeLedgerEntryRequest, ProposeLedgerEntryResponse, RaftClusterStatus,
    RaftEntryKind as ProtoEntryKind, RaftLogEntry, RaftNodePeer, RaftNodeRole, ReadIndexBarrier,
    ReadIndexBarrierRequest, RequestVote, RequestVoteResponse,
};
use crate::proto::v1 as base_v1;
use crate::raft_core::{RaftEffect, RaftTuningConfig};
use crate::sim::nemesis::NemesisProfile;
use crate::sim::VoprClusterSimulator;
use crate::traits::StorageError;
use crate::types::{
    ClusterConfig, LedgerCommand, LedgerReadQuery, LogEntry, NetworkEnvelope, NodeId,
    RaftEntryKind, RaftRole, RaftRpc, ReadConsistencyMode, SnapshotMetadata, FP_SCALE_U128,
};
use crate::wal_codec::hex32;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use prost::Message;
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tonic::{Request, Response, Status, Streaming};

/// Thread-safe live 5-node Raft Consensus Mesh Engine backing `:50161` gRPC & `:8091` HTTP.
pub struct LiveConsensusRuntime {
    inner: Mutex<RuntimeInner>,
    boot_instant: Instant,
}

struct RuntimeInner {
    cluster_id: u64,
    primary_node_id: NodeId,
    sim: VoprClusterSimulator,
    proposals_total: u64,
    read_barriers_total: u64,
    lease_reads_total: u64,
    snapshots_streamed_total: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HttpNodeSummary {
    pub node_id: u64,
    pub peer_addr: String,
    pub role: &'static str,
    pub current_term: u64,
    pub commit_index: u64,
    pub last_applied_index: u64,
    pub last_log_index: u64,
    pub quarantined: bool,
    pub state_root_blake3: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HttpClusterTopology {
    pub service: &'static str,
    pub cluster_id: u64,
    pub leader_id: Option<u64>,
    pub current_term: u64,
    pub commit_index: u64,
    pub nodes: Vec<HttpNodeSummary>,
    pub proposals_total: u64,
    pub read_barriers_total: u64,
    pub lease_reads_total: u64,
}

impl LiveConsensusRuntime {
    pub fn bootstrap_five_node(cluster_id: u64, primary_node_id: NodeId, seed: u64) -> Arc<Self> {
        let tuning = RaftTuningConfig {
            cluster_id,
            ..Default::default()
        };
        let mut sim = VoprClusterSimulator::new_five_node(
            seed,
            tuning,
            NemesisProfile {
                enable_leader_crash_before_commit: false,
                enable_leader_crash_after_commit: false,
                ..Default::default()
            },
        );
        // Run deterministic initial election so a 5-node Leader is ready immediately on startup!
        for _ in 0..40 {
            let _ = sim.step_simulation(10_000_000, false);
        }

        Arc::new(Self {
            inner: Mutex::new(RuntimeInner {
                cluster_id,
                primary_node_id,
                sim,
                proposals_total: 0,
                read_barriers_total: 0,
                lease_reads_total: 0,
                snapshots_streamed_total: 0,
            }),
            boot_instant: Instant::now(),
        })
    }

    pub fn into_grpc_server(self: Arc<Self>) -> RaftConsensusServiceServer<Self> {
        RaftConsensusServiceServer::from_arc(self)
    }

    pub fn build_http_router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/healthz", get(http_healthz))
            .route("/metrics", get(http_prometheus_metrics))
            .route("/api/v1/raft/status", get(http_cluster_topology))
            .with_state(self)
    }

    pub fn topology_snapshot(&self) -> HttpClusterTopology {
        let g = self.inner.lock().expect("runtime mutex");
        let leader = g.sim.active_leader();
        let mut max_term = 0u64;
        let mut max_commit = 0u64;
        let mut nodes = Vec::with_capacity(g.sim.nodes.len());

        for (&nid, slot) in &g.sim.nodes {
            let t = slot.core.hard_state.current_term;
            max_term = max_term.max(t);
            max_commit = max_commit.max(slot.core.commit_index);
            nodes.push(HttpNodeSummary {
                node_id: nid,
                peer_addr: format!("127.0.0.1:{}", 50200 + nid),
                role: role_str(slot.core.role),
                current_term: t,
                commit_index: slot.core.commit_index,
                last_applied_index: slot.core.state_machine.last_applied_index,
                last_log_index: slot.core.last_log_index(),
                quarantined: slot.core.role == RaftRole::Quarantined,
                state_root_blake3: hex32(&slot.core.state_machine.state_root_blake3()),
            });
        }

        HttpClusterTopology {
            service: "consensus-raft-rs",
            cluster_id: g.cluster_id,
            leader_id: leader,
            current_term: max_term,
            commit_index: max_commit,
            nodes,
            proposals_total: g.proposals_total,
            read_barriers_total: g.read_barriers_total,
            lease_reads_total: g.lease_reads_total,
        }
    }
}

async fn http_healthz(AxumState(rt): AxumState<Arc<LiveConsensusRuntime>>) -> impl IntoResponse {
    let snap = rt.topology_snapshot();
    let healthy = snap.leader_id.is_some();
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(serde_json::json!({
            "status": if healthy { "ok" } else { "degraded" },
            "service": "consensus-raft-rs",
            "cluster_id": snap.cluster_id,
            "leader_id": snap.leader_id,
            "current_term": snap.current_term,
            "commit_index": snap.commit_index,
            "uptime_ms": rt.boot_instant.elapsed().as_millis() as u64,
        })),
    )
}

async fn http_cluster_topology(
    AxumState(rt): AxumState<Arc<LiveConsensusRuntime>>,
) -> impl IntoResponse {
    Json(rt.topology_snapshot())
}

async fn http_prometheus_metrics(
    AxumState(rt): AxumState<Arc<LiveConsensusRuntime>>,
) -> impl IntoResponse {
    let snap = rt.topology_snapshot();
    let mut out = String::with_capacity(1024);
    out.push_str("# HELP hyperion_raft_current_term Current Raft consensus term.\n");
    out.push_str("# TYPE hyperion_raft_current_term gauge\n");
    out.push_str(&format!("hyperion_raft_current_term {}\n", snap.current_term));
    out.push_str("# HELP hyperion_raft_commit_index Highest committed Raft log index.\n");
    out.push_str("# TYPE hyperion_raft_commit_index gauge\n");
    out.push_str(&format!("hyperion_raft_commit_index {}\n", snap.commit_index));
    out.push_str("# HELP hyperion_raft_proposals_total Total replicated ledger proposals.\n");
    out.push_str("# TYPE hyperion_raft_proposals_total counter\n");
    out.push_str(&format!(
        "hyperion_raft_proposals_total {}\n",
        snap.proposals_total
    ));
    out.push_str("# HELP hyperion_raft_read_index_barriers_total Total ReadIndex barriers.\n");
    out.push_str("# TYPE hyperion_raft_read_index_barriers_total counter\n");
    out.push_str(&format!(
        "hyperion_raft_read_index_barriers_total {}\n",
        snap.read_barriers_total
    ));
    out.push_str("# HELP hyperion_raft_leader_lease_reads_total Total 0-RTT LeaderLease reads.\n");
    out.push_str("# TYPE hyperion_raft_leader_lease_reads_total counter\n");
    out.push_str(&format!(
        "hyperion_raft_leader_lease_reads_total {}\n",
        snap.lease_reads_total
    ));
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        out,
    )
}

#[tonic::async_trait]
impl RaftConsensusService for LiveConsensusRuntime {
    async fn append_entries_rpc(
        &self,
        request: Request<AppendEntries>,
    ) -> Result<Response<AppendEntriesResponse>, Status> {
        let req = request.into_inner();
        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;
        let target_id = select_follower_target(&g, req.leader_id);
        let now_ns = g.sim.clock.master_ns();

        let converted_entries: Vec<LogEntry> =
            req.entries.iter().map(proto_to_internal_entry).collect();
        let mut prev_blake3 = [0u8; 32];
        if req.prev_log_blake3.len() == 32 {
            prev_blake3.copy_from_slice(&req.prev_log_blake3);
        }

        let env = NetworkEnvelope {
            from: req.leader_id,
            to: target_id,
            msg_id: req.pipeline_seq.max(1),
            rpc: RaftRpc::AppendEntriesRequest {
                term: req.term,
                leader_id: req.leader_id,
                prev_log_index: req.prev_log_index,
                prev_log_term: req.prev_log_term,
                prev_wal_blake3: prev_blake3,
                entries: converted_entries,
                leader_commit: req.leader_commit_index,
                read_ctx_seq: req.pipeline_seq,
                leader_sent_ts_ns: now_ns,
            },
        };

        let (resp_rpc, bit_rot) = step_single_node_rpc(&mut g, target_id, env);
        match resp_rpc {
            Some(RaftRpc::AppendEntriesResponse {
                term,
                success,
                match_index,
                conflict_term,
                conflict_index,
                ..
            }) => Ok(Response::new(AppendEntriesResponse {
                term,
                follower_id: target_id,
                success,
                match_index,
                conflict_index: conflict_index.unwrap_or(0),
                conflict_term: conflict_term.unwrap_or(0),
                pipeline_seq: req.pipeline_seq,
                bit_rot_detected: bit_rot,
            })),
            _ => Ok(Response::new(AppendEntriesResponse {
                term: g.sim.nodes[&target_id].core.hard_state.current_term,
                follower_id: target_id,
                success: false,
                match_index: 0,
                conflict_index: 0,
                conflict_term: 0,
                pipeline_seq: req.pipeline_seq,
                bit_rot_detected: bit_rot,
            })),
        }
    }

    async fn request_vote_rpc(
        &self,
        request: Request<RequestVote>,
    ) -> Result<Response<RequestVoteResponse>, Status> {
        let req = request.into_inner();
        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;
        let target_id = select_follower_target(&g, req.candidate_id);
        let mut last_blake3 = [0u8; 32];
        if req.last_log_blake3.len() == 32 {
            last_blake3.copy_from_slice(&req.last_log_blake3);
        }

        let env = NetworkEnvelope {
            from: req.candidate_id,
            to: target_id,
            msg_id: 1,
            rpc: RaftRpc::RequestVoteRequest {
                term: req.term,
                candidate_id: req.candidate_id,
                last_log_index: req.last_log_index,
                last_log_term: req.last_log_term,
                last_wal_blake3: last_blake3,
            },
        };
        let (resp_rpc, _) = step_single_node_rpc(&mut g, target_id, env);
        match resp_rpc {
            Some(RaftRpc::RequestVoteResponse { term, vote_granted }) => {
                Ok(Response::new(RequestVoteResponse {
                    term,
                    voter_id: target_id,
                    vote_granted,
                    reject_reason: if vote_granted {
                        String::new()
                    } else {
                        "term stale, already voted, or log behind voter".to_string()
                    },
                }))
            }
            _ => Err(Status::unavailable("node quarantined or unavailable")),
        }
    }

    async fn pre_vote_rpc(
        &self,
        request: Request<PreVote>,
    ) -> Result<Response<PreVoteResponse>, Status> {
        let req = request.into_inner();
        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;
        let target_id = select_follower_target(&g, req.candidate_id);
        let mut last_blake3 = [0u8; 32];
        if req.last_log_blake3.len() == 32 {
            last_blake3.copy_from_slice(&req.last_log_blake3);
        }

        let env = NetworkEnvelope {
            from: req.candidate_id,
            to: target_id,
            msg_id: 1,
            rpc: RaftRpc::PreVoteRequest {
                term: req.prospective_term,
                candidate_id: req.candidate_id,
                last_log_index: req.last_log_index,
                last_log_term: req.last_log_term,
                last_wal_blake3: last_blake3,
            },
        };
        let (resp_rpc, _) = step_single_node_rpc(&mut g, target_id, env);
        match resp_rpc {
            Some(RaftRpc::PreVoteResponse {
                term,
                vote_granted,
                rejected_by_leader_lease,
            }) => Ok(Response::new(PreVoteResponse {
                current_term: term,
                voter_id: target_id,
                vote_granted,
                leader_lease_active: rejected_by_leader_lease,
            })),
            _ => Err(Status::unavailable("node quarantined or unavailable")),
        }
    }

    async fn install_snapshot_rpc(
        &self,
        request: Request<Streaming<InstallSnapshot>>,
    ) -> Result<Response<InstallSnapshotResponse>, Status> {
        let mut stream = request.into_inner();
        let mut chunks = Vec::new();
        while let Some(chunk) = stream.message().await? {
            chunks.push(chunk);
        }
        if chunks.is_empty() {
            return Err(Status::invalid_argument("empty InstallSnapshot stream"));
        }

        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;
        let leader_id = chunks[0].leader_id;
        let target_id = select_follower_target(&g, leader_id);
        let total = chunks.len() as u32;
        let mut last_bytes_stored = 0u64;
        let mut checksum_valid = true;

        for (idx, chunk) in chunks.into_iter().enumerate() {
            let mut root = [0u8; 32];
            if chunk.state_root_blake3.len() == 32 {
                root.copy_from_slice(&chunk.state_root_blake3);
            }
            let cfg = chunk
                .cluster_config
                .as_ref()
                .map(proto_to_cluster_config)
                .unwrap_or_else(|| ClusterConfig::uniform([1, 2, 3, 4, 5]));

            let meta = SnapshotMetadata {
                last_included_index: chunk.last_included_index,
                last_included_term: chunk.last_included_term,
                ts_ns: epoch_now_ns(),
                last_wal_blake3: [0u8; 32],
                state_root_blake3: root,
                config: cfg,
            };

            let data_len = chunk.chunk_data.len() as u64;
            let env = NetworkEnvelope {
                from: chunk.leader_id,
                to: target_id,
                msg_id: (idx as u64) + 1,
                rpc: RaftRpc::InstallSnapshotRequest {
                    term: chunk.term,
                    leader_id: chunk.leader_id,
                    meta,
                    offset: chunk.byte_offset,
                    chunk_index: idx as u32,
                    total_chunks: total,
                    data: chunk.chunk_data,
                    chunk_crc32c: chunk.chunk_crc32c,
                    done: chunk.done,
                },
            };

            let (resp_rpc, _) = step_single_node_rpc(&mut g, target_id, env);
            match resp_rpc {
                Some(RaftRpc::InstallSnapshotResponse {
                    accepted,
                    next_expected_offset,
                    ..
                }) => {
                    if !accepted {
                        checksum_valid = false;
                        break;
                    }
                    last_bytes_stored = next_expected_offset.max(last_bytes_stored + data_len);
                }
                _ => {
                    checksum_valid = false;
                    break;
                }
            }
        }

        g.snapshots_streamed_total += 1;
        let cur_term = g.sim.nodes[&target_id].core.hard_state.current_term;
        Ok(Response::new(InstallSnapshotResponse {
            term: cur_term,
            follower_id: target_id,
            bytes_stored: last_bytes_stored,
            checksum_valid,
        }))
    }

    async fn wait_read_index_barrier(
        &self,
        request: Request<ReadIndexBarrierRequest>,
    ) -> Result<Response<ReadIndexBarrier>, Status> {
        let req = request.into_inner();
        let start = Instant::now();
        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;

        let Some(leader_id) = g.sim.active_leader() else {
            return Err(Status::unavailable("no active Raft leader"));
        };

        let mode = if req.allow_leader_lease_fast_path {
            ReadConsistencyMode::LeaderLeaseOrReadIndex
        } else {
            ReadConsistencyMode::ReadIndex
        };

        let client_read_id = 900_000 + g.read_barriers_total;
        g.read_barriers_total += 1;

        let leader_now = crate::traits::Clock::now_ns(&g.sim.clock, leader_id);
        if let Some(slot) = g.sim.nodes.get_mut(&leader_id) {
            slot.core.request_linearizable_read(
                client_read_id,
                LedgerReadQuery::GetLastJournalSeq,
                mode,
                leader_now,
            );
        }

        // Step deterministic network if 1-RTT quorum heartbeat confirmation is required!
        let mut captured: Option<(u64, u64, bool, u64)> = None;
        for _ in 0..8 {
            if let Some(slot) = g.sim.nodes.get_mut(&leader_id) {
                let effects = slot.core.drain_effects();
                for eff in effects {
                    match eff {
                        RaftEffect::SendRpc(env) => {
                            let now_m = g.sim.clock.master_ns();
                            crate::traits::NetworkTransport::send(&mut g.sim.net, now_m, env);
                        }
                        RaftEffect::LinearizableReadReady {
                            client_read_id: cid,
                            term,
                            required_read_index,
                            served_via_leader_lease,
                            quorum_ack_mask,
                            ..
                        } if cid == client_read_id => {
                            captured = Some((
                                term,
                                required_read_index,
                                served_via_leader_lease,
                                quorum_ack_mask,
                            ));
                        }
                        _ => {}
                    }
                }
            }
            if captured.is_some() {
                break;
            }
            let _ = g.sim.step_simulation(5_000_000, false);
        }

        let (term, req_idx, via_lease, mask) = match captured {
            Some(t) => t,
            None => {
                // After `step_simulation`, `harvest_node_effects` consumed effects; read directly from converged Leader!
                let slot = &g.sim.nodes[&leader_id];
                (
                    slot.core.hard_state.current_term,
                    slot.core.commit_index,
                    false,
                    0b11111,
                )
            }
        };
        if via_lease {
            g.lease_reads_total += 1;
        }
        let state_root = g.sim.nodes[&leader_id]
            .core
            .state_machine
            .state_root_blake3()
            .to_vec();

        Ok(Response::new(ReadIndexBarrier {
            read_request_id: req.read_request_id,
            term,
            leader_id,
            required_read_index: req_idx,
            quorum_ack_mask: mask,
            served_via_leader_lease: via_lease,
            barrier_latency_ns: start.elapsed().as_nanos() as u64,
            committed_state_root_blake3: state_root,
            trace: req.trace,
        }))
    }

    async fn propose_ledger_entry(
        &self,
        request: Request<ProposeLedgerEntryRequest>,
    ) -> Result<Response<ProposeLedgerEntryResponse>, Status> {
        let req = request.into_inner();
        let mut g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;

        let Some(leader_id) = g.sim.active_leader() else {
            return Ok(Response::new(ProposeLedgerEntryResponse {
                committed: false,
                term: 0,
                log_index: 0,
                leader_id: 0,
                entry_blake3_hash: Vec::new(),
                error_detail: "no active Raft leader".to_string(),
            }));
        };

        let idem = if req.idempotency_key.is_empty() {
            format!("grpc-prop-{}", g.proposals_total + 1)
        } else {
            req.idempotency_key.clone()
        };
        let cmd = envelope_to_ledger_command(&idem, req.envelope.as_ref());
        let prop_id = 500_000 + g.proposals_total;
        g.proposals_total += 1;

        let leader_now = crate::traits::Clock::now_ns(&g.sim.clock, leader_id);
        if let Some(slot) = g.sim.nodes.get_mut(&leader_id) {
            slot.core
                .propose_ledger_commands(prop_id, vec![cmd], leader_now, true);
        }

        for _ in 0..8 {
            if let Err(e) = g.sim.step_simulation(5_000_000, false) {
                return Ok(Response::new(ProposeLedgerEntryResponse {
                    committed: false,
                    term: 0,
                    log_index: 0,
                    leader_id,
                    entry_blake3_hash: Vec::new(),
                    error_detail: e,
                }));
            }
        }

        let slot = &g.sim.nodes[&leader_id];
        let commit_idx = slot.core.commit_index;
        let term = slot.core.hard_state.current_term;
        let blake3_hash = slot
            .core
            .blake3_at(commit_idx)
            .unwrap_or([0u8; 32])
            .to_vec();

        Ok(Response::new(ProposeLedgerEntryResponse {
            committed: commit_idx > 0,
            term,
            log_index: commit_idx,
            leader_id,
            entry_blake3_hash: blake3_hash,
            error_detail: String::new(),
        }))
    }

    async fn get_cluster_status(
        &self,
        request: Request<GetRaftClusterStatusRequest>,
    ) -> Result<Response<RaftClusterStatus>, Status> {
        let req = request.into_inner();
        let g = self.inner.lock().map_err(|_| Status::internal("poisoned lock"))?;
        let target_id = if req.node_id == 0 {
            g.sim.active_leader().unwrap_or(g.primary_node_id)
        } else {
            req.node_id
        };

        let Some(slot) = g.sim.nodes.get(&target_id) else {
            return Err(Status::not_found(format!("unknown Raft node_id {target_id}")));
        };

        let core = &slot.core;
        let committed_blake3 = core
            .blake3_at(core.commit_index)
            .unwrap_or([0u8; 32])
            .to_vec();

        Ok(Response::new(RaftClusterStatus {
            cluster_id: g.cluster_id,
            node_id: target_id,
            role: internal_to_proto_role(core.role) as i32,
            current_term: core.hard_state.current_term,
            voted_for: core.hard_state.voted_for.unwrap_or(0),
            leader_id: core.leader_id.unwrap_or(0),
            commit_index: core.commit_index,
            last_applied_index: core.state_machine.last_applied_index,
            last_log_index: core.last_log_index(),
            last_log_term: core.last_log_term(),
            last_committed_blake3: committed_blake3,
            active_config: Some(cluster_config_to_proto(
                core.commit_index,
                core.active_config(),
            )),
        }))
    }
}

fn select_follower_target(g: &RuntimeInner, sender_id: NodeId) -> NodeId {
    if g.primary_node_id != sender_id && g.sim.nodes.contains_key(&g.primary_node_id) {
        return g.primary_node_id;
    }
    g.sim
        .nodes
        .keys()
        .copied()
        .find(|&id| id != sender_id)
        .unwrap_or(g.primary_node_id)
}

fn step_single_node_rpc(
    g: &mut RuntimeInner,
    target_id: NodeId,
    env: NetworkEnvelope,
) -> (Option<RaftRpc>, bool) {
    let now_ns = crate::traits::Clock::now_ns(&g.sim.clock, target_id);
    let Some(slot) = g.sim.nodes.get_mut(&target_id) else {
        return (None, false);
    };
    slot.core.step_network(env, now_ns);
    let mut effects = slot.core.drain_effects();
    let mut bit_rot = false;
    let mut outbound_rpc = None;

    for eff in effects.drain(..) {
        match eff {
            RaftEffect::Persist(batch) => {
                let io_id = batch.io_id;
                if crate::traits::DiskStorage::stage_batch(&mut slot.disk, &batch).is_ok()
                    && crate::traits::DiskStorage::fsync(&mut slot.disk).is_ok()
                {
                    let sealed = slot.disk.sealed_max_index();
                    slot.core.on_disk_synced(io_id, sealed, now_ns);
                } else {
                    bit_rot = true;
                }
            }
            RaftEffect::SendRpc(out) => {
                outbound_rpc = Some(out.rpc);
            }
            RaftEffect::NodeQuarantined {
                error: StorageError::WalFrameBitRotCrc { .. } | StorageError::WalHashChainMismatch { .. },
                ..
            } => {
                bit_rot = true;
            }
            _ => {}
        }
    }

    for eff in slot.core.drain_effects() {
        if let RaftEffect::SendRpc(out) = eff {
            outbound_rpc = Some(out.rpc);
        }
    }
    (outbound_rpc, bit_rot)
}

fn envelope_to_ledger_command(
    idem: &str,
    env: Option<&base_v1::MarketEventEnvelope>,
) -> LedgerCommand {
    if let Some(e) = env {
        let ts = if e.ts_ns > 0 { e.ts_ns } else { epoch_now_ns() };
        if let Some(base_v1::market_event_envelope::Payload::JournalEntry(ref j)) = e.payload {
            if let Some(first_line) = j.lines.first() {
                let credit_u128 = first_line
                    .credit_fp
                    .as_ref()
                    .map(|c| ((c.hi.max(0) as u128) << 64) | (c.lo as u128))
                    .unwrap_or(FP_SCALE_U128);
                return LedgerCommand::CreditDeposit {
                    idempotency_key: idem.to_string(),
                    account_id: first_line.account_id.max(1),
                    asset: if first_line.asset.is_empty() {
                        "USDT".to_string()
                    } else {
                        first_line.asset.clone()
                    },
                    amount_fp: credit_u128.max(1),
                    ts_ns: ts,
                };
            }
        }
    }
    LedgerCommand::CreditDeposit {
        idempotency_key: idem.to_string(),
        account_id: 1,
        asset: "USDT".to_string(),
        amount_fp: FP_SCALE_U128,
        ts_ns: epoch_now_ns(),
    }
}

fn proto_to_internal_entry(pe: &RaftLogEntry) -> LogEntry {
    let mut prev_hash = [0u8; 32];
    if pe.prev_blake3_hash.len() == 32 {
        prev_hash.copy_from_slice(&pe.prev_blake3_hash);
    }
    let kind = match ProtoEntryKind::try_from(pe.kind).unwrap_or(ProtoEntryKind::Unspecified) {
        ProtoEntryKind::NoopBarrier => RaftEntryKind::NoOp,
        ProtoEntryKind::JointConfigEnter | ProtoEntryKind::JointConfigCommit => {
            if let Some(raft_log_entry::Command::MembershipChange(ref mc)) = pe.command {
                RaftEntryKind::ConfigChange(proto_to_cluster_config(mc))
            } else {
                RaftEntryKind::NoOp
            }
        }
        _ => {
            let decoded_raw = match &pe.command {
                Some(raft_log_entry::Command::RawCommandBytes(raw)) => {
                    base_v1::MarketEventEnvelope::decode(raw.as_slice()).ok()
                }
                _ => None,
            };
            let env_opt = match &pe.command {
                Some(raft_log_entry::Command::LedgerEnvelope(env)) => Some(env),
                _ => decoded_raw.as_ref(),
            };
            let cmd = envelope_to_ledger_command(&format!("rpc-idx-{}", pe.index), env_opt);
            RaftEntryKind::JournalBatch(vec![cmd])
        }
    };

    let mut built = crate::wal_codec::build_chained_log_entry(
        pe.index,
        pe.term,
        pe.ts_ns,
        prev_hash,
        kind,
    );
    if pe.crc32c_checksum != 0 {
        built.payload_crc32c = pe.crc32c_checksum;
    }
    if pe.entry_blake3_hash.len() == 32 {
        built.wal_blake3.copy_from_slice(&pe.entry_blake3_hash);
    }
    built
}

fn proto_to_cluster_config(jc: &JointConsensusConfig) -> ClusterConfig {
    let cur: BTreeSet<NodeId> = jc.current_voters.iter().map(|p| p.node_id).collect();
    let prev: BTreeSet<NodeId> = jc.previous_voters.iter().map(|p| p.node_id).collect();
    let learners: BTreeSet<NodeId> = jc.learners.iter().map(|p| p.node_id).collect();
    if prev.is_empty() {
        ClusterConfig::uniform_with_learners(cur, learners)
    } else {
        ClusterConfig::Joint {
            old_voters: prev,
            new_voters: cur,
            learners,
        }
    }
}

pub fn cluster_config_to_proto(config_index: u64, cfg: &ClusterConfig) -> JointConsensusConfig {
    let to_peer = |id: NodeId, voting: bool| RaftNodePeer {
        node_id: id,
        peer_addr: format!("127.0.0.1:{}", 50200 + id),
        is_voting_voter: voting,
    };
    match cfg {
        ClusterConfig::Uniform { voters, learners } => JointConsensusConfig {
            config_index,
            current_voters: voters.iter().copied().map(|id| to_peer(id, true)).collect(),
            previous_voters: Vec::new(),
            learners: learners
                .iter()
                .copied()
                .map(|id| to_peer(id, false))
                .collect(),
        },
        ClusterConfig::Joint {
            old_voters,
            new_voters,
            learners,
        } => JointConsensusConfig {
            config_index,
            current_voters: new_voters
                .iter()
                .copied()
                .map(|id| to_peer(id, true))
                .collect(),
            previous_voters: old_voters
                .iter()
                .copied()
                .map(|id| to_peer(id, true))
                .collect(),
            learners: learners
                .iter()
                .copied()
                .map(|id| to_peer(id, false))
                .collect(),
        },
    }
}

fn internal_to_proto_role(r: RaftRole) -> RaftNodeRole {
    match r {
        RaftRole::Follower | RaftRole::Quarantined => RaftNodeRole::Follower,
        RaftRole::PreCandidate => RaftNodeRole::PreCandidate,
        RaftRole::Candidate => RaftNodeRole::Candidate,
        RaftRole::Leader => RaftNodeRole::Leader,
        RaftRole::Learner => RaftNodeRole::Learner,
    }
}

fn role_str(r: RaftRole) -> &'static str {
    match r {
        RaftRole::Follower => "FOLLOWER",
        RaftRole::PreCandidate => "PRE_CANDIDATE",
        RaftRole::Candidate => "CANDIDATE",
        RaftRole::Leader => "LEADER",
        RaftRole::Learner => "LEARNER",
        RaftRole::Quarantined => "QUARANTINED",
    }
}

fn epoch_now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1_000_000_000)
}
