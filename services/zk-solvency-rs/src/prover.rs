use ark_bn254::{Bn254, Fq, Fr, G1Affine, G2Affine};
use ark_crypto_primitives::snark::SNARK;
use ark_ec::AffineRepr;
use ark_ff::PrimeField;
use ark_groth16::{prepare_verifying_key, Groth16, PreparedVerifyingKey, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_serialize::CanonicalSerialize;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use crate::circuit::{CircuitPathWitness, EpochBatchSolvencyCircuit, MsstInclusionSolvencyCircuit};
use crate::error::{SolvencyError, SolvencyResult};
use crate::msst::{MsstInclusionProof, MsstTree};
use crate::poseidon_params::{fr_from_bytes_le, fr_to_bytes_le};

/// Deterministic CRS seed tag for reproducible testing/local setups (production deployments load
/// ceremony SRS from multi-party Powers-of-Tau transcript).
const DETERMINISTIC_SETUP_SEED_PREFIX: &[u8; 24] = b"HYPERION_CH13_GROTH16_PK";

/// Precomputed Groth16 keypair for `MsstInclusionSolvencyCircuit<DEPTH>`.
pub struct InclusionKeyMaterial<const DEPTH: usize> {
    pub pk: ProvingKey<Bn254>,
    pub vk: VerifyingKey<Bn254>,
    pub pvk: PreparedVerifyingKey<Bn254>,
    pub vk_bytes: Vec<u8>,
}

impl<const DEPTH: usize> InclusionKeyMaterial<DEPTH> {
    pub fn setup_deterministic(domain_tag: u64) -> SolvencyResult<Self> {
        let mut seed = [0u8; 32];
        seed[..24].copy_from_slice(DETERMINISTIC_SETUP_SEED_PREFIX);
        seed[24..32].copy_from_slice(&(domain_tag ^ (DEPTH as u64)).to_le_bytes());
        let mut rng = ChaCha20Rng::from_seed(seed);

        let blank = MsstInclusionSolvencyCircuit::<DEPTH>::blank_for_setup();
        let (pk, vk) = Groth16::<Bn254>::circuit_specific_setup(blank, &mut rng)
            .map_err(|e| SolvencyError::SynthesisFailure(format!("Groth16 setup error: {e}")))?;
        let pvk = prepare_verifying_key(&vk);
        let mut vk_bytes = Vec::new();
        vk.serialize_compressed(&mut vk_bytes)
            .map_err(|e| SolvencyError::SerializationError(format!("VK serialize: {e}")))?;

        Ok(Self {
            pk,
            vk,
            pvk,
            vk_bytes,
        })
    }
}

/// Precomputed Groth16 keypair for `EpochBatchSolvencyCircuit<ARITY_LOG2>`.
pub struct EpochBatchKeyMaterial<const ARITY_LOG2: usize> {
    pub is_leaf_batch: bool,
    pub pk: ProvingKey<Bn254>,
    pub vk: VerifyingKey<Bn254>,
    pub pvk: PreparedVerifyingKey<Bn254>,
    pub vk_bytes: Vec<u8>,
}

impl<const ARITY_LOG2: usize> EpochBatchKeyMaterial<ARITY_LOG2> {
    pub fn setup_deterministic(is_leaf_batch: bool, domain_tag: u64) -> SolvencyResult<Self> {
        let mut seed = [0u8; 32];
        seed[..24].copy_from_slice(DETERMINISTIC_SETUP_SEED_PREFIX);
        let flag = if is_leaf_batch { 0xA5A5_0000_0000_0000u64 } else { 0x5A5A_0000_0000_0000u64 };
        seed[24..32].copy_from_slice(&(domain_tag ^ flag ^ (ARITY_LOG2 as u64)).to_le_bytes());
        let mut rng = ChaCha20Rng::from_seed(seed);

        let blank = EpochBatchSolvencyCircuit::<ARITY_LOG2>::blank_for_setup(is_leaf_batch);
        let (pk, vk) = Groth16::<Bn254>::circuit_specific_setup(blank, &mut rng)
            .map_err(|e| SolvencyError::SynthesisFailure(format!("Epoch Groth16 setup: {e}")))?;
        let pvk = prepare_verifying_key(&vk);
        let mut vk_bytes = Vec::new();
        vk.serialize_compressed(&mut vk_bytes)
            .map_err(|e| SolvencyError::SerializationError(format!("VK serialize: {e}")))?;

        Ok(Self {
            is_leaf_batch,
            pk,
            vk,
            pvk,
            vk_bytes,
        })
    }
}

/// Publicly auditable Zero-Knowledge Epoch Solvency Receipt (`128`-byte Groth16 BN254 proof +
/// EVM `alt_bn128` `uint256[8]` calldata words).
/// Note: Does NOT expose `total_liabilities_fp` or `surplus_fp` on the wire!
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochSolvencyReceipt {
    pub epoch_id: u64,
    pub tree_depth: usize,
    pub active_accounts: usize,
    pub verified_reserves_fp: u128,
    pub root_poseidon_bytes: [u8; 32],
    pub root_blake3: [u8; 32],
    pub root_commitment_bytes: [u8; 32],
    /// Compressed canonical BN254 Groth16 proof (`128` bytes: `G1` 32B + `G2` 64B + `G1` 32B).
    pub groth16_proof_bytes: Vec<u8>,
    /// Uncompressed big-endian `uint256[8]` calldata `[A.x, A.y, B.x_c1, B.x_c0, B.y_c1, B.y_c0, C.x, C.y]`
    /// directly consumable by Solidity `SolvencyStateAnchor.sol` (`ecPairing` precompile `0x08`).
    pub evm_proof_words: [[u8; 32]; 8],
    pub vk_bytes: Vec<u8>,
}

/// Complete per-user ZK Solvency + MSST Inclusion Proof bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserZkSolvencyBundle {
    pub inclusion: MsstInclusionProof,
    pub verified_reserves_fp: u128,
    pub groth16_proof_bytes: Vec<u8>,
    pub evm_proof_words: [[u8; 32]; 8],
    pub vk_bytes: Vec<u8>,
}

#[inline(always)]
fn fq_to_be_word(val: Fq) -> [u8; 32] {
    let limbs = val.into_bigint().0;
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&limbs[3].to_be_bytes());
    out[8..16].copy_from_slice(&limbs[2].to_be_bytes());
    out[16..24].copy_from_slice(&limbs[1].to_be_bytes());
    out[24..32].copy_from_slice(&limbs[0].to_be_bytes());
    out
}

/// Encodes a BN254 Groth16 `Proof<Bn254>` into EIP-197 `alt_bn128` `uint256[8]` big-endian words.
/// Note: EIP-197 orders `Fq2` coordinates as `(c1_imaginary, c0_real)`.
pub fn proof_to_evm_words(proof: &Proof<Bn254>) -> [[u8; 32]; 8] {
    let (ax, ay) = proof.a.xy().unwrap_or((Fq::from(0u64), Fq::from(0u64)));
    let (bx, by) = proof
        .b
        .xy()
        .unwrap_or((ark_bn254::Fq2::default(), ark_bn254::Fq2::default()));
    let (cx, cy) = proof.c.xy().unwrap_or((Fq::from(0u64), Fq::from(0u64)));

    [
        fq_to_be_word(ax),
        fq_to_be_word(ay),
        fq_to_be_word(bx.c1),
        fq_to_be_word(bx.c0),
        fq_to_be_word(by.c1),
        fq_to_be_word(by.c0),
        fq_to_be_word(cx),
        fq_to_be_word(cy),
    ]
}

/// Validates R1CS satisfaction explicitly and returns the exact unsatisfied constraint name on failure.
pub fn verify_r1cs_satisfied<C: ConstraintSynthesizer<Fr>>(circuit: C) -> SolvencyResult<usize> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    circuit
        .generate_constraints(cs.clone())
        .map_err(|e| SolvencyError::SynthesisFailure(format!("{e}")))?;
    cs.finalize();
    if !cs.is_satisfied().unwrap_or(false) {
        let which = cs
            .which_is_unsatisfied()
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown_r1cs_gate".to_string());
        return Err(SolvencyError::UnsatisfiedConstraints {
            unsatisfied_gate: which,
        });
    }
    Ok(cs.num_constraints())
}

/// Builds a `MsstInclusionSolvencyCircuit<DEPTH>` instance from a tree, account proof, and verified reserves.
pub fn build_inclusion_circuit<const DEPTH: usize>(
    tree: &MsstTree,
    inclusion: &MsstInclusionProof,
    verified_reserves_fp: u128,
) -> SolvencyResult<MsstInclusionSolvencyCircuit<DEPTH>> {
    if tree.depth != DEPTH || inclusion.path.len() != DEPTH {
        return Err(SolvencyError::InvalidPathLength {
            expected: DEPTH,
            actual: inclusion.path.len(),
        });
    }

    let total_liabilities_fp = tree.total_liabilities_fp();
    if verified_reserves_fp < total_liabilities_fp {
        return Err(SolvencyError::InsufficientReserves {
            liabilities_fp: total_liabilities_fp,
            reserves_fp: verified_reserves_fp,
            deficit_fp: total_liabilities_fp - verified_reserves_fp,
        });
    }
    let surplus_fp = verified_reserves_fp - total_liabilities_fp;

    let mut path = [Some(CircuitPathWitness::default()); DEPTH];
    for (i, step) in inclusion.path.iter().enumerate() {
        path[i] = Some(CircuitPathWitness {
            sibling_hash: step.sibling_poseidon_fr()?,
            sibling_sum: Fr::from(step.sibling_sum_fp),
            is_right_child: step.is_right_child,
        });
    }

    Ok(MsstInclusionSolvencyCircuit {
        public_root_hash: Some(tree.root().poseidon_hash),
        public_root_commitment: Some(tree.root_commitment),
        public_verified_reserves: Some(Fr::from(verified_reserves_fp)),
        public_user_commitment: Some(fr_from_bytes_le(&inclusion.user_commitment_bytes)?),
        public_epoch_id: Some(Fr::from(tree.epoch_id)),
        account_id: Some(Fr::from(inclusion.account_id)),
        leaf_balance: Some(Fr::from(inclusion.balance_fp)),
        leaf_salt: Some(fr_from_bytes_le(&inclusion.leaf_salt_bytes)?),
        user_blinding: Some(fr_from_bytes_le(&inclusion.user_blinding_bytes)?),
        epoch_blinding: Some(tree.epoch_blinding),
        surplus: Some(Fr::from(surplus_fp)),
        path,
    })
}

/// Generates a real BN254 Groth16 Zero-Knowledge Solvency + Inclusion Proof for a specific account.
pub fn prove_user_inclusion_solvency<const DEPTH: usize>(
    keys: &InclusionKeyMaterial<DEPTH>,
    tree: &MsstTree,
    account_id: u64,
    verified_reserves_fp: u128,
    proof_entropy: [u8; 32],
) -> SolvencyResult<UserZkSolvencyBundle> {
    let inclusion = tree.generate_inclusion_proof(account_id)?;
    let circuit = build_inclusion_circuit::<DEPTH>(tree, &inclusion, verified_reserves_fp)?;

    // Defense-in-depth: validate R1CS satisfaction prior to QAP MSM so adversarial / corrupted
    // states are deterministically rejected even before pairing exponentiation.
    verify_r1cs_satisfied(circuit.clone())?;

    let mut rng = ChaCha20Rng::from_seed(proof_entropy);
    let proof = Groth16::<Bn254>::prove(&keys.pk, circuit, &mut rng)
        .map_err(|e| SolvencyError::SynthesisFailure(format!("Groth16 prove failed: {e}")))?;

    let mut groth16_proof_bytes = Vec::with_capacity(128);
    proof
        .serialize_compressed(&mut groth16_proof_bytes)
        .map_err(|e| SolvencyError::SerializationError(format!("proof serialize: {e}")))?;

    Ok(UserZkSolvencyBundle {
        inclusion,
        verified_reserves_fp,
        groth16_proof_bytes,
        evm_proof_words: proof_to_evm_words(&proof),
        vk_bytes: keys.vk_bytes.clone(),
    })
}

/// Builds an `EpochBatchSolvencyCircuit<ARITY_LOG2>` from the top `ARITY_LOG2` levels of `MsstTree`
/// (`tree.depth >= ARITY_LOG2`). When `tree.depth == ARITY_LOG2` and `is_leaf_batch == true`,
/// proves full leaf-level 64-bit non-negativity across every single leaf slot in the tree.
pub fn build_epoch_batch_circuit<const ARITY_LOG2: usize>(
    tree: &MsstTree,
    verified_reserves_fp: u128,
    is_leaf_batch: bool,
) -> SolvencyResult<EpochBatchSolvencyCircuit<ARITY_LOG2>> {
    if tree.depth < ARITY_LOG2 || (is_leaf_batch && tree.depth != ARITY_LOG2) {
        return Err(SolvencyError::InvalidTreeDepth {
            depth: tree.depth,
            max_depth: ARITY_LOG2,
        });
    }

    let total_liabilities_fp = tree.total_liabilities_fp();
    if verified_reserves_fp < total_liabilities_fp {
        return Err(SolvencyError::InsufficientReserves {
            liabilities_fp: total_liabilities_fp,
            reserves_fp: verified_reserves_fp,
            deficit_fp: total_liabilities_fp - verified_reserves_fp,
        });
    }
    let surplus_fp = verified_reserves_fp - total_liabilities_fp;

    let target_level = tree.depth - ARITY_LOG2;
    let slice = tree.level_nodes(target_level)?;
    let k = 1usize << ARITY_LOG2;

    let mut child_hashes = Vec::with_capacity(k);
    let mut child_sums = Vec::with_capacity(k);
    for node in slice.iter().take(k) {
        child_hashes.push(Some(node.poseidon_hash));
        child_sums.push(Some(Fr::from(node.sum_fp)));
    }

    let mut leaf_account_ids = vec![Some(Fr::from(0u64)); k];
    let mut leaf_salts = vec![Some(Fr::from(0u64)); k];
    if is_leaf_batch {
        for idx in 0..k {
            let p = tree.generate_inclusion_proof_by_index(idx)?;
            leaf_account_ids[idx] = Some(Fr::from(p.account_id));
            leaf_salts[idx] = Some(fr_from_bytes_le(&p.leaf_salt_bytes)?);
        }
    }

    Ok(EpochBatchSolvencyCircuit {
        public_root_hash: Some(tree.root().poseidon_hash),
        public_root_commitment: Some(tree.root_commitment),
        public_verified_reserves: Some(Fr::from(verified_reserves_fp)),
        public_epoch_id: Some(Fr::from(tree.epoch_id)),
        child_hashes,
        child_sums,
        is_leaf_batch,
        leaf_account_ids,
        leaf_salts,
        epoch_blinding: Some(tree.epoch_blinding),
        surplus: Some(Fr::from(surplus_fp)),
    })
}

/// Generates an Epoch-wide Zero-Knowledge Solvency Receipt (`128`-byte BN254 Groth16 SNARK).
pub fn prove_epoch_solvency<const ARITY_LOG2: usize>(
    keys: &EpochBatchKeyMaterial<ARITY_LOG2>,
    tree: &MsstTree,
    verified_reserves_fp: u128,
    proof_entropy: [u8; 32],
) -> SolvencyResult<EpochSolvencyReceipt> {
    let circuit =
        build_epoch_batch_circuit::<ARITY_LOG2>(tree, verified_reserves_fp, keys.is_leaf_batch)?;

    verify_r1cs_satisfied(circuit.clone())?;

    let mut rng = ChaCha20Rng::from_seed(proof_entropy);
    let proof = Groth16::<Bn254>::prove(&keys.pk, circuit, &mut rng)
        .map_err(|e| SolvencyError::SynthesisFailure(format!("Epoch Groth16 prove: {e}")))?;

    let mut groth16_proof_bytes = Vec::with_capacity(128);
    proof
        .serialize_compressed(&mut groth16_proof_bytes)
        .map_err(|e| SolvencyError::SerializationError(format!("proof serialize: {e}")))?;

    let root = tree.root();
    Ok(EpochSolvencyReceipt {
        epoch_id: tree.epoch_id,
        tree_depth: tree.depth,
        active_accounts: tree.active_accounts,
        verified_reserves_fp,
        root_poseidon_bytes: fr_to_bytes_le(root.poseidon_hash),
        root_blake3: root.blake3_digest,
        root_commitment_bytes: fr_to_bytes_le(tree.root_commitment),
        groth16_proof_bytes,
        evm_proof_words: proof_to_evm_words(&proof),
        vk_bytes: keys.vk_bytes.clone(),
    })
}

/// Suppresses unused import warnings if `G1Affine`/`G2Affine` are only referenced via `Proof<Bn254>`.
#[allow(dead_code)]
const _CURVE_TYPES_ASSERT: Option<(G1Affine, G2Affine)> = None;
