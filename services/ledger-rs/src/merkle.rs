use crate::fixed::AssetSymbol;

/// Number of top-level radix buckets (`2^6 = 64`), keeping the entire `128 * 32 = 4 KB`
/// (1 OS page / 64 cache lines) internal binary Merkle tree 100% resident in L1d CPU cache!
pub const ACCOUNT_BUCKET_BITS: usize = 6;
pub const ACCOUNT_BUCKET_COUNT: usize = 1 << ACCOUNT_BUCKET_BITS;

/// Pre-allocated fixed-capacity retention window for in-memory execution fill proofs.
/// The O(1)-amortized MMR frontier (`exec_frontier` + `exec_left_bag`) tracks exact cryptographic
/// state root for up to `2^32` fills in constant O(1) RAM!
pub const MAX_IN_MEMORY_EXEC_LEAVES: usize = 65_536;

/// 32-byte Blake3 keyed-hash domain-separation keys.
/// Using official BLAKE3 keyed mode keeps 64-byte Merkle branches (`left[32] || right[32]`) and
/// 48-byte LOB LtHash preimages inside a SINGLE 64-byte Blake3 compression block (2.0x faster).
const KEY_MERKLE_BRANCH: [u8; 32] = *b"HYPERION_MERKLE_BRANCH_KEY_V0001";
const KEY_ACCOUNT_LEAF: [u8; 32] = *b"HYPERION_ACCOUNT_LEAF_KEY_V00001";
const KEY_EXEC_FILL_LEAF: [u8; 32] = *b"HYPERION_EXEC_FILL_KEY_V00000001";
const KEY_LOB_ORDER: [u8; 32] = *b"HYPERION_LOB_ORDER_LTHASH_V00001";
const KEY_LOB_ROOT: [u8; 32] = *b"HYPERION_LOB_COMMITMENT_KEY_V001";
const DOMAIN_EMPTY_LEAF: &[u8] = b"HYPERION_MERKLE_EMPTY_V1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetBalanceLeaf {
    pub asset: AssetSymbol,
    pub available_fp: i128,
    pub locked_fp: i128,
}

impl AssetBalanceLeaf {
    #[inline(always)]
    pub const fn total_fp(&self) -> Result<i128, &'static str> {
        match self.available_fp.checked_add(self.locked_fp) {
            Some(t) => Ok(t),
            None => Err("asset total overflow"),
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInclusionProof {
    pub fill_id: u64,
    pub engine_seq: u64,
    pub leaf_blake3_hash: [u8; 32],
    pub state_root_blake3: [u8; 32],
    pub proof_path: Vec<ProofStep>,
}

/// Canonical domain-separated binary Merkle parent hash using single-block 64-byte BLAKE3 keyed mode:
/// `Blake3_Keyed(KEY_MERKLE_BRANCH, left[32] || right[32])`.
#[inline(always)]
pub fn hash_merkle_branch(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[0..32].copy_from_slice(left);
    buf[32..64].copy_from_slice(right);
    *blake3::keyed_hash(&KEY_MERKLE_BRANCH, &buf).as_bytes()
}

/// Verifies ANY Hyperion Merkle proof (Account Balance OR Execution Fill) directly against
/// `state_root_blake3` using pure binary parent hashing.
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
/// Uses a zero-heap stack buffer fast path (avoiding 2 KB `Hasher` stack initialization) for up to 8 assets.
#[inline]
pub fn hash_account_leaf(account_id: u64, sorted_balances: &[AssetBalanceLeaf]) -> [u8; 32] {
    if sorted_balances.len() <= 8 {
        let mut buf = [0u8; 560];
        let mut pos = 0usize;
        buf[pos..pos + 8].copy_from_slice(&account_id.to_le_bytes());
        pos += 8;
        buf[pos..pos + 4].copy_from_slice(&(sorted_balances.len() as u32).to_le_bytes());
        pos += 4;
        for bal in sorted_balances {
            let bytes = bal.asset.as_bytes();
            let blen = bytes.len();
            buf[pos..pos + 2].copy_from_slice(&(blen as u16).to_le_bytes());
            pos += 2;
            buf[pos..pos + blen].copy_from_slice(bytes);
            pos += blen;
            buf[pos..pos + 16].copy_from_slice(&bal.available_fp.to_le_bytes());
            pos += 16;
            buf[pos..pos + 16].copy_from_slice(&bal.locked_fp.to_le_bytes());
            pos += 16;
            let total = bal.available_fp.saturating_add(bal.locked_fp);
            buf[pos..pos + 16].copy_from_slice(&total.to_le_bytes());
            pos += 16;
        }
        return *blake3::keyed_hash(&KEY_ACCOUNT_LEAF, &buf[..pos]).as_bytes();
    }

    let mut hasher = blake3::Hasher::new_keyed(&KEY_ACCOUNT_LEAF);
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

/// Canonical deterministic Blake3 leaf hash for an immutable trade execution fill (`109` fixed bytes).
#[inline]
pub fn hash_execution_fill_leaf(fill: &ExecutionFillDigestInput) -> [u8; 32] {
    let mut buf = [0u8; 109];
    buf[0..8].copy_from_slice(&fill.fill_id.to_le_bytes());
    buf[8..16].copy_from_slice(&fill.engine_seq.to_le_bytes());
    buf[16..20].copy_from_slice(&fill.instrument_id.to_le_bytes());
    buf[20..28].copy_from_slice(&fill.maker_order_id.to_le_bytes());
    buf[28..36].copy_from_slice(&fill.taker_order_id.to_le_bytes());
    buf[36..44].copy_from_slice(&fill.maker_account_id.to_le_bytes());
    buf[44..52].copy_from_slice(&fill.taker_account_id.to_le_bytes());
    buf[52] = fill.aggressor_side;
    buf[53..61].copy_from_slice(&fill.price_fp.to_le_bytes());
    buf[61..69].copy_from_slice(&fill.qty_fp.to_le_bytes());
    buf[69..85].copy_from_slice(&fill.notional_fp.to_le_bytes());
    buf[85..93].copy_from_slice(&fill.maker_fee_fp.to_le_bytes());
    buf[93..101].copy_from_slice(&fill.taker_fee_fp.to_le_bytes());
    buf[101..109].copy_from_slice(&fill.exec_ts_ns.to_le_bytes());
    *blake3::keyed_hash(&KEY_EXEC_FILL_LEAF, &buf).as_bytes()
}

/// Bellare-Micciancio 256-bit Incremental Lattice/Multiset Accumulator (`LtHash`) for resting LOB orders.
/// Order-independent over internal slab indices while committing exact `(order_id, priority_seq)` FIFO state
/// in O(1) (`~150 ns` single-block insert, `~2 ns` cancel via cached lanes).
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

    /// Single-block 48-byte BLAKE3 keyed digest expanded into 4 × u64 additive lattice lanes.
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
        let mut buf = [0u8; 48];
        buf[0..4].copy_from_slice(&instrument_id.to_le_bytes());
        buf[4] = side;
        buf[8..16].copy_from_slice(&order_id.to_le_bytes());
        buf[16..24].copy_from_slice(&account_id.to_le_bytes());
        buf[24..32].copy_from_slice(&price_fp.to_le_bytes());
        buf[32..40].copy_from_slice(&remaining_qty_fp.to_le_bytes());
        buf[40..48].copy_from_slice(&priority_seq.to_le_bytes());
        let digest = blake3::keyed_hash(&KEY_LOB_ORDER, &buf);
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

    /// Single-block 32-byte BLAKE3 keyed commitment of the 256-bit LOB accumulator.
    #[inline(always)]
    pub fn commitment_hash(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[0..8].copy_from_slice(&self.lanes[0].to_le_bytes());
        buf[8..16].copy_from_slice(&self.lanes[1].to_le_bytes());
        buf[16..24].copy_from_slice(&self.lanes[2].to_le_bytes());
        buf[24..32].copy_from_slice(&self.lanes[3].to_le_bytes());
        *blake3::keyed_hash(&KEY_LOB_ROOT, &buf).as_bytes()
    }
}

#[inline(always)]
fn account_bucket_index(account_id: u64) -> usize {
    // SplitMix64 finalizer distributes sequential or sparse account IDs uniformly across 64 buckets.
    let mut z = account_id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z = z ^ (z >> 31);
    (z as usize) & (ACCOUNT_BUCKET_COUNT - 1)
}

/// Builds a canonical balanced binary Merkle subtree over a slice of leaves (used for multi-entry buckets
/// and execution peak proofs).
fn build_local_subtree_root_and_proof(
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

/// Zero-allocation inline bucket for 1..=4 sorted account leaves, overflowing gracefully to `overflow`
/// only if 5+ distinct account IDs hash into the same 6-bit radix bucket.
#[derive(Clone, Default)]
struct AccountBucket {
    inline: [(u64, [u8; 32]); 4],
    inline_len: u8,
    overflow: Option<Vec<(u64, [u8; 32])>>,
}

impl AccountBucket {
    #[inline(always)]
    fn clear(&mut self) {
        self.inline_len = 0;
        if let Some(v) = &mut self.overflow {
            v.clear();
        }
    }

    #[inline(always)]
    fn as_slice(&self) -> &[(u64, [u8; 32])] {
        if let Some(v) = &self.overflow {
            v.as_slice()
        } else {
            &self.inline[..self.inline_len as usize]
        }
    }

    #[inline]
    fn upsert(&mut self, account_id: u64, leaf_hash: [u8; 32]) {
        if let Some(v) = &mut self.overflow {
            match v.binary_search_by_key(&account_id, |&(id, _)| id) {
                Ok(pos) => v[pos].1 = leaf_hash,
                Err(pos) => v.insert(pos, (account_id, leaf_hash)),
            }
            return;
        }
        let n = self.inline_len as usize;
        for i in 0..n {
            if self.inline[i].0 == account_id {
                self.inline[i].1 = leaf_hash;
                return;
            }
        }
        if n < 4 {
            let mut pos = n;
            while pos > 0 && self.inline[pos - 1].0 > account_id {
                self.inline[pos] = self.inline[pos - 1];
                pos -= 1;
            }
            self.inline[pos] = (account_id, leaf_hash);
            self.inline_len = (n + 1) as u8;
        } else {
            let mut v = Vec::with_capacity(8);
            v.extend_from_slice(&self.inline);
            match v.binary_search_by_key(&account_id, |&(id, _)| id) {
                Ok(pos) => v[pos].1 = leaf_hash,
                Err(pos) => v.insert(pos, (account_id, leaf_hash)),
            }
            self.inline_len = 0;
            self.overflow = Some(v);
        }
    }
}

/// Unified Cryptographic State Merkle Engine combining:
/// 1. L1d-cache-resident Depth-6 Account Balance Merkle Tree (`64` radix buckets = `4 KB` tree)
/// 2. O(1) Incremental `LtHash` Limit Order Book commitment
/// 3. O(1)-Amortized Left-to-Right Bagged Merkle Mountain Range (`MMR`) Execution History Log
///
/// Unified State Root:
/// `state_root_blake3 = H(accounts_merkle_root || H(lob_commitment || executions_mmr_root))`
#[derive(Clone)]
pub struct IncrementalMerkleState {
    empty_leaf_hash: [u8; 32],
    /// Flat 1-indexed binary tree of length `2 * ACCOUNT_BUCKET_COUNT` (`128 * 32 = 4 KB`, L1d resident).
    /// Leaves live at `[ACCOUNT_BUCKET_COUNT .. 2 * ACCOUNT_BUCKET_COUNT]`; root lives at `[1]`.
    account_tree: Box<[[u8; 32]]>,
    /// Pre-allocated inline buckets (0 heap allocations on upsert for up to 4 colliding accounts/bucket).
    buckets: Box<[AccountBucket]>,
    /// Dirty bucket list coalesced per engine command (`0` heap allocs via single u64 bitmask!).
    dirty_buckets: [u8; ACCOUNT_BUCKET_COUNT],
    dirty_len: usize,
    dirty_mask: u64,
    /// Pre-allocated fixed buffer of execution fill leaves (0 reallocs during unbounded soak!).
    exec_leaves: Box<[[u8; 32]]>,
    exec_retained_len: usize,
    exec_total_count: usize,
    /// Complete power-of-two MMR peaks at height `h`.
    exec_frontier: [Option<[u8; 32]>; 32],
    /// Cached left-to-right bagged hash of all MMR peaks strictly higher than height `h`.
    /// Enables exact O(1) amortized root updates (average 2.0 Blake3 hashes per fill even at 1B fills!).
    exec_left_bag: [Option<[u8; 32]>; 32],
    cached_exec_root: [u8; 32],
    exec_root_changed: bool,
    cached_lob_lanes: [u64; 4],
    cached_lob_hash: [u8; 32],
    cached_right_branch: [u8; 32],
    cached_state_root: [u8; 32],
}

impl IncrementalMerkleState {
    pub fn new() -> Self {
        let empty_leaf_hash = *blake3::hash(DOMAIN_EMPTY_LEAF).as_bytes();
        let mut account_tree = vec![[0u8; 32]; ACCOUNT_BUCKET_COUNT * 2].into_boxed_slice();
        for slot in &mut account_tree[ACCOUNT_BUCKET_COUNT..ACCOUNT_BUCKET_COUNT * 2] {
            *slot = empty_leaf_hash;
        }
        for idx in (1..ACCOUNT_BUCKET_COUNT).rev() {
            account_tree[idx] =
                hash_merkle_branch(&account_tree[idx * 2], &account_tree[idx * 2 + 1]);
        }
        let buckets = vec![AccountBucket::default(); ACCOUNT_BUCKET_COUNT].into_boxed_slice();
        let lob_empty_acc = LobStateAccumulator::new();
        let cached_lob_lanes = lob_empty_acc.lanes();
        let cached_lob_hash = lob_empty_acc.commitment_hash();
        let cached_right_branch = hash_merkle_branch(&cached_lob_hash, &empty_leaf_hash);
        let cached_state_root = hash_merkle_branch(&account_tree[1], &cached_right_branch);

        Self {
            empty_leaf_hash,
            account_tree,
            buckets,
            dirty_buckets: [0u8; ACCOUNT_BUCKET_COUNT],
            dirty_len: 0,
            dirty_mask: 0,
            exec_leaves: vec![[0u8; 32]; MAX_IN_MEMORY_EXEC_LEAVES].into_boxed_slice(),
            exec_retained_len: 0,
            exec_total_count: 0,
            exec_frontier: [None; 32],
            exec_left_bag: [None; 32],
            cached_exec_root: empty_leaf_hash,
            exec_root_changed: false,
            cached_lob_lanes,
            cached_lob_hash,
            cached_right_branch,
            cached_state_root,
        }
    }

    pub fn clear_and_reset(&mut self) {
        for slot in &mut self.account_tree[ACCOUNT_BUCKET_COUNT..ACCOUNT_BUCKET_COUNT * 2] {
            *slot = self.empty_leaf_hash;
        }
        for idx in (1..ACCOUNT_BUCKET_COUNT).rev() {
            self.account_tree[idx] =
                hash_merkle_branch(&self.account_tree[idx * 2], &self.account_tree[idx * 2 + 1]);
        }
        for b in self.buckets.iter_mut() {
            b.clear();
        }
        self.dirty_len = 0;
        self.dirty_mask = 0;
        self.exec_retained_len = 0;
        self.exec_total_count = 0;
        self.exec_frontier = [None; 32];
        self.exec_left_bag = [None; 32];
        self.cached_exec_root = self.empty_leaf_hash;
        self.exec_root_changed = false;
        let lob_empty_acc = LobStateAccumulator::new();
        self.cached_lob_lanes = lob_empty_acc.lanes();
        self.cached_lob_hash = lob_empty_acc.commitment_hash();
        self.cached_right_branch = hash_merkle_branch(&self.cached_lob_hash, &self.empty_leaf_hash);
        self.cached_state_root =
            hash_merkle_branch(&self.account_tree[1], &self.cached_right_branch);
    }

    /// Marks an account leaf updated without recomputing parent hashes yet (coalesced per command).
    #[inline(always)]
    pub fn upsert_account_leaf(&mut self, account_id: u64, leaf_hash: [u8; 32]) {
        let b_idx = account_bucket_index(account_id);
        self.buckets[b_idx].upsert(account_id, leaf_hash);
        let bit = 1u64 << b_idx;
        if (self.dirty_mask & bit) == 0 {
            self.dirty_mask |= bit;
            self.dirty_buckets[self.dirty_len] = b_idx as u8;
            self.dirty_len += 1;
        }
    }

    /// Appends an execution fill leaf to the O(1)-amortized Left-to-Right Bagged MMR (`0` heap allocs!).
    /// Average cryptographic cost across unbounded fills is strictly `2.0` single-block Blake3 hashes!
    #[inline]
    pub fn append_execution_leaf(&mut self, fill_leaf_hash: [u8; 32]) {
        if self.exec_retained_len < self.exec_leaves.len() {
            self.exec_leaves[self.exec_retained_len] = fill_leaf_hash;
            self.exec_retained_len += 1;
        }
        self.exec_total_count += 1;
        let mut carry = fill_leaf_hash;
        let mut landed_h = 0usize;
        for h in 0..32 {
            match self.exec_frontier[h].take() {
                None => {
                    self.exec_frontier[h] = Some(carry);
                    landed_h = h;
                    break;
                }
                Some(left) => {
                    carry = hash_merkle_branch(&left, &carry);
                }
            }
        }
        let new_root = match self.exec_left_bag[landed_h] {
            Some(left_bag) => hash_merkle_branch(&left_bag, &carry),
            None => carry,
        };
        for slot in &mut self.exec_left_bag[..landed_h] {
            *slot = Some(new_root);
        }
        self.cached_exec_root = new_root;
        self.exec_root_changed = true;
    }

    /// Flushes all coalesced dirty account buckets using deduplicated level-by-level bottom-up hashing
    /// and computes the unified 32-byte Blake3 state root (`0` heap allocs!).
    #[inline]
    pub fn commit_state_root(&mut self, lob_acc: &LobStateAccumulator) -> [u8; 32] {
        let accounts_dirty = self.dirty_len > 0;
        let lob_lanes = lob_acc.lanes();
        let lob_dirty = lob_lanes != self.cached_lob_lanes;
        let exec_dirty = self.exec_root_changed;

        if !accounts_dirty && !lob_dirty && !exec_dirty {
            return self.cached_state_root;
        }

        if accounts_dirty {
            if self.dirty_len == 1 {
                // Ultra-fast single-account path: 6 single-block Blake3 hashes in L1d cache!
                let b_idx = self.dirty_buckets[0] as usize;
                self.dirty_len = 0;
                self.dirty_mask = 0;
                let leaf_slot = ACCOUNT_BUCKET_COUNT + b_idx;
                self.account_tree[leaf_slot] = self.compute_bucket_leaf(b_idx);
                let mut node = leaf_slot >> 1;
                while node >= 1 {
                    self.account_tree[node] = hash_merkle_branch(
                        &self.account_tree[node * 2],
                        &self.account_tree[node * 2 + 1],
                    );
                    node >>= 1;
                }
            } else {
                // Multi-account deduplicated bottom-up sweep via single u64 bitmask (zero heap allocs!)
                let mut cur_nodes = [0u8; ACCOUNT_BUCKET_COUNT];
                let mut next_nodes = [0u8; ACCOUNT_BUCKET_COUNT];
                let mut cur_len = 0usize;
                let mut seen_mask = 0u64;

                for i in 0..self.dirty_len {
                    let b_idx = self.dirty_buckets[i] as usize;
                    let leaf_slot = ACCOUNT_BUCKET_COUNT + b_idx;
                    self.account_tree[leaf_slot] = self.compute_bucket_leaf(b_idx);
                    let parent = leaf_slot >> 1; // 32..63
                    let bit = 1u64 << parent;
                    if (seen_mask & bit) == 0 {
                        seen_mask |= bit;
                        cur_nodes[cur_len] = parent as u8;
                        cur_len += 1;
                    }
                }
                self.dirty_len = 0;
                self.dirty_mask = 0;

                for _ in 0..ACCOUNT_BUCKET_BITS {
                    let mut next_len = 0usize;
                    let mut next_mask = 0u64;
                    for &node_u8 in &cur_nodes[..cur_len] {
                        let node = node_u8 as usize;
                        self.account_tree[node] = hash_merkle_branch(
                            &self.account_tree[node * 2],
                            &self.account_tree[node * 2 + 1],
                        );
                        if node > 1 {
                            let parent = node >> 1; // 1..31
                            let bit = 1u64 << parent;
                            if (next_mask & bit) == 0 {
                                next_mask |= bit;
                                next_nodes[next_len] = parent as u8;
                                next_len += 1;
                            }
                        }
                    }
                    if next_len == 0 {
                        break;
                    }
                    cur_nodes[..next_len].copy_from_slice(&next_nodes[..next_len]);
                    cur_len = next_len;
                }
            }
        }

        if lob_dirty {
            self.cached_lob_lanes = lob_lanes;
            self.cached_lob_hash = lob_acc.commitment_hash();
        }
        if lob_dirty || exec_dirty {
            self.exec_root_changed = false;
            self.cached_right_branch =
                hash_merkle_branch(&self.cached_lob_hash, &self.cached_exec_root);
        }

        self.cached_state_root =
            hash_merkle_branch(&self.account_tree[1], &self.cached_right_branch);
        self.cached_state_root
    }

    #[inline(always)]
    fn compute_bucket_leaf(&self, b_idx: usize) -> [u8; 32] {
        let entries = self.buckets[b_idx].as_slice();
        match entries.len() {
            0 => self.empty_leaf_hash,
            1 => entries[0].1,
            2 => hash_merkle_branch(&entries[0].1, &entries[1].1),
            3 => {
                let left = hash_merkle_branch(&entries[0].1, &entries[1].1);
                let right = hash_merkle_branch(&entries[2].1, &self.empty_leaf_hash);
                hash_merkle_branch(&left, &right)
            }
            4 => {
                let left = hash_merkle_branch(&entries[0].1, &entries[1].1);
                let right = hash_merkle_branch(&entries[2].1, &entries[3].1);
                hash_merkle_branch(&left, &right)
            }
            _ => {
                let hashes: Vec<[u8; 32]> = entries.iter().map(|&(_, h)| h).collect();
                build_local_subtree_root_and_proof(&hashes, None, &self.empty_leaf_hash).0
            }
        }
    }

    #[inline(always)]
    pub const fn state_root(&self) -> [u8; 32] {
        self.cached_state_root
    }

    #[inline(always)]
    pub fn execution_leaves(&self) -> &[[u8; 32]] {
        &self.exec_leaves[..self.exec_retained_len]
    }

    /// Generates a cryptographic binary Merkle inclusion proof for `account_id` terminating at `state_root_blake3`.
    pub fn generate_account_proof(
        &mut self,
        account_id: u64,
        lob_acc: &LobStateAccumulator,
    ) -> ([u8; 32], Vec<ProofStep>, [u8; 32]) {
        let state_root = self.commit_state_root(lob_acc);
        let b_idx = account_bucket_index(account_id);
        let entries = self.buckets[b_idx].as_slice();

        let (leaf_hash, mut proof) = if entries.is_empty() {
            (
                self.empty_leaf_hash,
                Vec::with_capacity(ACCOUNT_BUCKET_BITS + 2),
            )
        } else if entries.len() == 1 && entries[0].0 == account_id {
            (entries[0].1, Vec::with_capacity(ACCOUNT_BUCKET_BITS + 1))
        } else {
            match entries.binary_search_by_key(&account_id, |&(id, _)| id) {
                Ok(pos) => {
                    let hashes: Vec<[u8; 32]> = entries.iter().map(|&(_, h)| h).collect();
                    let (_, sub_proof) = build_local_subtree_root_and_proof(
                        &hashes,
                        Some(pos),
                        &self.empty_leaf_hash,
                    );
                    (entries[pos].1, sub_proof)
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

        proof.push(ProofStep {
            sibling_hash: self.cached_right_branch,
            is_left_sibling: false,
            depth: base_depth + ACCOUNT_BUCKET_BITS as u32,
        });

        (leaf_hash, proof, state_root)
    }

    /// Generates a cryptographic binary Merkle inclusion proof for execution `fill_id` (1-indexed)
    /// against the Left-to-Right Bagged MMR, terminating at the exact same `state_root_blake3`.
    pub fn generate_execution_proof(
        &mut self,
        fill_id: u64,
        engine_seq: u64,
        lob_acc: &LobStateAccumulator,
    ) -> Option<ExecutionInclusionProof> {
        if fill_id == 0
            || self.exec_total_count > self.exec_retained_len
            || (fill_id as usize) > self.exec_retained_len
        {
            return None;
        }
        let state_root = self.commit_state_root(lob_acc);
        let target_idx = (fill_id - 1) as usize;
        let total = self.exec_retained_len;
        let leaves_slice = &self.exec_leaves[..total];
        let leaf_hash = leaves_slice[target_idx];

        // Decompose `0..total` into exact power-of-two MMR peaks (largest -> smallest)
        let mut peaks: Vec<(usize, usize, [u8; 32])> = Vec::with_capacity(32);
        let mut offset = 0usize;
        for h in (0..32).rev() {
            let sz = 1usize << h;
            if (total & sz) != 0 {
                let peak_root = self.exec_frontier[h]?;
                peaks.push((offset, sz, peak_root));
                offset += sz;
            }
        }

        let peak_pos = peaks
            .iter()
            .position(|&(start, sz, _)| target_idx >= start && target_idx < start + sz)?;
        let (p_start, p_sz, _) = peaks[peak_pos];

        let (_, mut proof) = build_local_subtree_root_and_proof(
            &leaves_slice[p_start..p_start + p_sz],
            Some(target_idx - p_start),
            &self.empty_leaf_hash,
        );

        // If peaks exist strictly to the left of `peak_pos`, push their left-to-right bagged hash as LEFT sibling
        if peak_pos > 0 {
            let mut left_bag = peaks[0].2;
            for &(_, _, r) in &peaks[1..peak_pos] {
                left_bag = hash_merkle_branch(&left_bag, &r);
            }
            proof.push(ProofStep {
                sibling_hash: left_bag,
                is_left_sibling: true,
                depth: proof.len() as u32,
            });
        }

        // For each smaller peak strictly to the right of `peak_pos`, push as RIGHT sibling
        for &(_, _, r) in &peaks[peak_pos + 1..] {
            proof.push(ProofStep {
                sibling_hash: r,
                is_left_sibling: false,
                depth: proof.len() as u32,
            });
        }

        proof.push(ProofStep {
            sibling_hash: self.cached_lob_hash,
            is_left_sibling: true,
            depth: proof.len() as u32,
        });
        proof.push(ProofStep {
            sibling_hash: self.account_tree[1],
            is_left_sibling: true,
            depth: proof.len() as u32,
        });

        Some(ExecutionInclusionProof {
            fill_id,
            engine_seq,
            leaf_blake3_hash: leaf_hash,
            state_root_blake3: state_root,
            proof_path: proof,
        })
    }
}

impl Default for IncrementalMerkleState {
    fn default() -> Self {
        Self::new()
    }
}
