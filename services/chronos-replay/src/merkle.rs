#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use crate::fixed::AssetSymbol;

/// Number of top-level radix buckets (`2^12 = 4096`), matching Channel 02 `ledger-rs` bit-for-bit.
pub const ACCOUNT_BUCKET_BITS: usize = 12;
pub const ACCOUNT_BUCKET_COUNT: usize = 1 << ACCOUNT_BUCKET_BITS;

const DOMAIN_EMPTY_LEAF: &[u8] = b"HYPERION_MERKLE_EMPTY_V1";
const DOMAIN_ACCOUNT_LEAF: &[u8] = b"HYPERION_ACCOUNT_LEAF_V1";
const DOMAIN_EXEC_FILL_LEAF: &[u8] = b"HYPERION_EXEC_FILL_V1";
const DOMAIN_LOB_ORDER: &[u8] = b"HYPERION_LOB_ORDER_V1";
const DOMAIN_LOB_ROOT: &[u8] = b"HYPERION_LOB_COMMITMENT_V1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetBalanceLeaf {
    pub asset: AssetSymbol,
    pub available_fp: i128,
    pub locked_fp: i128,
}

impl AssetBalanceLeaf {
    #[inline(always)]
    pub const fn total_fp(&self) -> i128 {
        self.available_fp.saturating_add(self.locked_fp)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofStep {
    pub sibling_hash: [u8; 32],
    pub is_left_sibling: bool,
    pub depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionFillDigestInput {
    pub fill_id: u64,
    pub engine_seq: u64,
    pub instrument_id: u32,
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub maker_account_id: u64,
    pub taker_account_id: u64,
    pub aggressor_side: u8,
    pub price_fp: u64,
    pub qty_fp: u64,
    pub notional_fp: u128,
    pub maker_fee_fp: i64,
    pub taker_fee_fp: i64,
    pub exec_ts_ns: u64,
}

/// Canonical domain-separated binary Merkle parent hash `Blake3(0x01 || left || right)`.
#[inline(always)]
pub fn hash_merkle_branch(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 65];
    buf[0] = 0x01;
    buf[1..33].copy_from_slice(left);
    buf[33..65].copy_from_slice(right);
    *blake3::hash(&buf).as_bytes()
}

/// Verifies any Hyperion Merkle proof (Account Balance or Execution Fill) against `state_root_blake3`.
#[inline]
pub fn verify_merkle_proof(
    leaf_hash: &[u8; 32],
    proof_path: &[ProofStep],
    expected_state_root: &[u8; 32],
) -> bool {
    let mut current = *leaf_hash;
    for step in proof_path {
        current = if step.is_left_sibling {
            hash_merkle_branch(&step.sibling_hash, &current)
        } else {
            hash_merkle_branch(&current, &step.sibling_hash)
        };
    }
    current == *expected_state_root
}

/// Canonical deterministic Blake3 leaf hash for an account's sorted multi-asset balances.
pub fn hash_account_leaf(account_id: u64, sorted_balances: &[AssetBalanceLeaf]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[0x00]);
    hasher.update(DOMAIN_ACCOUNT_LEAF);
    hasher.update(&account_id.to_le_bytes());
    hasher.update(&(sorted_balances.len() as u32).to_le_bytes());
    for bal in sorted_balances {
        let bytes = bal.asset.as_bytes();
        hasher.update(&(bytes.len() as u16).to_le_bytes());
        hasher.update(bytes);
        hasher.update(&bal.available_fp.to_le_bytes());
        hasher.update(&bal.locked_fp.to_le_bytes());
        let total = bal.available_fp.saturating_add(bal.locked_fp);
        hasher.update(&total.to_le_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// Canonical deterministic Blake3 leaf hash for an immutable trade execution fill.
pub fn hash_execution_fill_leaf(fill: &ExecutionFillDigestInput) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[0x00]);
    hasher.update(DOMAIN_EXEC_FILL_LEAF);
    hasher.update(&fill.fill_id.to_le_bytes());
    hasher.update(&fill.engine_seq.to_le_bytes());
    hasher.update(&fill.instrument_id.to_le_bytes());
    hasher.update(&fill.maker_order_id.to_le_bytes());
    hasher.update(&fill.taker_order_id.to_le_bytes());
    hasher.update(&fill.maker_account_id.to_le_bytes());
    hasher.update(&fill.taker_account_id.to_le_bytes());
    hasher.update(&[fill.aggressor_side]);
    hasher.update(&fill.price_fp.to_le_bytes());
    hasher.update(&fill.qty_fp.to_le_bytes());
    hasher.update(&fill.notional_fp.to_le_bytes());
    hasher.update(&fill.maker_fee_fp.to_le_bytes());
    hasher.update(&fill.taker_fee_fp.to_le_bytes());
    hasher.update(&fill.exec_ts_ns.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// Bellare-Micciancio 256-bit Incremental Lattice/Multiset Accumulator (`LtHash`) for resting LOB orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LobStateAccumulator {
    lanes: [u64; 4],
}

impl LobStateAccumulator {
    #[inline(always)]
    pub const fn new() -> Self {
        Self { lanes: [0u64; 4] }
    }

    #[inline(always)]
    pub const fn from_lanes(lanes: [u64; 4]) -> Self {
        Self { lanes }
    }

    #[inline(always)]
    pub const fn lanes(&self) -> [u64; 4] {
        self.lanes
    }

    #[inline(always)]
    pub fn order_state_lanes(
        instrument_id: u32,
        order_id: u64,
        account_id: u64,
        side: u8,
        price_fp: u64,
        remaining_qty_fp: u64,
        priority_seq: u64,
    ) -> [u64; 4] {
        let mut buf = [0u8; 74];
        buf[0..21].copy_from_slice(DOMAIN_LOB_ORDER);
        buf[21..25].copy_from_slice(&instrument_id.to_le_bytes());
        buf[25..33].copy_from_slice(&order_id.to_le_bytes());
        buf[33..41].copy_from_slice(&account_id.to_le_bytes());
        buf[41] = side;
        buf[42..50].copy_from_slice(&price_fp.to_le_bytes());
        buf[50..58].copy_from_slice(&remaining_qty_fp.to_le_bytes());
        buf[58..66].copy_from_slice(&priority_seq.to_le_bytes());
        let digest = blake3::hash(&buf);
        let b = digest.as_bytes();
        [
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
            u64::from_le_bytes([b[16], b[17], b[18], b[19], b[20], b[21], b[22], b[23]]),
            u64::from_le_bytes([b[24], b[25], b[26], b[27], b[28], b[29], b[30], b[31]]),
        ]
    }

    #[inline(always)]
    pub fn add_lanes(&mut self, delta: [u64; 4]) {
        self.lanes[0] = self.lanes[0].wrapping_add(delta[0]);
        self.lanes[1] = self.lanes[1].wrapping_add(delta[1]);
        self.lanes[2] = self.lanes[2].wrapping_add(delta[2]);
        self.lanes[3] = self.lanes[3].wrapping_add(delta[3]);
    }

    #[inline(always)]
    pub fn sub_lanes(&mut self, delta: [u64; 4]) {
        self.lanes[0] = self.lanes[0].wrapping_sub(delta[0]);
        self.lanes[1] = self.lanes[1].wrapping_sub(delta[1]);
        self.lanes[2] = self.lanes[2].wrapping_sub(delta[2]);
        self.lanes[3] = self.lanes[3].wrapping_sub(delta[3]);
    }

    #[inline(always)]
    pub fn commitment_hash(&self) -> [u8; 32] {
        let mut buf = [0u8; 58];
        buf[0..26].copy_from_slice(DOMAIN_LOB_ROOT);
        buf[26..34].copy_from_slice(&self.lanes[0].to_le_bytes());
        buf[34..42].copy_from_slice(&self.lanes[1].to_le_bytes());
        buf[42..50].copy_from_slice(&self.lanes[2].to_le_bytes());
        buf[50..58].copy_from_slice(&self.lanes[3].to_le_bytes());
        *blake3::hash(&buf).as_bytes()
    }
}

#[inline(always)]
pub fn account_bucket_index(account_id: u64) -> usize {
    let mut z = account_id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z = z ^ (z >> 31);
    (z as usize) & (ACCOUNT_BUCKET_COUNT - 1)
}

pub fn build_local_subtree_root_and_proof(
    leaves: &[[u8; 32]],
    target_idx: Option<usize>,
    empty_hash: &[u8; 32],
) -> ([u8; 32], Vec<ProofStep>) {
    if leaves.is_empty() {
        return (*empty_hash, Vec::new());
    }
    if leaves.len() == 1 {
        return (leaves[0], Vec::new());
    }
    let pow2 = leaves.len().next_power_of_two();
    let mut layer = Vec::with_capacity(pow2);
    layer.extend_from_slice(leaves);
    layer.resize(pow2, *empty_hash);

    let mut proof = Vec::new();
    let mut cur_idx = target_idx;
    let mut depth = 0u32;
    while layer.len() > 1 {
        if let Some(idx) = cur_idx {
            let sib_idx = idx ^ 1;
            proof.push(ProofStep {
                sibling_hash: layer[sib_idx],
                is_left_sibling: sib_idx < idx,
                depth,
            });
            cur_idx = Some(idx >> 1);
        }
        let next_len = layer.len() >> 1;
        for i in 0..next_len {
            layer[i] = hash_merkle_branch(&layer[2 * i], &layer[2 * i + 1]);
        }
        layer.truncate(next_len);
        depth += 1;
    }
    (layer[0], proof)
}

/// Deterministic Blake3 State Commitment Tree matching Channel 02's `IncrementalMerkleState`:
/// `state_root_blake3 = H(0x01 || accounts_merkle_root || H(0x01 || lob_commitment || executions_root))`
#[derive(Clone)]
pub struct StateCommitmentTree {
    empty_leaf_hash: [u8; 32],
    /// Precomputed hashes for completely empty subtrees at heights `0..=ACCOUNT_BUCKET_BITS`,
    /// reducing `new()` and `clear_keep_capacity()` from 4,095 Blake3 calls to 12 (or 0 on reset!).
    empty_level_hashes: [[u8; 32]; ACCOUNT_BUCKET_BITS + 1],
    account_tree: Box<[[u8; 32]]>,
    buckets: Vec<Vec<(u64, [u8; 32])>>,
    populated_buckets: Vec<u16>,
    dirty_buckets: Vec<u16>,
    dirty_words: [u64; ACCOUNT_BUCKET_COUNT / 64],
    exec_leaf_count: u64,
    exec_frontier: [Option<[u8; 32]>; 32],
    cached_exec_root: [u8; 32],
    exec_root_dirty: bool,
    cached_state_root: [u8; 32],
}

impl StateCommitmentTree {
    pub fn new() -> Self {
        let empty_leaf_hash = *blake3::hash(DOMAIN_EMPTY_LEAF).as_bytes();
        let mut empty_level_hashes = [[0u8; 32]; ACCOUNT_BUCKET_BITS + 1];
        empty_level_hashes[0] = empty_leaf_hash;
        for h in 1..=ACCOUNT_BUCKET_BITS {
            empty_level_hashes[h] =
                hash_merkle_branch(&empty_level_hashes[h - 1], &empty_level_hashes[h - 1]);
        }

        let mut account_tree = vec![[0u8; 32]; ACCOUNT_BUCKET_COUNT * 2].into_boxed_slice();
        for (h, &lvl_hash) in empty_level_hashes.iter().enumerate() {
            let level_start = ACCOUNT_BUCKET_COUNT >> h;
            let level_end = level_start << 1;
            account_tree[level_start..level_end].fill(lvl_hash);
        }

        let mut buckets = Vec::with_capacity(ACCOUNT_BUCKET_COUNT);
        for _ in 0..ACCOUNT_BUCKET_COUNT {
            buckets.push(Vec::new());
        }
        let lob_empty = LobStateAccumulator::new().commitment_hash();
        let right_branch = hash_merkle_branch(&lob_empty, &empty_leaf_hash);
        let cached_state_root = hash_merkle_branch(&account_tree[1], &right_branch);

        Self {
            empty_leaf_hash,
            empty_level_hashes,
            account_tree,
            buckets,
            populated_buckets: Vec::with_capacity(64),
            dirty_buckets: Vec::with_capacity(64),
            dirty_words: [0u64; ACCOUNT_BUCKET_COUNT / 64],
            exec_leaf_count: 0,
            exec_frontier: [None; 32],
            cached_exec_root: empty_leaf_hash,
            exec_root_dirty: false,
            cached_state_root,
        }
    }

    pub fn clear_keep_capacity(&mut self) {
        for &b_u16 in &self.populated_buckets {
            let b_idx = b_u16 as usize;
            self.buckets[b_idx].clear();
            let mut node = ACCOUNT_BUCKET_COUNT + b_idx;
            for &lvl_hash in &self.empty_level_hashes {
                self.account_tree[node] = lvl_hash;
                node >>= 1;
                if node == 0 {
                    break;
                }
            }
        }
        self.populated_buckets.clear();
        for &b_u16 in &self.dirty_buckets {
            self.dirty_words[(b_u16 as usize) >> 6] = 0;
        }
        self.dirty_buckets.clear();
        self.exec_leaf_count = 0;
        self.exec_frontier = [None; 32];
        self.cached_exec_root = self.empty_leaf_hash;
        self.exec_root_dirty = false;
        let lob_empty = LobStateAccumulator::new().commitment_hash();
        let right_branch = hash_merkle_branch(&lob_empty, &self.empty_leaf_hash);
        self.cached_state_root = hash_merkle_branch(&self.account_tree[1], &right_branch);
    }

    pub fn restore_exec_frontier(&mut self, count: u64, frontier: &[Option<[u8; 32]>]) {
        self.exec_leaf_count = count;
        self.exec_frontier = [None; 32];
        for (i, item) in frontier.iter().copied().enumerate().take(32) {
            self.exec_frontier[i] = item;
        }
        self.exec_root_dirty = true;
    }

    pub fn export_exec_frontier(&self) -> (u64, Vec<Option<[u8; 32]>>) {
        (self.exec_leaf_count, self.exec_frontier.to_vec())
    }

    #[inline]
    pub fn upsert_account_leaf(&mut self, account_id: u64, leaf_hash: [u8; 32]) {
        let b_idx = account_bucket_index(account_id);
        let bucket = &mut self.buckets[b_idx];
        let was_empty = bucket.is_empty();
        match bucket.binary_search_by_key(&account_id, |&(id, _)| id) {
            Ok(pos) => bucket[pos].1 = leaf_hash,
            Err(pos) => bucket.insert(pos, (account_id, leaf_hash)),
        }
        if was_empty {
            self.populated_buckets.push(b_idx as u16);
        }
        let word_idx = b_idx >> 6;
        let bit_mask = 1u64 << (b_idx & 63);
        if (self.dirty_words[word_idx] & bit_mask) == 0 {
            self.dirty_words[word_idx] |= bit_mask;
            self.dirty_buckets.push(b_idx as u16);
        }
    }

    #[inline]
    pub fn append_execution_leaf(&mut self, fill_leaf_hash: [u8; 32]) {
        self.exec_leaf_count = self.exec_leaf_count.saturating_add(1);
        let mut carry = fill_leaf_hash;
        for slot in &mut self.exec_frontier {
            match slot.take() {
                None => {
                    *slot = Some(carry);
                    break;
                }
                Some(left) => {
                    carry = hash_merkle_branch(&left, &carry);
                }
            }
        }
        self.exec_root_dirty = true;
    }

    #[inline]
    fn refresh_exec_root(&mut self) -> [u8; 32] {
        if !self.exec_root_dirty {
            return self.cached_exec_root;
        }
        if self.exec_leaf_count == 0 {
            self.cached_exec_root = self.empty_leaf_hash;
            self.exec_root_dirty = false;
            return self.cached_exec_root;
        }
        if self.exec_leaf_count == 1 {
            self.cached_exec_root = self.exec_frontier[0].unwrap_or(self.empty_leaf_hash);
            self.exec_root_dirty = false;
            return self.cached_exec_root;
        }
        let mut empty_at_height = self.empty_leaf_hash;
        let mut acc: Option<[u8; 32]> = None;
        let pow2 = self.exec_leaf_count.next_power_of_two();
        let target_bits = pow2.trailing_zeros() as usize;
        for peak_opt in self.exec_frontier.iter().take(target_bits) {
            if let Some(peak) = *peak_opt {
                acc = Some(match acc {
                    None => hash_merkle_branch(&peak, &empty_at_height),
                    Some(right_sub) => hash_merkle_branch(&peak, &right_sub),
                });
            } else if let Some(right_sub) = acc {
                acc = Some(hash_merkle_branch(&right_sub, &empty_at_height));
            }
            empty_at_height = hash_merkle_branch(&empty_at_height, &empty_at_height);
        }
        if let Some(full_root) = self.exec_frontier[target_bits] {
            acc = Some(full_root);
        }
        self.cached_exec_root = acc.unwrap_or(self.empty_leaf_hash);
        self.exec_root_dirty = false;
        self.cached_exec_root
    }

    #[inline]
    pub fn commit_state_root(&mut self, lob_acc: &LobStateAccumulator) -> [u8; 32] {
        for &b_u16 in &self.dirty_buckets {
            let b_idx = b_u16 as usize;
            self.dirty_words[b_idx >> 6] = 0;
            let bucket = &self.buckets[b_idx];
            let leaf_slot = ACCOUNT_BUCKET_COUNT + b_idx;
            self.account_tree[leaf_slot] = match bucket.len() {
                0 => self.empty_leaf_hash,
                1 => bucket[0].1,
                _ => {
                    let hashes: Vec<[u8; 32]> = bucket.iter().map(|&(_, h)| h).collect();
                    build_local_subtree_root_and_proof(&hashes, None, &self.empty_leaf_hash).0
                }
            };
            let mut node = leaf_slot >> 1;
            while node >= 1 {
                self.account_tree[node] = hash_merkle_branch(
                    &self.account_tree[node * 2],
                    &self.account_tree[node * 2 + 1],
                );
                node >>= 1;
            }
        }
        self.dirty_buckets.clear();

        let exec_root = self.refresh_exec_root();
        let lob_hash = lob_acc.commitment_hash();
        let right_branch = hash_merkle_branch(&lob_hash, &exec_root);
        self.cached_state_root = hash_merkle_branch(&self.account_tree[1], &right_branch);
        self.cached_state_root
    }

    pub fn generate_account_proof(
        &mut self,
        account_id: u64,
        lob_acc: &LobStateAccumulator,
    ) -> ([u8; 32], Vec<ProofStep>, [u8; 32]) {
        let state_root = self.commit_state_root(lob_acc);
        let b_idx = account_bucket_index(account_id);
        let bucket = &self.buckets[b_idx];

        let (leaf_hash, mut proof) = if bucket.is_empty() {
            (
                self.empty_leaf_hash,
                Vec::with_capacity(ACCOUNT_BUCKET_BITS + 2),
            )
        } else if bucket.len() == 1 && bucket[0].0 == account_id {
            (bucket[0].1, Vec::with_capacity(ACCOUNT_BUCKET_BITS + 1))
        } else {
            match bucket.binary_search_by_key(&account_id, |&(id, _)| id) {
                Ok(pos) => {
                    let hashes: Vec<[u8; 32]> = bucket.iter().map(|&(_, h)| h).collect();
                    let (_, sub_proof) = build_local_subtree_root_and_proof(
                        &hashes,
                        Some(pos),
                        &self.empty_leaf_hash,
                    );
                    (bucket[pos].1, sub_proof)
                }
                Err(_) => (self.empty_leaf_hash, Vec::new()),
            }
        };

        let base_depth = proof.len() as u32;
        let mut node = ACCOUNT_BUCKET_COUNT + b_idx;
        for level in 0..ACCOUNT_BUCKET_BITS {
            let sib = node ^ 1;
            proof.push(ProofStep {
                sibling_hash: self.account_tree[sib],
                is_left_sibling: (sib & 1) == 0,
                depth: base_depth + level as u32,
            });
            node >>= 1;
        }

        let exec_root = self.refresh_exec_root();
        let lob_hash = lob_acc.commitment_hash();
        let right_branch = hash_merkle_branch(&lob_hash, &exec_root);
        proof.push(ProofStep {
            sibling_hash: right_branch,
            is_left_sibling: false,
            depth: base_depth + ACCOUNT_BUCKET_BITS as u32,
        });

        (leaf_hash, proof, state_root)
    }
}

impl Default for StateCommitmentTree {
    fn default() -> Self {
        Self::new()
    }
}
