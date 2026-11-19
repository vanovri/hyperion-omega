//! Deterministic `SimulatedNetworkSwitch` controlled by a single 64-bit PRNG seed.
//! Supports directed asymmetric partitions (`A -> B`, `B -> C`, `A -/-> C`), packet loss,
//! extreme bimodal latency spikes, packet duplication, and out-of-order shuffling.

use crate::prng::DeterministicPrng;
use crate::traits::NetworkTransport;
use crate::types::{NetworkEnvelope, NodeId};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};

#[derive(Clone, Copy, Debug)]
pub struct DirectedLinkPolicy {
    pub connected: bool,
    pub min_latency_ns: u64,
    pub jitter_ns: u64,
    pub tail_spike_ppm: u32,
    pub max_spike_ns: u64,
    pub drop_ppm: u32,
    pub duplicate_ppm: u32,
}

impl Default for DirectedLinkPolicy {
    fn default() -> Self {
        Self {
            connected: true,
            min_latency_ns: 1_500_000,   // 1.5 ms base LAN RTT/2
            jitter_ns: 2_500_000,        // +0..2.5 ms jitter (causes natural out-of-order!)
            tail_spike_ppm: 30_000,      // 3% tail latency spikes
            max_spike_ns: 80_000_000,    // up to 80 ms spike
            drop_ppm: 0,
            duplicate_ppm: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScheduledPacket {
    pub deliver_at_ns: u64,
    pub deterministic_seq: u64,
    pub envelope: NetworkEnvelope,
}

impl PartialEq for ScheduledPacket {
    fn eq(&self, other: &Self) -> bool {
        self.deliver_at_ns == other.deliver_at_ns
            && self.deterministic_seq == other.deterministic_seq
    }
}
impl Eq for ScheduledPacket {}

impl PartialOrd for ScheduledPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        self.deliver_at_ns
            .cmp(&other.deliver_at_ns)
            .then_with(|| self.deterministic_seq.cmp(&other.deterministic_seq))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkSwitchStats {
    pub total_sent: u64,
    pub dropped_partition: u64,
    pub dropped_loss: u64,
    pub duplicated: u64,
    pub delivered: u64,
}

pub struct SimulatedNetworkSwitch {
    prng: DeterministicPrng,
    default_policy: DirectedLinkPolicy,
    links: BTreeMap<(NodeId, NodeId), DirectedLinkPolicy>,
    in_flight: BinaryHeap<Reverse<ScheduledPacket>>,
    next_seq: u64,
    pub stats: NetworkSwitchStats,
}

impl SimulatedNetworkSwitch {
    pub fn new(seed: u64) -> Self {
        Self {
            prng: DeterministicPrng::new(seed ^ 0x517C_4E70_0001_u64),
            default_policy: DirectedLinkPolicy::default(),
            links: BTreeMap::new(),
            in_flight: BinaryHeap::new(),
            next_seq: 1,
            stats: NetworkSwitchStats::default(),
        }
    }

    pub fn set_default_policy(&mut self, policy: DirectedLinkPolicy) {
        self.default_policy = policy;
    }

    /// Configure a single directed link `from -> to` (enables asymmetric network partitions!).
    pub fn set_directed_link(&mut self, from: NodeId, to: NodeId, policy: DirectedLinkPolicy) {
        self.links.insert((from, to), policy);
    }

    pub fn set_directed_connectivity(&mut self, from: NodeId, to: NodeId, connected: bool) {
        let mut pol = self.policy_for(from, to);
        pol.connected = connected;
        self.links.insert((from, to), pol);
    }

    /// Restore full symmetric connectivity across `nodes` using `default_policy`.
    pub fn heal_all(&mut self, nodes: &[NodeId]) {
        self.links.clear();
        for &a in nodes {
            for &b in nodes {
                if a != b {
                    self.links.insert((a, b), self.default_policy);
                }
            }
        }
    }

    /// Inject a classic non-transitive Asymmetric Bridge Partition:
    /// `bridge` can talk bidirectionally with `group_a` and `group_b`,
    /// OR `group_a -> bridge -> group_b` one-way while `group_a` and `group_b` are completely cut off!
    pub fn apply_asymmetric_triangle_partition(
        &mut self,
        node_a: NodeId,
        node_b: NodeId,
        node_c: NodeId,
        all_nodes: &[NodeId],
    ) {
        self.heal_all(all_nodes);
        // Asymmetric fault: A -> B allowed, B -> C allowed, but A <-> C blocked AND B -> A blocked!
        self.set_directed_connectivity(node_a, node_c, false);
        self.set_directed_connectivity(node_c, node_a, false);
        self.set_directed_connectivity(node_b, node_a, false);
    }

    /// Symmetric majority/minority split.
    pub fn apply_symmetric_partition(&mut self, side_a: &[NodeId], side_b: &[NodeId]) {
        for &a in side_a {
            for &b in side_b {
                self.set_directed_connectivity(a, b, false);
                self.set_directed_connectivity(b, a, false);
            }
        }
    }

    pub fn policy_for(&self, from: NodeId, to: NodeId) -> DirectedLinkPolicy {
        self.links
            .get(&(from, to))
            .copied()
            .unwrap_or(self.default_policy)
    }

    pub fn peek_next_delivery_ns(&self) -> Option<u64> {
        self.in_flight.peek().map(|Reverse(pkt)| pkt.deliver_at_ns)
    }

    pub fn pop_due_packet(&mut self, up_to_ns: u64) -> Option<ScheduledPacket> {
        let is_due = self
            .in_flight
            .peek()
            .map(|Reverse(pkt)| pkt.deliver_at_ns <= up_to_ns)
            .unwrap_or(false);
        if is_due {
            let Reverse(pkt) = self.in_flight.pop().expect("peeked non-empty");
            self.stats.delivered += 1;
            Some(pkt)
        } else {
            None
        }
    }

    pub fn drop_all_inflight_for_node(&mut self, crashed_node: NodeId) {
        let drained: Vec<Reverse<ScheduledPacket>> = self.in_flight.drain().collect();
        for Reverse(pkt) in drained {
            if pkt.envelope.to != crashed_node {
                self.in_flight.push(Reverse(pkt));
            }
        }
    }
}

impl NetworkTransport for SimulatedNetworkSwitch {
    fn send(&mut self, now_ns: u64, envelope: NetworkEnvelope) {
        self.stats.total_sent += 1;
        let pol = self.policy_for(envelope.from, envelope.to);
        if !pol.connected {
            self.stats.dropped_partition += 1;
            return;
        }
        if self.prng.chance_ppm(pol.drop_ppm) {
            self.stats.dropped_loss += 1;
            return;
        }

        let delay = self.prng.bimodal_tail_latency_ns(
            pol.min_latency_ns,
            pol.jitter_ns,
            pol.tail_spike_ppm,
            pol.max_spike_ns,
        );
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);

        let should_dup = self.prng.chance_ppm(pol.duplicate_ppm);
        if should_dup {
            self.stats.duplicated += 1;
            let dup_delay = delay.saturating_add(self.prng.range_u64(100_000, 9_000_000));
            let dup_seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.in_flight.push(Reverse(ScheduledPacket {
                deliver_at_ns: now_ns.saturating_add(dup_delay),
                deterministic_seq: dup_seq,
                envelope: envelope.clone(),
            }));
        }

        self.in_flight.push(Reverse(ScheduledPacket {
            deliver_at_ns: now_ns.saturating_add(delay),
            deterministic_seq: seq,
            envelope,
        }));
    }
}
