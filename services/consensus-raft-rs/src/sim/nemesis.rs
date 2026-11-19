//! Nemesis Adversarial Fault Injector for HYPERION CH12 VOPR Simulation.
//! Aggressively injects:
//! - Asymmetric non-transitive network partitions (`A -> B`, `B -> C`, `A -/-> C`)
//! - Symmetric majority/minority splits & isolated node spinning (tests Pre-Vote!)
//! - Extreme tail latency spikes, packet duplication & out-of-order shuffling
//! - Disk `fsync` stalls on active Leaders
//! - Torn writes mid-block during sudden power loss
//! - Reactive surgical Leader crash-restarts **just BEFORE commit** and **just AFTER commit**
//! - Silent disk bit-rot & dynamic Joint Consensus membership transitions

use crate::prng::DeterministicPrng;
use crate::sim::network::{DirectedLinkPolicy, SimulatedNetworkSwitch};
use crate::types::NodeId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NemesisFaultEvent {
    AsymmetricTrianglePartition {
        a: NodeId,
        b: NodeId,
        c: NodeId,
    },
    SymmetricSplit {
        minority: Vec<NodeId>,
        majority: Vec<NodeId>,
    },
    IsolateSingleNode {
        isolated: NodeId,
    },
    ExtremePacketStorm {
        drop_ppm: u32,
        duplicate_ppm: u32,
        max_spike_ns: u64,
    },
    HealNetwork,
    LeaderFsyncStall {
        node_id: NodeId,
        stall_ns: u64,
    },
    CrashNodeTornWrite {
        node_id: NodeId,
        reason: String,
    },
    RestartNode {
        node_id: NodeId,
    },
    InjectDiskBitRot {
        node_id: NodeId,
    },
}

#[derive(Clone, Debug)]
pub struct NemesisProfile {
    pub fault_interval_ns: u64,
    pub enable_asymmetric_partitions: bool,
    pub enable_leader_crash_before_commit: bool,
    pub enable_leader_crash_after_commit: bool,
    pub enable_fsync_stalls: bool,
    pub enable_torn_writes: bool,
    pub enable_bit_rot: bool,
    /// Maximum concurrent crashed/quarantined nodes so cluster majority (`>= 3 of 5`) can recover and prove liveness.
    pub max_simultaneous_down_nodes: usize,
}

impl Default for NemesisProfile {
    fn default() -> Self {
        Self {
            fault_interval_ns: 180_000_000, // every 180 ms
            enable_asymmetric_partitions: true,
            enable_leader_crash_before_commit: true,
            enable_leader_crash_after_commit: true,
            enable_fsync_stalls: true,
            enable_torn_writes: true,
            enable_bit_rot: true,
            max_simultaneous_down_nodes: 2,
        }
    }
}

pub struct NemesisEngine {
    pub profile: NemesisProfile,
    prng: DeterministicPrng,
    next_fault_ns: u64,
    pub arm_crash_leader_before_commit: bool,
    pub arm_crash_leader_after_commit: bool,
    pub injected_log: Vec<(u64, NemesisFaultEvent)>,
}

impl NemesisEngine {
    pub fn new(seed: u64, profile: NemesisProfile, start_ns: u64) -> Self {
        let mut prng = DeterministicPrng::new(seed ^ 0x4E45_4D45_5349_5301_u64);
        let first = start_ns + prng.range_u64(80_000_000, profile.fault_interval_ns.max(90_000_000));
        Self {
            arm_crash_leader_before_commit: profile.enable_leader_crash_before_commit,
            arm_crash_leader_after_commit: profile.enable_leader_crash_after_commit,
            profile,
            prng,
            next_fault_ns: first,
            injected_log: Vec::new(),
        }
    }

    pub fn should_fire_scheduled(&self, now_ns: u64) -> bool {
        now_ns >= self.next_fault_ns
    }

    pub fn choose_scheduled_fault(
        &mut self,
        now_ns: u64,
        all_nodes: &[NodeId],
        current_leader: Option<NodeId>,
        crashed_or_quarantined_count: usize,
        net: &mut SimulatedNetworkSwitch,
    ) -> Option<NemesisFaultEvent> {
        let gap = self
            .prng
            .range_u64(self.profile.fault_interval_ns / 2, self.profile.fault_interval_ns * 3 / 2);
        self.next_fault_ns = now_ns.saturating_add(gap.max(40_000_000));

        if all_nodes.len() < 3 {
            return None;
        }

        let roll = self.prng.range_u64(0, 99);
        let ev = if roll < 22 && self.profile.enable_asymmetric_partitions {
            let mut shuffled = all_nodes.to_vec();
            self.prng.shuffle(&mut shuffled);
            let (a, b, c) = (shuffled[0], shuffled[1], shuffled[2]);
            net.apply_asymmetric_triangle_partition(a, b, c, all_nodes);
            NemesisFaultEvent::AsymmetricTrianglePartition { a, b, c }
        } else if roll < 38 {
            let mut shuffled = all_nodes.to_vec();
            self.prng.shuffle(&mut shuffled);
            let split_at = (shuffled.len() / 2).max(1);
            let minority = shuffled[..split_at].to_vec();
            let majority = shuffled[split_at..].to_vec();
            net.heal_all(all_nodes);
            net.apply_symmetric_partition(&minority, &majority);
            NemesisFaultEvent::SymmetricSplit { minority, majority }
        } else if roll < 50 {
            let idx = self.prng.range_usize(all_nodes.len());
            let isolated = all_nodes[idx];
            let others: Vec<NodeId> = all_nodes
                .iter()
                .copied()
                .filter(|&n| n != isolated)
                .collect();
            net.heal_all(all_nodes);
            net.apply_symmetric_partition(&[isolated], &others);
            NemesisFaultEvent::IsolateSingleNode { isolated }
        } else if roll < 66 {
            let drop_ppm = self.prng.range_u64(30_000, 150_000) as u32;
            let duplicate_ppm = self.prng.range_u64(80_000, 250_000) as u32;
            let max_spike_ns = self.prng.range_u64(60_000_000, 220_000_000);
            let pol = DirectedLinkPolicy {
                connected: true,
                min_latency_ns: 2_000_000,
                jitter_ns: 18_000_000, // heavy out-of-order shuffling!
                tail_spike_ppm: 180_000,
                max_spike_ns,
                drop_ppm,
                duplicate_ppm,
            };
            for &a in all_nodes {
                for &b in all_nodes {
                    if a != b {
                        net.set_directed_link(a, b, pol);
                    }
                }
            }
            NemesisFaultEvent::ExtremePacketStorm {
                drop_ppm,
                duplicate_ppm,
                max_spike_ns,
            }
        } else if roll < 78 && self.profile.enable_fsync_stalls {
            let target = current_leader.unwrap_or_else(|| {
                all_nodes[self.prng.range_usize(all_nodes.len())]
            });
            let stall_ns = self.prng.range_u64(180_000_000, 420_000_000);
            NemesisFaultEvent::LeaderFsyncStall {
                node_id: target,
                stall_ns,
            }
        } else if roll < 86
            && crashed_or_quarantined_count < self.profile.max_simultaneous_down_nodes
        {
            // Re-arm surgical leader crash hooks too!
            if self.profile.enable_leader_crash_before_commit && self.prng.chance_ppm(500_000) {
                self.arm_crash_leader_before_commit = true;
            }
            if self.profile.enable_leader_crash_after_commit && self.prng.chance_ppm(500_000) {
                self.arm_crash_leader_after_commit = true;
            }
            let target = current_leader.unwrap_or_else(|| {
                all_nodes[self.prng.range_usize(all_nodes.len())]
            });
            NemesisFaultEvent::CrashNodeTornWrite {
                node_id: target,
                reason: "scheduled sudden power-loss + torn write".to_string(),
            }
        } else {
            net.set_default_policy(DirectedLinkPolicy::default());
            net.heal_all(all_nodes);
            NemesisFaultEvent::HealNetwork
        };

        self.injected_log.push((now_ns, ev.clone()));
        Some(ev)
    }

    pub fn record_reactive_fault(&mut self, now_ns: u64, ev: NemesisFaultEvent) {
        self.injected_log.push((now_ns, ev));
    }
}
