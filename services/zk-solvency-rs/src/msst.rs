use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use ark_bn254::Fr;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{SolvencyError, SolvencyResult};
use crate::poseidon_params::{
    baseline_poseidon_hash, blake3_leaf_digest, blake3_node_digest, derive_leaf_salt,
    fr_from_bytes_le, fr_to_bytes_le, poseidon_leaf_hash, poseidon_node_hash,
    poseidon_root_commitment, poseidon_user_commitment, DOMAIN_MSST_LEAF, DOMAIN_MSST_NODE,
};

/// Maximum supported sparse tree depth (`2^24 = 16,777,216` accounts).
pub const MAX_MSST_DEPTH: usize = 24;

/// Default canonical depth covering up to `65,536` accounts (`2^16`).
pub const DEFAULT_MSST_DEPTH: usize = 16;

/// L1/L2 cache-resident Rayon tile size (`2^8 = 256` leaves -> 8 bottom levels built per worker
/// with zero cross-core false sharing or barrier overhead).
const SUBTREE_TILE_LOG2: usize = 8;

/// Input ledger account snapshot entry (fixed-point integer scale `1e8`, strictly non-negative `u64`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLeafInput {
    pub account_id: u64,
    pub balance_fp: u64,
}

/// Cache-line aligned (`64` bytes + `16` bytes = `128` bytes = exactly 2 CPU cache lines)
/// hybrid Merkle-Sum Sparse Tree node committing `(PoseidonHash, Blake3Digest, BalanceSum)`.
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsstNode {
    pub poseidon_hash: Fr,
    pub blake3_digest: [u8; 32],
    pub sum_fp: u128,
}

impl Default for MsstNode {
    fn default() -> Self {
        Self {
            poseidon_hash: Fr::from(0u64),
            blake3_digest: [0u8; 32],
            sum_fp: 0,
        }
    }
}

/// Precomputed canonical empty-leaf and empty-subtree table for depths `0..=MAX_MSST_DEPTH`.
/// Allows sparse trees (e.g., 1,024 active accounts inside a `2^16` or `2^20` address space)
/// to evaluate unpopulated subtrees in `O(1)` time.
pub struct SparseZeroTable {
    pub nodes: [MsstNode; MAX_MSST_DEPTH + 1],
}

static SPARSE_ZERO_TABLE: OnceLock<SparseZeroTable> = OnceLock::new();

pub fn sparse_zero_table() -> &'static SparseZeroTable {
    SPARSE_ZERO_TABLE.get_or_init(|| {
        let mut nodes = [MsstNode::default(); MAX_MSST_DEPTH + 1];
        let zero_salt = Fr::from(0u64);
        let leaf_p = poseidon_leaf_hash(0, 0, zero_salt);
        let leaf_b3 = blake3_leaf_digest(leaf_p, 0, 0);
        nodes[0] = MsstNode {
            poseidon_hash: leaf_p,
            blake3_digest: leaf_b3,
            sum_fp: 0,
        };

        for d in 1..=MAX_MSST_DEPTH {
            let prev = nodes[d - 1];
            let p = poseidon_node_hash(
                prev.poseidon_hash,
                Fr::from(0u64),
                prev.poseidon_hash,
                Fr::from(0u64),
            );
            let b3 = blake3_node_digest(p, &prev.blake3_digest, 0, &prev.blake3_digest, 0);
            nodes[d] = MsstNode {
                poseidon_hash: p,
                blake3_digest: b3,
                sum_fp: 0,
            };
        }
        SparseZeroTable { nodes }
    })
}

/// Single step in an account's Merkle-Sum Inclusion Proof from leaf (level `0`) to root (level `depth - 1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsstPathStep {
    /// Sibling's algebraic Poseidon hash (`32` bytes canonical little-endian `Fr`).
    pub sibling_poseidon_bytes: [u8; 32],
    /// Sibling's symmetric Blake3 hybrid digest (`32` bytes).
    pub sibling_blake3: [u8; 32],
    /// Sibling's committed non-negative balance sum in `1e8` atomic units.
    pub sibling_sum_fp: u128,
    /// `true` if the proven node at this level is the right child (so sibling is on the left).
    pub is_right_child: bool,
}

impl MsstPathStep {
    pub fn sibling_poseidon_fr(&self) -> SolvencyResult<Fr> {
        fr_from_bytes_le(&self.sibling_poseidon_bytes)
    }
}

/// Portable, self-contained Merkle-Sum Inclusion Proof verifiable in `< 1 ms` on Browser Wasm or EVM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsstInclusionProof {
    pub epoch_id: u64,
    pub tree_depth: usize,
    pub leaf_index: usize,
    pub account_id: u64,
    pub balance_fp: u64,
    pub leaf_salt_bytes: [u8; 32],
    pub leaf_poseidon_bytes: [u8; 32],
    pub leaf_blake3: [u8; 32],
    pub user_blinding_bytes: [u8; 32],
    pub user_commitment_bytes: [u8; 32],
    pub root_poseidon_bytes: [u8; 32],
    pub root_blake3: [u8; 32],
    pub root_commitment_bytes: [u8; 32],
    pub path: Vec<MsstPathStep>,
}

/// Cryptographic Merkle-Sum Sparse Tree (MSST) storing all populated & sparse levels in a
/// contiguous flat array (`levels[0]` = leaves of length `2^depth`, `levels[depth]` = `[root]`).
#[derive(Clone, Debug)]
pub struct MsstTree {
    pub epoch_id: u64,
    pub depth: usize,
    pub active_accounts: usize,
    pub epoch_blinding: Fr,
    pub root_commitment: Fr,
    /// Contiguous flat arena containing all `(2^(depth + 1) - 1)` nodes laid out level-by-level.
    arena: Vec<MsstNode>,
    /// Start offset of each level `0..=depth` inside `arena`.
    level_offsets: Vec<usize>,
    /// Canonical sorted leaf inputs (`account_id`, `balance_fp`).
    sorted_accounts: Vec<AccountLeafInput>,
    /// Deterministic per-leaf blinding salts (`sorted_accounts.len()`).
    leaf_salts: Vec<Fr>,
    /// Fast `O(1)` lookup from `account_id -> leaf_index`.
    account_index: HashMap<u64, usize>,
}

impl MsstTree {
    /// Computes required tree depth `>= 1` for `count` accounts.
    pub fn required_depth(count: usize) -> usize {
        if count <= 2 {
            1
        } else {
            count.next_power_of_two().trailing_zeros() as usize
        }
    }

    fn prepare_sorted_accounts(
        accounts: &[AccountLeafInput],
        depth: usize,
    ) -> SolvencyResult<Vec<AccountLeafInput>> {
        if accounts.is_empty() {
            return Err(SolvencyError::EmptyAccountSet);
        }
        if depth == 0 || depth > MAX_MSST_DEPTH {
            return Err(SolvencyError::InvalidTreeDepth {
                depth,
                max_depth: MAX_MSST_DEPTH,
            });
        }
        let capacity = 1usize << depth;
        if accounts.len() > capacity {
            return Err(SolvencyError::TreeCapacityExceeded {
                count: accounts.len(),
                depth,
                capacity,
            });
        }

        let mut sorted = accounts.to_vec();
        sorted.sort_unstable_by_key(|a| a.account_id);
        for pair in sorted.windows(2) {
            if pair[0].account_id == pair[1].account_id {
                return Err(SolvencyError::DuplicateAccountId {
                    account_id: pair[0].account_id,
                });
            }
        }
        Ok(sorted)
    }

    fn compute_level_offsets(depth: usize) -> (usize, Vec<usize>) {
        let mut offsets = Vec::with_capacity(depth + 1);
        let mut cur = 0usize;
        for d in 0..=depth {
            offsets.push(cur);
            cur += 1usize << (depth - d);
        }
        (cur, offsets)
    }

    /// **Optimization Round 1 (Baseline)**:
    /// Sequential per-level `Vec` construction using generic `PoseidonSponge::new` (retained for
    /// head-to-head flamegraph/allocation benchmarking and bit-for-bit differential verification).
    pub fn build_baseline_sequential(
        accounts: &[AccountLeafInput],
        depth: usize,
        epoch_id: u64,
        epoch_master_seed: &[u8; 32],
    ) -> SolvencyResult<Self> {
        let sorted = Self::prepare_sorted_accounts(accounts, depth)?;
        let capacity = 1usize << depth;
        let zero_tbl = sparse_zero_table();

        let mut leaf_salts = Vec::with_capacity(sorted.len());
        let mut account_index = HashMap::with_capacity(sorted.len());
        let mut cur_level = Vec::with_capacity(capacity);

        for (idx, acc) in sorted.iter().enumerate() {
            account_index.insert(acc.account_id, idx);
            let salt = derive_leaf_salt(epoch_master_seed, acc.account_id, epoch_id);
            leaf_salts.push(salt);
            let p_hash = baseline_poseidon_hash(&[
                Fr::from(DOMAIN_MSST_LEAF),
                Fr::from(acc.account_id),
                Fr::from(acc.balance_fp),
                salt,
            ]);
            let b3 = blake3_leaf_digest(p_hash, acc.account_id, acc.balance_fp);
            cur_level.push(MsstNode {
                poseidon_hash: p_hash,
                blake3_digest: b3,
                sum_fp: u128::from(acc.balance_fp),
            });
        }
        cur_level.resize(capacity, zero_tbl.nodes[0]);

        let (total_nodes, level_offsets) = Self::compute_level_offsets(depth);
        let mut arena = Vec::with_capacity(total_nodes);
        arena.extend_from_slice(&cur_level);

        for level in 0..depth {
            let mut next_level = Vec::with_capacity(cur_level.len() / 2);
            for pair in cur_level.as_chunks::<2>().0 {
                let left = pair[0];
                let right = pair[1];
                let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                    SolvencyError::LiabilitySumOverflow {
                        level,
                        left: left.sum_fp,
                        right: right.sum_fp,
                    },
                )?;
                let p_hash = baseline_poseidon_hash(&[
                    Fr::from(DOMAIN_MSST_NODE),
                    left.poseidon_hash,
                    Fr::from(left.sum_fp),
                    right.poseidon_hash,
                    Fr::from(right.sum_fp),
                ]);
                let b3 = blake3_node_digest(
                    p_hash,
                    &left.blake3_digest,
                    left.sum_fp,
                    &right.blake3_digest,
                    right.sum_fp,
                );
                next_level.push(MsstNode {
                    poseidon_hash: p_hash,
                    blake3_digest: b3,
                    sum_fp,
                });
            }
            arena.extend_from_slice(&next_level);
            cur_level = next_level;
        }

        let epoch_blinding = derive_leaf_salt(epoch_master_seed, u64::MAX, epoch_id);
        let root = arena[level_offsets[depth]];
        let root_commitment =
            poseidon_root_commitment(root.poseidon_hash, root.sum_fp, epoch_id, epoch_blinding);

        Ok(Self {
            epoch_id,
            depth,
            active_accounts: sorted.len(),
            epoch_blinding,
            root_commitment,
            arena,
            level_offsets,
            sorted_accounts: sorted,
            leaf_salts,
            account_index,
        })
    }

    /// **Optimization Round 2 (Zero-Allocation Flat Arena & Unrolled Register Poseidon + Sparse Skip)**:
    /// Allocates a single flat node arena upfront, eliminates all ~145 per-hash sponge `Vec`
    /// allocations via `FastPoseidon6`, and skips unpopulated sparse subtrees in `O(1)`.
    pub fn build_zero_alloc_arena(
        accounts: &[AccountLeafInput],
        depth: usize,
        epoch_id: u64,
        epoch_master_seed: &[u8; 32],
    ) -> SolvencyResult<Self> {
        let sorted = Self::prepare_sorted_accounts(accounts, depth)?;
        let active = sorted.len();
        let zero_tbl = sparse_zero_table();
        let (total_nodes, level_offsets) = Self::compute_level_offsets(depth);

        let mut arena = vec![MsstNode::default(); total_nodes];
        let mut leaf_salts = Vec::with_capacity(active);
        let mut account_index = HashMap::with_capacity(active);

        // Populate active leaves; initialize remaining sparse leaves from precomputed zero table.
        for (idx, acc) in sorted.iter().enumerate() {
            account_index.insert(acc.account_id, idx);
            let salt = derive_leaf_salt(epoch_master_seed, acc.account_id, epoch_id);
            leaf_salts.push(salt);
            let p_hash = poseidon_leaf_hash(acc.account_id, acc.balance_fp, salt);
            let b3 = blake3_leaf_digest(p_hash, acc.account_id, acc.balance_fp);
            arena[idx] = MsstNode {
                poseidon_hash: p_hash,
                blake3_digest: b3,
                sum_fp: u128::from(acc.balance_fp),
            };
        }
        let cap = 1usize << depth;
        if active < cap {
            arena[active..cap].fill(zero_tbl.nodes[0]);
        }

        // Bottom-up flat arena pass with sparse subtree short-circuiting.
        let mut active_span = active;
        for level in 0..depth {
            let child_off = level_offsets[level];
            let parent_off = level_offsets[level + 1];
            let parent_count = 1usize << (depth - 1 - level);
            let populated_parents = active_span.div_ceil(2);

            for p_idx in 0..populated_parents {
                let left = arena[child_off + (p_idx << 1)];
                let right = arena[child_off + (p_idx << 1) + 1];
                let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                    SolvencyError::LiabilitySumOverflow {
                        level,
                        left: left.sum_fp,
                        right: right.sum_fp,
                    },
                )?;
                let p_hash = poseidon_node_hash(
                    left.poseidon_hash,
                    Fr::from(left.sum_fp),
                    right.poseidon_hash,
                    Fr::from(right.sum_fp),
                );
                let b3 = blake3_node_digest(
                    p_hash,
                    &left.blake3_digest,
                    left.sum_fp,
                    &right.blake3_digest,
                    right.sum_fp,
                );
                arena[parent_off + p_idx] = MsstNode {
                    poseidon_hash: p_hash,
                    blake3_digest: b3,
                    sum_fp,
                };
            }
            if populated_parents < parent_count {
                arena[(parent_off + populated_parents)..(parent_off + parent_count)]
                    .fill(zero_tbl.nodes[level + 1]);
            }
            active_span = populated_parents;
        }

        let epoch_blinding = derive_leaf_salt(epoch_master_seed, u64::MAX, epoch_id);
        let root = arena[level_offsets[depth]];
        let root_commitment =
            poseidon_root_commitment(root.poseidon_hash, root.sum_fp, epoch_id, epoch_blinding);

        Ok(Self {
            epoch_id,
            depth,
            active_accounts: active,
            epoch_blinding,
            root_commitment,
            arena,
            level_offsets,
            sorted_accounts: sorted,
            leaf_salts,
            account_index,
        })
    }

    /// **Optimization Round 3 (Production Default — Cache-Line Aligned `#[repr(C, align(64))]` +
    /// Rayon Parallel L1-Cache Subtree Tiling + Sparse Short-Circuiting)**:
    /// Summarizes tens of thousands of accounts into a single Root Commitment in milliseconds.
    pub fn build_parallel(
        accounts: &[AccountLeafInput],
        depth: usize,
        epoch_id: u64,
        epoch_master_seed: &[u8; 32],
    ) -> SolvencyResult<Self> {
        let sorted = Self::prepare_sorted_accounts(accounts, depth)?;
        let active = sorted.len();
        let zero_tbl = sparse_zero_table();
        let (total_nodes, level_offsets) = Self::compute_level_offsets(depth);

        let mut account_index = HashMap::with_capacity(active);
        for (idx, acc) in sorted.iter().enumerate() {
            account_index.insert(acc.account_id, idx);
        }

        let mut arena = vec![MsstNode::default(); total_nodes];
        let cap = 1usize << depth;

        // 1. Parallel leaf hashing + deterministic blinding salt derivation across Rayon workers.
        let mut leaf_salts = vec![Fr::from(0u64); active];
        arena[..active]
            .par_iter_mut()
            .zip(leaf_salts.par_iter_mut())
            .enumerate()
            .for_each(|(idx, (node_slot, salt_slot))| {
                let acc = sorted[idx];
                let salt = derive_leaf_salt(epoch_master_seed, acc.account_id, epoch_id);
                let p_hash = poseidon_leaf_hash(acc.account_id, acc.balance_fp, salt);
                let b3 = blake3_leaf_digest(p_hash, acc.account_id, acc.balance_fp);
                *salt_slot = salt;
                *node_slot = MsstNode {
                    poseidon_hash: p_hash,
                    blake3_digest: b3,
                    sum_fp: u128::from(acc.balance_fp),
                };
            });

        if active < cap {
            arena[active..cap].fill(zero_tbl.nodes[0]);
        }

        // 2. Parallel L1-tiled bottom-up reduction across levels.
        let tile_levels = SUBTREE_TILE_LOG2.min(depth);
        let tile_leaves = 1usize << tile_levels;
        let populated_tiles = active.div_ceil(tile_leaves);

        if tile_levels > 0 && populated_tiles >= 2 {
            // Build each 256-leaf tile locally inside a Rayon worker's L1 cache (`511` nodes)
            // with zero cross-thread synchronization across the bottom `tile_levels` levels!
            #[derive(Clone)]
            struct TileResult {
                levels: Vec<Vec<MsstNode>>,
            }

            let tile_results: Result<Vec<TileResult>, SolvencyError> = (0..populated_tiles)
                .into_par_iter()
                .map(|tile_id| {
                    let leaf_start = tile_id * tile_leaves;
                    let mut cur = arena[leaf_start..(leaf_start + tile_leaves)].to_vec();
                    let mut built_levels = Vec::with_capacity(tile_levels);

                    for lv in 0..tile_levels {
                        let mut next = Vec::with_capacity(cur.len() >> 1);
                        for pair in cur.as_chunks::<2>().0 {
                            let left = pair[0];
                            let right = pair[1];
                            let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                                SolvencyError::LiabilitySumOverflow {
                                    level: lv,
                                    left: left.sum_fp,
                                    right: right.sum_fp,
                                },
                            )?;
                            let p_hash = poseidon_node_hash(
                                left.poseidon_hash,
                                Fr::from(left.sum_fp),
                                right.poseidon_hash,
                                Fr::from(right.sum_fp),
                            );
                            let b3 = blake3_node_digest(
                                p_hash,
                                &left.blake3_digest,
                                left.sum_fp,
                                &right.blake3_digest,
                                right.sum_fp,
                            );
                            next.push(MsstNode {
                                poseidon_hash: p_hash,
                                blake3_digest: b3,
                                sum_fp,
                            });
                        }
                        built_levels.push(next.clone());
                        cur = next;
                    }
                    Ok(TileResult {
                        levels: built_levels,
                    })
                })
                .collect();

            let tile_results = tile_results?;
            for lv in 0..tile_levels {
                let level_idx = lv + 1;
                let off = level_offsets[level_idx];
                let total_at_level = 1usize << (depth - level_idx);
                let nodes_per_tile = 1usize << (tile_levels - level_idx);
                let mut written = 0usize;
                for tr in &tile_results {
                    let slice = &tr.levels[lv];
                    arena[(off + written)..(off + written + nodes_per_tile)]
                        .copy_from_slice(slice);
                    written += nodes_per_tile;
                }
                if written < total_at_level {
                    arena[(off + written)..(off + total_at_level)].fill(zero_tbl.nodes[level_idx]);
                }
            }
        }

        // Complete upper levels (`start_level..depth`) using parallel slice splits when large.
        let start_level = if tile_levels > 0 && populated_tiles >= 2 {
            tile_levels
        } else {
            0
        };
        let mut active_span = active >> start_level;
        if (active & ((1usize << start_level) - 1)) != 0 {
            active_span += 1;
        }

        for level in start_level..depth {
            let child_off = level_offsets[level];
            let parent_off = level_offsets[level + 1];
            let parent_count = 1usize << (depth - 1 - level);
            let populated_parents = active_span.div_ceil(2);

            let (head, tail) = arena.split_at_mut(parent_off);
            let children = &head[child_off..(child_off + (parent_count << 1))];
            let parents = &mut tail[..parent_count];

            if populated_parents >= 64 {
                let overflow_err: Result<(), SolvencyError> = parents[..populated_parents]
                    .par_iter_mut()
                    .enumerate()
                    .try_for_each(|(p_idx, out)| {
                        let left = children[p_idx << 1];
                        let right = children[(p_idx << 1) + 1];
                        let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                            SolvencyError::LiabilitySumOverflow {
                                level,
                                left: left.sum_fp,
                                right: right.sum_fp,
                            },
                        )?;
                        let p_hash = poseidon_node_hash(
                            left.poseidon_hash,
                            Fr::from(left.sum_fp),
                            right.poseidon_hash,
                            Fr::from(right.sum_fp),
                        );
                        let b3 = blake3_node_digest(
                            p_hash,
                            &left.blake3_digest,
                            left.sum_fp,
                            &right.blake3_digest,
                            right.sum_fp,
                        );
                        *out = MsstNode {
                            poseidon_hash: p_hash,
                            blake3_digest: b3,
                            sum_fp,
                        };
                        Ok(())
                    });
                overflow_err?;
            } else {
                for (p_idx, out) in parents[..populated_parents].iter_mut().enumerate() {
                    let left = children[p_idx << 1];
                    let right = children[(p_idx << 1) + 1];
                    let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                        SolvencyError::LiabilitySumOverflow {
                            level,
                            left: left.sum_fp,
                            right: right.sum_fp,
                        },
                    )?;
                    let p_hash = poseidon_node_hash(
                        left.poseidon_hash,
                        Fr::from(left.sum_fp),
                        right.poseidon_hash,
                        Fr::from(right.sum_fp),
                    );
                    let b3 = blake3_node_digest(
                        p_hash,
                        &left.blake3_digest,
                        left.sum_fp,
                        &right.blake3_digest,
                        right.sum_fp,
                    );
                    *out = MsstNode {
                        poseidon_hash: p_hash,
                        blake3_digest: b3,
                        sum_fp,
                    };
                }
            }

            if populated_parents < parent_count {
                parents[populated_parents..parent_count].fill(zero_tbl.nodes[level + 1]);
            }
            active_span = populated_parents;
        }

        let epoch_blinding = derive_leaf_salt(epoch_master_seed, u64::MAX, epoch_id);
        let root = arena[level_offsets[depth]];
        let root_commitment =
            poseidon_root_commitment(root.poseidon_hash, root.sum_fp, epoch_id, epoch_blinding);

        Ok(Self {
            epoch_id,
            depth,
            active_accounts: active,
            epoch_blinding,
            root_commitment,
            arena,
            level_offsets,
            sorted_accounts: sorted,
            leaf_salts,
            account_index,
        })
    }

    /// Returns the root `MsstNode` (`(root_poseidon_hash, root_blake3, total_liabilities_fp)`).
    #[inline]
    pub fn root(&self) -> MsstNode {
        self.arena[self.level_offsets[self.depth]]
    }

    /// Returns total committed liabilities across all accounts in `1e8` fixed-point atomic units.
    #[inline]
    pub fn total_liabilities_fp(&self) -> u128 {
        self.root().sum_fp
    }

    /// Returns slice of all `2^(depth - level)` nodes at `level` (`0` = leaves, `depth` = `[root]`).
    pub fn level_nodes(&self, level: usize) -> SolvencyResult<&[MsstNode]> {
        if level > self.depth {
            return Err(SolvencyError::InvalidTreeDepth {
                depth: level,
                max_depth: self.depth,
            });
        }
        let start = self.level_offsets[level];
        let count = 1usize << (self.depth - level);
        Ok(&self.arena[start..(start + count)])
    }

    /// Generates an individual account's Merkle-Sum Inclusion Proof by `account_id`.
    pub fn generate_inclusion_proof(&self, account_id: u64) -> SolvencyResult<MsstInclusionProof> {
        let &leaf_index = self
            .account_index
            .get(&account_id)
            .ok_or(SolvencyError::AccountNotFound { account_id })?;
        self.generate_inclusion_proof_by_index(leaf_index)
    }

    /// Generates a Merkle-Sum Inclusion Proof for leaf slot `leaf_index`.
    pub fn generate_inclusion_proof_by_index(
        &self,
        leaf_index: usize,
    ) -> SolvencyResult<MsstInclusionProof> {
        let capacity = 1usize << self.depth;
        if leaf_index >= capacity {
            return Err(SolvencyError::LeafIndexOutOfBounds {
                index: leaf_index,
                capacity,
            });
        }

        let (account_id, balance_fp, leaf_salt) = if leaf_index < self.active_accounts {
            let acc = self.sorted_accounts[leaf_index];
            (acc.account_id, acc.balance_fp, self.leaf_salts[leaf_index])
        } else {
            (0u64, 0u64, Fr::from(0u64))
        };

        // Derive user-specific blinding factor deterministically from leaf salt (domain-separated).
        let user_blinding = poseidon_leaf_hash(account_id ^ 0x5553_4552_424C_4E44, balance_fp, leaf_salt);
        let user_commitment = poseidon_user_commitment(account_id, balance_fp, user_blinding);

        let leaf_node = self.arena[self.level_offsets[0] + leaf_index];
        let root_node = self.root();

        let mut path = Vec::with_capacity(self.depth);
        let mut cur_idx = leaf_index;
        for level in 0..self.depth {
            let sibling_idx = cur_idx ^ 1;
            let is_right_child = (cur_idx & 1) == 1;
            let sibling = self.arena[self.level_offsets[level] + sibling_idx];
            path.push(MsstPathStep {
                sibling_poseidon_bytes: fr_to_bytes_le(sibling.poseidon_hash),
                sibling_blake3: sibling.blake3_digest,
                sibling_sum_fp: sibling.sum_fp,
                is_right_child,
            });
            cur_idx >>= 1;
        }

        Ok(MsstInclusionProof {
            epoch_id: self.epoch_id,
            tree_depth: self.depth,
            leaf_index,
            account_id,
            balance_fp,
            leaf_salt_bytes: fr_to_bytes_le(leaf_salt),
            leaf_poseidon_bytes: fr_to_bytes_le(leaf_node.poseidon_hash),
            leaf_blake3: leaf_node.blake3_digest,
            user_blinding_bytes: fr_to_bytes_le(user_blinding),
            user_commitment_bytes: fr_to_bytes_le(user_commitment),
            root_poseidon_bytes: fr_to_bytes_le(root_node.poseidon_hash),
            root_blake3: root_node.blake3_digest,
            root_commitment_bytes: fr_to_bytes_le(self.root_commitment),
            path,
        })
    }
}

/// **Pass 3 Cache-Line Aligned Zero-Allocation Reusable MSST Workspace**:
/// Pre-allocates the contiguous `#[repr(C, align(64))]` node arena once so repeated epoch
/// snapshot builds execute with **0 heap allocations (`0 B/op`, `0 allocs/op`)** using disjoint
/// cache-line-aligned Rayon slice chunks (`par_chunks_mut`).
pub struct MsstReusableWorkspace {
    depth: usize,
    level_offsets: Vec<usize>,
    arena: Vec<MsstNode>,
}

impl MsstReusableWorkspace {
    pub fn new(depth: usize) -> SolvencyResult<Self> {
        if depth == 0 || depth > MAX_MSST_DEPTH {
            return Err(SolvencyError::InvalidTreeDepth {
                depth,
                max_depth: MAX_MSST_DEPTH,
            });
        }
        // Force one-time lazy static initialization outside steady-state hot path.
        let _ = sparse_zero_table();
        let _ = crate::poseidon_params::fast_poseidon();

        let (total_nodes, level_offsets) = MsstTree::compute_level_offsets(depth);
        Ok(Self {
            depth,
            level_offsets,
            arena: vec![MsstNode::default(); total_nodes],
        })
    }

    /// Rebuilds the entire Merkle-Sum Sparse Tree in place over pre-sorted unique accounts
    /// with **zero heap allocations (`0 allocs/op`, `0 B/op`)**.
    /// Returns `(root_node, root_commitment)`.
    pub fn rebuild_parallel_zero_alloc(
        &mut self,
        sorted_unique_accounts: &[AccountLeafInput],
        epoch_id: u64,
        epoch_master_seed: &[u8; 32],
    ) -> SolvencyResult<(MsstNode, Fr)> {
        if sorted_unique_accounts.is_empty() {
            return Err(SolvencyError::EmptyAccountSet);
        }
        let cap = 1usize << self.depth;
        let active = sorted_unique_accounts.len();
        if active > cap {
            return Err(SolvencyError::TreeCapacityExceeded {
                count: active,
                depth: self.depth,
                capacity: cap,
            });
        }

        let zero_tbl = sparse_zero_table();

        // 1. Disjoint cache-line-aligned parallel leaf hashing (`128` leaves = `256` cache lines per chunk).
        if active >= 256 {
            self.arena[..active]
                .par_chunks_mut(128)
                .enumerate()
                .for_each(|(chunk_id, chunk)| {
                    let base = chunk_id * 128;
                    for (offset, slot) in chunk.iter_mut().enumerate() {
                        let acc = sorted_unique_accounts[base + offset];
                        let salt = derive_leaf_salt(epoch_master_seed, acc.account_id, epoch_id);
                        let p_hash = poseidon_leaf_hash(acc.account_id, acc.balance_fp, salt);
                        let b3 = blake3_leaf_digest(p_hash, acc.account_id, acc.balance_fp);
                        *slot = MsstNode {
                            poseidon_hash: p_hash,
                            blake3_digest: b3,
                            sum_fp: u128::from(acc.balance_fp),
                        };
                    }
                });
        } else {
            for (idx, acc) in sorted_unique_accounts.iter().enumerate() {
                let salt = derive_leaf_salt(epoch_master_seed, acc.account_id, epoch_id);
                let p_hash = poseidon_leaf_hash(acc.account_id, acc.balance_fp, salt);
                let b3 = blake3_leaf_digest(p_hash, acc.account_id, acc.balance_fp);
                self.arena[idx] = MsstNode {
                    poseidon_hash: p_hash,
                    blake3_digest: b3,
                    sum_fp: u128::from(acc.balance_fp),
                };
            }
        }

        if active < cap {
            self.arena[active..cap].fill(zero_tbl.nodes[0]);
        }

        // 2. Disjoint level-by-level parallel reduction with zero heap allocations.
        let mut active_span = active;
        for level in 0..self.depth {
            let child_off = self.level_offsets[level];
            let parent_off = self.level_offsets[level + 1];
            let parent_count = 1usize << (self.depth - 1 - level);
            let populated_parents = active_span.div_ceil(2);

            let (head, tail) = self.arena.split_at_mut(parent_off);
            let children = &head[child_off..(child_off + (parent_count << 1))];
            let parents = &mut tail[..parent_count];

            if populated_parents >= 256 {
                let overflow_flag = AtomicBool::new(false);
                parents[..populated_parents]
                    .par_chunks_mut(128)
                    .enumerate()
                    .for_each(|(chunk_id, p_chunk)| {
                        let p_base = chunk_id * 128;
                        for (off, out) in p_chunk.iter_mut().enumerate() {
                            let p_idx = p_base + off;
                            let left = children[p_idx << 1];
                            let right = children[(p_idx << 1) + 1];
                            let Some(sum_fp) = left.sum_fp.checked_add(right.sum_fp) else {
                                overflow_flag.store(true, Ordering::Relaxed);
                                return;
                            };
                            let p_hash = poseidon_node_hash(
                                left.poseidon_hash,
                                Fr::from(left.sum_fp),
                                right.poseidon_hash,
                                Fr::from(right.sum_fp),
                            );
                            let b3 = blake3_node_digest(
                                p_hash,
                                &left.blake3_digest,
                                left.sum_fp,
                                &right.blake3_digest,
                                right.sum_fp,
                            );
                            *out = MsstNode {
                                poseidon_hash: p_hash,
                                blake3_digest: b3,
                                sum_fp,
                            };
                        }
                    });
                if overflow_flag.load(Ordering::Relaxed) {
                    return Err(SolvencyError::LiabilitySumOverflow {
                        level,
                        left: u128::MAX,
                        right: 1,
                    });
                }
            } else {
                for (p_idx, out) in parents[..populated_parents].iter_mut().enumerate() {
                    let left = children[p_idx << 1];
                    let right = children[(p_idx << 1) + 1];
                    let sum_fp = left.sum_fp.checked_add(right.sum_fp).ok_or(
                        SolvencyError::LiabilitySumOverflow {
                            level,
                            left: left.sum_fp,
                            right: right.sum_fp,
                        },
                    )?;
                    let p_hash = poseidon_node_hash(
                        left.poseidon_hash,
                        Fr::from(left.sum_fp),
                        right.poseidon_hash,
                        Fr::from(right.sum_fp),
                    );
                    let b3 = blake3_node_digest(
                        p_hash,
                        &left.blake3_digest,
                        left.sum_fp,
                        &right.blake3_digest,
                        right.sum_fp,
                    );
                    *out = MsstNode {
                        poseidon_hash: p_hash,
                        blake3_digest: b3,
                        sum_fp,
                    };
                }
            }

            if populated_parents < parent_count {
                parents[populated_parents..parent_count].fill(zero_tbl.nodes[level + 1]);
            }
            active_span = populated_parents;
        }

        let root = self.arena[self.level_offsets[self.depth]];
        let epoch_blinding = derive_leaf_salt(epoch_master_seed, u64::MAX, epoch_id);
        let root_commitment =
            poseidon_root_commitment(root.poseidon_hash, root.sum_fp, epoch_id, epoch_blinding);

        Ok((root, root_commitment))
    }
}
