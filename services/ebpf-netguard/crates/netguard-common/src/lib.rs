#![no_std]
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! `#![no_std]` shared ABI types, HDR histogram indexing math, RFC 1071 checksums,
//! and verifier-safe zero-copy packet parser shared between Kernel eBPF (`netguard-ebpf`)
//! and the Userspace Control Daemon (`netguard-daemon`).

pub mod checksum;
pub mod parser;

/// HYPERION v1 Binary Wire Protocol Constants (`contracts/ws-binary-protocol.md`).
pub const HYPERION_WIRE_MAGIC_LE: u16 = 0x4859; // Wire bytes: [0x59 ('Y'), 0x48 ('H')]
pub const HYPERION_WIRE_VERSION: u8 = 0x01;
pub const HYPERION_WIRE_HEADER_LEN: usize = 32;
pub const HYPERION_MAX_INSTRUMENT_ID: u16 = 64;
pub const HYPERION_MAX_PAYLOAD_LEN: u32 = 4 * 1024 * 1024;

/// Default Go Edge Gateway port protected by XDP ingress shield (`00-hyperion-contract.md`).
pub const DEFAULT_GATEWAY_PORT: u16 = 8080;
/// Wire-speed UDP hairpin echo probe port (`XDP_TX` responder).
pub const XDP_PROBE_UDP_PORT: u16 = 18095;
pub const XDP_PROBE_REQUEST_MAGIC: [u8; 8] = *b"HYPPING1";
pub const XDP_PROBE_REPLY_MAGIC: [u8; 8] = *b"HYPPONG1";

/// HDR Log2-Linear Histogram Dimensions (64 coarse log2 buckets x 8 linear sub-buckets = 512 bins).
pub const HDR_COARSE_BUCKETS: usize = 64;
pub const HDR_SUB_BUCKETS: usize = 8;
pub const HDR_TOTAL_BINS: usize = HDR_COARSE_BUCKETS * HDR_SUB_BUCKETS;

/// Canonical Linux XDP return action codes (`enum xdp_action`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum XdpAction {
    Aborted = 0,
    Drop = 1,
    Pass = 2,
    Tx = 3,
    Redirect = 4,
}

impl XdpAction {
    #[inline]
    pub const fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Aborted,
            1 => Self::Drop,
            2 => Self::Pass,
            3 => Self::Tx,
            4 => Self::Redirect,
            _ => Self::Aborted,
        }
    }

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aborted => "XDP_ABORTED",
            Self::Drop => "XDP_DROP",
            Self::Pass => "XDP_PASS",
            Self::Tx => "XDP_TX",
            Self::Redirect => "XDP_REDIRECT",
        }
    }
}

/// Canonical Linux TC action codes.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TcAction {
    Ok = 0,
    Shot = 2,
    Pipe = 3,
}

/// Blocklist / Mitigation Policy Action stored in BPF maps.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleAction {
    Allow = 0,
    HardDrop = 1,
    /// Drops new inbound TCP SYN while preserving established 5-tuple sessions without RST.
    DropNewSynOnly = 2,
    /// Overrides per-IP / per-account token-bucket rate limit in packets/requests per sec.
    ThrottleRps = 3,
    /// Challenges inbound TCP SYN via stateless `XDP_TX` SipHash SYN-Cookie reply.
    SynCookieTx = 4,
}

/// Exhaustive classification & drop reason telemetry code (`0..31`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    None = 0,
    TruncatedEth = 1,
    TruncatedVlan = 2,
    NonIpPass = 3,
    TruncatedIpv4 = 4,
    InvalidIpv4IhlOrVer = 5,
    InvalidIpv4TotLen = 6,
    MalformedIpv4Fragment = 7,
    LandAttack = 8,
    TruncatedIpv6 = 9,
    InvalidIpv6VerOrLen = 10,
    Ipv4BlocklistMatch = 11,
    Ipv6BlocklistMatch = 12,
    TruncatedTcp = 13,
    InvalidTcpDoff = 14,
    InvalidTcpFlags = 15,
    TruncatedUdp = 16,
    InvalidUdpLen = 17,
    SynFloodRateLimited = 18,
    WsStormRateLimited = 19,
    HyperionBadMagic = 20,
    HyperionBadVersion = 21,
    HyperionNonZeroReserved = 22,
    HyperionInvalidMsgType = 23,
    HyperionPayloadOversize = 24,
    AccountFrozenMitigation = 25,
    XdpTxProbeReply = 26,
    XdpTxSynCookie = 27,
    EstablishedConntrackPass = 28,
    InvalidIpv4Checksum = 29,
    InvalidL4Checksum = 30,
    ExcessiveVlanNesting = 31,
}

pub const DROP_REASON_COUNT: usize = 32;

impl DropReason {
    #[inline]
    pub const fn from_u8(code: u8) -> Self {
        match code {
            0 => Self::None,
            1 => Self::TruncatedEth,
            2 => Self::TruncatedVlan,
            3 => Self::NonIpPass,
            4 => Self::TruncatedIpv4,
            5 => Self::InvalidIpv4IhlOrVer,
            6 => Self::InvalidIpv4TotLen,
            7 => Self::MalformedIpv4Fragment,
            8 => Self::LandAttack,
            9 => Self::TruncatedIpv6,
            10 => Self::InvalidIpv6VerOrLen,
            11 => Self::Ipv4BlocklistMatch,
            12 => Self::Ipv6BlocklistMatch,
            13 => Self::TruncatedTcp,
            14 => Self::InvalidTcpDoff,
            15 => Self::InvalidTcpFlags,
            16 => Self::TruncatedUdp,
            17 => Self::InvalidUdpLen,
            18 => Self::SynFloodRateLimited,
            19 => Self::WsStormRateLimited,
            20 => Self::HyperionBadMagic,
            21 => Self::HyperionBadVersion,
            22 => Self::HyperionNonZeroReserved,
            23 => Self::HyperionInvalidMsgType,
            24 => Self::HyperionPayloadOversize,
            25 => Self::AccountFrozenMitigation,
            26 => Self::XdpTxProbeReply,
            27 => Self::XdpTxSynCookie,
            28 => Self::EstablishedConntrackPass,
            29 => Self::InvalidIpv4Checksum,
            30 => Self::InvalidL4Checksum,
            _ => Self::ExcessiveVlanNesting,
        }
    }

    #[inline]
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::None => "pass_ok",
            Self::TruncatedEth => "truncated_eth",
            Self::TruncatedVlan => "truncated_vlan",
            Self::NonIpPass => "non_ip_pass",
            Self::TruncatedIpv4 => "truncated_ipv4",
            Self::InvalidIpv4IhlOrVer => "invalid_ipv4_ihl_or_ver",
            Self::InvalidIpv4TotLen => "invalid_ipv4_tot_len",
            Self::MalformedIpv4Fragment => "malformed_ipv4_fragment",
            Self::LandAttack => "land_attack",
            Self::TruncatedIpv6 => "truncated_ipv6",
            Self::InvalidIpv6VerOrLen => "invalid_ipv6_ver_or_len",
            Self::Ipv4BlocklistMatch => "ipv4_blocklist_match",
            Self::Ipv6BlocklistMatch => "ipv6_blocklist_match",
            Self::TruncatedTcp => "truncated_tcp",
            Self::InvalidTcpDoff => "invalid_tcp_doff",
            Self::InvalidTcpFlags => "invalid_tcp_flags",
            Self::TruncatedUdp => "truncated_udp",
            Self::InvalidUdpLen => "invalid_udp_len",
            Self::SynFloodRateLimited => "syn_flood_rate_limited",
            Self::WsStormRateLimited => "ws_storm_rate_limited",
            Self::HyperionBadMagic => "hyperion_bad_magic",
            Self::HyperionBadVersion => "hyperion_bad_version",
            Self::HyperionNonZeroReserved => "hyperion_nonzero_reserved",
            Self::HyperionInvalidMsgType => "hyperion_invalid_msg_type",
            Self::HyperionPayloadOversize => "hyperion_payload_oversize",
            Self::AccountFrozenMitigation => "account_frozen_mitigation",
            Self::XdpTxProbeReply => "xdp_tx_probe_reply",
            Self::XdpTxSynCookie => "xdp_tx_syn_cookie",
            Self::EstablishedConntrackPass => "established_conntrack_pass",
            Self::InvalidIpv4Checksum => "invalid_ipv4_checksum",
            Self::InvalidL4Checksum => "invalid_l4_checksum",
            Self::ExcessiveVlanNesting => "excessive_vlan_nesting",
        }
    }
}

/// Network-layer address normalized for zero-allocation 5-tuple hashing and LPM lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpAddrWire {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl IpAddrWire {
    #[inline]
    pub const fn to_canonical_16(self) -> [u8; 16] {
        match self {
            Self::V4(b) => [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, b[0], b[1], b[2], b[3]],
            Self::V6(b) => b,
        }
    }
}

/// Canonical bidirectional-capable L3/L4 5-tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FiveTuple {
    pub src_ip: IpAddrWire,
    pub dst_ip: IpAddrWire,
    pub src_port: u16,
    pub dst_port: u16,
    pub proto: u8,
}

impl FiveTuple {
    #[inline]
    pub const fn reverse(self) -> Self {
        Self {
            src_ip: self.dst_ip,
            dst_ip: self.src_ip,
            src_port: self.dst_port,
            dst_port: self.src_port,
            proto: self.proto,
        }
    }
}

/// Shared `#[repr(C)]` rule value matching `struct block_rule_val` in `hyperion_bpf_common.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRuleVal {
    pub action: RuleAction,
    pub rate_override_rps: u32,
    pub expires_ts_ns: u64,
    pub rule_generation: u64,
}

impl BlockRuleVal {
    #[inline]
    pub const fn is_active(&self, now_ns: u64) -> bool {
        self.expires_ts_ns == 0 || now_ns < self.expires_ts_ns
    }
}

/// Maps any `u64` nanosecond latency sample into `[0, 511]` in `O(1)` using hardware `leading_zeros`.
///
/// Bins `0..8` represent exact `0..7 ns`.
/// For `value_ns >= 8`, let `msb = 63 - value_ns.leading_zeros()` (`3..=63`),
/// coarse group = `msb - 2` (`1..=61`), and linear 3-bit sub-bucket = `(value_ns >> (msb - 3)) & 0x7`.
#[inline]
pub const fn hdr_bin_index(value_ns: u64) -> usize {
    if value_ns < 8 {
        return value_ns as usize;
    }
    let msb = (63 - value_ns.leading_zeros()) as usize; // 3..=63
    let shift = msb - 3;
    let sub = ((value_ns >> shift) & 0x7) as usize;
    let bin = ((msb - 2) * HDR_SUB_BUCKETS) + sub;
    if bin >= HDR_TOTAL_BINS {
        HDR_TOTAL_BINS - 1
    } else {
        bin
    }
}

/// Exact inclusive `(low_ns, high_ns)` bounds of HDR bin `bin` (`0..512`).
#[inline]
pub const fn hdr_bin_bounds_ns(bin: usize) -> (u64, u64) {
    let clamped = if bin >= HDR_TOTAL_BINS {
        HDR_TOTAL_BINS - 1
    } else {
        bin
    };
    if clamped < 8 {
        let v = clamped as u64;
        return (v, v);
    }
    let group = clamped / HDR_SUB_BUCKETS; // 1..=63
    let sub = (clamped % HDR_SUB_BUCKETS) as u64; // 0..=7
    let msb = group + 2; // 3..=65
    if msb >= 64 {
        return (u64::MAX - 7, u64::MAX);
    }
    let shift = msb - 3;
    let step = 1_u64 << shift;
    let low = (1_u64 << msb) | (sub << shift);
    let high = low.saturating_add(step - 1);
    (low, high)
}

/// Midpoint integer nanosecond estimate for quantile reporting (zero floating-point).
#[inline]
pub const fn hdr_bin_midpoint_ns(bin: usize) -> u64 {
    let (lo, hi) = hdr_bin_bounds_ns(bin);
    lo + ((hi - lo) / 2)
}
