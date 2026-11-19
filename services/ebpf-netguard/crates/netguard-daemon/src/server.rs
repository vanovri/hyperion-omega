//! Axum HTTP (`/healthz`, `/metrics`, `/api/v1/netguard/*` on `:8095`) &
//! Tonic gRPC (`NetGuardControl` on `:50165`) Servers for Aegis SRE & Prometheus.

use crate::aegis_bridge::{now_unix_ns, AegisMitigationBridge, CidrRuleInput, MitigationOutcome};
use crate::pipeline::NetGuardSharedState;
use crate::pb::net_guard_control_server::{NetGuardControl, NetGuardControlServer};
use crate::pb::{
    ApplyNetGuardMitigationRequest, ApplyNetGuardMitigationResponse, DropReasonMetric,
    GetNetGuardTelemetryRequest, HdrQuantilesNs, NetGuardSecurityEvent, NetGuardTelemetrySnapshot,
    RevokeNetGuardMitigationRequest, RevokeNetGuardMitigationResponse, StreamNetGuardEventsRequest,
};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use hyperion_proto::{MitigationActionType, RiskMitigationCommand};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct DaemonServerContext {
    pub shared: Arc<NetGuardSharedState>,
    pub aegis: Arc<AegisMitigationBridge>,
    pub event_tx: broadcast::Sender<NetGuardSecurityEvent>,
    pub bpf_elf_verified: bool,
    pub bpf_xdp_insns: usize,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub channel: u32,
    pub interface: String,
    pub rcu_generation: u64,
    pub bpf_elf_verified: bool,
    pub bpf_xdp_insns: usize,
}

#[derive(Debug, Deserialize)]
pub struct RestMitigationRequest {
    pub command_id: String,
    #[serde(default)]
    pub incident_id: String,
    #[serde(default)]
    pub cidr_rules: Vec<CidrRuleInput>,
    #[serde(default)]
    pub target_account_ids: Vec<u64>,
    #[serde(default)]
    pub account_action: u32,
    #[serde(default)]
    pub rate_limit_override_rps: u32,
    #[serde(default)]
    pub cooldown_duration_ms: u64,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub rollback_token: String,
    #[serde(default)]
    pub preserve_established_connections: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct RestRevokeRequest {
    #[serde(default)]
    pub rollback_token: String,
    #[serde(default)]
    pub cidrs: Vec<String>,
    #[serde(default)]
    pub account_ids: Vec<u64>,
}

/// Builds the Axum HTTP router bound to canonical port `8095`.
pub fn build_http_router(ctx: DaemonServerContext) -> Router {
    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/metrics", get(handle_metrics))
        .route("/api/v1/netguard/telemetry", get(handle_telemetry_json))
        .route("/api/v1/netguard/mitigations", post(handle_apply_mitigation))
        .route("/api/v1/netguard/revoke", post(handle_revoke_mitigation))
        .with_state(ctx)
}

async fn handle_healthz(State(ctx): State<DaemonServerContext>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "ebpf-netguard",
        channel: 16,
        interface: ctx.shared.telemetry.interface_name.clone(),
        rcu_generation: ctx.shared.policy_store.current_generation(),
        bpf_elf_verified: ctx.bpf_elf_verified,
        bpf_xdp_insns: ctx.bpf_xdp_insns,
    })
}

async fn handle_metrics(State(ctx): State<DaemonServerContext>) -> impl IntoResponse {
    let snap = ctx.shared.policy_store.load_snapshot();
    let report = ctx.shared.telemetry.snapshot(
        now_unix_ns(),
        snap.generation,
        snap.ipv4_rule_count,
        snap.ipv6_rule_count,
        snap.account_rules.len() as u32,
        ctx.shared.conntrack.active_count(),
    );
    let body = ctx.shared.telemetry.render_prometheus(&report);
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

async fn handle_telemetry_json(State(ctx): State<DaemonServerContext>) -> impl IntoResponse {
    let snap = ctx.shared.policy_store.load_snapshot();
    let report = ctx.shared.telemetry.snapshot(
        now_unix_ns(),
        snap.generation,
        snap.ipv4_rule_count,
        snap.ipv6_rule_count,
        snap.account_rules.len() as u32,
        ctx.shared.conntrack.active_count(),
    );
    Json(report)
}

async fn handle_apply_mitigation(
    State(ctx): State<DaemonServerContext>,
    Json(req): Json<RestMitigationRequest>,
) -> Result<Json<MitigationOutcome>, (StatusCode, String)> {
    let action = match req.account_action {
        2 => MitigationActionType::FreezeAccount as i32,
        4 => MitigationActionType::EnableVolatilityGuard as i32,
        5 => MitigationActionType::RestoreNormal as i32,
        _ => MitigationActionType::ThrottleRateLimit as i32,
    };
    let cmd = RiskMitigationCommand {
        command_id: req.command_id,
        source_alert_id: String::new(),
        incident_id: req.incident_id,
        action,
        target_account_ids: req.target_account_ids,
        target_symbol: String::new(),
        rate_limit_override_rps: req.rate_limit_override_rps,
        cooldown_duration_ms: req.cooldown_duration_ms,
        dry_run: req.dry_run,
        requires_human_approval: false,
        rollback_token: req.rollback_token,
        reason: req.reason,
        issued_by: "rest-api".to_string(),
        issued_ts_ns: now_unix_ns(),
        expires_ts_ns: 0,
        ed25519_signature: Vec::new(),
        audit_chain_blake3: Vec::new(),
        trace: None,
    };

    let outcome = ctx
        .aegis
        .apply_risk_command(
            &cmd,
            &req.cidr_rules,
            req.preserve_established_connections,
            now_unix_ns(),
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let _ = ctx.event_tx.send(NetGuardSecurityEvent {
        event_ts_ns: now_unix_ns(),
        event_type: "MITIGATION_APPLIED".to_string(),
        src_ip: String::new(),
        dst_port: 8080,
        xdp_action: "RCU_SWAP".to_string(),
        drop_reason: outcome.detail.clone(),
        rcu_generation: outcome.rcu_generation,
        summary: format!("rollback_token={}", outcome.rollback_token),
    });

    Ok(Json(outcome))
}

async fn handle_revoke_mitigation(
    State(ctx): State<DaemonServerContext>,
    Json(req): Json<RestRevokeRequest>,
) -> Json<serde_json::Value> {
    let t0 = Instant::now();
    let (revoked, snap) =
        ctx.shared
            .policy_store
            .revoke(&req.rollback_token, &req.cidrs, &req.account_ids);
    let latency_ns = t0.elapsed().as_nanos() as u64;
    Json(serde_json::json!({
        "revoked": revoked,
        "rcu_generation": snap.generation,
        "revoke_latency_ns": latency_ns,
        "remaining_cidr_rules": snap.ipv4_rule_count + snap.ipv6_rule_count,
        "remaining_account_rules": snap.account_rules.len(),
    }))
}

/// Tonic gRPC Service implementation bound to canonical port `50165`.
pub struct NetGuardGrpcService {
    pub ctx: DaemonServerContext,
}

impl NetGuardGrpcService {
    pub fn into_server(ctx: DaemonServerContext) -> NetGuardControlServer<Self> {
        NetGuardControlServer::new(Self { ctx })
    }
}

#[tonic::async_trait]
impl NetGuardControl for NetGuardGrpcService {
    async fn apply_mitigation(
        &self,
        request: Request<ApplyNetGuardMitigationRequest>,
    ) -> Result<Response<ApplyNetGuardMitigationResponse>, Status> {
        let req = request.into_inner();
        let cidrs: Vec<CidrRuleInput> = req
            .cidr_rules
            .into_iter()
            .map(|c| CidrRuleInput {
                cidr: c.cidr,
                rule_action: c.rule_action,
                rate_override_rps: c.rate_override_rps,
                duration_ms: c.duration_ms,
            })
            .collect();

        let now = now_unix_ns();
        let outcome = if !req.signed_risk_command_pb.is_empty() {
            self.ctx
                .aegis
                .apply_signed_protobuf_command(
                    &req.signed_risk_command_pb,
                    &cidrs,
                    req.preserve_established_connections,
                    now,
                )
                .map_err(|e| Status::permission_denied(e.to_string()))?
        } else {
            let action = match req.account_action {
                2 => MitigationActionType::FreezeAccount as i32,
                4 => MitigationActionType::EnableVolatilityGuard as i32,
                5 => MitigationActionType::RestoreNormal as i32,
                _ => MitigationActionType::ThrottleRateLimit as i32,
            };
            let cmd = RiskMitigationCommand {
                command_id: req.command_id,
                source_alert_id: String::new(),
                incident_id: req.incident_id,
                action,
                target_account_ids: req.target_account_ids,
                target_symbol: String::new(),
                rate_limit_override_rps: req.rate_limit_override_rps,
                cooldown_duration_ms: req.cooldown_duration_ms,
                dry_run: req.dry_run,
                requires_human_approval: false,
                rollback_token: req.rollback_token,
                reason: req.reason,
                issued_by: "grpc-control".to_string(),
                issued_ts_ns: now,
                expires_ts_ns: 0,
                ed25519_signature: Vec::new(),
                audit_chain_blake3: Vec::new(),
                trace: None,
            };
            self.ctx
                .aegis
                .apply_risk_command(&cmd, &cidrs, req.preserve_established_connections, now)
                .map_err(|e| Status::invalid_argument(e.to_string()))?
        };

        let _ = self.ctx.event_tx.send(NetGuardSecurityEvent {
            event_ts_ns: now,
            event_type: "GRPC_MITIGATION".to_string(),
            src_ip: String::new(),
            dst_port: 8080,
            xdp_action: "RCU_SWAP".to_string(),
            drop_reason: outcome.detail.clone(),
            rcu_generation: outcome.rcu_generation,
            summary: format!("rollback={}", outcome.rollback_token),
        });

        Ok(Response::new(ApplyNetGuardMitigationResponse {
            applied: outcome.applied,
            dry_run: outcome.dry_run,
            rcu_generation: outcome.rcu_generation,
            apply_latency_ns: outcome.apply_latency_ns,
            rollback_token: outcome.rollback_token,
            active_cidr_rules: outcome.active_cidr_rules,
            active_account_rules: outcome.active_account_rules,
            preserved_established_sessions: outcome.preserved_established_sessions,
            detail: outcome.detail,
        }))
    }

    async fn revoke_mitigation(
        &self,
        request: Request<RevokeNetGuardMitigationRequest>,
    ) -> Result<Response<RevokeNetGuardMitigationResponse>, Status> {
        let req = request.into_inner();
        let t0 = Instant::now();
        let (revoked, snap) = self.ctx.shared.policy_store.revoke(
            &req.rollback_token,
            &req.cidrs,
            &req.account_ids,
        );
        let revoke_latency_ns = t0.elapsed().as_nanos() as u64;

        Ok(Response::new(RevokeNetGuardMitigationResponse {
            revoked,
            rcu_generation: snap.generation,
            revoke_latency_ns,
            remaining_cidr_rules: snap.ipv4_rule_count + snap.ipv6_rule_count,
            remaining_account_rules: snap.account_rules.len() as u32,
        }))
    }

    async fn get_telemetry_snapshot(
        &self,
        _request: Request<GetNetGuardTelemetryRequest>,
    ) -> Result<Response<NetGuardTelemetrySnapshot>, Status> {
        let snap = self.ctx.shared.policy_store.load_snapshot();
        let r = self.ctx.shared.telemetry.snapshot(
            now_unix_ns(),
            snap.generation,
            snap.ipv4_rule_count,
            snap.ipv6_rule_count,
            snap.account_rules.len() as u32,
            self.ctx.shared.conntrack.active_count(),
        );

        let to_pb_q = |q: crate::telemetry::QuantileSummary| HdrQuantilesNs {
            sample_count: q.sample_count,
            min_ns: q.min,
            p50_ns: q.p50,
            p90_ns: q.p90,
            p95_ns: q.p95,
            p99_ns: q.p99,
            p999_ns: q.p999,
            max_ns: q.max,
            sum_ns: q.sum,
        };

        Ok(Response::new(NetGuardTelemetrySnapshot {
            interface_name: r.interface_name,
            snapshot_ts_ns: r.snapshot_ts_ns,
            rcu_generation: r.rcu_generation,
            xdp_pass_packets: r.xdp_pass_packets,
            xdp_drop_packets: r.xdp_drop_packets,
            xdp_tx_packets: r.xdp_tx_packets,
            tcp_rtt_ns: Some(to_pb_q(r.tcp_rtt_ns)),
            socket_backlog_depth: Some(to_pb_q(r.socket_backlog_depth)),
            socket_backlog_overflow_drops: r.socket_backlog_overflow_drops,
            tcp_retransmits_total: r.tcp_retransmits_total,
            tcp_segments_total: r.tcp_segments_total,
            retransmit_spike_ratio_ppm: r.retransmit_spike_ratio_ppm,
            active_ipv4_cidr_rules: r.active_ipv4_cidr_rules,
            active_ipv6_cidr_rules: r.active_ipv6_cidr_rules,
            active_frozen_accounts: r.active_frozen_accounts,
            active_conntrack_sessions: r.active_conntrack_sessions,
            drop_reasons: r
                .drop_reasons
                .into_iter()
                .map(|(reason, packets)| DropReasonMetric { reason, packets })
                .collect(),
        }))
    }

    type StreamSecurityEventsStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<NetGuardSecurityEvent, Status>> + Send>,
    >;

    async fn stream_security_events(
        &self,
        _request: Request<StreamNetGuardEventsRequest>,
    ) -> Result<Response<Self::StreamSecurityEventsStream>, Status> {
        let rx = self.ctx.event_tx.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(|res| match res {
            Ok(ev) => Some(Ok(ev)),
            Err(_) => None,
        });
        Ok(Response::new(Box::pin(stream)))
    }
}
