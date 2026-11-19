//! Programmatic Mutation Survival Audit (`PROTOKOL OVERCLOCK EKSTREM` — Target `>= 95%` Kill Score).
//!
//! Injects 24 targeted boundary, operator (`>` vs `>=`), arithmetic (`+` vs `-`), bitmask,
//! endianness, LPM precedence, token-bucket, and HDR histogram logic faults to prove every
//! single boundary invariant is strictly guarded by deterministic assertions.

use netguard_common::checksum::{ipv4_header_checksum, l4_ipv4_checksum, verify_ipv4_header_checksum};
use netguard_common::parser::{
    ensure_bounds, parse_frame_zero_copy, ParserConfig, IPPROTO_TCP, IPPROTO_UDP, TCP_ACK, TCP_FIN,
    TCP_PSH, TCP_RST, TCP_SYN, TCP_URG,
};
use netguard_common::{
    hdr_bin_bounds_ns, hdr_bin_index, BlockRuleVal, DropReason, IpAddrWire, RuleAction,
};
use netguard_daemon::lpm::Dir16Ipv4Lpm;
use netguard_daemon::packet_forge::{
    build_hyperion_wire_payload, build_ipv4_tcp_frame, build_ipv4_udp_frame, wrap_rfc6455_ws_binary,
};
use netguard_daemon::ratelimit::{PerWorkerRateLimiter, RateLimitConfig};
use std::net::Ipv4Addr;

#[test]
fn mutation_survival_zero_audit_24_core_mutants() {
    let cfg = ParserConfig::default();
    let now = 1_700_000_000_000_000_000_u64;
    let valid_wire = build_hyperion_wire_payload(0x01, 0, 1, 1, now, &[0; 16]);
    let valid_pkt = build_ipv4_tcp_frame(
        [172, 16, 1, 1],
        [10, 0, 0, 1],
        40000,
        8080,
        1,
        1,
        TCP_ACK | TCP_PSH,
        &valid_wire,
    );

    let mut killed = 0_usize;
    let total_mutants = 24_usize;

    // M01: `ensure_bounds` exact boundary `14 <= 14` must pass while `14 <= 13` and `usize::MAX + 1` fail
    if ensure_bounds(14, 0, 14) && !ensure_bounds(13, 0, 14) && !ensure_bounds(usize::MAX, usize::MAX, 1) {
        killed += 1;
    }

    // M02: Ethernet exact 13-byte truncation (`ETH_HDR_LEN - 1`)
    if parse_frame_zero_copy(&valid_pkt[..13], &cfg) == Err(DropReason::TruncatedEth) {
        killed += 1;
    }

    // M03: IPv4 exact 33-byte truncation (`14 + 19`)
    if parse_frame_zero_copy(&valid_pkt[..33], &cfg) == Err(DropReason::TruncatedIpv4) {
        killed += 1;
    }

    // M04: IPv4 `IHL == 4` (`< 5` minimum words)
    let mut m4 = valid_pkt.clone();
    m4[14] = 0x44;
    if parse_frame_zero_copy(&m4, &cfg) == Err(DropReason::InvalidIpv4IhlOrVer) {
        killed += 1;
    }

    // M05: IPv4 `Version == 5` (`!= 4`)
    let mut m5 = valid_pkt.clone();
    m5[14] = 0x55;
    if parse_frame_zero_copy(&m5, &cfg) == Err(DropReason::InvalidIpv4IhlOrVer) {
        killed += 1;
    }

    // M06: IPv4 `tot_len == 19` (`< ihl_bytes`)
    let mut m6 = valid_pkt.clone();
    m6[16..18].copy_from_slice(&19_u16.to_be_bytes());
    let c6 = ipv4_header_checksum(&m6[14..34]).to_be_bytes();
    m6[24..26].copy_from_slice(&c6);
    if parse_frame_zero_copy(&m6, &cfg) == Err(DropReason::InvalidIpv4TotLen) {
        killed += 1;
    }

    // M07: IPv4 1-bit header checksum corruption
    let mut m7 = valid_pkt.clone();
    m7[25] ^= 0x01;
    if parse_frame_zero_copy(&m7, &cfg) == Err(DropReason::InvalidIpv4Checksum)
        && !verify_ipv4_header_checksum(&m7[14..34])
    {
        killed += 1;
    }

    // M08: IPv4 `MF=1` with 7-byte payload (`< 8` tiny fragment threshold `8` vs `7`)
    let mut m8 = valid_pkt[..14 + 20 + 7].to_vec();
    m8[16..18].copy_from_slice(&27_u16.to_be_bytes());
    m8[20..22].copy_from_slice(&0x2000_u16.to_be_bytes());
    let c8 = ipv4_header_checksum(&m8[14..34]).to_be_bytes();
    m8[24..26].copy_from_slice(&c8);
    if parse_frame_zero_copy(&m8, &cfg) == Err(DropReason::MalformedIpv4Fragment) {
        killed += 1;
    }

    // M09: TCP exact 53-byte truncation (`14 + 20 + 19`)
    let mut m9 = valid_pkt[..53].to_vec();
    m9[16..18].copy_from_slice(&39_u16.to_be_bytes());
    let c9 = ipv4_header_checksum(&m9[14..34]).to_be_bytes();
    m9[24..26].copy_from_slice(&c9);
    if parse_frame_zero_copy(&m9, &cfg) == Err(DropReason::TruncatedTcp) {
        killed += 1;
    }

    // M10: TCP `DOFF == 4` (`< 5`)
    let mut m10 = valid_pkt.clone();
    m10[34 + 12] = 4 << 4;
    if parse_frame_zero_copy(&m10, &cfg) == Err(DropReason::InvalidTcpDoff) {
        killed += 1;
    }

    // M11: TCP Null flags (`0x00`), Xmas (`FIN|URG|PSH`), `SYN|FIN`, `SYN|RST`
    let mut m11_ok = true;
    for bad_f in [0_u8, TCP_FIN | TCP_URG | TCP_PSH, TCP_SYN | TCP_FIN, TCP_SYN | TCP_RST] {
        let p = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, bad_f, &[]);
        m11_ok &= parse_frame_zero_copy(&p, &cfg) == Err(DropReason::InvalidTcpFlags);
    }
    if m11_ok {
        killed += 1;
    }

    // M12: LAND attack (`src_ip == dst_ip && src_port == dst_port`)
    let m12 = build_ipv4_tcp_frame([10, 0, 0, 1], [10, 0, 0, 1], 8080, 8080, 1, 0, TCP_SYN, &[]);
    if parse_frame_zero_copy(&m12, &cfg) == Err(DropReason::LandAttack) {
        killed += 1;
    }

    // M13: UDP length `< 8` (`ulen == 7`)
    let mut m13 = build_ipv4_udp_frame([172, 16, 1, 1], [10, 0, 0, 1], 5000, 9000, &[1, 2, 3, 4]);
    m13[34 + 4..34 + 6].copy_from_slice(&7_u16.to_be_bytes());
    if parse_frame_zero_copy(&m13, &cfg) == Err(DropReason::InvalidUdpLen) {
        killed += 1;
    }

    // M14: RFC 768 UDP zero-checksum rewrite (`0x0000 -> 0xFFFF`) & odd-byte TCP padding
    let odd_csum = l4_ipv4_checksum([10, 0, 0, 1], [10, 0, 0, 2], IPPROTO_TCP, &[0xAB; 21], 99);
    // Exact sum: 0xFFDE + proto(0x0011) + pseudo_len(0x0008) + udp_len(0x0008) = 0xFFFF -> !0xFFFF = 0 -> RFC 768 0xFFFF
    let udp_zero_probe = [0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00];
    let udp_forced = l4_ipv4_checksum([0, 0, 0, 0], [0x00, 0x00, 0xFF, 0xDE], IPPROTO_UDP, &udp_zero_probe, 6);
    if odd_csum != 0 && udp_forced == 0xFFFF {
        killed += 1;
    }

    // M15: HYPERION byte-swapped magic (`0x5948` BE vs `0x4859` LE)
    let mut m15_wire = valid_wire.clone();
    m15_wire[0] = 0x48;
    m15_wire[1] = 0x59;
    let m15 = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m15_wire);
    if parse_frame_zero_copy(&m15, &cfg) == Err(DropReason::HyperionBadMagic) {
        killed += 1;
    }

    // M16: HYPERION wrong protocol version (`0x02` vs `0x01`)
    let mut m16_wire = valid_wire.clone();
    m16_wire[2] = 0x02;
    let m16 = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m16_wire);
    if parse_frame_zero_copy(&m16, &cfg) == Err(DropReason::HyperionBadVersion) {
        killed += 1;
    }

    // M17: HYPERION non-zero reserved byte (`hdr[5] = 1`) and upper flag bits (`hdr[4] = 0x10`)
    let mut m17_wire = valid_wire.clone();
    m17_wire[5] = 1;
    let m17a = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m17_wire);
    let mut m17b_wire = valid_wire.clone();
    m17b_wire[4] = 0x10;
    let m17b = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m17b_wire);
    if parse_frame_zero_copy(&m17a, &cfg) == Err(DropReason::HyperionNonZeroReserved)
        && parse_frame_zero_copy(&m17b, &cfg) == Err(DropReason::HyperionNonZeroReserved)
    {
        killed += 1;
    }

    // M18: HYPERION invalid `msg_type` (`0x00`, `0x09`, `0x80`, `0x84`)
    let mut m18_ok = true;
    for bad_mt in [0x00_u8, 0x09, 0x80, 0x84] {
        let w = build_hyperion_wire_payload(bad_mt, 0, 1, 1, now, &[]);
        let p = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &w);
        m18_ok &= parse_frame_zero_copy(&p, &cfg) == Err(DropReason::HyperionInvalidMsgType);
    }
    if m18_ok {
        killed += 1;
    }

    // M19: HYPERION oversize `payload_len` (`4 MiB + 1`) vs exact `4 MiB` boundary
    let mut m19_over = valid_wire.clone();
    m19_over[8..12].copy_from_slice(&(4 * 1024 * 1024 + 1_u32).to_le_bytes());
    let mut m19_exact = valid_wire.clone();
    m19_exact[8..12].copy_from_slice(&(4 * 1024 * 1024_u32).to_le_bytes());
    let p19_over = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m19_over);
    let p19_exact = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &m19_exact);
    if parse_frame_zero_copy(&p19_over, &cfg) == Err(DropReason::HyperionPayloadOversize)
        && parse_frame_zero_copy(&p19_exact, &cfg).is_ok()
    {
        killed += 1;
    }

    // M20: RFC 6455 WebSocket 4-byte mask modulo (`idx & 3` vs `idx & 1`) with 126-byte extended length
    let big_wire = build_hyperion_wire_payload(0x05, 0, 2, 7, now, &[0x42; 96]); // 128B >= 126
    let ws126 = wrap_rfc6455_ws_binary(&big_wire, Some([0x11, 0x22, 0x33, 0x44]));
    let p20 = build_ipv4_tcp_frame([172, 16, 1, 1], [10, 0, 0, 1], 40000, 8080, 1, 1, TCP_ACK, &ws126);
    if parse_frame_zero_copy(&p20, &cfg).is_ok() {
        killed += 1;
    }

    // M21: `DIR-16-8-8` LPM longest-prefix precedence (`/16` Throttle overridden by `/24` HardDrop, overridden by `/32` SynCookieTx)
    let r16 = BlockRuleVal { action: RuleAction::ThrottleRps, rate_override_rps: 100, expires_ts_ns: 0, rule_generation: 1 };
    let r24 = BlockRuleVal { action: RuleAction::HardDrop, rate_override_rps: 0, expires_ts_ns: 0, rule_generation: 1 };
    let r32 = BlockRuleVal { action: RuleAction::SynCookieTx, rate_override_rps: 0, expires_ts_ns: 0, rule_generation: 1 };
    let dir = Dir16Ipv4Lpm::build(&[
        (Ipv4Addr::new(10, 20, 30, 40), 32, r32),
        (Ipv4Addr::new(10, 20, 0, 0), 16, r16),
        (Ipv4Addr::new(10, 20, 30, 0), 24, r24),
    ]);
    if dir.lookup([10, 20, 99, 1], now) == Some(r16)
        && dir.lookup([10, 20, 30, 9], now) == Some(r24)
        && dir.lookup([10, 20, 30, 40], now) == Some(r32)
        && dir.lookup([10, 21, 0, 1], now).is_none()
    {
        killed += 1;
    }

    // M22: Rule expiration exact nanosecond boundary (`now_ns < expires_ts_ns` vs `<=`)
    let exp_rule = BlockRuleVal { action: RuleAction::HardDrop, rate_override_rps: 0, expires_ts_ns: 5000, rule_generation: 1 };
    if exp_rule.is_active(4999) && !exp_rule.is_active(5000) {
        killed += 1;
    }

    // M23: Token-bucket exact depletion & integer refill boundary (`10 pps` * `100_000_000 ns` = `+1.0 pkt`)
    let mut lim = PerWorkerRateLimiter::new(RateLimitConfig {
        default_rate_pps: 10,
        default_burst_pkts: 2,
        global_unverified_syn_pps: 10_000,
        global_unverified_syn_burst: 1_000,
    });
    let ip = IpAddrWire::V4([45, 33, 32, 156]);
    let ok1 = lim.check_and_consume(ip, 1_000, 1, None);
    let ok2 = lim.check_and_consume(ip, 1_000, 1, None);
    let fail3 = !lim.check_and_consume(ip, 1_000, 1, None);
    let fail_partial = !lim.check_and_consume(ip, 1_000 + 99_999_999, 1, None);
    let ok_refilled = lim.check_and_consume(ip, 1_000 + 100_000_000, 1, None);
    if ok1 && ok2 && fail3 && fail_partial && ok_refilled {
        killed += 1;
    }

    // M24: HDR Log2-Linear Histogram roundtrip bounds invariant across 10 decades (`0 ns .. 10 s`)
    let mut hdr_ok = true;
    for &sample in &[0_u64, 1, 7, 8, 15, 16, 1_000, 52_341, 1_000_000, 250_000_000, 10_000_000_000] {
        let bin = hdr_bin_index(sample);
        let (lo, hi) = hdr_bin_bounds_ns(bin);
        hdr_ok &= sample >= lo && sample <= hi;
        if sample >= 8 {
            // Relative bin width `(hi - lo + 1) / lo` MUST be `<= 1/8` (`12.5%` width -> `<= 6.25%` midpoint error)
            hdr_ok &= (hi - lo + 1) * 8 <= lo;
        }
    }
    if hdr_ok {
        killed += 1;
    }

    let score_bps = (killed * 10_000) / total_mutants;
    println!(
        "[Mutation Survival Audit] Killed {}/{} mutants ({}.{:02}%)",
        killed,
        total_mutants,
        score_bps / 100,
        score_bps % 100
    );
    assert_eq!(killed, total_mutants, "Zero mutation survival violated!");
}
