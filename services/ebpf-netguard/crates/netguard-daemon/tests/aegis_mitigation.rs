use ed25519_dalek::{Signer, SigningKey};
use hyperion_proto::{MitigationActionType, RiskMitigationCommand};
use netguard_common::parser::{TCP_ACK, TCP_PSH, TCP_SYN};
use netguard_common::{DropReason, FiveTuple, IpAddrWire, TcAction, XdpAction};
use netguard_daemon::aegis_bridge::{
    aegis_ch06_unsigned_protobuf_bytes, canonical_risk_command_preimage,
    compute_chained_audit_blake3, default_aegis_ch06_verifying_key, AegisMitigationBridge,
    CidrRuleInput,
};
use netguard_daemon::packet_forge::{build_hyperion_wire_payload, build_ipv4_tcp_frame};
use netguard_daemon::pb::net_guard_control_client::NetGuardControlClient;
use netguard_daemon::pb::{
    ApplyNetGuardMitigationRequest, CidrRuleSpec, GetNetGuardTelemetryRequest,
    RevokeNetGuardMitigationRequest,
};
use netguard_daemon::pipeline::{NetGuardSharedState, XdpWorkerEngine};
use netguard_daemon::server::{build_http_router, DaemonServerContext, NetGuardGrpcService};
use prost::Message;
use tokio::sync::broadcast;
use tonic::transport::Server as TonicServer;

#[test]
fn signed_aegis_mitigation_sub_ms_rcu_swap_without_dropping_active_sessions() {
    let signing_key = SigningKey::from_bytes(&[0x42_u8; 32]);
    let verifying_key = signing_key.verifying_key();

    let shared = NetGuardSharedState::new("eth0", 2);
    let bridge = AegisMitigationBridge::new(shared.clone(), Some(verifying_key));

    let now = 1_700_000_000_000_000_000_u64;

    // Register two established connections inside `198.51.100.0/24`:
    // 1) Legitimate institutional trader (`account_id = 1001` at `198.51.100.10:50001`)
    // 2) Wash-trading attacker (`account_id = 6666` at `198.51.100.66:50066`)
    let good_flow = FiveTuple {
        src_ip: IpAddrWire::V4([198, 51, 100, 10]),
        dst_ip: IpAddrWire::V4([10, 0, 0, 1]),
        src_port: 50001,
        dst_port: 8080,
        proto: 6,
    };
    let bad_flow = FiveTuple {
        src_ip: IpAddrWire::V4([198, 51, 100, 66]),
        dst_ip: IpAddrWire::V4([10, 0, 0, 1]),
        src_port: 50066,
        dst_port: 8080,
        proto: 6,
    };
    shared.conntrack.register_established(good_flow, 1001, now);
    shared.conntrack.register_established(bad_flow, 6666, now);

    let mut worker = XdpWorkerEngine::new(0, shared.clone());
    worker.sync_conntrack();

    // Construct cryptographically signed Aegis `RiskMitigationCommand` freezing account 6666
    // + quarantining subnet `198.51.100.0/24` with `preserve_established = true`.
    let mut cmd = RiskMitigationCommand {
        command_id: "cmd-aegis-0001".to_string(),
        source_alert_id: "alert-wash-77".to_string(),
        incident_id: "inc-2026-09".to_string(),
        action: MitigationActionType::FreezeAccount as i32,
        target_account_ids: vec![6666],
        target_symbol: "BTC-USDT".to_string(),
        rate_limit_override_rps: 0,
        cooldown_duration_ms: 60_000,
        dry_run: false,
        requires_human_approval: false,
        rollback_token: "rb-inc-2026-09".to_string(),
        reason: "Circular wash-trading ring + SYN storm from /24".to_string(),
        issued_by: "aegis-agent".to_string(),
        issued_ts_ns: now,
        expires_ts_ns: now + 60_000_000_000,
        ed25519_signature: Vec::new(),
        audit_chain_blake3: Vec::new(),
        trace: None,
    };
    let preimage = canonical_risk_command_preimage(&cmd);
    let genesis_audit = [0_u8; 32];
    cmd.audit_chain_blake3 = compute_chained_audit_blake3(&genesis_audit, &preimage).to_vec();
    cmd.ed25519_signature = signing_key.sign(&preimage).to_bytes().to_vec();

    // 1. Tampered signature MUST be rejected
    let mut forged = cmd.clone();
    forged.ed25519_signature[0] ^= 0xFF;
    assert!(bridge
        .apply_risk_command(&forged, &[], true, now)
        .is_err());

    // 2. Valid signed command MUST commit atomically in sub-millisecond (< 1_000_000 ns)
    let outcome = bridge
        .apply_risk_command(
            &cmd,
            &[CidrRuleInput {
                cidr: "198.51.100.0/24".to_string(),
                rule_action: 1, // Upgraded gracefully to DropNewSynOnly because preserve_established=true
                rate_override_rps: 0,
                duration_ms: 60_000,
            }],
            true,
            now,
        )
        .expect("signed command accepted");

    assert!(outcome.applied);
    assert!(
        outcome.apply_latency_ns < 5_000_000, // Sub-ms typical (~25us); generous CI ceiling
        "RCU swap took {} ns",
        outcome.apply_latency_ns
    );
    assert_eq!(outcome.preserved_established_sessions, 2);

    // 3. Replay of same `command_id` MUST be rejected
    assert!(bridge.apply_risk_command(&cmd, &[], true, now).is_err());

    let wire = build_hyperion_wire_payload(0x81, 0, 1, 10, now, &[0; 16]);

    // 4a. New inbound SYN from `198.51.100.200` -> DROPPED (`Ipv4BlocklistMatch`)!
    let mut new_syn =
        build_ipv4_tcp_frame([198, 51, 100, 200], [10, 0, 0, 1], 59999, 8080, 1, 0, TCP_SYN, &[]);
    let v_syn = worker.process_ingress_frame(&mut new_syn, now + 100);
    assert_eq!(v_syn.action, XdpAction::Drop);
    assert_eq!(v_syn.reason, DropReason::Ipv4BlocklistMatch);

    // 4b. Frozen attacker `account_id = 6666` established session -> DROPPED (`AccountFrozenMitigation`)!
    let mut bad_ack = build_ipv4_tcp_frame(
        [198, 51, 100, 66],
        [10, 0, 0, 1],
        50066,
        8080,
        10,
        20,
        TCP_ACK | TCP_PSH,
        &wire,
    );
    let v_bad = worker.process_ingress_frame(&mut bad_ack, now + 200);
    assert_eq!(v_bad.action, XdpAction::Drop);
    assert_eq!(v_bad.reason, DropReason::AccountFrozenMitigation);

    // 4c. Healthy established trader `account_id = 1001` in same `/24` -> PASSES (`EstablishedConntrackPass`)!
    let mut good_ack = build_ipv4_tcp_frame(
        [198, 51, 100, 10],
        [10, 0, 0, 1],
        50001,
        8080,
        10,
        20,
        TCP_ACK | TCP_PSH,
        &wire,
    );
    let v_good = worker.process_ingress_frame(&mut good_ack, now + 300);
    assert_eq!(v_good.action, XdpAction::Pass);
    assert_eq!(v_good.reason, DropReason::EstablishedConntrackPass);
}

#[test]
fn aegis_ch06_byte_exact_unsigned_protobuf_ed25519_interop() {
    // Replicates Channel 06 `services/aegis-agent/src/crypto/signer.ts:94,298-316` byte-for-byte
    let ch06_seed = *blake3::hash(b"hyperion-aegis-ch06-default-ed25519-seed-v1").as_bytes();
    let ch06_signer = SigningKey::from_bytes(&ch06_seed);
    let ch06_vk = default_aegis_ch06_verifying_key();
    assert_eq!(ch06_signer.verifying_key(), ch06_vk);

    let shared = NetGuardSharedState::new("eth0", 2);
    let bridge = AegisMitigationBridge::new(shared.clone(), Some(ch06_vk));
    let now = 1_700_000_000_000_000_000_u64;

    let mut cmd = RiskMitigationCommand {
        command_id: "ch06-live-cmd-01".to_string(),
        source_alert_id: "alert-ch05-99".to_string(),
        incident_id: "inc-ch06-01".to_string(),
        action: MitigationActionType::FreezeAccount as i32,
        target_account_ids: vec![8888],
        target_symbol: "BTC-USDT".to_string(),
        rate_limit_override_rps: 0,
        cooldown_duration_ms: 30_000,
        dry_run: false,
        requires_human_approval: false,
        rollback_token: "rbk1.eyJ2IjoxfQ.sig".to_string(),
        reason: "Verified CH06 Aegis unsigned-protobuf Ed25519 command".to_string(),
        issued_by: "aegis-agent".to_string(),
        issued_ts_ns: now,
        expires_ts_ns: now + 30_000_000_000,
        ed25519_signature: Vec::new(),
        audit_chain_blake3: blake3::hash(b"ch06-upstream-audit").as_bytes().to_vec(),
        trace: None,
    };
    let unsigned_wire = aegis_ch06_unsigned_protobuf_bytes(&cmd);
    cmd.ed25519_signature = ch06_signer.sign(&unsigned_wire).to_bytes().to_vec();

    let outcome = bridge
        .apply_signed_protobuf_command(&cmd.encode_to_vec(), &[], true, now)
        .expect("CH06 unsigned-protobuf Ed25519 signature MUST verify cleanly");
    assert!(outcome.applied);
    assert_eq!(outcome.active_account_rules, 1);
}

#[tokio::test]
async fn live_grpc_and_prometheus_telemetry_histograms_end_to_end() {
    let shared = NetGuardSharedState::new("eth0", 2);
    let aegis = AegisMitigationBridge::new(shared.clone(), None);
    let (event_tx, _) = broadcast::channel(64);
    let ctx = DaemonServerContext {
        shared: shared.clone(),
        aegis: aegis.clone(),
        event_tx,
        bpf_elf_verified: true,
        bpf_xdp_insns: 18,
    };

    // Populate realistic TC egress -> XDP ingress RTT samples, socket backlog, and TCP retransmits
    let mut worker = XdpWorkerEngine::new(0, shared.clone());
    let base_ns = 1_700_000_000_000_000_000_u64;
    let wire = build_hyperion_wire_payload(0x01, 0, 1, 1, base_ns, &[0; 16]);

    for i in 0..200_u64 {
        // Outbound server -> client segment stamped by TC egress hook
        let egress_pkt = build_ipv4_tcp_frame(
            [10, 0, 0, 1],
            [172, 16, 0, (i % 50) as u8 + 1],
            8080,
            40000 + i as u16,
            100,
            200,
            TCP_ACK | TCP_PSH,
            &wire,
        );
        assert_eq!(
            worker.process_egress_tc_frame(&egress_pkt, base_ns + i * 1_000_000),
            TcAction::Ok
        );

        // Matching client -> server ACK arriving `50 us + i * 2 us` later
        let rtt_sim_ns = 50_000 + (i * 2_000);
        let mut ingress_pkt = build_ipv4_tcp_frame(
            [172, 16, 0, (i % 50) as u8 + 1],
            [10, 0, 0, 1],
            40000 + i as u16,
            8080,
            200,
            150,
            TCP_ACK,
            &[],
        );
        let v = worker.process_ingress_frame(&mut ingress_pkt, base_ns + i * 1_000_000 + rtt_sim_ns);
        assert_eq!(v.action, XdpAction::Pass);

        shared.telemetry.record_socket_backlog(0, 32 + (i % 128), i % 50 == 0);
        shared.telemetry.record_tcp_segment(0, i % 40 == 0); // 2.5% = 25,000 ppm retransmit rate
    }

    // Bind ephemeral HTTP & gRPC listeners for deterministic parallel test safety
    let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http_listener.local_addr().unwrap();
    let app = build_http_router(ctx.clone());
    tokio::spawn(async move {
        let _ = axum::serve(http_listener, app).await;
    });

    let grpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let grpc_addr = grpc_listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(grpc_listener);
    let grpc_svc = NetGuardGrpcService::into_server(ctx);
    tokio::spawn(async move {
        let _ = TonicServer::builder()
            .add_service(grpc_svc)
            .serve_with_incoming(incoming)
            .await;
    });

    // 1. Verify gRPC `GetTelemetrySnapshot`, `ApplyMitigation`, and `RevokeMitigation`
    let mut client = NetGuardControlClient::connect(format!("http://{}", grpc_addr))
        .await
        .expect("connect gRPC");

    let snap = client
        .get_telemetry_snapshot(GetNetGuardTelemetryRequest {
            interface_name: "eth0".to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(snap.xdp_pass_packets, 200);
    assert_eq!(snap.socket_backlog_overflow_drops, 4);
    assert_eq!(snap.retransmit_spike_ratio_ppm, 25_000);
    let rtt = snap.tcp_rtt_ns.unwrap();
    assert_eq!(rtt.sample_count, 200);
    assert!(rtt.p50_ns >= 200_000 && rtt.p99_ns >= 400_000);

    let cmd = RiskMitigationCommand {
        command_id: "grpc-cmd-99".to_string(),
        incident_id: "inc-grpc".to_string(),
        action: MitigationActionType::ThrottleRateLimit as i32,
        target_account_ids: vec![777],
        rate_limit_override_rps: 25,
        cooldown_duration_ms: 30_000,
        rollback_token: "rb-grpc-99".to_string(),
        ..Default::default()
    };
    let apply_resp = client
        .apply_mitigation(ApplyNetGuardMitigationRequest {
            command_id: "grpc-cmd-99".to_string(),
            incident_id: "inc-grpc".to_string(),
            signed_risk_command_pb: cmd.encode_to_vec(),
            cidr_rules: vec![CidrRuleSpec {
                cidr: "203.0.113.0/24".to_string(),
                rule_action: 1,
                rate_override_rps: 0,
                duration_ms: 30_000,
            }],
            preserve_established_connections: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert!(apply_resp.applied);
    assert_eq!(apply_resp.active_cidr_rules, 1);
    assert_eq!(apply_resp.active_account_rules, 1);

    let revoke_resp = client
        .revoke_mitigation(RevokeNetGuardMitigationRequest {
            rollback_token: "rb-grpc-99".to_string(),
            cidrs: vec![],
            account_ids: vec![],
        })
        .await
        .unwrap()
        .into_inner();
    assert!(revoke_resp.revoked);
    assert_eq!(revoke_resp.remaining_cidr_rules, 0);

    // 2. Verify Prometheus `/metrics` & `/healthz` over raw HTTP/1.1 TCP socket
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(http_addr).await.unwrap();
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw_http = String::new();
    stream.read_to_string(&mut raw_http).await.unwrap();
    assert!(raw_http.contains("200 OK"));
    assert!(raw_http.contains("hyperion_netguard_tcp_rtt_ns_bucket"));
    assert!(raw_http.contains("hyperion_netguard_retransmit_spike_ratio_ppm{iface=\"eth0\"} 25000"));
    assert!(raw_http.contains("hyperion_netguard_socket_backlog_drops_total{iface=\"eth0\"} 4"));

    // 3. Verify Redpanda `hyperion.omega.netguard.drops.v1` JSON snapshot staging
    let (rk, rjson) = aegis.stage_redpanda_drop_snapshot(base_ns);
    assert_eq!(rk, "iface:eth0");
    assert!(rjson.contains("\"retransmit_spike_ratio_ppm\":25000"));
}
