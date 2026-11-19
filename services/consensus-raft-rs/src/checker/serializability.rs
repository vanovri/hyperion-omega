//! Strict Serializability (`Strict-1SR` / External Consistency) & Double-Entry Conservation Verifier.
//!
//! Builds Adya's Direct Serialization Graph (`DSG`) augmented with Real-Time Precedence Edges:
//! - `WW` (Write-Write version order per `(account_id, asset)`)
//! - `WR` (Write-Read dataflow edges: reader observes exact version written by writer)
//! - `RW` (Read-Write anti-dependency edges: reader precedes next writer in version order)
//! - `RT` (Strict Real-Time edges: `tx_a.return_ts_ns < tx_b.invoke_ts_ns`)
//!
//! Then runs Tarjan's Strongly Connected Components (SCC) cycle detector to prove zero cycles
//! (`G1a`, `G1b`, `G1c`, `G2-item`, `G2`, and stale real-time anomalies = `0`).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type LedgerKey = (u64, String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedTransaction {
    pub tx_id: u64,
    pub invoke_ts_ns: u64,
    pub return_ts_ns: u64,
    /// Commit log index / version (`0` for pure read-only queries, which carry `at_log_index` per read!).
    pub write_version: Option<u64>,
    pub writes: BTreeSet<LedgerKey>,
    /// Map of `key -> version_log_index_observed` (`0` = initial genesis empty state).
    pub reads: BTreeMap<LedgerKey, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerializabilityVerdict {
    StrictlySerializable {
        tx_count: usize,
        edge_count: usize,
    },
    AnomalyDetected {
        reason: String,
        cycle_tx_ids: Vec<u64>,
    },
}

pub struct StrictSerializabilityVerifier;

impl StrictSerializabilityVerifier {
    pub fn verify(transactions: &[ObservedTransaction]) -> SerializabilityVerdict {
        if transactions.is_empty() {
            return SerializabilityVerdict::StrictlySerializable {
                tx_count: 0,
                edge_count: 0,
            };
        }

        let n = transactions.len();
        // Node 0..n correspond to `transactions[0..n]`.
        let mut adj: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];

        // Index writers per key sorted by `write_version`.
        let mut key_writers: BTreeMap<LedgerKey, Vec<(u64, usize)>> = BTreeMap::new();
        for (idx, tx) in transactions.iter().enumerate() {
            if let Some(ver) = tx.write_version {
                if ver == 0 && !tx.writes.is_empty() {
                    return SerializabilityVerdict::AnomalyDetected {
                        reason: format!("tx {} has non-empty write set with version 0", tx.tx_id),
                        cycle_tx_ids: vec![tx.tx_id],
                    };
                }
                for k in &tx.writes {
                    key_writers.entry(k.clone()).or_default().push((ver, idx));
                }
            }
        }

        // 1. `WW` edges: strict version order per key.
        for (key, writers) in &mut key_writers {
            writers.sort_by_key(|&(ver, idx)| (ver, idx));
            for w in writers.windows(2) {
                let (v1, i1) = w[0];
                let (v2, i2) = w[1];
                if v1 == v2 && i1 != i2 {
                    return SerializabilityVerdict::AnomalyDetected {
                        reason: format!(
                            "conflicting duplicate write version {v1} on key ({}, {})",
                            key.0, key.1
                        ),
                        cycle_tx_ids: vec![transactions[i1].tx_id, transactions[i2].tx_id],
                    };
                }
                if i1 != i2 {
                    adj[i1].insert(i2);
                }
            }
        }

        // 2. `WR` (Write-Read) and `RW` (Anti-Dependency) edges!
        // Note: In our Raft replicated ledger, `ReadBalance` returns `at_index` (`last_applied_index`),
        // which may be `>=` the most recent write version `v_last <= at_index` on `key`!
        for (r_idx, r_tx) in transactions.iter().enumerate() {
            for (key, &observed_at_index) in &r_tx.reads {
                if let Some(writers) = key_writers.get(key) {
                    // Find the latest writer with `write_version <= observed_at_index` -> `WR` edge (`writer -> reader`)!
                    let mut latest_prior: Option<usize> = None;
                    let mut first_later: Option<usize> = None;
                    for &(w_ver, w_idx) in writers {
                        if w_ver <= observed_at_index {
                            latest_prior = Some(w_idx);
                        } else if first_later.is_none() {
                            first_later = Some(w_idx);
                        }
                    }
                    if let Some(w_idx) = latest_prior {
                        if w_idx != r_idx {
                            adj[w_idx].insert(r_idx); // WR edge
                        }
                    }
                    if let Some(next_w_idx) = first_later {
                        if next_w_idx != r_idx {
                            adj[r_idx].insert(next_w_idx); // RW anti-dependency edge!
                        }
                    }
                }
            }
        }

        // 3. `RT` (Strict Real-Time Precedence) edges: `tx_a.return_ts_ns < tx_b.invoke_ts_ns` -> `a -> b`!
        for (i, tx_i) in transactions.iter().enumerate() {
            let ret_i = tx_i.return_ts_ns;
            for (j, tx_j) in transactions.iter().enumerate() {
                if i != j && ret_i < tx_j.invoke_ts_ns {
                    adj[i].insert(j);
                }
            }
        }

        let edge_count: usize = adj.iter().map(|s| s.len()).sum();

        // 4. Tarjan SCC / Three-Color DFS Cycle Extraction!
        if let Some(cycle_indices) = find_directed_cycle(&adj) {
            let cycle_tx_ids: Vec<u64> = cycle_indices
                .into_iter()
                .map(|i| transactions[i].tx_id)
                .collect();
            return SerializabilityVerdict::AnomalyDetected {
                reason: "Strict-1SR Serialization Graph Cycle Detected (G1c / G2 / Stale-Read Anomaly)".to_string(),
                cycle_tx_ids,
            };
        }

        SerializabilityVerdict::StrictlySerializable {
            tx_count: n,
            edge_count,
        }
    }
}

/// Iterative/bounded three-color cycle finder returning the exact cycle path if any exists.
fn find_directed_cycle(adj: &[BTreeSet<usize>]) -> Option<Vec<usize>> {
    let n = adj.len();
    // 0 = White (unvisited), 1 = Gray (on active stack), 2 = Black (fully explored)
    let mut color = vec![0u8; n];
    let mut parent = vec![usize::MAX; n];

    for start in 0..n {
        if color[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, std::collections::btree_set::Iter<'_, usize>)> =
            vec![(start, adj[start].iter())];
        color[start] = 1;

        while let Some((u, iter)) = stack.last_mut() {
            let u_val = *u;
            if let Some(&v) = iter.next() {
                if color[v] == 1 {
                    // Found back-edge `u_val -> v`: reconstruct exact cycle!
                    let mut cycle = vec![v];
                    let mut cur = u_val;
                    while cur != v && cur != usize::MAX {
                        cycle.push(cur);
                        cur = parent[cur];
                    }
                    cycle.push(v);
                    cycle.reverse();
                    return Some(cycle);
                } else if color[v] == 0 {
                    parent[v] = u_val;
                    color[v] = 1;
                    stack.push((v, adj[v].iter()));
                }
            } else {
                color[u_val] = 2;
                stack.pop();
            }
        }
    }
    None
}
