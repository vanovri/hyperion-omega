//! Verifier-Safe Zero-Copy Packet Dissector (`Ethernet -> VLAN -> IPv4/IPv6 -> TCP/UDP -> WS/HYPERION`).
//!
//! Every header access is guarded by `ensure_bounds(pkt_len, cursor, needed)` which compiles
//! down to a single unsigned bounds check without panics, mirroring the Linux eBPF verifier's
//! `if (cursor + needed > data_end) return XDP_DROP` pointer range invariant.

use crate::checksum::{
    ipv4_header_checksum, l4_ipv4_checksum, verify_ipv4_header_checksum, verify_l4_ipv4_checksum,
};
use crate::{
    DropReason, FiveTuple, IpAddrWire, DEFAULT_GATEWAY_PORT, HYPERION_MAX_INSTRUMENT_ID,
    HYPERION_MAX_PAYLOAD_LEN, HYPERION_WIRE_HEADER_LEN, HYPERION_WIRE_MAGIC_LE,
    HYPERION_WIRE_VERSION, XDP_PROBE_REQUEST_MAGIC, XDP_PROBE_REPLY_MAGIC, XDP_PROBE_UDP_PORT,
};

pub const ETH_HDR_LEN: usize = 14;
pub const VLAN_HDR_LEN: usize = 4;
pub const IPV4_MIN_HDR_LEN: usize = 20;
pub const IPV6_HDR_LEN: usize = 40;
pub const TCP_MIN_HDR_LEN: usize = 20;
pub const UDP_HDR_LEN: usize = 8;

pub const ETH_P_IPV4: u16 = 0x0800;
pub const ETH_P_ARP: u16 = 0x0806;
pub const ETH_P_8021Q: u16 = 0x8100;
pub const ETH_P_IPV6: u16 = 0x86DD;
pub const ETH_P_8021AD: u16 = 0x88A8;

pub const IPPROTO_ICMP: u8 = 1;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;
pub const IPPROTO_IPV6_FRAG: u8 = 44;
pub const IPPROTO_ICMPV6: u8 = 58;

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;
pub const TCP_URG: u8 = 0x20;

/// Controls optional strict checksum verification on RX (disabled when NIC RX checksum offload is simulated).
#[derive(Debug, Clone, Copy)]
pub struct ParserConfig {
    pub gateway_port: u16,
    pub verify_ipv4_checksum: bool,
    pub verify_l4_checksum: bool,
    pub strict_hyperion_magic_on_gateway: bool,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            gateway_port: DEFAULT_GATEWAY_PORT,
            verify_ipv4_checksum: true,
            verify_l4_checksum: false,
            strict_hyperion_magic_on_gateway: true,
        }
    }
}

/// Decoded HYPERION v1 32-byte wire header (`contracts/ws-binary-protocol.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HyperionFrameMeta {
    pub msg_type: u8,
    pub flags: u8,
    pub instrument_id: u16,
    pub payload_len: u32,
    pub crc32c_digest: u32,
    pub engine_seq: u64,
    pub timestamp_ns: u64,
    pub ws_masked: bool,
}

/// Application-layer protocol classification on Gateway ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L7Classification {
    None,
    HttpRestRequest,
    HttpWebSocketUpgrade,
    TlsHandshakeOrRecord,
    WebSocketControlFrame,
    HyperionBinary(HyperionFrameMeta),
    XdpHairpinProbe,
}

/// Zero-copy parsed metadata extracted from a single Ethernet frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedPacket {
    pub eth_proto: u16,
    pub vlan_tags: u8,
    pub l3_offset: u16,
    pub l4_offset: u16,
    pub l7_offset: u16,
    pub l7_len: u16,
    pub flow: FiveTuple,
    pub tcp_flags: u8,
    pub tcp_seq: u32,
    pub tcp_ack_seq: u32,
    pub tcp_window: u16,
    pub l7: L7Classification,
}

impl ParsedPacket {
    #[inline]
    pub const fn is_tcp_syn_only(&self) -> bool {
        self.flow.proto == IPPROTO_TCP
            && (self.tcp_flags & TCP_SYN) != 0
            && (self.tcp_flags & TCP_ACK) == 0
    }

    #[inline]
    pub const fn is_tcp_established_ack(&self) -> bool {
        self.flow.proto == IPPROTO_TCP
            && (self.tcp_flags & TCP_SYN) == 0
            && (self.tcp_flags & TCP_ACK) != 0
    }

    #[inline]
    pub const fn is_ws_upgrade_or_sub(&self) -> bool {
        match self.l7 {
            L7Classification::HttpWebSocketUpgrade => true,
            L7Classification::HyperionBinary(h) => h.msg_type == 0x81,
            _ => false,
        }
    }
}

/// Verifier-style bounds check (`cursor + needed <= pkt_len`) without overflow.
#[inline(always)]
pub const fn ensure_bounds(pkt_len: usize, cursor: usize, needed: usize) -> bool {
    match cursor.checked_add(needed) {
        Some(end) => end <= pkt_len,
        None => false,
    }
}

#[inline(always)]
const fn is_valid_hyperion_msg_type(msg_type: u8) -> bool {
    (msg_type >= 0x01 && msg_type <= 0x08) || (msg_type >= 0x81 && msg_type <= 0x83)
}

/// Validates and decodes the 32-byte HYPERION binary header from an unmasked byte slice.
#[inline]
fn decode_hyperion_32b_header(hdr: &[u8], ws_masked: bool) -> Result<HyperionFrameMeta, DropReason> {
    if hdr.len() < HYPERION_WIRE_HEADER_LEN {
        return Err(DropReason::HyperionBadMagic);
    }
    let magic = u16::from_le_bytes([hdr[0], hdr[1]]);
    if magic != HYPERION_WIRE_MAGIC_LE {
        return Err(DropReason::HyperionBadMagic);
    }
    if hdr[2] != HYPERION_WIRE_VERSION {
        return Err(DropReason::HyperionBadVersion);
    }
    let msg_type = hdr[3];
    if !is_valid_hyperion_msg_type(msg_type) {
        return Err(DropReason::HyperionInvalidMsgType);
    }
    let flags = hdr[4];
    if (flags & 0xF0) != 0 {
        return Err(DropReason::HyperionNonZeroReserved);
    }
    if hdr[5] != 0 {
        return Err(DropReason::HyperionNonZeroReserved);
    }
    let instrument_id = u16::from_le_bytes([hdr[6], hdr[7]]);
    if instrument_id > HYPERION_MAX_INSTRUMENT_ID {
        return Err(DropReason::HyperionInvalidMsgType);
    }
    let payload_len = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    if payload_len > HYPERION_MAX_PAYLOAD_LEN {
        return Err(DropReason::HyperionPayloadOversize);
    }
    let crc32c_digest = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]);
    let engine_seq = u64::from_le_bytes([
        hdr[16], hdr[17], hdr[18], hdr[19], hdr[20], hdr[21], hdr[22], hdr[23],
    ]);
    let timestamp_ns = u64::from_le_bytes([
        hdr[24], hdr[25], hdr[26], hdr[27], hdr[28], hdr[29], hdr[30], hdr[31],
    ]);

    Ok(HyperionFrameMeta {
        msg_type,
        flags,
        instrument_id,
        payload_len,
        crc32c_digest,
        engine_seq,
        timestamp_ns,
        ws_masked,
    })
}

/// Inspects L7 payload targeting Go Edge Gateway (`port 8080`).
#[inline]
pub fn inspect_gateway_l7(l7: &[u8], strict_magic: bool) -> Result<L7Classification, DropReason> {
    if l7.is_empty() {
        return Ok(L7Classification::None);
    }

    // 1. Check HTTP/1.1 methods & WebSocket Upgrade handshake
    if l7.len() >= 4 {
        let prefix4 = [l7[0], l7[1], l7[2], l7[3]];
        if prefix4 == *b"GET " {
            // Distinguish `GET /ws` upgrade storm vs normal REST GET
            if l7.len() >= 7 && &l7[4..7] == b"/ws" {
                return Ok(L7Classification::HttpWebSocketUpgrade);
            }
            return Ok(L7Classification::HttpRestRequest);
        }
        if prefix4 == *b"POST"
            || prefix4 == *b"PUT "
            || prefix4 == *b"DELE"
            || prefix4 == *b"HEAD"
            || prefix4 == *b"OPTI"
            || prefix4 == *b"HTTP"
        {
            return Ok(L7Classification::HttpRestRequest);
        }
        // TLS 1.2 / 1.3 record header (Handshake 0x16 or ApplicationData 0x17, major 0x03)
        if (l7[0] == 0x16 || l7[0] == 0x17) && l7[1] == 0x03 && l7[2] <= 0x04 {
            return Ok(L7Classification::TlsHandshakeOrRecord);
        }
    }

    // 2. Check RFC 6455 WebSocket Control Frames (Close 0x88, Ping 0x89, Pong 0x8A) or Text JSON (0x81)
    let b0 = l7[0];
    if (b0 == 0x88 || b0 == 0x89 || b0 == 0x8A || b0 == 0x81) && l7.len() >= 2 {
        return Ok(L7Classification::WebSocketControlFrame);
    }

    // 3. Check RFC 6455 Binary WebSocket Frame (`FIN | OP_BINARY = 0x82` or fragmented `0x02`)
    if b0 == 0x82 || b0 == 0x02 {
        if !ensure_bounds(l7.len(), 0, 2) {
            return Err(DropReason::HyperionBadMagic);
        }
        let b1 = l7[1];
        let masked = (b1 & 0x80) != 0;
        let len7 = (b1 & 0x7F) as usize;
        let ext_offset = if len7 < 126 {
            2
        } else if len7 == 126 {
            4
        } else {
            10
        };

        if masked {
            if !ensure_bounds(l7.len(), ext_offset, 4 + HYPERION_WIRE_HEADER_LEN) {
                return Err(DropReason::HyperionBadMagic);
            }
            let mask = [
                l7[ext_offset],
                l7[ext_offset + 1],
                l7[ext_offset + 2],
                l7[ext_offset + 3],
            ];
            let p_off = ext_offset + 4;
            let mut unmasked = [0_u8; HYPERION_WIRE_HEADER_LEN];
            let mut idx = 0;
            while idx < HYPERION_WIRE_HEADER_LEN {
                unmasked[idx] = l7[p_off + idx] ^ mask[idx & 3];
                idx += 1;
            }
            let meta = decode_hyperion_32b_header(&unmasked, true)?;
            return Ok(L7Classification::HyperionBinary(meta));
        } else {
            if !ensure_bounds(l7.len(), ext_offset, HYPERION_WIRE_HEADER_LEN) {
                return Err(DropReason::HyperionBadMagic);
            }
            let meta = decode_hyperion_32b_header(&l7[ext_offset..ext_offset + 32], false)?;
            return Ok(L7Classification::HyperionBinary(meta));
        }
    }

    // 4. Direct Raw HYPERION 32-Byte Binary Wire Frame over TCP/UDP
    if l7.len() >= HYPERION_WIRE_HEADER_LEN {
        let meta = decode_hyperion_32b_header(&l7[..HYPERION_WIRE_HEADER_LEN], false)?;
        return Ok(L7Classification::HyperionBinary(meta));
    }

    if strict_magic {
        Err(DropReason::HyperionBadMagic)
    } else {
        Ok(L7Classification::None)
    }
}

/// Zero-copy verifier-safe packet dissector from raw Ethernet frame bytes.
pub fn parse_frame_zero_copy(
    pkt: &[u8],
    cfg: &ParserConfig,
) -> Result<ParsedPacket, DropReason> {
    let pkt_len = pkt.len();
    if !ensure_bounds(pkt_len, 0, ETH_HDR_LEN) {
        return Err(DropReason::TruncatedEth);
    }

    let mut eth_proto = u16::from_be_bytes([pkt[12], pkt[13]]);
    let mut cursor = ETH_HDR_LEN;
    let mut vlan_tags = 0_u8;

    // Bounded max-2 VLAN tag peel (802.1Q / 802.1ad QinQ)
    while eth_proto == ETH_P_8021Q || eth_proto == ETH_P_8021AD {
        if vlan_tags >= 2 {
            return Err(DropReason::ExcessiveVlanNesting);
        }
        if !ensure_bounds(pkt_len, cursor, VLAN_HDR_LEN) {
            return Err(DropReason::TruncatedVlan);
        }
        eth_proto = u16::from_be_bytes([pkt[cursor + 2], pkt[cursor + 3]]);
        cursor += VLAN_HDR_LEN;
        vlan_tags += 1;
    }

    let l3_offset = cursor;
    let (src_ip, dst_ip, l4_proto, l4_offset, l4_end) = match eth_proto {
        ETH_P_IPV4 => {
            if !ensure_bounds(pkt_len, l3_offset, IPV4_MIN_HDR_LEN) {
                return Err(DropReason::TruncatedIpv4);
            }
            let ver_ihl = pkt[l3_offset];
            let version = ver_ihl >> 4;
            let ihl_words = (ver_ihl & 0x0F) as usize;
            if version != 4 || ihl_words < 5 {
                return Err(DropReason::InvalidIpv4IhlOrVer);
            }
            let ihl_bytes = ihl_words * 4;
            if !ensure_bounds(pkt_len, l3_offset, ihl_bytes) {
                return Err(DropReason::TruncatedIpv4);
            }
            let tot_len = u16::from_be_bytes([pkt[l3_offset + 2], pkt[l3_offset + 3]]) as usize;
            if tot_len < ihl_bytes || !ensure_bounds(pkt_len, l3_offset, tot_len) {
                return Err(DropReason::InvalidIpv4TotLen);
            }

            if cfg.verify_ipv4_checksum
                && !verify_ipv4_header_checksum(&pkt[l3_offset..l3_offset + ihl_bytes])
            {
                return Err(DropReason::InvalidIpv4Checksum);
            }

            // Fragmentation security checks:
            // - Non-zero fragment offset bypasses L4 port/SYN inspection -> drop
            // - More-Fragments (MF=1) with < 8B L4 payload -> tiny fragment attack -> drop
            // - Reassembled offset + payload > 65535 -> Ping-of-Death / Teardrop overflow -> drop
            let frag_field = u16::from_be_bytes([pkt[l3_offset + 6], pkt[l3_offset + 7]]);
            let more_frags = (frag_field & 0x2000) != 0;
            let frag_off_bytes = ((frag_field & 0x1FFF) as usize) * 8;
            let ip_payload_len = tot_len - ihl_bytes;
            if frag_off_bytes > 0
                || (more_frags && ip_payload_len < 8)
                || (frag_off_bytes + ip_payload_len > 65535)
            {
                return Err(DropReason::MalformedIpv4Fragment);
            }

            let proto = pkt[l3_offset + 9];
            let saddr = [
                pkt[l3_offset + 12],
                pkt[l3_offset + 13],
                pkt[l3_offset + 14],
                pkt[l3_offset + 15],
            ];
            let daddr = [
                pkt[l3_offset + 16],
                pkt[l3_offset + 17],
                pkt[l3_offset + 18],
                pkt[l3_offset + 19],
            ];
            (
                IpAddrWire::V4(saddr),
                IpAddrWire::V4(daddr),
                proto,
                l3_offset + ihl_bytes,
                l3_offset + tot_len,
            )
        }
        ETH_P_IPV6 => {
            if !ensure_bounds(pkt_len, l3_offset, IPV6_HDR_LEN) {
                return Err(DropReason::TruncatedIpv6);
            }
            let version = pkt[l3_offset] >> 4;
            if version != 6 {
                return Err(DropReason::InvalidIpv6VerOrLen);
            }
            let payload_len =
                u16::from_be_bytes([pkt[l3_offset + 4], pkt[l3_offset + 5]]) as usize;
            let total_end = l3_offset + IPV6_HDR_LEN + payload_len;
            if !ensure_bounds(pkt_len, l3_offset + IPV6_HDR_LEN, payload_len) {
                return Err(DropReason::InvalidIpv6VerOrLen);
            }
            let mut nexthdr = pkt[l3_offset + 6];
            let mut saddr = [0_u8; 16];
            let mut daddr = [0_u8; 16];
            saddr.copy_from_slice(&pkt[l3_offset + 8..l3_offset + 24]);
            daddr.copy_from_slice(&pkt[l3_offset + 24..l3_offset + 40]);

            let mut ext_cursor = l3_offset + IPV6_HDR_LEN;
            // Bounded extension-header skip (Hop-by-Hop = 0, Routing = 43, DestOpts = 60)
            let mut ext_steps = 0_u8;
            while nexthdr == 0 || nexthdr == 43 || nexthdr == 60 {
                if ext_steps >= 2 || !ensure_bounds(total_end, ext_cursor, 8) {
                    return Err(DropReason::InvalidIpv6VerOrLen);
                }
                let next = pkt[ext_cursor];
                let ext_len = ((pkt[ext_cursor + 1] as usize) + 1) * 8;
                if !ensure_bounds(total_end, ext_cursor, ext_len) {
                    return Err(DropReason::InvalidIpv6VerOrLen);
                }
                nexthdr = next;
                ext_cursor += ext_len;
                ext_steps += 1;
            }
            // Drop IPv6 Fragment headers (`nexthdr == 44`) attempting L4 filter evasion
            if nexthdr == IPPROTO_IPV6_FRAG {
                return Err(DropReason::MalformedIpv4Fragment);
            }
            (
                IpAddrWire::V6(saddr),
                IpAddrWire::V6(daddr),
                nexthdr,
                ext_cursor,
                total_end,
            )
        }
        _ => {
            // Non-IP L2 frame (e.g. ARP 0x0806, LLDP) -> safe XDP_PASS with NonIpPass marker
            return Err(DropReason::NonIpPass);
        }
    };

    match l4_proto {
        IPPROTO_TCP => {
            if !ensure_bounds(l4_end, l4_offset, TCP_MIN_HDR_LEN) {
                return Err(DropReason::TruncatedTcp);
            }
            let src_port = u16::from_be_bytes([pkt[l4_offset], pkt[l4_offset + 1]]);
            let dst_port = u16::from_be_bytes([pkt[l4_offset + 2], pkt[l4_offset + 3]]);
            let tcp_seq = u32::from_be_bytes([
                pkt[l4_offset + 4],
                pkt[l4_offset + 5],
                pkt[l4_offset + 6],
                pkt[l4_offset + 7],
            ]);
            let tcp_ack_seq = u32::from_be_bytes([
                pkt[l4_offset + 8],
                pkt[l4_offset + 9],
                pkt[l4_offset + 10],
                pkt[l4_offset + 11],
            ]);
            let doff_words = (pkt[l4_offset + 12] >> 4) as usize;
            if doff_words < 5 {
                return Err(DropReason::InvalidTcpDoff);
            }
            let tcp_hdr_bytes = doff_words * 4;
            if !ensure_bounds(l4_end, l4_offset, tcp_hdr_bytes) {
                return Err(DropReason::TruncatedTcp);
            }
            let flags = pkt[l4_offset + 13];
            let window = u16::from_be_bytes([pkt[l4_offset + 14], pkt[l4_offset + 15]]);

            let syn = (flags & TCP_SYN) != 0;
            let fin = (flags & TCP_FIN) != 0;
            let rst = (flags & TCP_RST) != 0;
            let urg = (flags & TCP_URG) != 0;
            let psh = (flags & TCP_PSH) != 0;
            let ack = (flags & TCP_ACK) != 0;

            if flags == 0
                || (syn && fin)
                || (syn && rst)
                || (fin && urg && psh && !ack)
            {
                return Err(DropReason::InvalidTcpFlags);
            }
            if src_ip == dst_ip && src_port == dst_port {
                return Err(DropReason::LandAttack);
            }

            if cfg.verify_l4_checksum {
                if let (IpAddrWire::V4(s4), IpAddrWire::V4(d4)) = (src_ip, dst_ip) {
                    if !verify_l4_ipv4_checksum(s4, d4, IPPROTO_TCP, &pkt[l4_offset..l4_end]) {
                        return Err(DropReason::InvalidL4Checksum);
                    }
                }
            }

            let l7_offset = l4_offset + tcp_hdr_bytes;
            let l7_slice = &pkt[l7_offset..l4_end];
            let l7 = if (dst_port == cfg.gateway_port || src_port == cfg.gateway_port)
                && !l7_slice.is_empty()
            {
                inspect_gateway_l7(l7_slice, cfg.strict_hyperion_magic_on_gateway)?
            } else {
                L7Classification::None
            };

            Ok(ParsedPacket {
                eth_proto,
                vlan_tags,
                l3_offset: l3_offset as u16,
                l4_offset: l4_offset as u16,
                l7_offset: l7_offset as u16,
                l7_len: l7_slice.len() as u16,
                flow: FiveTuple {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    proto: IPPROTO_TCP,
                },
                tcp_flags: flags,
                tcp_seq,
                tcp_ack_seq,
                tcp_window: window,
                l7,
            })
        }
        IPPROTO_UDP => {
            if !ensure_bounds(l4_end, l4_offset, UDP_HDR_LEN) {
                return Err(DropReason::TruncatedUdp);
            }
            let src_port = u16::from_be_bytes([pkt[l4_offset], pkt[l4_offset + 1]]);
            let dst_port = u16::from_be_bytes([pkt[l4_offset + 2], pkt[l4_offset + 3]]);
            let ulen = u16::from_be_bytes([pkt[l4_offset + 4], pkt[l4_offset + 5]]) as usize;
            if ulen < UDP_HDR_LEN || !ensure_bounds(l4_end, l4_offset, ulen) {
                return Err(DropReason::InvalidUdpLen);
            }
            if src_ip == dst_ip && src_port == dst_port {
                return Err(DropReason::LandAttack);
            }
            if cfg.verify_l4_checksum {
                if let (IpAddrWire::V4(s4), IpAddrWire::V4(d4)) = (src_ip, dst_ip) {
                    if !verify_l4_ipv4_checksum(s4, d4, IPPROTO_UDP, &pkt[l4_offset..l4_offset + ulen]) {
                        return Err(DropReason::InvalidL4Checksum);
                    }
                }
            }

            let l7_offset = l4_offset + UDP_HDR_LEN;
            let l7_slice = &pkt[l7_offset..l4_offset + ulen];
            let l7 = if dst_port == XDP_PROBE_UDP_PORT
                && l7_slice.len() >= 8
                && l7_slice[..8] == XDP_PROBE_REQUEST_MAGIC
            {
                L7Classification::XdpHairpinProbe
            } else if dst_port == cfg.gateway_port && !l7_slice.is_empty() {
                inspect_gateway_l7(l7_slice, cfg.strict_hyperion_magic_on_gateway)?
            } else {
                L7Classification::None
            };

            Ok(ParsedPacket {
                eth_proto,
                vlan_tags,
                l3_offset: l3_offset as u16,
                l4_offset: l4_offset as u16,
                l7_offset: l7_offset as u16,
                l7_len: l7_slice.len() as u16,
                flow: FiveTuple {
                    src_ip,
                    dst_ip,
                    src_port,
                    dst_port,
                    proto: IPPROTO_UDP,
                },
                tcp_flags: 0,
                tcp_seq: 0,
                tcp_ack_seq: 0,
                tcp_window: 0,
                l7,
            })
        }
        _ => Ok(ParsedPacket {
            eth_proto,
            vlan_tags,
            l3_offset: l3_offset as u16,
            l4_offset: l4_offset as u16,
            l7_offset: l4_offset as u16,
            l7_len: (l4_end - l4_offset) as u16,
            flow: FiveTuple {
                src_ip,
                dst_ip,
                src_port: 0,
                dst_port: 0,
                proto: l4_proto,
            },
            tcp_flags: 0,
            tcp_seq: 0,
            tcp_ack_seq: 0,
            tcp_window: 0,
            l7: L7Classification::None,
        }),
    }
}

/// Rewrites an inbound UDP `HYPPING1` frame in-place into an outbound `HYPPONG1` `XDP_TX` reply
/// (swaps Ethernet MACs, swaps IPv4/IPv6 addresses, swaps UDP ports, updates RFC 1071 checksums).
pub fn rewrite_udp_probe_xdp_tx(pkt: &mut [u8], parsed: &ParsedPacket) -> Result<(), DropReason> {
    if !ensure_bounds(pkt.len(), 0, ETH_HDR_LEN) {
        return Err(DropReason::TruncatedEth);
    }
    let l3 = parsed.l3_offset as usize;
    let l4 = parsed.l4_offset as usize;
    let l7 = parsed.l7_offset as usize;
    if !ensure_bounds(pkt.len(), l7, 8) {
        return Err(DropReason::TruncatedUdp);
    }

    // 1. Swap Ethernet MACs
    let mut dst_mac = [0_u8; 6];
    dst_mac.copy_from_slice(&pkt[0..6]);
    pkt.copy_within(6..12, 0);
    pkt[6..12].copy_from_slice(&dst_mac);

    // 2. Swap IP addresses
    match (parsed.flow.src_ip, parsed.flow.dst_ip) {
        (IpAddrWire::V4(src), IpAddrWire::V4(dst)) => {
            if !ensure_bounds(pkt.len(), l3, IPV4_MIN_HDR_LEN) {
                return Err(DropReason::TruncatedIpv4);
            }
            pkt[l3 + 12..l3 + 16].copy_from_slice(&dst);
            pkt[l3 + 16..l3 + 20].copy_from_slice(&src);
            // Recompute IPv4 header checksum
            let ihl_bytes = ((pkt[l3] & 0x0F) as usize) * 4;
            let ip_csum = ipv4_header_checksum(&pkt[l3..l3 + ihl_bytes]).to_be_bytes();
            pkt[l3 + 10..l3 + 12].copy_from_slice(&ip_csum);

            // Swap UDP ports & write HYPPONG1
            let sp = parsed.flow.src_port.to_be_bytes();
            let dp = parsed.flow.dst_port.to_be_bytes();
            pkt[l4..l4 + 2].copy_from_slice(&dp);
            pkt[l4 + 2..l4 + 4].copy_from_slice(&sp);
            pkt[l7..l7 + 8].copy_from_slice(&XDP_PROBE_REPLY_MAGIC);

            let ulen = u16::from_be_bytes([pkt[l4 + 4], pkt[l4 + 5]]) as usize;
            let udp_csum =
                l4_ipv4_checksum(dst, src, IPPROTO_UDP, &pkt[l4..l4 + ulen], 6).to_be_bytes();
            pkt[l4 + 6..l4 + 8].copy_from_slice(&udp_csum);
        }
        (IpAddrWire::V6(src), IpAddrWire::V6(dst)) => {
            if !ensure_bounds(pkt.len(), l3, IPV6_HDR_LEN) {
                return Err(DropReason::TruncatedIpv6);
            }
            pkt[l3 + 8..l3 + 24].copy_from_slice(&dst);
            pkt[l3 + 24..l3 + 40].copy_from_slice(&src);
            let sp = parsed.flow.src_port.to_be_bytes();
            let dp = parsed.flow.dst_port.to_be_bytes();
            pkt[l4..l4 + 2].copy_from_slice(&dp);
            pkt[l4 + 2..l4 + 4].copy_from_slice(&sp);
            pkt[l7..l7 + 8].copy_from_slice(&XDP_PROBE_REPLY_MAGIC);
            pkt[l4 + 6..l4 + 8].copy_from_slice(&[0, 0]);
        }
        _ => return Err(DropReason::TruncatedIpv4),
    }

    Ok(())
}

/// Rewrites an inbound TCP SYN in-place into a stateless `XDP_TX` SYN+ACK cookie challenge.
pub fn rewrite_tcp_syn_cookie_xdp_tx(
    pkt: &mut [u8],
    parsed: &ParsedPacket,
    cookie_isn: u32,
) -> Result<(), DropReason> {
    let l3 = parsed.l3_offset as usize;
    let l4 = parsed.l4_offset as usize;
    if !ensure_bounds(pkt.len(), l4, TCP_MIN_HDR_LEN) {
        return Err(DropReason::TruncatedTcp);
    }

    let mut dst_mac = [0_u8; 6];
    dst_mac.copy_from_slice(&pkt[0..6]);
    pkt.copy_within(6..12, 0);
    pkt[6..12].copy_from_slice(&dst_mac);

    if let (IpAddrWire::V4(src), IpAddrWire::V4(dst)) = (parsed.flow.src_ip, parsed.flow.dst_ip) {
        if !ensure_bounds(pkt.len(), l3, IPV4_MIN_HDR_LEN) {
            return Err(DropReason::TruncatedIpv4);
        }
        pkt[l3 + 12..l3 + 16].copy_from_slice(&dst);
        pkt[l3 + 16..l3 + 20].copy_from_slice(&src);
        let ihl_bytes = ((pkt[l3] & 0x0F) as usize) * 4;
        let ip_csum = ipv4_header_checksum(&pkt[l3..l3 + ihl_bytes]).to_be_bytes();
        pkt[l3 + 10..l3 + 12].copy_from_slice(&ip_csum);

        let sp = parsed.flow.src_port.to_be_bytes();
        let dp = parsed.flow.dst_port.to_be_bytes();
        pkt[l4..l4 + 2].copy_from_slice(&dp);
        pkt[l4 + 2..l4 + 4].copy_from_slice(&sp);
        pkt[l4 + 4..l4 + 8].copy_from_slice(&cookie_isn.to_be_bytes());
        pkt[l4 + 8..l4 + 12].copy_from_slice(&parsed.tcp_seq.wrapping_add(1).to_be_bytes());
        pkt[l4 + 13] = TCP_SYN | TCP_ACK;

        let tot_len = u16::from_be_bytes([pkt[l3 + 2], pkt[l3 + 3]]) as usize;
        let l4_end = l3 + tot_len;
        let tcp_csum = l4_ipv4_checksum(dst, src, IPPROTO_TCP, &pkt[l4..l4_end], 16).to_be_bytes();
        pkt[l4 + 16..l4 + 18].copy_from_slice(&tcp_csum);
        Ok(())
    } else {
        Err(DropReason::TruncatedIpv4)
    }
}
