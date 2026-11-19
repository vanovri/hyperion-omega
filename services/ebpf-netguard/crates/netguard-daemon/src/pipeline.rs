//! High-Speed XDP Packet Processing Pipeline & 3-Stage Optimization Tiers.
//!
//! Implements `KernelBpfMapCallbacks` so both the `#![no_std]` native XDP classifier
//! (`hyperion_xdp_filter_frame`) and the Userspace eBPF Bytecode VM (`BpfVm::execute_xdp`)
//! execute over identical lock-free RCU maps, token buckets, conntrack tables, and HDR histograms.

use crate::ebpf_vm::{BpfVm, VerifiedBpfProgram};
use crate::lpm::{parse_cidr_spec, PolicySnapshot, RcuPolicyStore};
use crate::ratelimit::{ConntrackTable, PerWorkerRateLimiter, RateLimitConfig};
use crate::telemetry::NetGuardTelemetryRegistry;
use netguard_common::parser::{parse_frame_zero_copy, ParserConfig};
use netguard_common::{
    BlockRuleVal, DropReason, FiveTuple, IpAddrWire, RuleAction, TcAction, XdpAction,
};
use netguard_ebpf::{
    hyperion_tc_egress_frame, hyperion_xdp_filter_frame, KernelBpfMapCallbacks, XdpVerdict,
};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

const RTT_RING_SLOTS: usize = 1024;
const RTT_RING_MASK: usize = RTT_RING_SLOTS - 1;

#[derive(Debug, Clone, Copy, Default)]
struct FlowTimestampSlot {
    flow_hash: u64,
    tx_ts_ns: u64,
}

#[inline(always)]
fn hash_five_tuple(ft: &FiveTuple) -> u64 {
    let mut h: u64 = 0x517C_C1B7_2722_0A95;
    match (ft.src_ip, ft.dst_ip) {
        (IpAddrWire::V4(s), IpAddrWire::V4(d)) => {
            let sd = ((u32::from_ne_bytes(s) as u64) << 32) | (u32::from_ne_bytes(d) as u64);
            h ^= sd.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        }
        _ => {
            let s16 = ft.src_ip.to_canonical_16();
            let d16 = ft.dst_ip.to_canonical_16();
            let s_lo = u64::from_ne_bytes([s16[0], s16[1], s16[2], s16[3], s16[4], s16[5], s16[6], s16[7]]);
            let s_hi = u64::from_ne_bytes([s16[8], s16[9], s16[10], s16[11], s16[12], s16[13], s16[14], s16[15]]);
            let d_lo = u64::from_ne_bytes([d16[0], d16[1], d16[2], d16[3], d16[4], d16[5], d16[6], d16[7]]);
            let d_hi = u64::from_ne_bytes([d16[8], d16[9], d16[10], d16[11], d16[12], d16[13], d16[14], d16[15]]);
            h ^= s_lo.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            h ^= s_hi.wrapping_mul(0x94D0_49BB_1331_11EB);
            h ^= d_lo.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            h ^= d_hi.wrapping_mul(0x517C_C1B7_2722_0A95);
        }
    }
    h ^= ((ft.src_port as u64) << 16) | (ft.dst_port as u64) | ((ft.proto as u64) << 40);
    h = (h ^ (h >> 30)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (h ^ (h >> 31)) | 1
}

/// Shared control-plane handles referenced by all XDP worker cores.
pub struct NetGuardSharedState {
    pub policy_store: RcuPolicyStore,
    pub conntrack: ConntrackTable,
    pub telemetry: NetGuardTelemetryRegistry,
    pub parser_cfg: ParserConfig,
    pub rate_cfg: RateLimitConfig,
    pub syn_cookie_secret: u64,
}

impl NetGuardSharedState {
    pub fn new(iface: impl Into<String>, num_cpus: usize) -> Arc<Self> {
        Arc::new(Self {
            policy_store: RcuPolicyStore::new(),
            conntrack: ConntrackTable::new(),
            telemetry: NetGuardTelemetryRegistry::new(iface, num_cpus),
            parser_cfg: ParserConfig::default(),
            rate_cfg: RateLimitConfig::default(),
            syn_cookie_secret: 0x4859_5045_5249_4F4E, // "HYPERION"
        })
    }
}

/// Tier-3 Production Cache-Line Aligned (`#[repr(align(64))]`) Per-Core XDP Worker.
///
/// Zero locks and zero heap allocations on the packet hot path:
/// - Checks `shared.policy_store.current_generation()` (`Ordering::Acquire`) and only reloads
///   `Arc<PolicySnapshot>` when Aegis publishes a new RCU epoch.
/// - Uses `DIR-16-8-8` O(1) flat table for IPv4 LPM (`1..3` L2-cached array loads).
/// - Uses per-worker L1 open-addressed token bucket & RTT timestamp ring.
#[repr(align(64))]
pub struct XdpWorkerEngine {
    pub cpu_id: usize,
    pub clock_ns: u64,
    pub use_radix_for_ipv4: bool,
    shared: Arc<NetGuardSharedState>,
    cached_gen: u64,
    cached_policy: Arc<PolicySnapshot>,
    cached_conntrack_gen: u64,
    local_conntrack: HashMap<FiveTuple, u64>,
    limiter: PerWorkerRateLimiter,
    has_egress_rtt_stamps: bool,
    tx_tstamp_ring: Box<[FlowTimestampSlot; RTT_RING_SLOTS]>,
}

impl XdpWorkerEngine {
    pub fn new(cpu_id: usize, shared: Arc<NetGuardSharedState>) -> Self {
        let snap = shared.policy_store.load_snapshot();
        let gen = snap.generation;
        let ct_gen = shared.conntrack.current_generation();
        let local_ct = shared.conntrack.snapshot_map();
        let limiter = PerWorkerRateLimiter::new(shared.rate_cfg);
        Self {
            cpu_id,
            clock_ns: 1_700_000_000_000_000_000,
            use_radix_for_ipv4: false,
            shared,
            cached_gen: gen,
            cached_policy: snap,
            cached_conntrack_gen: ct_gen,
            local_conntrack: local_ct,
            limiter,
            has_egress_rtt_stamps: false,
            tx_tstamp_ring: Box::new([FlowTimestampSlot::default(); RTT_RING_SLOTS]),
        }
    }

    /// Synchronizes local established-session cache from shared control plane (zero lock during steady bursts).
    pub fn sync_conntrack(&mut self) {
        self.local_conntrack = self.shared.conntrack.snapshot_map();
        self.cached_conntrack_gen = self.shared.conntrack.current_generation();
    }

    #[inline(always)]
    fn refresh_rcu_if_needed(&mut self) {
        let cur_gen = self.shared.policy_store.current_generation();
        if cur_gen != self.cached_gen {
            self.cached_policy = self.shared.policy_store.load_snapshot();
            self.cached_gen = self.cached_policy.generation;
        }
        let ct_gen = self.shared.conntrack.current_generation();
        if ct_gen != self.cached_conntrack_gen {
            self.local_conntrack = self.shared.conntrack.snapshot_map();
            self.cached_conntrack_gen = ct_gen;
        }
    }

    /// Fast-path native `#![no_std]` XDP execution (`0` heap allocs, `DIR-16-8-8` LPM).
    #[inline(always)]
    pub fn process_ingress_frame(&mut self, frame: &mut [u8], now_ns: u64) -> XdpVerdict {
        self.clock_ns = now_ns;
        self.refresh_rcu_if_needed();
        let cfg = self.shared.parser_cfg;
        hyperion_xdp_filter_frame(frame, &cfg, self)
    }

    /// Executes the verified 64-bit eBPF ISA bytecode inside Userspace `BpfVm`.
    #[inline]
    pub fn process_ingress_via_bpf_vm(
        &mut self,
        prog: &VerifiedBpfProgram,
        frame: &mut [u8],
        now_ns: u64,
    ) -> XdpVerdict {
        self.clock_ns = now_ns;
        self.refresh_rcu_if_needed();
        let cfg = self.shared.parser_cfg;
        BpfVm::execute_xdp(prog, frame, &cfg, self)
    }

    /// Executes TC egress hook to stamp outbound TCP segment time for RTT histogram calculation.
    #[inline]
    pub fn process_egress_tc_frame(&mut self, frame: &[u8], now_ns: u64) -> TcAction {
        self.clock_ns = now_ns;
        let cfg = self.shared.parser_cfg;
        hyperion_tc_egress_frame(frame, &cfg, self)
    }
}

impl KernelBpfMapCallbacks for XdpWorkerEngine {
    #[inline(always)]
    fn ktime_get_ns(&self) -> u64 {
        self.clock_ns
    }

    #[inline(always)]
    fn lookup_ipv4_lpm(&self, ip: [u8; 4], now_ns: u64) -> Option<BlockRuleVal> {
        if self.use_radix_for_ipv4 {
            self.cached_policy.ipv4_radix.lookup_bits(&ip, 32, now_ns)
        } else {
            self.cached_policy.ipv4_dir16.lookup(ip, now_ns)
        }
    }

    #[inline(always)]
    fn lookup_ipv6_lpm(&self, ip: [u8; 16], now_ns: u64) -> Option<BlockRuleVal> {
        self.cached_policy.ipv6_radix.lookup_bits(&ip, 128, now_ns)
    }

    #[inline(always)]
    fn is_account_frozen(&self, account_id: u64, now_ns: u64) -> bool {
        match self.cached_policy.account_rules.get(&account_id) {
            Some(rule) => rule.is_active(now_ns) && rule.action == RuleAction::HardDrop,
            None => false,
        }
    }

    #[inline(always)]
    fn check_established_conntrack(&mut self, flow: &FiveTuple, _now_ns: u64) -> Option<u64> {
        if self.local_conntrack.is_empty() {
            return None;
        }
        self.local_conntrack.get(flow).copied()
    }

    #[inline(always)]
    fn check_token_bucket(
        &mut self,
        src_ip: IpAddrWire,
        now_ns: u64,
        cost_packets: u32,
        rate_override_rps: Option<u32>,
    ) -> bool {
        self.limiter
            .check_and_consume(src_ip, now_ns, cost_packets, rate_override_rps)
    }

    #[inline]
    fn compute_syn_cookie(&self, flow: &FiveTuple, client_seq: u32, now_ns: u64) -> u32 {
        let minute_bucket = now_ns / 60_000_000_000;
        let h = hash_five_tuple(flow)
            ^ self.shared.syn_cookie_secret
            ^ ((client_seq as u64) << 17)
            ^ minute_bucket;
        (h ^ (h >> 32)) as u32
    }

    #[inline(always)]
    fn record_ingress_rtt_if_matched(&mut self, flow: &FiveTuple, now_ns: u64) {
        if !self.has_egress_rtt_stamps {
            return;
        }
        let rev = flow.reverse();
        let fh = hash_five_tuple(&rev);
        let slot = &mut self.tx_tstamp_ring[(fh as usize) & RTT_RING_MASK];
        if slot.flow_hash == fh && now_ns >= slot.tx_ts_ns {
            let rtt_ns = now_ns - slot.tx_ts_ns;
            slot.flow_hash = 0;
            self.shared.telemetry.record_tcp_rtt_ns(self.cpu_id, rtt_ns);
        }
    }

    #[inline(always)]
    fn record_egress_tx_timestamp(&mut self, flow: &FiveTuple, now_ns: u64) {
        self.has_egress_rtt_stamps = true;
        let fh = hash_five_tuple(flow);
        let slot = &mut self.tx_tstamp_ring[(fh as usize) & RTT_RING_MASK];
        slot.flow_hash = fh;
        slot.tx_ts_ns = now_ns;
    }

    #[inline(always)]
    fn bump_reason_counter(&mut self, action: XdpAction, reason: DropReason) {
        self.shared
            .telemetry
            .record_verdict(self.cpu_id, action, reason);
    }
}

/// Tier-1 Unoptimized Baseline Pipeline (Mutex + Linear CIDR Scan + Per-Packet Vec Copy)
/// retained exclusively to measure honest 3-stage micro-optimization speedups in `benches/xdp_pps_bench.rs`.
pub struct BaselineMutexPipeline {
    rules: Mutex<Vec<(u32, u8, BlockRuleVal)>>,
    buckets: Mutex<HashMap<[u8; 4], (u64, u64)>>,
    cfg: ParserConfig,
}

impl BaselineMutexPipeline {
    pub fn new(cidrs: &[(&str, BlockRuleVal)]) -> Self {
        let mut list = Vec::new();
        for (spec, rule) in cidrs {
            if let Ok((IpAddr::V4(v4), p)) = parse_cidr_spec(spec) {
                list.push((u32::from_be_bytes(v4.octets()), p, *rule));
            }
        }
        Self {
            rules: Mutex::new(list),
            buckets: Mutex::new(HashMap::new()),
            cfg: ParserConfig::default(),
        }
    }

    pub fn process_baseline(&self, frame: &[u8], now_ns: u64) -> XdpAction {
        // Unoptimized heap copy + mutex lock + O(N) linear scan
        let owned = frame.to_vec();
        let Ok(parsed) = parse_frame_zero_copy(&owned, &self.cfg) else {
            return XdpAction::Drop;
        };
        if let IpAddrWire::V4(s4) = parsed.flow.src_ip {
            let addr_u32 = u32::from_be_bytes(s4);
            let rules = self.rules.lock().unwrap_or_else(|e| e.into_inner());
            let mut best_p: Option<(u8, BlockRuleVal)> = None;
            for &(net, p, r) in rules.iter() {
                let mask = if p == 0 { 0 } else { !0_u32 << (32 - p) };
                if (addr_u32 & mask) == net
                    && r.is_active(now_ns)
                    && best_p.is_none_or(|(bp, _)| p >= bp)
                {
                    best_p = Some((p, r));
                }
            }
            if matches!(best_p, Some((_, rule)) if rule.action == RuleAction::HardDrop) {
                return XdpAction::Drop;
            }
            if parsed.is_tcp_syn_only() {
                let mut bmap = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
                let entry = bmap.entry(s4).or_insert((50_000_000, now_ns));
                if entry.0 < 5_000_000 {
                    return XdpAction::Drop;
                }
                entry.0 -= 5_000_000;
            }
        }
        XdpAction::Pass
    }
}
