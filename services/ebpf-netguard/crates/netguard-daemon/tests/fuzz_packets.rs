use netguard_common::parser::{TCP_ACK, TCP_PSH};
use netguard_common::{BlockRuleVal, DropReason, FiveTuple, IpAddrWire, RuleAction, XdpAction};
use netguard_daemon::ebpf_vm::build_canonical_xdp_program;
use netguard_daemon::lpm::{parse_cidr_spec, CidrEntry};
use netguard_daemon::packet_forge::{
    build_hyperion_wire_payload, build_ipv4_tcp_frame, build_malformed_fragment_corpus,
    build_overread_exploit_corpus, generate_10k_spoofed_syn_storm, SplitMix64,
};
use netguard_daemon::pipeline::{NetGuardSharedState, XdpWorkerEngine};
use proptest::prelude::*;

#[test]
fn deterministic_250k_fuzz_sweep_zero_panic_and_100pct_vm_parity() {
    let shared_native = NetGuardSharedState::new("eth0", 1);
    let shared_vm = NetGuardSharedState::new("eth0", 1);

    for s in [&shared_native, &shared_vm] {
        let (ip, p) = parse_cidr_spec("203.0.113.0/24").unwrap();
        s.policy_store.upsert_batch(
            vec![CidrEntry {
                cidr_text: "203.0.113.0/24".to_string(),
                ip,
                prefix_len: p,
                rule: BlockRuleVal {
                    action: RuleAction::HardDrop,
                    rate_override_rps: 0,
                    expires_ts_ns: 0,
                    rule_generation: 1,
                },
                rollback_token: "fuzz".to_string(),
            }],
            Vec::new(),
        );
    }

    let mut w_native = XdpWorkerEngine::new(0, shared_native);
    let mut w_vm = XdpWorkerEngine::new(0, shared_vm);
    let prog = build_canonical_xdp_program();
    let mut rng = SplitMix64::new(0x4859_5045_5249_4F4E);
    let base_ts = 1_700_000_000_000_000_000_u64;

    // 1. Audit all explicit malformed fragment & over-read exploit frames
    for (idx, exploit) in build_malformed_fragment_corpus()
        .into_iter()
        .chain(build_overread_exploit_corpus())
        .enumerate()
    {
        let mut f1 = exploit.clone();
        let mut f2 = exploit;
        let v1 = w_native.process_ingress_frame(&mut f1, base_ts + idx as u64);
        let v2 = w_vm.process_ingress_via_bpf_vm(&prog, &mut f2, base_ts + idx as u64);
        assert_eq!(v1.action, v2.action, "parity mismatch on exploit #{idx}");
        assert_eq!(v1.reason, v2.reason, "reason mismatch on exploit #{idx}");
        // Every malformed fragment / truncated IP frame must be dropped (except non-IP EtherTypes if any)
        if v1.reason != DropReason::NonIpPass {
            assert_eq!(v1.action, XdpAction::Drop);
        }
    }

    // 2. Blast 250,000 randomized & bit-corrupted raw packet buffers (`0..256` bytes)
    let template_hyp = build_hyperion_wire_payload(0x04, 0, 1, 99, base_ts, &[0x11; 64]);
    let template_pkt = build_ipv4_tcp_frame(
        [172, 16, 5, 5],
        [10, 0, 0, 1],
        41000,
        8080,
        10,
        20,
        TCP_ACK | TCP_PSH,
        &template_hyp,
    );

    let mut buf_a = Vec::with_capacity(300);
    let mut buf_b = Vec::with_capacity(300);

    for i in 0..250_000_u64 {
        buf_a.clear();
        let mode = rng.next_u64() & 3;
        if mode == 0 {
            // Pure random noise slice (`0..180` bytes)
            let len = (rng.next_u64() as usize) % 180;
            for _ in 0..len {
                buf_a.push(rng.next_u64() as u8);
            }
        } else {
            // Mutated / truncated clone of a real Ethernet+IPv4+TCP+HYPERION frame
            let cut = ((rng.next_u64() as usize) % (template_pkt.len() + 16)).min(template_pkt.len());
            buf_a.extend_from_slice(&template_pkt[..cut]);
            let flips = 1 + ((rng.next_u64() as usize) % 4);
            if !buf_a.is_empty() {
                for _ in 0..flips {
                    let pos = (rng.next_u64() as usize) % buf_a.len();
                    buf_a[pos] ^= (1_u8 << ((rng.next_u64() as u8) & 7)).max(1);
                }
            }
        }
        buf_b.clone_from(&buf_a);

        let ts = base_ts + 1_000_000 + i * 1_000_000; // 1ms apart to test parser deterministically
        let va = w_native.process_ingress_frame(&mut buf_a, ts);
        let vb = w_vm.process_ingress_via_bpf_vm(&prog, &mut buf_b, ts);
        assert_eq!(va.action, vb.action, "VM parity failure at fuzz iter {i}");
        assert_eq!(va.reason, vb.reason, "VM reason failure at fuzz iter {i}");
        assert_eq!(buf_a, buf_b, "in-place packet mutation mismatch at iter {i}");
    }
}

#[test]
fn spoofed_10k_ip_syn_storm_shield_preserves_established_traders() {
    let shared = NetGuardSharedState::new("eth0", 1);
    let legit_flow = FiveTuple {
        src_ip: IpAddrWire::V4([10, 42, 0, 7]),
        dst_ip: IpAddrWire::V4([10, 0, 0, 1]),
        src_port: 51500,
        dst_port: 8080,
        proto: 6,
    };
    let now = 1_700_000_000_000_000_000_u64;
    shared.conntrack.register_established(legit_flow, 10042, now);

    let mut worker = XdpWorkerEngine::new(0, shared);
    worker.sync_conntrack();

    let storm = generate_10k_spoofed_syn_storm(0xBEEF_2026);
    assert_eq!(storm.len(), 10_000);

    // Legitimate established trader order/heartbeat traffic (not WS connection-storm flood)
    let legit_wire = build_hyperion_wire_payload(0x01, 0, 1, 1, now, &[0; 16]);
    let legit_pkt = build_ipv4_tcp_frame(
        [10, 42, 0, 7],
        [10, 0, 0, 1],
        51500,
        8080,
        500,
        600,
        TCP_ACK | TCP_PSH,
        &legit_wire,
    );

    let mut syn_dropped = 0_usize;
    let mut legit_passed = 0_usize;

    for (idx, mut syn_frame) in storm.into_iter().enumerate() {
        // Simultaneous burst within 1 millisecond (`+100ns` per packet = 10 Mpps arrival rate!)
        let ts = now + (idx as u64 * 100);
        let v_syn = worker.process_ingress_frame(&mut syn_frame, ts);
        if v_syn.action == XdpAction::Drop {
            syn_dropped += 1;
        }

        // Every 10th attacker packet, interleave a legitimate established trader packet
        if idx % 10 == 0 {
            let mut lp = legit_pkt.clone();
            let v_legit = worker.process_ingress_frame(&mut lp, ts + 50);
            assert_eq!(
                v_legit.action,
                XdpAction::Pass,
                "legitimate trader dropped during 10k SYN flood at idx {idx}"
            );
            legit_passed += 1;
        }
    }

    assert_eq!(legit_passed, 1_000);
    // Global unverified-SYN burst is 250 -> >9,700 of the 10,000 spoofed SYNs MUST be dropped!
    assert!(
        syn_dropped >= 9_700,
        "expected >= 9700 spoofed SYNs dropped, got {syn_dropped}"
    );
}

#[test]
fn ws_connection_storm_rate_limited_precisely() {
    let shared = NetGuardSharedState::new("eth0", 1);
    let mut worker = XdpWorkerEngine::new(0, shared);
    let now = 1_700_000_000_000_000_000_u64;
    let ws_upgrade = b"GET /ws HTTP/1.1\r\nHost: gateway.hyperion.local\r\nUpgrade: websocket\r\n\r\n";
    let mut passed = 0_usize;
    let mut dropped_ws = 0_usize;

    // Single attacker IP sends 50 WebSocket upgrade handshakes within 50 microseconds
    for i in 0..50_u64 {
        let mut pkt = build_ipv4_tcp_frame(
            [198, 51, 100, 202],
            [10, 0, 0, 1],
            40000 + i as u16,
            8080,
            100,
            200,
            TCP_ACK | TCP_PSH,
            ws_upgrade,
        );
        let v = worker.process_ingress_frame(&mut pkt, now + i * 1_000);
        match v.action {
            XdpAction::Pass => passed += 1,
            XdpAction::Drop => {
                assert_eq!(v.reason, DropReason::WsStormRateLimited);
                dropped_ws += 1;
            }
            other => panic!("unexpected action {other:?}"),
        }
    }
    // Burst=50, WS upgrade cost=3 -> exactly 16 pass (48 tokens), remaining 34 dropped!
    assert_eq!(passed, 16);
    assert_eq!(dropped_ws, 34);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn proptest_arbitrary_bytes_never_panic_and_match_vm(raw in proptest::collection::vec(any::<u8>(), 0..512)) {
        let shared1 = NetGuardSharedState::new("lo", 1);
        let shared2 = NetGuardSharedState::new("lo", 1);
        let mut w1 = XdpWorkerEngine::new(0, shared1);
        let mut w2 = XdpWorkerEngine::new(0, shared2);
        let prog = build_canonical_xdp_program();

        let mut b1 = raw.clone();
        let mut b2 = raw;
        let v1 = w1.process_ingress_frame(&mut b1, 1_000_000_000);
        let v2 = w2.process_ingress_via_bpf_vm(&prog, &mut b2, 1_000_000_000);

        prop_assert_eq!(v1.action, v2.action);
        prop_assert_eq!(v1.reason, v2.reason);
        prop_assert!(matches!(v1.action, XdpAction::Pass | XdpAction::Drop | XdpAction::Tx));
    }

    #[test]
    fn proptest_valid_hyperion_frames_pass_and_bad_magic_drops(
        src_ip in any::<[u8; 4]>(),
        instrument_id in 0_u16..=64_u16,
        seq in any::<u64>(),
        corrupt_magic in any::<bool>(),
    ) {
        let safe_src = [src_ip[0].clamp(11, 220), src_ip[1], src_ip[2], src_ip[3].max(1)];
        let dst = [10, 0, 0, 1];
        let mut wire = build_hyperion_wire_payload(0x02, 0, instrument_id, seq, 123456789, &[0; 56]);
        if corrupt_magic {
            wire[0] ^= 0xFF;
        }
        let mut pkt = build_ipv4_tcp_frame(safe_src, dst, 45000, 8080, 1, 1, TCP_ACK | TCP_PSH, &wire);

        let shared = NetGuardSharedState::new("eth0", 1);
        let mut worker = XdpWorkerEngine::new(0, shared);
        let verdict = worker.process_ingress_frame(&mut pkt, 2_000_000_000);

        if corrupt_magic {
            prop_assert_eq!(verdict.action, XdpAction::Drop);
            prop_assert_eq!(verdict.reason, DropReason::HyperionBadMagic);
        } else {
            prop_assert_eq!(verdict.action, XdpAction::Pass);
            prop_assert_eq!(verdict.reason, DropReason::None);
        }
    }
}
