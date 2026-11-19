//! Wing & Gong / Lowe / Porcupine Bitset-Memoized Concurrent History Linearizability Checker.
//!
//! Proves that every concurrent execution history of client writes (`CreditDeposit`, `Transfer`)
//! and reads (`ReadIndex` / `LeaderLease` balance queries) — including crashed/timed-out
//! **indeterminate** operations (`return_ts_ns == None`) — is linearizable with respect to a
//! sequential atomic fixed-point ledger specification, OR produces an explicit violation witness!

use crate::types::FP_SCALE_I128;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientOpSpec {
    CreditDeposit {
        idempotency_key: String,
        account_id: u64,
        asset: String,
        amount_fp: u128,
    },
    Transfer {
        idempotency_key: String,
        from_account: u64,
        to_account: u64,
        asset: String,
        amount_fp: u128,
    },
    ReadBalance {
        account_id: u64,
        asset: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientOpReturn {
    WriteAck { applied: bool },
    BalanceVal { total_fp: i128 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrentHistoryOp {
    pub op_id: usize,
    pub client_id: u64,
    pub invoke_ts_ns: u64,
    /// `Some(ts)` if the client received a definitive response;
    /// `None` if the Leader crashed or timed out mid-flight (outcome indeterminate: may take effect once or never!).
    pub return_ts_ns: Option<u64>,
    pub spec: ClientOpSpec,
    pub observed: Option<ClientOpReturn>,
}

impl ConcurrentHistoryOp {
    #[inline]
    pub fn is_completed(&self) -> bool {
        self.return_ts_ns.is_some() && self.observed.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinearizabilityVerdict {
    Linearizable {
        /// Exact total sequential order of `op_id`s witnessing linearizability.
        witness_order: Vec<usize>,
        explored_states: usize,
    },
    Violation {
        reason: String,
        deepest_prefix_witness: Vec<usize>,
        explored_states: usize,
    },
}

/// Abstract sequential reference model for the WGL Linearizability Checker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SequentialLedgerModel {
    balances: BTreeMap<(u64, String), i128>,
    executed_keys: BTreeMap<String, bool>,
}

impl SequentialLedgerModel {
    fn state_digest(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        for ((acct, asset), &bal) in &self.balances {
            if bal != 0 {
                h.update(&acct.to_le_bytes());
                h.update(asset.as_bytes());
                h.update(&bal.to_le_bytes());
            }
        }
        for (k, &ok) in &self.executed_keys {
            h.update(k.as_bytes());
            h.update(&[u8::from(ok)]);
        }
        *h.finalize().as_bytes()
    }

    /// Attempt to step `op` against the sequential model.
    /// Returns `Some(next_model)` iff stepping `op` produces an output matching `op.observed`
    /// (or if `op` is indeterminate `observed == None`, applies its state transition cleanly).
    fn try_step(&self, op: &ConcurrentHistoryOp) -> Option<Self> {
        let mut next = self.clone();
        match &op.spec {
            ClientOpSpec::CreditDeposit {
                idempotency_key,
                account_id,
                asset,
                amount_fp,
            } => {
                let applied = if let Some(&prev_res) = next.executed_keys.get(idempotency_key) {
                    prev_res
                } else if *amount_fp == 0 {
                    next.executed_keys.insert(idempotency_key.clone(), false);
                    false
                } else {
                    let delta = i128::try_from(*amount_fp).ok()?;
                    let entry = next
                        .balances
                        .entry((*account_id, asset.clone()))
                        .or_insert(0);
                    *entry = entry.checked_add(delta)?;
                    next.executed_keys.insert(idempotency_key.clone(), true);
                    true
                };

                if let Some(ref obs) = op.observed {
                    if obs != &(ClientOpReturn::WriteAck { applied }) {
                        return None;
                    }
                }
                Some(next)
            }
            ClientOpSpec::Transfer {
                idempotency_key,
                from_account,
                to_account,
                asset,
                amount_fp,
            } => {
                let applied = if let Some(&prev_res) = next.executed_keys.get(idempotency_key) {
                    prev_res
                } else if *amount_fp == 0 {
                    next.executed_keys.insert(idempotency_key.clone(), false);
                    false
                } else if from_account == to_account {
                    next.executed_keys.insert(idempotency_key.clone(), true);
                    true
                } else {
                    let delta = i128::try_from(*amount_fp).ok()?;
                    let cur_from = next
                        .balances
                        .get(&(*from_account, asset.clone()))
                        .copied()
                        .unwrap_or(0);
                    if cur_from < delta {
                        next.executed_keys.insert(idempotency_key.clone(), false);
                        false
                    } else {
                        let cur_to = next
                            .balances
                            .get(&(*to_account, asset.clone()))
                            .copied()
                            .unwrap_or(0);
                        let new_to = cur_to.checked_add(delta)?;
                        let new_from = cur_from.checked_sub(delta)?;
                        next.balances
                            .insert((*from_account, asset.clone()), new_from);
                        next.balances.insert((*to_account, asset.clone()), new_to);
                        next.executed_keys.insert(idempotency_key.clone(), true);
                        true
                    }
                };

                if let Some(ref obs) = op.observed {
                    if obs != &(ClientOpReturn::WriteAck { applied }) {
                        return None;
                    }
                }
                Some(next)
            }
            ClientOpSpec::ReadBalance { account_id, asset } => {
                let bal = next
                    .balances
                    .get(&(*account_id, asset.clone()))
                    .copied()
                    .unwrap_or(0);
                let _ = FP_SCALE_I128;
                if let Some(ref obs) = op.observed {
                    if obs != &(ClientOpReturn::BalanceVal { total_fp: bal }) {
                        return None;
                    }
                }
                Some(next)
            }
        }
    }
}

/// Multi-word bitset supporting arbitrary concurrent history lengths (`N > 64`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct OpBitSet {
    words: Vec<u64>,
}

impl OpBitSet {
    fn new(len: usize) -> Self {
        Self {
            words: vec![0u64; len.div_ceil(64).max(1)],
        }
    }

    #[inline]
    fn contains(&self, idx: usize) -> bool {
        (self.words[idx / 64] & (1u64 << (idx % 64))) != 0
    }

    #[inline]
    fn insert(&mut self, idx: usize) {
        self.words[idx / 64] |= 1u64 << (idx % 64);
    }
}

pub struct LinearizabilityChecker {
    pub max_states_budget: usize,
}

impl Default for LinearizabilityChecker {
    fn default() -> Self {
        Self {
            max_states_budget: 500_000,
        }
    }
}

impl LinearizabilityChecker {
    pub fn verify(&self, history: &[ConcurrentHistoryOp]) -> LinearizabilityVerdict {
        if history.is_empty() {
            return LinearizabilityVerdict::Linearizable {
                witness_order: Vec::new(),
                explored_states: 0,
            };
        }

        // Partition history into connected account components (Porcupine independent-subhistory decomposition!):
        // Since Deposit(acct) and ReadBalance(acct) only touch `acct`, and Transfer(a, b) links `a` and `b`,
        // connected components of accounts are 100% independent under Herlihy & Wing's locality theorem!
        let components = partition_connected_components(history);
        let mut combined_witness = Vec::with_capacity(history.len());
        let mut total_explored = 0usize;

        for comp_indices in components {
            let sub_ops: Vec<ConcurrentHistoryOp> =
                comp_indices.iter().map(|&i| history[i].clone()).collect();
            match self.verify_component(&sub_ops) {
                LinearizabilityVerdict::Linearizable {
                    witness_order,
                    explored_states,
                } => {
                    total_explored += explored_states;
                    combined_witness.extend(witness_order);
                }
                LinearizabilityVerdict::Violation {
                    reason,
                    deepest_prefix_witness,
                    explored_states,
                } => {
                    return LinearizabilityVerdict::Violation {
                        reason,
                        deepest_prefix_witness,
                        explored_states: total_explored + explored_states,
                    };
                }
            }
        }

        LinearizabilityVerdict::Linearizable {
            witness_order: combined_witness,
            explored_states: total_explored,
        }
    }

    fn verify_component(&self, ops: &[ConcurrentHistoryOp]) -> LinearizabilityVerdict {
        let mut sorted: Vec<ConcurrentHistoryOp> = ops.to_vec();
        sorted.sort_by(|a, b| {
            a.invoke_ts_ns
                .cmp(&b.invoke_ts_ns)
                .then_with(|| a.return_ts_ns.unwrap_or(u64::MAX).cmp(&b.return_ts_ns.unwrap_or(u64::MAX)))
                .then_with(|| a.op_id.cmp(&b.op_id))
        });

        let n = sorted.len();
        let completed_indices: Vec<usize> = (0..n).filter(|&i| sorted[i].is_completed()).collect();
        if completed_indices.is_empty() {
            return LinearizabilityVerdict::Linearizable {
                witness_order: Vec::new(),
                explored_states: 1,
            };
        }

        let mut ctx = WglSearchCtx {
            ops: &sorted,
            completed_indices: &completed_indices,
            visited: HashSet::new(),
            explored: 0,
            max_budget: self.max_states_budget,
            deepest_witness: Vec::new(),
        };

        let init_scheduled = OpBitSet::new(n);
        let init_model = SequentialLedgerModel::default();
        let mut current_witness = Vec::with_capacity(n);

        if ctx.dfs(init_scheduled, init_model, &mut current_witness) {
            LinearizabilityVerdict::Linearizable {
                witness_order: current_witness,
                explored_states: ctx.explored,
            }
        } else {
            let reason = if ctx.explored >= ctx.max_budget {
                format!(
                    "WGL state budget ({}) exhausted without finding valid linearization",
                    ctx.max_budget
                )
            } else {
                format!(
                    "NON-LINEARIZABLE HISTORY DETECTED after exploring {} states (deepest legal prefix length = {} of {} completed ops)",
                    ctx.explored,
                    ctx.deepest_witness.len(),
                    completed_indices.len()
                )
            };
            LinearizabilityVerdict::Violation {
                reason,
                deepest_prefix_witness: ctx.deepest_witness,
                explored_states: ctx.explored,
            }
        }
    }
}

struct WglSearchCtx<'a> {
    ops: &'a [ConcurrentHistoryOp],
    completed_indices: &'a [usize],
    visited: HashSet<(OpBitSet, [u8; 32])>,
    explored: usize,
    max_budget: usize,
    deepest_witness: Vec<usize>,
}

impl<'a> WglSearchCtx<'a> {
    fn dfs(
        &mut self,
        scheduled: OpBitSet,
        model: SequentialLedgerModel,
        witness: &mut Vec<usize>,
    ) -> bool {
        if self.explored >= self.max_budget {
            return false;
        }

        // Check if all completed ops have been scheduled!
        // Remaining unscheduled indeterminate (`return_ts_ns == None`) ops can legally never take effect
        // or take effect after all observed operations.
        let mut min_unscheduled_return_ns: Option<u64> = None;
        for &ci in self.completed_indices {
            if !scheduled.contains(ci) {
                let ret = self.ops[ci].return_ts_ns.expect("completed has return_ts");
                min_unscheduled_return_ns =
                    Some(min_unscheduled_return_ns.map_or(ret, |m| m.min(ret)));
            }
        }

        let Some(min_ret_ns) = min_unscheduled_return_ns else {
            return true;
        };

        if witness.len() > self.deepest_witness.len() {
            self.deepest_witness = witness.clone();
        }

        let memo_key = (scheduled.clone(), model.state_digest());
        if !self.visited.insert(memo_key) {
            return false;
        }
        self.explored += 1;

        // Every unscheduled op `i` whose `invoke_ts_ns <= min_ret_ns` is minimally enabled in the real-time partial order!
        // Prioritize completed writes -> completed reads -> indeterminate writes for rapid convergence.
        let mut candidates: Vec<usize> = (0..self.ops.len())
            .filter(|&i| !scheduled.contains(i) && self.ops[i].invoke_ts_ns <= min_ret_ns)
            .collect();

        candidates.sort_by_key(|&i| {
            let op = &self.ops[i];
            let prio = match (&op.return_ts_ns, &op.spec) {
                (Some(_), ClientOpSpec::CreditDeposit { .. } | ClientOpSpec::Transfer { .. }) => 0u8,
                (Some(_), ClientOpSpec::ReadBalance { .. }) => 1u8,
                (None, _) => 2u8,
            };
            (prio, op.return_ts_ns.unwrap_or(u64::MAX), op.invoke_ts_ns)
        });

        for cand_idx in candidates {
            let cand_op = &self.ops[cand_idx];
            if let Some(next_model) = model.try_step(cand_op) {
                let mut next_sched = scheduled.clone();
                next_sched.insert(cand_idx);
                witness.push(cand_op.op_id);
                if self.dfs(next_sched, next_model, witness) {
                    return true;
                }
                witness.pop();
            }
        }

        false
    }
}

/// Union-Find decomposition of operations by touched `(account_id, asset)` keys
/// (Herlihy & Wing 1990 Locality Theorem: a history is linearizable iff each independent object component is linearizable).
fn partition_connected_components(history: &[ConcurrentHistoryOp]) -> Vec<Vec<usize>> {
    let n = history.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[rb] = ra;
        }
    }

    let mut key_owner: BTreeMap<(u64, String), usize> = BTreeMap::new();
    let mut idem_owner: BTreeMap<String, usize> = BTreeMap::new();

    for (idx, op) in history.iter().enumerate() {
        let mut keys = Vec::new();
        match &op.spec {
            ClientOpSpec::CreditDeposit {
                idempotency_key,
                account_id,
                asset,
                ..
            } => {
                keys.push((*account_id, asset.clone()));
                if let Some(&prev) = idem_owner.get(idempotency_key) {
                    union(&mut parent, idx, prev);
                } else {
                    idem_owner.insert(idempotency_key.clone(), idx);
                }
            }
            ClientOpSpec::Transfer {
                idempotency_key,
                from_account,
                to_account,
                asset,
                ..
            } => {
                keys.push((*from_account, asset.clone()));
                keys.push((*to_account, asset.clone()));
                if let Some(&prev) = idem_owner.get(idempotency_key) {
                    union(&mut parent, idx, prev);
                } else {
                    idem_owner.insert(idempotency_key.clone(), idx);
                }
            }
            ClientOpSpec::ReadBalance { account_id, asset } => {
                keys.push((*account_id, asset.clone()));
            }
        }
        for k in keys {
            if let Some(&prev_idx) = key_owner.get(&k) {
                union(&mut parent, idx, prev_idx);
            } else {
                key_owner.insert(k, idx);
            }
        }
    }

    let mut groups: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().insert(i);
    }
    groups
        .into_values()
        .map(|s| s.into_iter().collect())
        .collect()
}
