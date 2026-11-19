use netguard_common::checksum::{
    rfc1624_update_u16, verify_ipv4_header_checksum, verify_l4_ipv4_checksum,
};
use netguard_common::parser::{
    parse_frame_zero_copy, ParserConfig, IPPROTO_TCP, IPPROTO_UDP, TCP_ACK, TCP_PSH, TCP_SYN,
};
use netguard_common::{
    BlockRuleVal, DropReason, RuleAction, XdpAction, XDP_PROBE_REPLY_MAGIC,
};
use netguard_daemon::ebpf_vm::{
    build_canonical_xdp_program, BpfInsn, BpfStaticVerifier, VerifierError,
};
use netguard_daemon::elf_bpf::{emit_elf64_ebpf_object, inspect_elf64_ebpf_object, EM_BPF};
use netguard_daemon::lpm::{parse_cidr_spec, CidrEntry};
use netguard_daemon::packet_forge::{
    build_hyperion_wire_payload, build_ipv4_tcp_frame, build_ipv6_tcp_frame,
    build_qinq_vlan_ipv4_tcp_frame, build_xdp_hairpin_probe_packet, wrap_rfc6455_ws_binary,
};
use netguard_daemon::pipeline::{NetGuardSharedState, XdpWorkerEngine};

#[test]
fn static_verifier_accepts_canonical_xdp_and_rejects_overreads_and_loops() {
    let valid_prog = build_canonical_xdp_program();
    assert_eq!(valid_prog.max_verified_pkt_guard, 18);
    assert_eq!(valid_prog.insns().len(), 18);

    // 1. Defective eBPF program: reads Ethernet EtherType without `data + 14 > data_end` check!
    let unguarded = [
        BpfInsn::ldx_w(2, 1, 0),  // R2 = ctx->data
        BpfInsn::ldx_h(0, 2, 12), // ILLEGAL: reads [12..14] with verified_len == 0!
        BpfInsn::exit_insn(),
    ];
    assert_eq!(
        BpfStaticVerifier::verify(&unguarded).unwrap_err(),
        VerifierError::UnboundedPacketRead {
            pc: 1,
            needed_end: 14,
            verified_len: 0
        }
    );

    // 2. Defective eBPF program: checks `data + 10 > data_end`, then reads byte 12 (`14 > 10`)!
    let short_guard = [
        BpfInsn::ldx_w(2, 1, 0),
        BpfInsn::ldx_w(3, 1, 4),
        BpfInsn::mov64_reg(4, 2),
        BpfInsn::add64_imm(4, 10),
        BpfInsn::jgt_reg(4, 3, 2), // fallthrough only proves 10 bytes!
        BpfInsn::ldx_h(0, 2, 12),  // ILLEGAL: needs 14 bytes!
        BpfInsn::exit_insn(),
        BpfInsn::mov64_imm(0, 1),
        BpfInsn::exit_insn(),
    ];
    assert_eq!(
        BpfStaticVerifier::verify(&short_guard).unwrap_err(),
        VerifierError::UnboundedPacketRead {
            pc: 5,
            needed_end: 14,
            verified_len: 10
        }
    );

    // 3. Defective eBPF program: backward jump infinite loop!
    let infinite_loop = [
        BpfInsn::mov64_imm(0, 2),
        BpfInsn::ja(-1), // Jumps back to pc 1!
        BpfInsn::exit_insn(),
    ];
    assert_eq!(
        BpfStaticVerifier::verify(&infinite_loop).unwrap_err(),
        VerifierError::BackwardOrOutOfBoundsJump { pc: 1, off: -1 }
    );

    // 4. Defective eBPF program: writes to read-only frame pointer R10!
    let write_r10 = [BpfInsn::mov64_imm(10, 0), BpfInsn::exit_insn()];
    assert_eq!(
        BpfStaticVerifier::verify(&write_r10).unwrap_err(),
        VerifierError::WriteToReadOnlyFramePtr { pc: 0 }
    );

    // 5. Defective eBPF program: exits without initializing R0!
    let uninit_r0 = [BpfInsn::exit_insn()];
    assert_eq!(
        BpfStaticVerifier::verify(&uninit_r0).unwrap_err(),
        VerifierError::UninitializedRegister { pc: 0, reg: 0 }
    );
}

#[test]
fn elf64_em_bpf_object_emission_and_section_inspection() {
    let prog = build_canonical_xdp_program();
    let elf_bytes = emit_elf64_ebpf_object(&prog);
    let report = inspect_elf64_ebpf_object(&elf_bytes).expect("valid ELF64 EM_BPF");
    assert_eq!(report.machine, EM_BPF);
    assert!(report.is_64bit_le);
    assert_eq!(report.xdp_insn_count, 18);
    assert_eq!(report.tc_insn_count, 2);
    assert_eq!(report.maps_byte_size, 8 * 28);
    assert_eq!(report.license, "Dual BSD/GPL");
    assert!(report.section_names.contains(&"xdp".to_string()));
    assert!(report.section_names.contains(&"tc".to_string()));
    assert!(report.section_names.contains(&".maps".to_string()));
}

#[test]
fn rfc1071_checksums_qinq_masked_ws_and_xdp_tx_hairpin_rewrites() {
    let shared = NetGuardSharedState::new("eth0", 2);
    let mut worker = XdpWorkerEngine::new(0, shared.clone());
    let prog = build_canonical_xdp_program();
    let now = 1_700_000_000_000_000_000_u64;

    // 1. QinQ Double-VLAN + Masked RFC 6455 Binary WebSocket + HYPERION 0x4859 Frame
    let hyp = build_hyperion_wire_payload(0x81, 0x01, 1, 0, now, &[0xAA; 16]);
    let masked_ws = wrap_rfc6455_ws_binary(&hyp, Some([0xDE, 0xAD, 0xBE, 0xEF]));
    let mut qinq_pkt = build_qinq_vlan_ipv4_tcp_frame(
        100,
        200,
        [172, 16, 1, 10],
        [10, 0, 0, 1],
        55000,
        8080,
        &masked_ws,
    );
    let v_ok = worker.process_ingress_via_bpf_vm(&prog, &mut qinq_pkt, now);
    assert_eq!(v_ok.action, XdpAction::Pass);
    assert_eq!(v_ok.reason, DropReason::None);
    assert_eq!(v_ok.parsed.unwrap().vlan_tags, 2);

    // Corrupt 1 bit of the masked magic byte inside the WebSocket payload -> MUST XDP_DROP!
    let mut qinq_bad_magic = qinq_pkt.clone();
    let ws_payload_magic_byte = 14 + 8 + 20 + 20 + 2 + 4; // Eth+QinQ+IPv4+TCP+WS(2B)+Mask(4B)
    qinq_bad_magic[ws_payload_magic_byte] ^= 0x01;
    let v_bad = worker.process_ingress_via_bpf_vm(&prog, &mut qinq_bad_magic, now + 1);
    assert_eq!(v_bad.action, XdpAction::Drop);
    assert_eq!(v_bad.reason, DropReason::HyperionBadMagic);

    // 2. IPv6 + HYPERION Binary Frame + IPv6 `/48` LPM Blocklist Drop
    let src_v6 = [0x20, 0x01, 0x0d, 0xb8, 0xba, 0xd0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 9];
    let dst_v6 = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut v6_pkt = build_ipv6_tcp_frame(src_v6, dst_v6, 42000, 8080, TCP_ACK | TCP_PSH, &hyp);
    assert_eq!(
        worker.process_ingress_via_bpf_vm(&prog, &mut v6_pkt, now + 2).action,
        XdpAction::Pass
    );

    let (ip6_net, p6) = parse_cidr_spec("2001:db8:bad0::/48").unwrap();
    shared.policy_store.upsert_batch(
        vec![CidrEntry {
            cidr_text: "2001:db8:bad0::/48".to_string(),
            ip: ip6_net,
            prefix_len: p6,
            rule: BlockRuleVal {
                action: RuleAction::HardDrop,
                rate_override_rps: 0,
                expires_ts_ns: 0,
                rule_generation: 0,
            },
            rollback_token: "rb-v6".to_string(),
        }],
        Vec::new(),
    );
    assert_eq!(
        worker.process_ingress_via_bpf_vm(&prog, &mut v6_pkt, now + 3).action,
        XdpAction::Drop
    );

    // 3. UDP `HYPPING1` Wire-Speed Probe -> `XDP_TX` `HYPPONG1` with valid RFC 1071 checksums
    let mut probe_pkt = build_xdp_hairpin_probe_packet([10, 10, 10, 55], [10, 0, 0, 1]);
    let v_tx = worker.process_ingress_via_bpf_vm(&prog, &mut probe_pkt, now + 4);
    assert_eq!(v_tx.action, XdpAction::Tx);
    assert_eq!(v_tx.reason, DropReason::XdpTxProbeReply);
    assert!(verify_ipv4_header_checksum(&probe_pkt[14..34]));
    assert!(verify_l4_ipv4_checksum(
        [10, 0, 0, 1],
        [10, 10, 10, 55],
        IPPROTO_UDP,
        &probe_pkt[34..]
    ));
    assert_eq!(&probe_pkt[42..50], &XDP_PROBE_REPLY_MAGIC);

    // 4. Stateless `XDP_TX` TCP SYN-Cookie Challenge Rewrite
    let (syn_net, syn_p) = parse_cidr_spec("192.0.2.0/24").unwrap();
    shared.policy_store.upsert_batch(
        vec![CidrEntry {
            cidr_text: "192.0.2.0/24".to_string(),
            ip: syn_net,
            prefix_len: syn_p,
            rule: BlockRuleVal {
                action: RuleAction::SynCookieTx,
                rate_override_rps: 0,
                expires_ts_ns: 0,
                rule_generation: 0,
            },
            rollback_token: "rb-syncookie".to_string(),
        }],
        Vec::new(),
    );
    let mut syn_pkt = build_ipv4_tcp_frame([192, 0, 2, 77], [10, 0, 0, 1], 60001, 8080, 999, 0, TCP_SYN, &[]);
    let v_cookie = worker.process_ingress_via_bpf_vm(&prog, &mut syn_pkt, now + 5);
    assert_eq!(v_cookie.action, XdpAction::Tx);
    assert_eq!(v_cookie.reason, DropReason::XdpTxSynCookie);
    assert!(verify_ipv4_header_checksum(&syn_pkt[14..34]));
    assert!(verify_l4_ipv4_checksum(
        [10, 0, 0, 1],
        [192, 0, 2, 77],
        IPPROTO_TCP,
        &syn_pkt[34..]
    ));
    let parsed_reply = parse_frame_zero_copy(&syn_pkt, &ParserConfig::default()).unwrap();
    assert_eq!(parsed_reply.tcp_flags, TCP_SYN | TCP_ACK);
    assert_eq!(parsed_reply.tcp_ack_seq, 1000);

    // 5. RFC 1624 incremental checksum equivalence check
    let old_word = 0x1234_u16;
    let new_word = 0xABCD_u16;
    let hdr_before = [0x45, 0x00, 0x00, 0x28, 0x12, 0x34, 0x40, 0x00, 64, 6, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
    let csum_before = netguard_common::checksum::ipv4_header_checksum(&hdr_before);
    let mut hdr_after = hdr_before;
    hdr_after[4..6].copy_from_slice(&new_word.to_be_bytes());
    let csum_full = netguard_common::checksum::ipv4_header_checksum(&hdr_after);
    let csum_inc = rfc1624_update_u16(csum_before, old_word, new_word);
    assert_eq!(csum_inc, csum_full);
}
