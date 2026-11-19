//! High-Speed Raw Binary Packet Forge (`Ethernet / VLAN / IPv4 / IPv6 / TCP / UDP / WS / HYPERION`)
//! with Exact RFC 1071 Checksums & Adversarial Attack Suites (Fragmentation, Truncation, 10k Spoofed Storm).

use netguard_common::checksum::{ipv4_header_checksum, l4_ipv4_checksum, l4_ipv6_checksum};
use netguard_common::parser::{
    ETH_P_8021AD, ETH_P_8021Q, ETH_P_IPV4, ETH_P_IPV6, IPPROTO_TCP, IPPROTO_UDP, TCP_ACK, TCP_FIN,
    TCP_PSH, TCP_RST, TCP_SYN, TCP_URG,
};
use netguard_common::{
    DEFAULT_GATEWAY_PORT, HYPERION_WIRE_MAGIC_LE, HYPERION_WIRE_VERSION, XDP_PROBE_REQUEST_MAGIC,
    XDP_PROBE_UDP_PORT,
};

const SERVER_MAC: [u8; 6] = [0x02, 0x48, 0x59, 0x00, 0x00, 0x01];
const CLIENT_MAC: [u8; 6] = [0x02, 0x48, 0x59, 0xAA, 0xBB, 0xCC];

/// Deterministic `SplitMix64` PRNG for reproducible 10,000-IP spoofing & benchmark packet generation.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    #[inline]
    pub fn next_public_ipv4(&mut self) -> [u8; 4] {
        let r = self.next_u64() as u32;
        let b0 = ((r >> 24) as u8).clamp(11, 220);
        let b1 = (r >> 16) as u8;
        let b2 = (r >> 8) as u8;
        let b3 = ((r & 0xFF) as u8).max(1);
        [b0, b1, b2, b3]
    }
}

/// Builds a canonical 32-byte Little-Endian HYPERION v1 wire frame (`contracts/ws-binary-protocol.md`).
pub fn build_hyperion_wire_payload(
    msg_type: u8,
    flags: u8,
    instrument_id: u16,
    engine_seq: u64,
    timestamp_ns: u64,
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + body.len());
    out.extend_from_slice(&HYPERION_WIRE_MAGIC_LE.to_le_bytes()); // [0..2] 0x59, 0x48
    out.push(HYPERION_WIRE_VERSION); // [2] 0x01
    out.push(msg_type); // [3]
    out.push(flags); // [4]
    out.push(0x00); // [5] reserved MUST be 0
    out.extend_from_slice(&instrument_id.to_le_bytes()); // [6..8]
    out.extend_from_slice(&(body.len() as u32).to_le_bytes()); // [8..12]
    out.extend_from_slice(&0_u32.to_le_bytes()); // [12..16] crc32c
    out.extend_from_slice(&engine_seq.to_le_bytes()); // [16..24]
    out.extend_from_slice(&timestamp_ns.to_le_bytes()); // [24..32]
    out.extend_from_slice(body);
    out
}

/// Wraps a binary payload inside an RFC 6455 Binary WebSocket frame (`opcode = 0x82`),
/// optionally masked with a 4-byte client XOR key.
pub fn wrap_rfc6455_ws_binary(payload: &[u8], mask_key: Option<[u8; 4]>) -> Vec<u8> {
    let mut ws = Vec::with_capacity(14 + payload.len());
    ws.push(0x82); // FIN=1 | OP_BINARY=2
    let mask_bit = if mask_key.is_some() { 0x80 } else { 0x00 };
    if payload.len() < 126 {
        ws.push(mask_bit | (payload.len() as u8));
    } else if payload.len() <= 65535 {
        ws.push(mask_bit | 126);
        ws.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        ws.push(mask_bit | 127);
        ws.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }

    if let Some(mask) = mask_key {
        ws.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            ws.push(b ^ mask[i & 3]);
        }
    } else {
        ws.extend_from_slice(payload);
    }
    ws
}

/// Builds a complete Ethernet II + IPv4 + TCP frame with exact RFC 1071 IPv4 and TCP checksums.
#[allow(clippy::too_many_arguments)]
pub fn build_ipv4_tcp_frame(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack_seq: u32,
    tcp_flags: u8,
    l7_payload: &[u8],
) -> Vec<u8> {
    let tcp_len = 20 + l7_payload.len();
    let ip_tot_len = 20 + tcp_len;
    let mut pkt = Vec::with_capacity(14 + ip_tot_len);

    // Ethernet II (14B)
    pkt.extend_from_slice(&SERVER_MAC);
    pkt.extend_from_slice(&CLIENT_MAC);
    pkt.extend_from_slice(&ETH_P_IPV4.to_be_bytes());

    // IPv4 Header (20B)
    let l3_off = pkt.len();
    pkt.push(0x45); // Version=4, IHL=5
    pkt.push(0x00); // DSCP/ECN
    pkt.extend_from_slice(&(ip_tot_len as u16).to_be_bytes());
    pkt.extend_from_slice(&0x1337_u16.to_be_bytes()); // Identification
    pkt.extend_from_slice(&0x4000_u16.to_be_bytes()); // DF=1, FragOffset=0
    pkt.push(64); // TTL
    pkt.push(IPPROTO_TCP);
    pkt.extend_from_slice(&[0, 0]); // Checksum placeholder
    pkt.extend_from_slice(&src_ip);
    pkt.extend_from_slice(&dst_ip);
    let ip_csum = ipv4_header_checksum(&pkt[l3_off..l3_off + 20]).to_be_bytes();
    pkt[l3_off + 10..l3_off + 12].copy_from_slice(&ip_csum);

    // TCP Header (20B) + L7 payload
    let l4_off = pkt.len();
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&ack_seq.to_be_bytes());
    pkt.push(5 << 4); // DOFF=5 (20B)
    pkt.push(tcp_flags);
    pkt.extend_from_slice(&65535_u16.to_be_bytes()); // Window
    pkt.extend_from_slice(&[0, 0]); // Checksum placeholder
    pkt.extend_from_slice(&[0, 0]); // Urgent ptr
    pkt.extend_from_slice(l7_payload);

    let tcp_csum = l4_ipv4_checksum(src_ip, dst_ip, IPPROTO_TCP, &pkt[l4_off..], 16).to_be_bytes();
    pkt[l4_off + 16..l4_off + 18].copy_from_slice(&tcp_csum);
    pkt
}

/// Builds a QinQ Double-VLAN (`802.1ad` + `802.1Q`) + IPv4 + TCP frame.
pub fn build_qinq_vlan_ipv4_tcp_frame(
    outer_vlan: u16,
    inner_vlan: u16,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    l7_payload: &[u8],
) -> Vec<u8> {
    let base = build_ipv4_tcp_frame(
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        100,
        200,
        TCP_ACK | TCP_PSH,
        l7_payload,
    );
    let mut out = Vec::with_capacity(base.len() + 8);
    out.extend_from_slice(&base[0..12]);
    out.extend_from_slice(&ETH_P_8021AD.to_be_bytes());
    out.extend_from_slice(&outer_vlan.to_be_bytes());
    out.extend_from_slice(&ETH_P_8021Q.to_be_bytes());
    out.extend_from_slice(&inner_vlan.to_be_bytes());
    out.extend_from_slice(&ETH_P_IPV4.to_be_bytes());
    out.extend_from_slice(&base[14..]);
    out
}

/// Builds a complete Ethernet II + IPv6 + TCP frame with exact RFC 8200 pseudo-header checksum.
pub fn build_ipv6_tcp_frame(
    src_ip: [u8; 16],
    dst_ip: [u8; 16],
    src_port: u16,
    dst_port: u16,
    tcp_flags: u8,
    l7_payload: &[u8],
) -> Vec<u8> {
    let tcp_len = 20 + l7_payload.len();
    let mut pkt = Vec::with_capacity(14 + 40 + tcp_len);

    pkt.extend_from_slice(&SERVER_MAC);
    pkt.extend_from_slice(&CLIENT_MAC);
    pkt.extend_from_slice(&ETH_P_IPV6.to_be_bytes());

    // IPv6 Header (40B)
    pkt.extend_from_slice(&0x6000_0000_u32.to_be_bytes()); // Version = 6
    pkt.extend_from_slice(&(tcp_len as u16).to_be_bytes());
    pkt.push(IPPROTO_TCP);
    pkt.push(64); // Hop limit
    pkt.extend_from_slice(&src_ip);
    pkt.extend_from_slice(&dst_ip);

    let l4_off = pkt.len();
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&1000_u32.to_be_bytes());
    pkt.extend_from_slice(&2000_u32.to_be_bytes());
    pkt.push(5 << 4);
    pkt.push(tcp_flags);
    pkt.extend_from_slice(&65535_u16.to_be_bytes());
    pkt.extend_from_slice(&[0, 0]);
    pkt.extend_from_slice(&[0, 0]);
    pkt.extend_from_slice(l7_payload);

    let tcp_csum =
        l4_ipv6_checksum(src_ip, dst_ip, IPPROTO_TCP, &pkt[l4_off..], 16).to_be_bytes();
    pkt[l4_off + 16..l4_off + 18].copy_from_slice(&tcp_csum);
    pkt
}

/// Builds an Ethernet II + IPv4 + UDP frame with exact RFC 768 checksum (`HYPPING1` probe ready).
pub fn build_ipv4_udp_frame(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let ip_tot_len = 20 + udp_len;
    let mut pkt = Vec::with_capacity(14 + ip_tot_len);

    pkt.extend_from_slice(&SERVER_MAC);
    pkt.extend_from_slice(&CLIENT_MAC);
    pkt.extend_from_slice(&ETH_P_IPV4.to_be_bytes());

    let l3_off = pkt.len();
    pkt.push(0x45);
    pkt.push(0x00);
    pkt.extend_from_slice(&(ip_tot_len as u16).to_be_bytes());
    pkt.extend_from_slice(&0x2468_u16.to_be_bytes());
    pkt.extend_from_slice(&0x4000_u16.to_be_bytes());
    pkt.push(64);
    pkt.push(IPPROTO_UDP);
    pkt.extend_from_slice(&[0, 0]);
    pkt.extend_from_slice(&src_ip);
    pkt.extend_from_slice(&dst_ip);
    let ip_csum = ipv4_header_checksum(&pkt[l3_off..l3_off + 20]).to_be_bytes();
    pkt[l3_off + 10..l3_off + 12].copy_from_slice(&ip_csum);

    let l4_off = pkt.len();
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&(udp_len as u16).to_be_bytes());
    pkt.extend_from_slice(&[0, 0]);
    pkt.extend_from_slice(payload);

    let udp_csum = l4_ipv4_checksum(src_ip, dst_ip, IPPROTO_UDP, &pkt[l4_off..], 6).to_be_bytes();
    pkt[l4_off + 6..l4_off + 8].copy_from_slice(&udp_csum);
    pkt
}

/// Builds a wire-speed UDP hairpin probe packet (`HYPPING1` -> expects `XDP_TX` `HYPPONG1`).
pub fn build_xdp_hairpin_probe_packet(src_ip: [u8; 4], dst_ip: [u8; 4]) -> Vec<u8> {
    build_ipv4_udp_frame(
        src_ip,
        dst_ip,
        54321,
        XDP_PROBE_UDP_PORT,
        &XDP_PROBE_REQUEST_MAGIC,
    )
}

/// Generates adversarial malformed IPv4/IPv6 fragment frames (Teardrop, tiny fragment, Ping-of-Death).
pub fn build_malformed_fragment_corpus() -> Vec<Vec<u8>> {
    let mut corpus = Vec::new();

    // 1. Non-zero fragment offset (attempting L4 TCP SYN filter bypass)
    let mut f1 = build_ipv4_tcp_frame([203, 0, 113, 9], [10, 0, 0, 1], 4444, 8080, 1, 0, TCP_SYN, &[]);
    f1[14 + 6..14 + 8].copy_from_slice(&0x0001_u16.to_be_bytes()); // frag_off = 8 bytes
    let c1 = ipv4_header_checksum(&f1[14..34]).to_be_bytes();
    f1[14 + 10..14 + 12].copy_from_slice(&c1);
    corpus.push(f1);

    // 2. Tiny first fragment (`MF = 1` with only 4 bytes of IP payload < 8B minimum L4 header)
    let mut f2 = vec![0_u8; 14 + 24];
    f2[0..6].copy_from_slice(&SERVER_MAC);
    f2[6..12].copy_from_slice(&CLIENT_MAC);
    f2[12..14].copy_from_slice(&ETH_P_IPV4.to_be_bytes());
    f2[14] = 0x45;
    f2[16..18].copy_from_slice(&24_u16.to_be_bytes());
    f2[20..22].copy_from_slice(&0x2000_u16.to_be_bytes()); // MF=1, offset=0
    f2[22] = 64;
    f2[23] = IPPROTO_TCP;
    f2[26..30].copy_from_slice(&[203, 0, 113, 10]);
    f2[30..34].copy_from_slice(&[10, 0, 0, 1]);
    let c2 = ipv4_header_checksum(&f2[14..34]).to_be_bytes();
    f2[24..26].copy_from_slice(&c2);
    corpus.push(f2);

    // 3. Ping-of-Death fragment overflow (`frag_off * 8 + payload > 65535`)
    let mut f3 = build_ipv4_tcp_frame([203, 0, 113, 11], [10, 0, 0, 1], 4444, 8080, 1, 0, TCP_ACK, &[0; 32]);
    f3[14 + 6..14 + 8].copy_from_slice(&0x1FFF_u16.to_be_bytes()); // offset = 65528 + 52 > 65535
    let c3 = ipv4_header_checksum(&f3[14..34]).to_be_bytes();
    f3[14 + 10..14 + 12].copy_from_slice(&c3);
    corpus.push(f3);

    corpus
}

/// Generates truncated & header-length-forged over-read exploit frames across every protocol layer.
pub fn build_overread_exploit_corpus() -> Vec<Vec<u8>> {
    let valid_ws = wrap_rfc6455_ws_binary(
        &build_hyperion_wire_payload(0x81, 0, 1, 0, 1_700_000_000, &[0; 16]),
        Some([0x12, 0x34, 0x56, 0x78]),
    );
    let full = build_ipv4_tcp_frame(
        [198, 51, 100, 77],
        [10, 0, 0, 1],
        51000,
        DEFAULT_GATEWAY_PORT,
        10,
        20,
        TCP_ACK | TCP_PSH,
        &valid_ws,
    );

    let mut exploits = Vec::with_capacity(full.len() + 8);
    // Every exact byte truncation from 0 bytes up to the 32B HYPERION wire header boundary (92B)
    let header_boundary = 14 + 20 + 20 + 2 + 4 + 32; // Eth + IPv4 + TCP + WS(2B) + Mask(4B) + HYPERION(32B)
    for cut in 0..header_boundary.min(full.len()) {
        let sliced = full[..cut].to_vec();
        // If IPv4 header is complete (`cut >= 34`), also test valid IPv4 tot_len with truncated TCP/WS/HYPERION
        // (skip `cut == 54`, which is a valid 0-byte-payload TCP ACK segment)
        if cut >= 34 && cut != 54 {
            let mut adjusted = sliced.clone();
            let new_tot = (cut - 14) as u16;
            adjusted[16..18].copy_from_slice(&new_tot.to_be_bytes());
            let csum = ipv4_header_checksum(&adjusted[14..34]).to_be_bytes();
            adjusted[24..26].copy_from_slice(&csum);
            exploits.push(adjusted);
        }
        exploits.push(sliced);
    }

    // Forged IPv4 IHL = 15 (claims 60B IPv4 header on a 34B frame!)
    let mut forged_ihl = full[..34].to_vec();
    forged_ihl[14] = 0x4F;
    exploits.push(forged_ihl);

    // Forged IPv4 tot_len = 9000 (claims jumbo payload past end of 54B buffer!)
    let mut forged_tot = full[..54].to_vec();
    forged_tot[16..18].copy_from_slice(&9000_u16.to_be_bytes());
    let csum = ipv4_header_checksum(&forged_tot[14..34]).to_be_bytes();
    forged_tot[24..26].copy_from_slice(&csum);
    exploits.push(forged_tot);

    // Forged TCP DOFF = 15 (claims 60B TCP header on a 54B frame!)
    let mut forged_doff = full[..54].to_vec();
    forged_doff[16..18].copy_from_slice(&40_u16.to_be_bytes());
    let csum2 = ipv4_header_checksum(&forged_doff[14..34]).to_be_bytes();
    forged_doff[24..26].copy_from_slice(&csum2);
    forged_doff[34 + 12] = 15 << 4;
    exploits.push(forged_doff);

    // TCP Xmas, Null, SYN+FIN, and LAND attack frames
    for bad_flags in [0_u8, TCP_FIN | TCP_URG | TCP_PSH, TCP_SYN | TCP_FIN, TCP_SYN | TCP_RST] {
        exploits.push(build_ipv4_tcp_frame(
            [198, 51, 100, 88],
            [10, 0, 0, 1],
            40000,
            DEFAULT_GATEWAY_PORT,
            1,
            0,
            bad_flags,
            &[],
        ));
    }
    // LAND attack (`src_ip == dst_ip && src_port == dst_port`)
    exploits.push(build_ipv4_tcp_frame(
        [10, 0, 0, 1],
        [10, 0, 0, 1],
        DEFAULT_GATEWAY_PORT,
        DEFAULT_GATEWAY_PORT,
        1,
        0,
        TCP_SYN,
        &[],
    ));

    exploits
}

/// Generates `10,000` unique spoofed random IPv4 SYN flood packets (`seed` deterministic).
pub fn generate_10k_spoofed_syn_storm(seed: u64) -> Vec<Vec<u8>> {
    let mut rng = SplitMix64::new(seed);
    let mut packets = Vec::with_capacity(10_000);
    for i in 0..10_000_u32 {
        let spoof_ip = rng.next_public_ipv4();
        let sport = 1024 + ((rng.next_u64() as u16) % 60_000);
        packets.push(build_ipv4_tcp_frame(
            spoof_ip,
            [10, 0, 0, 1],
            sport,
            DEFAULT_GATEWAY_PORT,
            i.wrapping_mul(7919),
            0,
            TCP_SYN,
            &[],
        ));
    }
    packets
}
