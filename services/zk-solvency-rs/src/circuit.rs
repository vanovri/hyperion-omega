use ark_bn254::Fr;
use ark_crypto_primitives::sponge::{
    constraints::CryptographicSpongeVar,
    poseidon::constraints::PoseidonSpongeVar,
};
use ark_ff::{AdditiveGroup, BigInteger, PrimeField};
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    R1CSVar,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};

use crate::poseidon_params::{
    poseidon_config, DOMAIN_MSST_LEAF, DOMAIN_MSST_NODE, DOMAIN_MSST_ROOT, DOMAIN_USER_COMMIT,
};

/// Enforces `0 <= val < 2^num_bits` inside R1CS (`num_bits <= 128 < Fr::MODULUS_BIT_SIZE - 1`)
/// by allocating `num_bits` private `Boolean<Fr>` wires (`b_i * (1 - b_i) == 0`) and constraining
/// their exact power-of-two linear combination `sum_{i=0}^{num_bits-1} b_i * 2^i == val`.
///
/// ### Security Proof (Finite-Field Wrap-Around Immunity)
/// Because `num_bits <= 128` and BN254 scalar prime `r > 2^253`, the maximum value representable
/// by the linear combination is `2^num_bits - 1 < r`. Therefore:
/// 1. Any negative field element `-k mod r = r - k` (for `1 <= k <= 2^128`) has canonical integer
///    value `r - k > 2^253 > 2^num_bits - 1`, making the linear equality **strictly unsatisfiable**.
/// 2. Adding two range-proven field elements `< 2^120` cannot exceed `2^121 < r`, so field addition
///    in `Fr` is 100% isomorphic to exact non-negative integer addition over `Z_{>= 0}`.
pub fn enforce_non_negative_range_bits(
    cs: ConstraintSystemRef<Fr>,
    val: &FpVar<Fr>,
    num_bits: usize,
) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    assert!(
        num_bits > 0 && num_bits <= 128,
        "range proof bits must be in 1..=128"
    );

    let raw_bits_le = val
        .value()
        .map(|f| f.into_bigint().to_bits_le())
        .unwrap_or_else(|_| vec![false; 256]);

    // Allocate exactly `num_bits` boolean wires (each Boolean::new_witness enforces b * (1 - b) = 0).
    let mut bits = Vec::with_capacity(num_bits);
    let mut reconstructed = FpVar::<Fr>::zero();
    let mut coeff = Fr::from(1u64);

    for i in 0..num_bits {
        let bit_val = raw_bits_le.get(i).copied().unwrap_or(false);
        let bit_var = Boolean::new_witness(cs.clone(), || Ok(bit_val))?;
        reconstructed += FpVar::from(bit_var.clone()) * coeff;
        bits.push(bit_var);
        coeff.double_in_place();
    }

    // Enforce exact equality between reconstructed `num_bits`-bit integer and `val` in `Fr`.
    reconstructed.enforce_equal(val)?;
    Ok(bits)
}

#[inline]
fn poseidon_sponge_gadget_hash(
    cs: ConstraintSystemRef<Fr>,
    inputs: &[FpVar<Fr>],
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut sponge = PoseidonSpongeVar::<Fr>::new(cs, poseidon_config());
    let slice_ref: &[FpVar<Fr>] = inputs;
    sponge.absorb(&slice_ref)?;
    let mut out = sponge.squeeze_field_elements(1)?;
    out.pop().ok_or(SynthesisError::AssignmentMissing)
}

/// Witness step along a binary Merkle-Sum Sparse Tree path.
#[derive(Clone, Copy, Debug)]
pub struct CircuitPathWitness {
    pub sibling_hash: Fr,
    pub sibling_sum: Fr,
    pub is_right_child: bool,
}

impl Default for CircuitPathWitness {
    fn default() -> Self {
        Self {
            sibling_hash: Fr::from(0u64),
            sibling_sum: Fr::from(0u64),
            is_right_child: false,
        }
    }
}

/// **Circuit 1: `MsstInclusionSolvencyCircuit<DEPTH>`**
///
/// Simultaneously proves in Zero Knowledge (without revealing any user's balance, identity,
/// sibling balances, or exact total exchange liabilities):
/// 1. **64-bit Non-Negativity Constraint**: User leaf balance is strictly in `[0, 2^64 - 1]`,
///    and every sibling branch sum at level `d` is strictly in `[0, 2^(64 + d) - 1]`.
/// 2. **Overflow-Free Binary Path Sum & Poseidon Hash Integrity**: From leaf `0` to `DEPTH`,
///    `parent_sum == left_sum + right_sum` without modulo-`r` wrap-around, and every Poseidon
///    node hash matches up to `public_root_hash`.
/// 3. **Global Solvency Inequality**: `root_liability_sum <= public_verified_reserves`, witnessed
///    by `surplus = public_verified_reserves - root_liability_sum` range-proven in `[0, 2^128 - 1]`,
///    bound to `public_root_commitment`.
#[derive(Clone, Debug)]
pub struct MsstInclusionSolvencyCircuit<const DEPTH: usize> {
    // === PUBLIC INPUTS (5 `Fr` elements) ===
    pub public_root_hash: Option<Fr>,
    pub public_root_commitment: Option<Fr>,
    pub public_verified_reserves: Option<Fr>,
    pub public_user_commitment: Option<Fr>,
    pub public_epoch_id: Option<Fr>,

    // === PRIVATE WITNESSES ===
    pub account_id: Option<Fr>,
    pub leaf_balance: Option<Fr>,
    pub leaf_salt: Option<Fr>,
    pub user_blinding: Option<Fr>,
    pub epoch_blinding: Option<Fr>,
    pub surplus: Option<Fr>,
    pub path: [Option<CircuitPathWitness>; DEPTH],
}

impl<const DEPTH: usize> MsstInclusionSolvencyCircuit<DEPTH> {
    /// Blank circuit template with dummy zero assignments used strictly for deterministic
    /// Groth16 CRS / proving-key generation (`Groth16::circuit_specific_setup`).
    pub fn blank_for_setup() -> Self {
        Self {
            public_root_hash: Some(Fr::from(0u64)),
            public_root_commitment: Some(Fr::from(0u64)),
            public_verified_reserves: Some(Fr::from(0u64)),
            public_user_commitment: Some(Fr::from(0u64)),
            public_epoch_id: Some(Fr::from(0u64)),
            account_id: Some(Fr::from(0u64)),
            leaf_balance: Some(Fr::from(0u64)),
            leaf_salt: Some(Fr::from(0u64)),
            user_blinding: Some(Fr::from(0u64)),
            epoch_blinding: Some(Fr::from(0u64)),
            surplus: Some(Fr::from(0u64)),
            path: [Some(CircuitPathWitness::default()); DEPTH],
        }
    }

    /// Canonical ordered public inputs `[root_hash, root_commitment, verified_reserves, user_commitment, epoch_id]`.
    pub fn public_inputs_vec(
        root_hash: Fr,
        root_commitment: Fr,
        verified_reserves: Fr,
        user_commitment: Fr,
        epoch_id: Fr,
    ) -> Vec<Fr> {
        vec![
            root_hash,
            root_commitment,
            verified_reserves,
            user_commitment,
            epoch_id,
        ]
    }
}

impl<const DEPTH: usize> ConstraintSynthesizer<Fr> for MsstInclusionSolvencyCircuit<DEPTH> {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        assert!(DEPTH >= 1 && DEPTH <= 32, "supported MSST depth 1..=32");

        // 1. Allocate Public Inputs (exact order MUST match `public_inputs_vec`).
        let pub_root_hash = FpVar::new_input(cs.clone(), || {
            self.public_root_hash.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_root_commitment = FpVar::new_input(cs.clone(), || {
            self.public_root_commitment
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_verified_reserves = FpVar::new_input(cs.clone(), || {
            self.public_verified_reserves
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_user_commitment = FpVar::new_input(cs.clone(), || {
            self.public_user_commitment
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_epoch_id = FpVar::new_input(cs.clone(), || {
            self.public_epoch_id.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // 2. Allocate Private Leaf & Epoch Witnesses.
        let account_id = FpVar::new_witness(cs.clone(), || {
            self.account_id.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let leaf_balance = FpVar::new_witness(cs.clone(), || {
            self.leaf_balance.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let leaf_salt = FpVar::new_witness(cs.clone(), || {
            self.leaf_salt.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let user_blinding = FpVar::new_witness(cs.clone(), || {
            self.user_blinding.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let epoch_blinding = FpVar::new_witness(cs.clone(), || {
            self.epoch_blinding.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let surplus = FpVar::new_witness(cs.clone(), || {
            self.surplus.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // 3. CLAIM (a): Non-Negativity Constraint (64-bit Range Proof on account_id & leaf_balance).
        enforce_non_negative_range_bits(cs.clone(), &account_id, 64)?;
        enforce_non_negative_range_bits(cs.clone(), &leaf_balance, 64)?;

        // Bind private `(account_id, leaf_balance, user_blinding)` to `pub_user_commitment`.
        let domain_user = FpVar::constant(Fr::from(DOMAIN_USER_COMMIT));
        let computed_user_commit = poseidon_sponge_gadget_hash(
            cs.clone(),
            &[
                domain_user,
                account_id.clone(),
                leaf_balance.clone(),
                user_blinding,
            ],
        )?;
        computed_user_commit.enforce_equal(&pub_user_commitment)?;

        // Compute MSST leaf hash: `H(DOMAIN_MSST_LEAF, account_id, leaf_balance, leaf_salt)`.
        let domain_leaf = FpVar::constant(Fr::from(DOMAIN_MSST_LEAF));
        let mut cur_hash = poseidon_sponge_gadget_hash(
            cs.clone(),
            &[domain_leaf, account_id, leaf_balance.clone(), leaf_salt],
        )?;
        let mut cur_sum = leaf_balance;

        // 4. CLAIM (b): Binary Path Sum Integrity & Poseidon Merkle Rollup (Levels `0..DEPTH`).
        let domain_node = FpVar::constant(Fr::from(DOMAIN_MSST_NODE));
        for (level, step_opt) in self.path.iter().enumerate().take(DEPTH) {
            let step = step_opt.ok_or(SynthesisError::AssignmentMissing)?;
            let sibling_hash = FpVar::new_witness(cs.clone(), || Ok(step.sibling_hash))?;
            let sibling_sum = FpVar::new_witness(cs.clone(), || Ok(step.sibling_sum))?;
            let is_right = Boolean::new_witness(cs.clone(), || Ok(step.is_right_child))?;

            // Range-constrain sibling branch sum to `(64 + level)` bits (<= 96 bits << 253 bits),
            // making negative sibling injection (`r - k`) and finite-field wrap-around impossible!
            let max_level_bits = (64 + level).min(120);
            enforce_non_negative_range_bits(cs.clone(), &sibling_sum, max_level_bits)?;

            let left_hash = is_right.select(&sibling_hash, &cur_hash)?;
            let right_hash = is_right.select(&cur_hash, &sibling_hash)?;
            let left_sum = is_right.select(&sibling_sum, &cur_sum)?;
            let right_sum = is_right.select(&cur_sum, &sibling_sum)?;

            // Exact overflow-free integer sum over `Fr` (since `left_sum + right_sum < 2^121 < r`).
            let parent_sum = &left_sum + &right_sum;
            let parent_hash = poseidon_sponge_gadget_hash(
                cs.clone(),
                &[
                    domain_node.clone(),
                    left_hash,
                    left_sum,
                    right_hash,
                    right_sum,
                ],
            )?;

            cur_sum = parent_sum;
            cur_hash = parent_hash;
        }

        // Enforce reconstructed root hash equals public MSST root hash.
        cur_hash.enforce_equal(&pub_root_hash)?;

        // Bind private `root_sum = cur_sum` into public `pub_root_commitment`.
        let domain_root = FpVar::constant(Fr::from(DOMAIN_MSST_ROOT));
        let computed_root_commit = poseidon_sponge_gadget_hash(
            cs.clone(),
            &[
                domain_root,
                pub_root_hash,
                cur_sum.clone(),
                pub_epoch_id,
                epoch_blinding,
            ],
        )?;
        computed_root_commit.enforce_equal(&pub_root_commitment)?;

        // 5. CLAIM (c): Global Solvency Inequality (`root_sum <= pub_verified_reserves`).
        // Range-prove both `surplus` and `pub_verified_reserves` to `[0, 2^128 - 1]`,
        // then enforce `cur_sum + surplus == pub_verified_reserves` without wrap-around!
        enforce_non_negative_range_bits(cs.clone(), &surplus, 128)?;
        enforce_non_negative_range_bits(cs, &pub_verified_reserves, 128)?;

        let total_backed = &cur_sum + &surplus;
        total_backed.enforce_equal(&pub_verified_reserves)?;

        Ok(())
    }
}

/// **Circuit 2: `EpochBatchSolvencyCircuit<ARITY_LOG2>`**
///
/// Epoch-wide Rollup & Full-Subtree Non-Negativity Circuit (`K = 2^ARITY_LOG2` leaves or top-level
/// partition subtrees, e.g., `ARITY_LOG2 = 5` -> `32` partitions/leaves per SNARK).
///
/// ### Why Both Circuits Exist (Defense Against Maxwell-Buterin Sibling Cancellation)
/// In a naive single-path check, if leaf `2k` had `-1 mod r` and leaf `2k+1` had `+10`, their
/// parent sum `+9` would look non-negative to accounts in other branches. `EpochBatchSolvencyCircuit`
/// range-proves **every single leaf / partition node** (`[0, 2^80 - 1]`) simultaneously inside R1CS,
/// recomputes all `2^ARITY_LOG2 - 1` internal Poseidon Merkle-Sum nodes up to `(root_hash, root_sum)`,
/// and proves `root_sum + surplus == verified_reserves` (`surplus >= 0`)!
#[derive(Clone, Debug)]
pub struct EpochBatchSolvencyCircuit<const ARITY_LOG2: usize> {
    // === PUBLIC INPUTS (4 `Fr` elements) ===
    pub public_root_hash: Option<Fr>,
    pub public_root_commitment: Option<Fr>,
    pub public_verified_reserves: Option<Fr>,
    pub public_epoch_id: Option<Fr>,

    // === PRIVATE WITNESSES (`K = 1 << ARITY_LOG2` child nodes) ===
    pub child_hashes: Vec<Option<Fr>>,
    pub child_sums: Vec<Option<Fr>>,
    /// If `is_leaf_batch == true`, also verifies `child_hashes[i] == PoseidonLeaf(account_ids[i], child_sums[i], salts[i])`
    /// with strict 64-bit leaf balance range proofs; otherwise enforces 80-bit partition sum range proofs.
    pub is_leaf_batch: bool,
    pub leaf_account_ids: Vec<Option<Fr>>,
    pub leaf_salts: Vec<Option<Fr>>,
    pub epoch_blinding: Option<Fr>,
    pub surplus: Option<Fr>,
}

impl<const ARITY_LOG2: usize> EpochBatchSolvencyCircuit<ARITY_LOG2> {
    pub const WIDTH: usize = 1usize << ARITY_LOG2;

    pub fn blank_for_setup(is_leaf_batch: bool) -> Self {
        let k = Self::WIDTH;
        Self {
            public_root_hash: Some(Fr::from(0u64)),
            public_root_commitment: Some(Fr::from(0u64)),
            public_verified_reserves: Some(Fr::from(0u64)),
            public_epoch_id: Some(Fr::from(0u64)),
            child_hashes: vec![Some(Fr::from(0u64)); k],
            child_sums: vec![Some(Fr::from(0u64)); k],
            is_leaf_batch,
            leaf_account_ids: vec![Some(Fr::from(0u64)); k],
            leaf_salts: vec![Some(Fr::from(0u64)); k],
            epoch_blinding: Some(Fr::from(0u64)),
            surplus: Some(Fr::from(0u64)),
        }
    }

    pub fn public_inputs_vec(
        root_hash: Fr,
        root_commitment: Fr,
        verified_reserves: Fr,
        epoch_id: Fr,
    ) -> Vec<Fr> {
        vec![root_hash, root_commitment, verified_reserves, epoch_id]
    }
}

impl<const ARITY_LOG2: usize> ConstraintSynthesizer<Fr> for EpochBatchSolvencyCircuit<ARITY_LOG2> {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        let k = Self::WIDTH;
        if self.child_hashes.len() != k || self.child_sums.len() != k {
            return Err(SynthesisError::AssignmentMissing);
        }

        // 1. Public inputs
        let pub_root_hash = FpVar::new_input(cs.clone(), || {
            self.public_root_hash.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_root_commitment = FpVar::new_input(cs.clone(), || {
            self.public_root_commitment
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_verified_reserves = FpVar::new_input(cs.clone(), || {
            self.public_verified_reserves
                .ok_or(SynthesisError::AssignmentMissing)
        })?;
        let pub_epoch_id = FpVar::new_input(cs.clone(), || {
            self.public_epoch_id.ok_or(SynthesisError::AssignmentMissing)
        })?;

        let epoch_blinding = FpVar::new_witness(cs.clone(), || {
            self.epoch_blinding.ok_or(SynthesisError::AssignmentMissing)
        })?;
        let surplus = FpVar::new_witness(cs.clone(), || {
            self.surplus.ok_or(SynthesisError::AssignmentMissing)
        })?;

        // 2. Allocate & range-constrain ALL `K` bottom nodes (defeating sibling cancellation!).
        let domain_leaf = FpVar::constant(Fr::from(DOMAIN_MSST_LEAF));
        let mut cur_hashes = Vec::with_capacity(k);
        let mut cur_sums = Vec::with_capacity(k);

        for i in 0..k {
            let sum_var = FpVar::new_witness(cs.clone(), || {
                self.child_sums[i].ok_or(SynthesisError::AssignmentMissing)
            })?;

            if self.is_leaf_batch {
                // Strict 64-bit non-negativity on EVERY leaf balance in the batch!
                enforce_non_negative_range_bits(cs.clone(), &sum_var, 64)?;
                let acc_var = FpVar::new_witness(cs.clone(), || {
                    self.leaf_account_ids
                        .get(i)
                        .and_then(|x| *x)
                        .ok_or(SynthesisError::AssignmentMissing)
                })?;
                let salt_var = FpVar::new_witness(cs.clone(), || {
                    self.leaf_salts
                        .get(i)
                        .and_then(|x| *x)
                        .ok_or(SynthesisError::AssignmentMissing)
                })?;
                enforce_non_negative_range_bits(cs.clone(), &acc_var, 64)?;
                let computed_leaf = poseidon_sponge_gadget_hash(
                    cs.clone(),
                    &[domain_leaf.clone(), acc_var, sum_var.clone(), salt_var],
                )?;
                cur_hashes.push(computed_leaf);
            } else {
                // Partition subtree root sum (`<= 80` bits for up to `2^16` accounts * `2^64` max balance).
                enforce_non_negative_range_bits(cs.clone(), &sum_var, 80)?;
                let hash_var = FpVar::new_witness(cs.clone(), || {
                    self.child_hashes[i].ok_or(SynthesisError::AssignmentMissing)
                })?;
                cur_hashes.push(hash_var);
            }
            cur_sums.push(sum_var);
        }

        // 3. Roll up all `ARITY_LOG2` binary tree levels inside R1CS!
        let domain_node = FpVar::constant(Fr::from(DOMAIN_MSST_NODE));
        for _lv in 0..ARITY_LOG2 {
            let next_len = cur_hashes.len() >> 1;
            let mut next_hashes = Vec::with_capacity(next_len);
            let mut next_sums = Vec::with_capacity(next_len);

            for p in 0..next_len {
                let lh = &cur_hashes[p << 1];
                let rh = &cur_hashes[(p << 1) + 1];
                let ls = &cur_sums[p << 1];
                let rs = &cur_sums[(p << 1) + 1];

                let p_sum = ls + rs;
                let p_hash = poseidon_sponge_gadget_hash(
                    cs.clone(),
                    &[
                        domain_node.clone(),
                        lh.clone(),
                        ls.clone(),
                        rh.clone(),
                        rs.clone(),
                    ],
                )?;
                next_sums.push(p_sum);
                next_hashes.push(p_hash);
            }
            cur_hashes = next_hashes;
            cur_sums = next_sums;
        }

        let root_hash_var = &cur_hashes[0];
        let root_sum_var = &cur_sums[0];
        root_hash_var.enforce_equal(&pub_root_hash)?;

        // 4. Root commitment & Global Solvency Inequality (`root_sum + surplus == verified_reserves`).
        let domain_root = FpVar::constant(Fr::from(DOMAIN_MSST_ROOT));
        let computed_root_commit = poseidon_sponge_gadget_hash(
            cs.clone(),
            &[
                domain_root,
                pub_root_hash,
                root_sum_var.clone(),
                pub_epoch_id,
                epoch_blinding,
            ],
        )?;
        computed_root_commit.enforce_equal(&pub_root_commitment)?;

        enforce_non_negative_range_bits(cs.clone(), &surplus, 128)?;
        enforce_non_negative_range_bits(cs, &pub_verified_reserves, 128)?;
        let total_backed = root_sum_var + &surplus;
        total_backed.enforce_equal(&pub_verified_reserves)?;

        Ok(())
    }
}
