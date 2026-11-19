//! Lock-Free Per-CPU HDR / Log2-Linear Kernel Socket Latency Histograms & Exporters.
//!
//! Tracks:
//!   1. TCP Round-Trip Time (`tcp_rtt_ns`) across 512 Log2-Linear HDR bins (`<= 6.25%` error)
//!   2. Socket Accept/Receive Backlog Queue Depth (`socket_backlog_depth`) & overflow drops
//!   3. TCP Retransmission Spikes (`retransmit_spike_ratio_ppm`, integer parts-per-million)
//!   4. Per-Action (`XDP_PASS`, `XDP_DROP`, `XDP_TX`) and Per-Reason packet counters
//!
//! Zero floating-point arithmetic (`#![deny(clippy::float_arithmetic)]`).

use netguard_common::{
    hdr_bin_bounds_ns, hdr_bin_index, hdr_bin_midpoint_ns, DropReason, XdpAction,
    DROP_REASON_COUNT, HDR_TOTAL_BINS,
};
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cache-line isolated (`64B`) atomic HDR histogram shard for a single CPU / worker core.
#[repr(align(64))]
pub struct AtomicHdrShard {
    bins: [AtomicU64; HDR_TOTAL_BINS],
    count: AtomicU64,
    sum: AtomicU64,
    min_val: AtomicU64,
    max_val: AtomicU64,
}

impl Default for AtomicHdrShard {
    fn default() -> Self {
        Self::new()
    }
}

impl AtomicHdrShard {
    pub fn new() -> Self {
        Self {
            bins: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min_val: AtomicU64::new(u64::MAX),
            max_val: AtomicU64::new(0),
        }
    }

    #[inline(always)]
    pub fn record(&self, value: u64) {
        let bin = hdr_bin_index(value);
        self.bins[bin].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.min_val.fetch_min(value, Ordering::Relaxed);
        self.max_val.fetch_max(value, Ordering::Relaxed);
    }
}

/// Cache-line isolated (`64B`) per-CPU telemetry slot eliminating cross-core false sharing.
#[repr(align(64))]
pub struct PerCpuTelemetrySlot {
    pub tcp_rtt_ns: AtomicHdrShard,
    pub socket_backlog: AtomicHdrShard,
    pub backlog_overflow_drops: AtomicU64,
    pub tcp_segments_total: AtomicU64,
    pub tcp_retransmits_total: AtomicU64,
    pub xdp_actions: [AtomicU64; 5],
    pub drop_reasons: [AtomicU64; DROP_REASON_COUNT],
}

impl Default for PerCpuTelemetrySlot {
    fn default() -> Self {
        Self::new()
    }
}

impl PerCpuTelemetrySlot {
    pub fn new() -> Self {
        Self {
            tcp_rtt_ns: AtomicHdrShard::new(),
            socket_backlog: AtomicHdrShard::new(),
            backlog_overflow_drops: AtomicU64::new(0),
            tcp_segments_total: AtomicU64::new(0),
            tcp_retransmits_total: AtomicU64::new(0),
            xdp_actions: std::array::from_fn(|_| AtomicU64::new(0)),
            drop_reasons: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct QuantileSummary {
    pub sample_count: u64,
    pub min: u64,
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
    pub sum: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryReport {
    pub interface_name: String,
    pub snapshot_ts_ns: u64,
    pub rcu_generation: u64,
    pub xdp_pass_packets: u64,
    pub xdp_drop_packets: u64,
    pub xdp_tx_packets: u64,
    pub tcp_rtt_ns: QuantileSummary,
    pub socket_backlog_depth: QuantileSummary,
    pub socket_backlog_overflow_drops: u64,
    pub tcp_segments_total: u64,
    pub tcp_retransmits_total: u64,
    pub retransmit_spike_ratio_ppm: u32,
    pub active_ipv4_cidr_rules: u32,
    pub active_ipv6_cidr_rules: u32,
    pub active_frozen_accounts: u32,
    pub active_conntrack_sessions: u64,
    pub drop_reasons: Vec<(String, u64)>,
}

/// Multi-core lock-free kernel socket & XDP telemetry registry.
pub struct NetGuardTelemetryRegistry {
    pub interface_name: String,
    slots: Vec<PerCpuTelemetrySlot>,
}

impl NetGuardTelemetryRegistry {
    pub fn new(interface_name: impl Into<String>, num_cpus: usize) -> Self {
        let cpus = num_cpus.max(1);
        let mut slots = Vec::with_capacity(cpus);
        for _ in 0..cpus {
            slots.push(PerCpuTelemetrySlot::new());
        }
        Self {
            interface_name: interface_name.into(),
            slots,
        }
    }

    #[inline(always)]
    pub fn slot(&self, cpu_id: usize) -> &PerCpuTelemetrySlot {
        &self.slots[cpu_id % self.slots.len()]
    }

    #[inline]
    pub fn record_tcp_rtt_ns(&self, cpu_id: usize, rtt_ns: u64) {
        self.slot(cpu_id).tcp_rtt_ns.record(rtt_ns);
    }

    #[inline]
    pub fn record_socket_backlog(&self, cpu_id: usize, depth: u64, overflow_dropped: bool) {
        let s = self.slot(cpu_id);
        s.socket_backlog.record(depth);
        if overflow_dropped {
            s.backlog_overflow_drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn record_tcp_segment(&self, cpu_id: usize, is_retransmit: bool) {
        let s = self.slot(cpu_id);
        s.tcp_segments_total.fetch_add(1, Ordering::Relaxed);
        if is_retransmit {
            s.tcp_retransmits_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn record_verdict(&self, cpu_id: usize, action: XdpAction, reason: DropReason) {
        let s = self.slot(cpu_id);
        let a_idx = (action as usize).min(4);
        s.xdp_actions[a_idx].fetch_add(1, Ordering::Relaxed);
        s.drop_reasons[reason as usize].fetch_add(1, Ordering::Relaxed);
    }

    fn merge_hdr_shards<'a>(
        shards: impl Iterator<Item = &'a AtomicHdrShard>,
    ) -> (QuantileSummary, [u64; HDR_TOTAL_BINS]) {
        let mut merged = [0_u64; HDR_TOTAL_BINS];
        let mut total_count = 0_u64;
        let mut total_sum = 0_u64;
        let mut min_v = u64::MAX;
        let mut max_v = 0_u64;

        for s in shards {
            let c = s.count.load(Ordering::Relaxed);
            if c == 0 {
                continue;
            }
            total_count = total_count.saturating_add(c);
            total_sum = total_sum.saturating_add(s.sum.load(Ordering::Relaxed));
            min_v = min_v.min(s.min_val.load(Ordering::Relaxed));
            max_v = max_v.max(s.max_val.load(Ordering::Relaxed));
            for (i, b) in s.bins.iter().enumerate() {
                merged[i] = merged[i].saturating_add(b.load(Ordering::Relaxed));
            }
        }

        if total_count == 0 {
            return (QuantileSummary::default(), merged);
        }

        // Pure integer quantile rank lookup (`rank = ceil(count * permille / 1000)`)
        let quantile_at_permille = |permille: u64| -> u64 {
            let target = (total_count as u128 * permille as u128).div_ceil(1000);
            let target_u64 = (target as u64).max(1);
            let mut cum = 0_u64;
            for (bin, &cnt) in merged.iter().enumerate() {
                cum = cum.saturating_add(cnt);
                if cum >= target_u64 {
                    return hdr_bin_midpoint_ns(bin).clamp(min_v, max_v);
                }
            }
            max_v
        };

        let summary = QuantileSummary {
            sample_count: total_count,
            min: if min_v == u64::MAX { 0 } else { min_v },
            p50: quantile_at_permille(500),
            p90: quantile_at_permille(900),
            p95: quantile_at_permille(950),
            p99: quantile_at_permille(990),
            p999: quantile_at_permille(999),
            max: max_v,
            sum: total_sum,
        };
        (summary, merged)
    }

    pub fn snapshot(
        &self,
        now_ns: u64,
        rcu_generation: u64,
        active_ipv4_cidr_rules: u32,
        active_ipv6_cidr_rules: u32,
        active_frozen_accounts: u32,
        active_conntrack_sessions: u64,
    ) -> TelemetryReport {
        let (rtt_q, _) = Self::merge_hdr_shards(self.slots.iter().map(|s| &s.tcp_rtt_ns));
        let (backlog_q, _) = Self::merge_hdr_shards(self.slots.iter().map(|s| &s.socket_backlog));

        let mut actions = [0_u64; 5];
        let mut reasons = [0_u64; DROP_REASON_COUNT];
        let mut backlog_drops = 0_u64;
        let mut segs = 0_u64;
        let mut retrans = 0_u64;

        for s in &self.slots {
            backlog_drops =
                backlog_drops.saturating_add(s.backlog_overflow_drops.load(Ordering::Relaxed));
            segs = segs.saturating_add(s.tcp_segments_total.load(Ordering::Relaxed));
            retrans = retrans.saturating_add(s.tcp_retransmits_total.load(Ordering::Relaxed));
            for (i, a) in actions.iter_mut().enumerate() {
                *a = a.saturating_add(s.xdp_actions[i].load(Ordering::Relaxed));
            }
            for (r, slot_cnt) in reasons.iter_mut().enumerate() {
                *slot_cnt = slot_cnt.saturating_add(s.drop_reasons[r].load(Ordering::Relaxed));
            }
        }

        let ratio_ppm = if segs == 0 {
            0_u32
        } else {
            (((retrans as u128) * 1_000_000_u128) / (segs as u128)).min(1_000_000) as u32
        };

        let mut drop_reasons = Vec::new();
        for (code, &cnt) in reasons.iter().enumerate() {
            if cnt > 0 {
                drop_reasons.push((
                    DropReason::from_u8(code as u8).as_label().to_string(),
                    cnt,
                ));
            }
        }

        TelemetryReport {
            interface_name: self.interface_name.clone(),
            snapshot_ts_ns: now_ns,
            rcu_generation,
            xdp_pass_packets: actions[XdpAction::Pass as usize],
            xdp_drop_packets: actions[XdpAction::Drop as usize],
            xdp_tx_packets: actions[XdpAction::Tx as usize],
            tcp_rtt_ns: rtt_q,
            socket_backlog_depth: backlog_q,
            socket_backlog_overflow_drops: backlog_drops,
            tcp_segments_total: segs,
            tcp_retransmits_total: retrans,
            retransmit_spike_ratio_ppm: ratio_ppm,
            active_ipv4_cidr_rules,
            active_ipv6_cidr_rules,
            active_frozen_accounts,
            active_conntrack_sessions,
            drop_reasons,
        }
    }

    /// Exports OpenMetrics / Prometheus exposition text (`GET /metrics` on port `8095`)
    /// scraped live by Prometheus and `aegis-agent` (`analyze_system_telemetry`).
    pub fn render_prometheus(&self, report: &TelemetryReport) -> String {
        let (_, rtt_bins) = Self::merge_hdr_shards(self.slots.iter().map(|s| &s.tcp_rtt_ns));
        let iface = &report.interface_name;
        let mut out = String::with_capacity(4096);

        let _ = writeln!(out, "# HELP hyperion_netguard_xdp_packets_total Total packets processed by XDP action");
        let _ = writeln!(out, "# TYPE hyperion_netguard_xdp_packets_total counter");
        let _ = writeln!(out, "hyperion_netguard_xdp_packets_total{{iface=\"{iface}\",action=\"XDP_PASS\"}} {}", report.xdp_pass_packets);
        let _ = writeln!(out, "hyperion_netguard_xdp_packets_total{{iface=\"{iface}\",action=\"XDP_DROP\"}} {}", report.xdp_drop_packets);
        let _ = writeln!(out, "hyperion_netguard_xdp_packets_total{{iface=\"{iface}\",action=\"XDP_TX\"}} {}", report.xdp_tx_packets);

        let _ = writeln!(out, "# HELP hyperion_netguard_drop_reason_packets_total Packet counts broken down by XDP classification reason");
        let _ = writeln!(out, "# TYPE hyperion_netguard_drop_reason_packets_total counter");
        for (reason, cnt) in &report.drop_reasons {
            let _ = writeln!(out, "hyperion_netguard_drop_reason_packets_total{{iface=\"{iface}\",reason=\"{reason}\"}} {cnt}");
        }

        // Standard cumulative Prometheus histogram buckets in nanoseconds (`le`)
        let prom_bounds_ns: [u64; 10] = [
            1_000,       // 1 us
            5_000,       // 5 us
            10_000,      // 10 us
            50_000,      // 50 us
            100_000,     // 100 us
            500_000,     // 500 us
            1_000_000,   // 1 ms
            5_000_000,   // 5 ms
            25_000_000,  // 25 ms
            100_000_000, // 100 ms
        ];
        let _ = writeln!(out, "# HELP hyperion_netguard_tcp_rtt_ns Kernel TCP round-trip latency distribution in nanoseconds");
        let _ = writeln!(out, "# TYPE hyperion_netguard_tcp_rtt_ns histogram");
        for &le_ns in &prom_bounds_ns {
            let mut cum = 0_u64;
            for (bin, &cnt) in rtt_bins.iter().enumerate() {
                if cnt > 0 && hdr_bin_bounds_ns(bin).1 <= le_ns {
                    cum = cum.saturating_add(cnt);
                }
            }
            let _ = writeln!(out, "hyperion_netguard_tcp_rtt_ns_bucket{{iface=\"{iface}\",le=\"{le_ns}\"}} {cum}");
        }
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_ns_bucket{{iface=\"{iface}\",le=\"+Inf\"}} {}", report.tcp_rtt_ns.sample_count);
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_ns_sum{{iface=\"{iface}\"}} {}", report.tcp_rtt_ns.sum);
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_ns_count{{iface=\"{iface}\"}} {}", report.tcp_rtt_ns.sample_count);

        let _ = writeln!(out, "# HELP hyperion_netguard_tcp_rtt_quantile_ns Live HDR quantile estimates in nanoseconds");
        let _ = writeln!(out, "# TYPE hyperion_netguard_tcp_rtt_quantile_ns gauge");
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_quantile_ns{{iface=\"{iface}\",quantile=\"0.50\"}} {}", report.tcp_rtt_ns.p50);
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_quantile_ns{{iface=\"{iface}\",quantile=\"0.95\"}} {}", report.tcp_rtt_ns.p95);
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_quantile_ns{{iface=\"{iface}\",quantile=\"0.99\"}} {}", report.tcp_rtt_ns.p99);
        let _ = writeln!(out, "hyperion_netguard_tcp_rtt_quantile_ns{{iface=\"{iface}\",quantile=\"0.999\"}} {}", report.tcp_rtt_ns.p999);

        let _ = writeln!(out, "# HELP hyperion_netguard_socket_backlog_p99_depth Kernel socket accept/receive backlog p99 depth");
        let _ = writeln!(out, "# TYPE hyperion_netguard_socket_backlog_p99_depth gauge");
        let _ = writeln!(out, "hyperion_netguard_socket_backlog_p99_depth{{iface=\"{iface}\"}} {}", report.socket_backlog_depth.p99);

        let _ = writeln!(out, "# HELP hyperion_netguard_socket_backlog_drops_total Kernel socket backlog overflow packet drops");
        let _ = writeln!(out, "# TYPE hyperion_netguard_socket_backlog_drops_total counter");
        let _ = writeln!(out, "hyperion_netguard_socket_backlog_drops_total{{iface=\"{iface}\"}} {}", report.socket_backlog_overflow_drops);

        let _ = writeln!(out, "# HELP hyperion_netguard_tcp_retransmits_total Kernel TCP retransmitted segments");
        let _ = writeln!(out, "# TYPE hyperion_netguard_tcp_retransmits_total counter");
        let _ = writeln!(out, "hyperion_netguard_tcp_retransmits_total{{iface=\"{iface}\"}} {}", report.tcp_retransmits_total);

        let _ = writeln!(out, "# HELP hyperion_netguard_retransmit_spike_ratio_ppm TCP retransmission spike ratio in parts-per-million");
        let _ = writeln!(out, "# TYPE hyperion_netguard_retransmit_spike_ratio_ppm gauge");
        let _ = writeln!(out, "hyperion_netguard_retransmit_spike_ratio_ppm{{iface=\"{iface}\"}} {}", report.retransmit_spike_ratio_ppm);

        let _ = writeln!(out, "# HELP hyperion_netguard_rcu_generation Monotonic atomic RCU BPF policy generation");
        let _ = writeln!(out, "# TYPE hyperion_netguard_rcu_generation gauge");
        let _ = writeln!(out, "hyperion_netguard_rcu_generation{{iface=\"{iface}\"}} {}", report.rcu_generation);

        out
    }
}
