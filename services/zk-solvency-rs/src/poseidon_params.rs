use std::sync::OnceLock;

use ark_bn254::Fr;
use ark_crypto_primitives::sponge::{
    poseidon::{find_poseidon_ark_and_mds, PoseidonConfig, PoseidonSponge},
    CryptographicSponge, FieldBasedCryptographicSponge,
};
use ark_ff::{Field, PrimeField};
use ark_serialize::CanonicalDeserialize;

use crate::error::{SolvencyError, SolvencyResult};

/// Poseidon sponge geometry over BN254 scalar field `Fr` (254-bit prime):
/// - `RATE = 5`, `CAPACITY = 1` -> `WIDTH = 6`
/// - `FULL_ROUNDS = 8` (4 initial + 4 terminal), `PARTIAL_ROUNDS = 57`, `ALPHA = 5`
///
/// Every 4-element leaf hash and 5-element internal/root hash executes in
/// **exactly 1 Poseidon permutation** (zero multi-block sponge overhead).
pub const POSEIDON_RATE: usize = 5;
pub const POSEIDON_CAPACITY: usize = 1;
pub const POSEIDON_WIDTH: usize = POSEIDON_RATE + POSEIDON_CAPACITY;
pub const POSEIDON_FULL_ROUNDS: usize = 8;
pub const POSEIDON_PARTIAL_ROUNDS: usize = 57;
pub const POSEIDON_TOTAL_ROUNDS: usize = POSEIDON_FULL_ROUNDS + POSEIDON_PARTIAL_ROUNDS;
pub const POSEIDON_ALPHA: u64 = 5;

/// Canonical domain-separation tags (ASCII-encoded integers) preventing cross-context
/// second-preimage or leaf/internal node confusion attacks.
pub const DOMAIN_MSST_LEAF: u64 = 0x4859_505F_4C45_4146; // "HYP_LEAF"
pub const DOMAIN_MSST_NODE: u64 = 0x4859_505F_4E4F_4445; // "HYP_NODE"
pub const DOMAIN_MSST_ROOT: u64 = 0x4859_505F_524F_4F54; // "HYP_ROOT"
pub const DOMAIN_USER_COMMIT: u64 = 0x4859_505F_5553_4552; // "HYP_USER"

const BLAKE3_LEAF_CONTEXT: &[u8; 8] = b"HYPMSSTL";
const BLAKE3_NODE_CONTEXT: &[u8; 8] = b"HYPMSSTN";
const BLAKE3_SALT_CONTEXT: &str = "HYPERION_OMEGA_CH13_MSST_LEAF_BLINDING_V1";

/// Cache-line aligned, zero-allocation unrolled Poseidon parameter table using
/// Grassi et al. (ePrint 2019/458 Appendix B) Sparse MDS Factorization + Forward Constant
/// Compression for the 57 partial rounds.
///
/// Reduces field multiplications per partial round from `t^2 = 36` to `2t - 1 = 11` (`-69.4%`)
/// while remaining **100% bit-for-bit algebraically identical** to `PoseidonSponge` and
/// `PoseidonSpongeVar` in R1CS.
#[repr(C, align(64))]
#[derive(Clone, Debug)]
pub struct FastPoseidon6 {
    pub ark_init: [[Fr; POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2],
    pub ark_final: [[Fr; POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2],
    /// Compressed scalar round constants added solely to lane 0 in each of the 57 partial rounds.
    pub partial_c0: [Fr; POSEIDON_PARTIAL_ROUNDS],
    /// Standard dense 6x6 Cauchy MDS matrix for full rounds.
    pub mds: [[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
    /// Pre-folded dense 6x6 matrix applied at the end of the last initial full round (`r = 3`).
    pub pre_sparse_mds: [[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
    /// Sparse partial-round first row `w_i[0..6]` (`6` muls).
    pub sparse_w: [[Fr; POSEIDON_WIDTH]; POSEIDON_PARTIAL_ROUNDS],
    /// Sparse partial-round first column tail `v_i[0..5]` (`5` muls).
    pub sparse_v: [[Fr; POSEIDON_WIDTH - 1]; POSEIDON_PARTIAL_ROUNDS],
}

static POSEIDON_CONFIG: OnceLock<PoseidonConfig<Fr>> = OnceLock::new();
static FAST_POSEIDON: OnceLock<FastPoseidon6> = OnceLock::new();

fn mat_mul_6x6(
    a: &[[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
    b: &[[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
) -> [[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH] {
    let mut out = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_WIDTH];
    for i in 0..POSEIDON_WIDTH {
        for j in 0..POSEIDON_WIDTH {
            let mut sum = Fr::from(0u64);
            for k in 0..POSEIDON_WIDTH {
                sum += a[i][k] * b[k][j];
            }
            out[i][j] = sum;
        }
    }
    out
}

fn invert_5x5(mat: &[[Fr; 5]; 5]) -> [[Fr; 5]; 5] {
    let mut aug = [[Fr::from(0u64); 10]; 5];
    for i in 0..5 {
        for j in 0..5 {
            aug[i][j] = mat[i][j];
        }
        aug[i][5 + i] = Fr::from(1u64);
    }
    for col in 0..5 {
        let mut pivot = col;
        while pivot < 5 && aug[pivot][col] == Fr::from(0u64) {
            pivot += 1;
        }
        aug.swap(col, pivot);
        let inv_diag = aug[col][col].inverse().unwrap_or(Fr::from(1u64));
        for entry in aug[col].iter_mut().skip(col) {
            *entry *= inv_diag;
        }
        for row in 0..5 {
            if row != col {
                let factor = aug[row][col];
                if factor != Fr::from(0u64) {
                    let pivot_row = aug[col];
                    for c in col..10 {
                        aug[row][c] -= factor * pivot_row[c];
                    }
                }
            }
        }
    }
    let mut inv = [[Fr::from(0u64); 5]; 5];
    for i in 0..5 {
        inv[i].copy_from_slice(&aug[i][5..10]);
    }
    inv
}

/// Returns canonical BN254 Poseidon configuration generated via IAIK Grain LFSR.
pub fn poseidon_config() -> &'static PoseidonConfig<Fr> {
    POSEIDON_CONFIG.get_or_init(|| {
        let (ark, mds) = find_poseidon_ark_and_mds::<Fr>(
            Fr::MODULUS_BIT_SIZE as u64,
            POSEIDON_RATE,
            POSEIDON_FULL_ROUNDS as u64,
            POSEIDON_PARTIAL_ROUNDS as u64,
            0,
        );
        PoseidonConfig::new(
            POSEIDON_FULL_ROUNDS,
            POSEIDON_PARTIAL_ROUNDS,
            POSEIDON_ALPHA,
            mds,
            ark,
            POSEIDON_RATE,
            POSEIDON_CAPACITY,
        )
    })
}

/// Returns cache-line aligned sparse-factored Poseidon tables algebraically identical to `poseidon_config()`.
pub fn fast_poseidon() -> &'static FastPoseidon6 {
    FAST_POSEIDON.get_or_init(|| {
        let cfg = poseidon_config();
        let half_full = POSEIDON_FULL_ROUNDS / 2;

        let mut work_ark = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_TOTAL_ROUNDS];
        for (r, row) in cfg.ark.iter().enumerate() {
            for (c, val) in row.iter().enumerate() {
                work_ark[r][c] = *val;
            }
        }
        let mut mds = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_WIDTH];
        for (r, row) in cfg.mds.iter().enumerate() {
            for (c, val) in row.iter().enumerate() {
                mds[r][c] = *val;
            }
        }

        // Step 1: Forward constant compression across all 57 partial rounds.
        // Because S-box only touches lane 0, tail constants `work_ark[r][1..6]` commute past S-box
        // and push forward through dense `mds` into `work_ark[r + 1]`.
        let mut partial_c0 = [Fr::from(0u64); POSEIDON_PARTIAL_ROUNDS];
        for (p, c0_slot) in partial_c0.iter_mut().enumerate() {
            let r = half_full + p;
            *c0_slot = work_ark[r][0];
            let tail = [
                Fr::from(0u64),
                work_ark[r][1],
                work_ark[r][2],
                work_ark[r][3],
                work_ark[r][4],
                work_ark[r][5],
            ];
            for (row_idx, m_row) in mds.iter().enumerate() {
                let pushed = m_row[1] * tail[1]
                    + m_row[2] * tail[2]
                    + m_row[3] * tail[3]
                    + m_row[4] * tail[4]
                    + m_row[5] * tail[5];
                work_ark[r + 1][row_idx] += pushed;
            }
        }

        // Step 2: Backward sparse matrix factorization `A = S_p * block_diag(1, A_hat)`
        // pushing `block_diag(1, A_hat)` backward past `Sbox_0` and `partial_c0`!
        let mut sparse_w = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_PARTIAL_ROUNDS];
        let mut sparse_v = [[Fr::from(0u64); POSEIDON_WIDTH - 1]; POSEIDON_PARTIAL_ROUNDS];
        let mut cur_mat = mds;

        for p in (0..POSEIDON_PARTIAL_ROUNDS).rev() {
            let mut a_hat = [[Fr::from(0u64); 5]; 5];
            for i in 0..5 {
                a_hat[i].copy_from_slice(&cur_mat[i + 1][1..6]);
            }
            let a_hat_inv = invert_5x5(&a_hat);

            // Row 0 of S_p: `[cur_mat[0][0], cur_mat[0][1..6] * a_hat_inv]`
            sparse_w[p][0] = cur_mat[0][0];
            for j in 0..5 {
                let mut acc = Fr::from(0u64);
                for k in 0..5 {
                    acc += cur_mat[0][k + 1] * a_hat_inv[k][j];
                }
                sparse_w[p][j + 1] = acc;
            }
            // Col 0 tail of S_p: `cur_mat[1..6][0]`
            for i in 0..5 {
                sparse_v[p][i] = cur_mat[i + 1][0];
            }

            // Form `M'' = block_diag(1, a_hat)` and absorb into previous round: `cur_mat = M'' * mds`
            let mut m_tail = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_WIDTH];
            m_tail[0][0] = Fr::from(1u64);
            for i in 0..5 {
                m_tail[i + 1][1..6].copy_from_slice(&a_hat[i]);
            }
            cur_mat = mat_mul_6x6(&m_tail, &mds);
        }

        let mut ark_init = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2];
        ark_init.copy_from_slice(&work_ark[0..half_full]);
        let mut ark_final = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2];
        ark_final.copy_from_slice(&work_ark[(half_full + POSEIDON_PARTIAL_ROUNDS)..POSEIDON_TOTAL_ROUNDS]);

        FastPoseidon6 {
            ark_init,
            ark_final,
            partial_c0,
            mds,
            pre_sparse_mds: cur_mat,
            sparse_w,
            sparse_v,
        }
    })
}

#[inline(always)]
fn sbox_alpha5(x: Fr) -> Fr {
    let x2 = x.square();
    let x4 = x2.square();
    x4 * x
}

impl FastPoseidon6 {
    #[inline]
    pub fn permute(&self, state: &mut [Fr; POSEIDON_WIDTH]) {
        // Initial 3 full rounds with standard `mds`
        for rk in &self.ark_init[..3] {
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);

            for (st_elem, m) in state.iter_mut().zip(&self.mds) {
                *st_elem =
                    s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }

        // 4th initial full round with pre-folded `pre_sparse_mds`
        {
            let rk = &self.ark_init[3];
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);

            for (st_elem, m) in state.iter_mut().zip(&self.pre_sparse_mds) {
                *st_elem =
                    s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }

        // Middle 57 partial rounds using sparse factored matrix (11 muls/round instead of 36!)
        for p in 0..POSEIDON_PARTIAL_ROUNDS {
            let s0 = sbox_alpha5(state[0] + self.partial_c0[p]);
            let w = &self.sparse_w[p];
            let v = &self.sparse_v[p];

            let next0 = s0 * w[0]
                + state[1] * w[1]
                + state[2] * w[2]
                + state[3] * w[3]
                + state[4] * w[4]
                + state[5] * w[5];
            state[1] += s0 * v[0];
            state[2] += s0 * v[1];
            state[3] += s0 * v[2];
            state[4] += s0 * v[3];
            state[5] += s0 * v[4];
            state[0] = next0;
        }

        // Terminal 4 full rounds (round 0 uses forward-compressed `ark_final[0]`)
        for rk in &self.ark_final {
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);

            for (st_elem, m) in state.iter_mut().zip(&self.mds) {
                *st_elem =
                    s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }
    }

    /// Zero-allocation sponge hash for 4 field elements (matches `PoseidonSponge` bit-for-bit).
    #[inline]
    pub fn hash_4(&self, inputs: [Fr; 4]) -> Fr {
        // Capacity lane is state[0] = 0; rate lanes start at state[1..6].
        let mut state = [
            Fr::from(0u64),
            inputs[0],
            inputs[1],
            inputs[2],
            inputs[3],
            Fr::from(0u64),
        ];
        self.permute(&mut state);
        state[POSEIDON_CAPACITY]
    }

    /// Zero-allocation sponge hash for 5 field elements (matches `PoseidonSponge` bit-for-bit).
    #[inline]
    pub fn hash_5(&self, inputs: [Fr; 5]) -> Fr {
        let mut state = [
            Fr::from(0u64),
            inputs[0],
            inputs[1],
            inputs[2],
            inputs[3],
            inputs[4],
        ];
        self.permute(&mut state);
        state[POSEIDON_CAPACITY]
    }
}

/// Baseline reference Poseidon hash using `ark_crypto_primitives::sponge::poseidon::PoseidonSponge`.
/// Retained for Round-1 baseline benchmarking and bit-for-bit equivalence verification.
pub fn baseline_poseidon_hash(inputs: &[Fr]) -> Fr {
    let mut sponge = PoseidonSponge::<Fr>::new(poseidon_config());
    sponge.absorb(&inputs);
    sponge.squeeze_native_field_elements(1)[0]
}

/// Canonical Poseidon MSST leaf hash: `H(DOMAIN_MSST_LEAF, account_id, balance_fp, salt)`.
#[inline]
pub fn poseidon_leaf_hash(account_id: u64, balance_fp: u64, salt: Fr) -> Fr {
    fast_poseidon().hash_4([
        Fr::from(DOMAIN_MSST_LEAF),
        Fr::from(account_id),
        Fr::from(balance_fp),
        salt,
    ])
}

/// Raw field-element leaf hash (used by circuits & adversarial witness tests).
#[inline]
pub fn poseidon_leaf_hash_fr(account_id: Fr, balance_fp: Fr, salt: Fr) -> Fr {
    fast_poseidon().hash_4([Fr::from(DOMAIN_MSST_LEAF), account_id, balance_fp, salt])
}

/// Canonical Poseidon MSST internal node hash:
/// `H(DOMAIN_MSST_NODE, left_hash, left_sum, right_hash, right_sum)`.
#[inline]
pub fn poseidon_node_hash(
    left_hash: Fr,
    left_sum: Fr,
    right_hash: Fr,
    right_sum: Fr,
) -> Fr {
    fast_poseidon().hash_5([
        Fr::from(DOMAIN_MSST_NODE),
        left_hash,
        left_sum,
        right_hash,
        right_sum,
    ])
}

/// Public Root Commitment hiding the exact aggregate liability sum while binding `(root_hash, root_sum, epoch_id)`:
/// `H(DOMAIN_MSST_ROOT, root_hash, root_sum, epoch_id, epoch_blinding)`.
#[inline]
pub fn poseidon_root_commitment(
    root_hash: Fr,
    root_sum: u128,
    epoch_id: u64,
    epoch_blinding: Fr,
) -> Fr {
    fast_poseidon().hash_5([
        Fr::from(DOMAIN_MSST_ROOT),
        root_hash,
        Fr::from(root_sum),
        Fr::from(epoch_id),
        epoch_blinding,
    ])
}

/// User-specific blinded balance commitment allowing a user to verify their own account & balance
/// against the ZK proof without revealing either to external observers:
/// `H(DOMAIN_USER_COMMIT, account_id, balance_fp, user_blinding)`.
#[inline]
pub fn poseidon_user_commitment(account_id: u64, balance_fp: u64, user_blinding: Fr) -> Fr {
    fast_poseidon().hash_4([
        Fr::from(DOMAIN_USER_COMMIT),
        Fr::from(account_id),
        Fr::from(balance_fp),
        user_blinding,
    ])
}

/// Deterministically derives per-account blinding salt from an epoch master secret and `account_id`.
pub fn derive_leaf_salt(epoch_master_seed: &[u8; 32], account_id: u64, epoch_id: u64) -> Fr {
    let mut hasher = blake3::Hasher::new_derive_key(BLAKE3_SALT_CONTEXT);
    hasher.update(epoch_master_seed);
    hasher.update(&epoch_id.to_le_bytes());
    hasher.update(&account_id.to_le_bytes());
    let digest = hasher.finalize();
    Fr::from_le_bytes_mod_order(digest.as_bytes())
}

/// Hybrid symmetric Blake3 digest binding a leaf's algebraic Poseidon hash, `account_id`, and `balance_fp`.
#[inline]
pub fn blake3_leaf_digest(poseidon_hash: Fr, account_id: u64, balance_fp: u64) -> [u8; 32] {
    let mut buf = [0u8; 56];
    buf[0..8].copy_from_slice(BLAKE3_LEAF_CONTEXT);
    let fr_bytes = fr_to_bytes_le(poseidon_hash);
    buf[8..40].copy_from_slice(&fr_bytes);
    buf[40..48].copy_from_slice(&account_id.to_le_bytes());
    buf[48..56].copy_from_slice(&balance_fp.to_le_bytes());
    *blake3::hash(&buf).as_bytes()
}

/// Hybrid symmetric Blake3 digest binding internal node Poseidon hash, left/right Blake3 digests, and child sums.
#[inline]
pub fn blake3_node_digest(
    poseidon_hash: Fr,
    left_blake3: &[u8; 32],
    left_sum: u128,
    right_blake3: &[u8; 32],
    right_sum: u128,
) -> [u8; 32] {
    let mut buf = [0u8; 136];
    buf[0..8].copy_from_slice(BLAKE3_NODE_CONTEXT);
    buf[8..40].copy_from_slice(&fr_to_bytes_le(poseidon_hash));
    buf[40..72].copy_from_slice(left_blake3);
    buf[72..88].copy_from_slice(&left_sum.to_le_bytes());
    buf[88..120].copy_from_slice(right_blake3);
    buf[120..136].copy_from_slice(&right_sum.to_le_bytes());
    *blake3::hash(&buf).as_bytes()
}

/// Serializes a BN254 scalar field element into canonical 32-byte little-endian representation
/// directly from its stack-resident `[u64; 4]` limbs (**0 heap allocations**).
#[inline(always)]
pub fn fr_to_bytes_le(val: Fr) -> [u8; 32] {
    let limbs = val.into_bigint().0;
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&limbs[0].to_le_bytes());
    out[8..16].copy_from_slice(&limbs[1].to_le_bytes());
    out[16..24].copy_from_slice(&limbs[2].to_le_bytes());
    out[24..32].copy_from_slice(&limbs[3].to_le_bytes());
    out
}

/// Deserializes a canonical 32-byte little-endian BN254 scalar field element (rejecting non-canonical `>= r`).
pub fn fr_from_bytes_le(bytes: &[u8; 32]) -> SolvencyResult<Fr> {
    Fr::deserialize_compressed(&bytes[..])
        .map_err(|e| SolvencyError::SerializationError(format!("invalid canonical Fr bytes: {e}")))
}

/// Serializes `Fr` as 32-byte big-endian `uint256` word for Ethereum/Solidity `alt_bn128` EVM contracts.
#[inline]
pub fn fr_to_evm_be_word(val: Fr) -> [u8; 32] {
    let mut out = fr_to_bytes_le(val);
    out.reverse();
    out
}
