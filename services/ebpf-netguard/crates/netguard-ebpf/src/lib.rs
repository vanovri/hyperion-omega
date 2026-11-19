#![no_std]
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! `#![no_std]` Aya-compatible Rust eBPF Kernel Entrypoints (`XDP` ingress filter & `TC` socket RTT probe).
//!
//! Shares the exact zero-copy packet dissector and `#[repr(C)]` map value layouts from
//! `netguard-common` so kernel bytecode and userspace harness execute identical logic.

use netguard_common::parser::{
    parse_frame_zero_copy, rewrite_tcp_syn_cookie_xdp_tx, rewrite_udp_probe_xdp_tx,
    L7Classification, ParsedPacket, ParserConfig,
};
use netguard_common::{
    BlockRuleVal, DropReason, FiveTuple, IpAddrWire, RuleAction, TcAction, XdpAction,
};

/// Kernel BPF Map callback trait abstracting `aya_ebpf::maps::{LpmTrie, LruHashMap, PerCpuArray}`
/// and the deterministic Userspace BPF Map engine without heap allocation.
pub trait KernelBpfMapCallbacks {
    fn ktime_get_ns(&self) -> u64;
    fn lookup_ipv4_lpm(&self, ip: [u8; 4], now_ns: u64) -> Option<BlockRuleVal>;
    fn lookup_ipv6_lpm(&self, ip: [u8; 16], now_ns: u64) -> Option<BlockRuleVal>;
    fn is_account_frozen(&self, account_id: u64, now_ns: u64) -> bool;
    fn check_established_conntrack(&mut self, flow: &FiveTuple, now_ns: u64) -> Option<u64>;
    fn check_token_bucket(
        &mut self,
        src_ip: IpAddrWire,
        now_ns: u64,
        cost_packets: u32,
        rate_override_rps: Option<u32>,
    ) -> bool;
    fn compute_syn_cookie(&self, flow: &FiveTuple, client_seq: u32, now_ns: u64) -> u32;
    fn record_ingress_rtt_if_matched(&mut self, flow: &FiveTuple, now_ns: u64);
    fn record_egress_tx_timestamp(&mut self, flow: &FiveTuple, now_ns: u64);
    fn bump_reason_counter(&mut self, action: XdpAction, reason: DropReason);
}

/// Decision output from `#![no_std]` XDP ingress execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XdpVerdict {
    pub action: XdpAction,
    pub reason: DropReason,
    pub parsed: Option<ParsedPacket>,
}

/// Executes the complete zero-trust `#![no_std]` XDP ingress filter over a mutable raw Ethernet buffer.
#[inline]
pub fn hyperion_xdp_filter_frame<M: KernelBpfMapCallbacks>(
    frame: &mut [u8],
    cfg: &ParserConfig,
    maps: &mut M,
) -> XdpVerdict {
    let parsed = match parse_frame_zero_copy(frame, cfg) {
        Ok(p) => p,
        Err(DropReason::NonIpPass) => {
            maps.bump_reason_counter(XdpAction::Pass, DropReason::NonIpPass);
            return XdpVerdict {
                action: XdpAction::Pass,
                reason: DropReason::NonIpPass,
                parsed: None,
            };
        }
        Err(drop_reason) => {
            maps.bump_reason_counter(XdpAction::Drop, drop_reason);
            return XdpVerdict {
                action: XdpAction::Drop,
                reason: drop_reason,
                parsed: None,
            };
        }
    };

    let now_ns = maps.ktime_get_ns();

    // 1. Wire-speed UDP hairpin echo probe (`XDP_TX`)
    if parsed.l7 == L7Classification::XdpHairpinProbe
        && rewrite_udp_probe_xdp_tx(frame, &parsed).is_ok()
    {
        maps.bump_reason_counter(XdpAction::Tx, DropReason::XdpTxProbeReply);
        return XdpVerdict {
            action: XdpAction::Tx,
            reason: DropReason::XdpTxProbeReply,
            parsed: Some(parsed),
        };
    }

    // 2. LPM CIDR Blocklist Lookup (IPv4 / IPv6)
    let matched_rule = match parsed.flow.src_ip {
        IpAddrWire::V4(v4) => maps.lookup_ipv4_lpm(v4, now_ns),
        IpAddrWire::V6(v6) => maps.lookup_ipv6_lpm(v6, now_ns),
    };

    let mut rate_override_rps: Option<u32> = None;
    if let Some(rule) = matched_rule {
        match rule.action {
            RuleAction::HardDrop => {
                let reason = match parsed.flow.src_ip {
                    IpAddrWire::V4(_) => DropReason::Ipv4BlocklistMatch,
                    IpAddrWire::V6(_) => DropReason::Ipv6BlocklistMatch,
                };
                maps.bump_reason_counter(XdpAction::Drop, reason);
                return XdpVerdict {
                    action: XdpAction::Drop,
                    reason,
                    parsed: Some(parsed),
                };
            }
            RuleAction::DropNewSynOnly => {
                if parsed.is_tcp_syn_only() {
                    let reason = match parsed.flow.src_ip {
                        IpAddrWire::V4(_) => DropReason::Ipv4BlocklistMatch,
                        IpAddrWire::V6(_) => DropReason::Ipv6BlocklistMatch,
                    };
                    maps.bump_reason_counter(XdpAction::Drop, reason);
                    return XdpVerdict {
                        action: XdpAction::Drop,
                        reason,
                        parsed: Some(parsed),
                    };
                }
            }
            RuleAction::SynCookieTx => {
                if parsed.is_tcp_syn_only() {
                    let cookie = maps.compute_syn_cookie(&parsed.flow, parsed.tcp_seq, now_ns);
                    if rewrite_tcp_syn_cookie_xdp_tx(frame, &parsed, cookie).is_ok() {
                        maps.bump_reason_counter(XdpAction::Tx, DropReason::XdpTxSynCookie);
                        return XdpVerdict {
                            action: XdpAction::Tx,
                            reason: DropReason::XdpTxSynCookie,
                            parsed: Some(parsed),
                        };
                    }
                }
            }
            RuleAction::ThrottleRps => {
                rate_override_rps = Some(rule.rate_override_rps);
            }
            RuleAction::Allow => {}
        }
    }

    // 3. Stateful Established Conntrack Check (preserves healthy active TCP sessions without RST,
    //    UNLESS Aegis explicitly froze the bound `account_id`!).
    if parsed.is_tcp_established_ack() {
        if let Some(bound_account_id) = maps.check_established_conntrack(&parsed.flow, now_ns) {
            if bound_account_id != 0 && maps.is_account_frozen(bound_account_id, now_ns) {
                maps.bump_reason_counter(XdpAction::Drop, DropReason::AccountFrozenMitigation);
                return XdpVerdict {
                    action: XdpAction::Drop,
                    reason: DropReason::AccountFrozenMitigation,
                    parsed: Some(parsed),
                };
            }
            maps.record_ingress_rtt_if_matched(&parsed.flow, now_ns);
            if let Some(rule) = matched_rule {
                if rule.action == RuleAction::DropNewSynOnly {
                    maps.bump_reason_counter(
                        XdpAction::Pass,
                        DropReason::EstablishedConntrackPass,
                    );
                    return XdpVerdict {
                        action: XdpAction::Pass,
                        reason: DropReason::EstablishedConntrackPass,
                        parsed: Some(parsed),
                    };
                }
            }
        }
    }

    // 4. Per-Source-IP Token-Bucket Rate Limiter (SYN Flood & WebSocket Connection Storm)
    let is_syn = parsed.is_tcp_syn_only();
    let is_ws_storm_candidate = parsed.is_ws_upgrade_or_sub();
    let cost_packets = if is_syn {
        5
    } else if is_ws_storm_candidate {
        3
    } else {
        1
    };

    if (is_syn || is_ws_storm_candidate || rate_override_rps.is_some())
        && !maps.check_token_bucket(parsed.flow.src_ip, now_ns, cost_packets, rate_override_rps)
    {
        let reason = if is_ws_storm_candidate && !is_syn {
            DropReason::WsStormRateLimited
        } else {
            DropReason::SynFloodRateLimited
        };
        maps.bump_reason_counter(XdpAction::Drop, reason);
        return XdpVerdict {
            action: XdpAction::Drop,
            reason,
            parsed: Some(parsed),
        };
    }

    maps.record_ingress_rtt_if_matched(&parsed.flow, now_ns);
    maps.bump_reason_counter(XdpAction::Pass, DropReason::None);
    XdpVerdict {
        action: XdpAction::Pass,
        reason: DropReason::None,
        parsed: Some(parsed),
    }
}

/// `#![no_std]` TC egress hook recording outbound TCP segment timestamps for kernel RTT tracking.
#[inline]
pub fn hyperion_tc_egress_frame<M: KernelBpfMapCallbacks>(
    frame: &[u8],
    cfg: &ParserConfig,
    maps: &mut M,
) -> TcAction {
    if let Ok(parsed) = parse_frame_zero_copy(frame, cfg) {
        if parsed.flow.proto == netguard_common::parser::IPPROTO_TCP {
            let now_ns = maps.ktime_get_ns();
            maps.record_egress_tx_timestamp(&parsed.flow, now_ns);
        }
    }
    TcAction::Ok
}
