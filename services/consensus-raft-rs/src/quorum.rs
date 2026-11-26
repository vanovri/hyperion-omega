//! Cache-line aligned (`#[repr(align(64))]`) zero-allocation Quorum & Joint-Consensus Commit Calculator.
//! Eliminates heap `Vec` allocation and sorting overhead on the hot `AppendEntriesResponse` path.

use crate::types::{ClusterConfig, LogIndex, NodeId, MAX_CLUSTER_VOTERS};
use std::collections::{BTreeMap, BTreeSet};

#[repr(align(64))]
#[derive(Clone, Debug)]
pub struct AlignedQuorumTracker {
    voter_ids: [NodeId; MAX_CLUSTER_VOTERS],
    match_indices: [LogIndex; MAX_CLUSTER_VOTERS],
    len: usize,
    is_joint: bool,
}

impl Default for AlignedQuorumTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AlignedQuorumTracker {
    #[inline]
    pub const fn new() -> Self {
        Self {
            voter_ids: [0; MAX_CLUSTER_VOTERS],
            match_indices: [0; MAX_CLUSTER_VOTERS],
            len: 0,
            is_joint: false,
        }
    }

    pub fn rebuild_from_config(
        &mut self,
        config: &ClusterConfig,
        match_map: &BTreeMap<NodeId, LogIndex>,
    ) {
        let all = config.all_voters();
        self.len = all.len().min(MAX_CLUSTER_VOTERS);
        self.is_joint = config.is_joint();
        for (slot, id) in all.into_iter().take(MAX_CLUSTER_VOTERS).enumerate() {
            self.voter_ids[slot] = id;
            self.match_indices[slot] = match_map.get(&id).copied().unwrap_or(0);
        }
    }

    #[inline]
    pub fn update_match_index(&mut self, node_id: NodeId, match_idx: LogIndex) {
        for i in 0..self.len {
            if self.voter_ids[i] == node_id {
                if match_idx > self.match_indices[i] {
                    self.match_indices[i] = match_idx;
                }
                return;
            }
        }
    }

    /// Computes the highest `LogIndex` replicated on a strict quorum of `config`
    /// using stack-only insertion sort (`0` heap allocations!).
    #[inline]
    pub fn committed_quorum_index(&self, config: &ClusterConfig) -> LogIndex {
        if !self.is_joint {
            return self.uniform_fast_quorum_index();
        }
        match config {
            ClusterConfig::Uniform { voters, .. } => self.subset_quorum_index(voters),
            ClusterConfig::Joint {
                old_voters,
                new_voters,
                ..
            } => {
                let q_old = self.subset_quorum_index(old_voters);
                let q_new = self.subset_quorum_index(new_voters);
                q_old.min(q_new)
            }
        }
    }

    #[inline]
    fn uniform_fast_quorum_index(&self) -> LogIndex {
        let n = self.len;
        if n == 0 || n > MAX_CLUSTER_VOTERS {
            return 0;
        }
        let mut buf = [0u64; MAX_CLUSTER_VOTERS];
        for (i, &m) in self.match_indices.iter().enumerate().take(n) {
            let mut pos = i;
            while pos > 0 && buf[pos - 1] < m {
                buf[pos] = buf[pos - 1];
                pos -= 1;
            }
            buf[pos] = m;
        }
        buf[n / 2]
    }

    #[inline]
    fn subset_quorum_index(&self, subset: &BTreeSet<NodeId>) -> LogIndex {
        let n = subset.len();
        if n == 0 || n > MAX_CLUSTER_VOTERS {
            return 0;
        }
        let mut buf = [0u64; MAX_CLUSTER_VOTERS];

        for (count, &voter) in subset.iter().enumerate() {
            let mut m = 0u64;
            for i in 0..self.len {
                if self.voter_ids[i] == voter {
                    m = self.match_indices[i];
                    break;
                }
            }
            // Insert into `buf[0..count]` sorted descending via stack insertion sort (N <= 16 -> ~8ns!).
            let mut pos = count;
            while pos > 0 && buf[pos - 1] < m {
                buf[pos] = buf[pos - 1];
                pos -= 1;
            }
            buf[pos] = m;
        }

        // Strict majority index in descending array of length `n` is `(n / 2)`.
        // Example: n = 5 -> needed = 3 -> 0-based index `5 / 2 = 2` (3rd highest match_index!).
        // Example: n = 4 -> needed = 3 -> 0-based index `4 / 2 = 2` (3rd highest match_index!).
        let majority_pos = n / 2;
        buf[majority_pos]
    }
}
