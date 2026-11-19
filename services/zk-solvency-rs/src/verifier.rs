use ark_bn254::{Bn254, Fr};
use ark_groth16::{prepare_verifying_key, Groth16, PreparedVerifyingKey, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;

use crate::circuit::{EpochBatchSolvencyCircuit, MsstInclusionSolvencyCircuit};
use crate::error::{SolvencyError, SolvencyResult};
use crate::msst::{MsstInclusionProof, MAX_MSST_DEPTH};
use crate::poseidon_params::{
    blake3_leaf_digest, blake3_node_digest, fr_from_bytes_le, fr_to_bytes_le, poseidon_leaf_hash,
    poseidon_node_hash, poseidon_user_commitment,
};
use crate::prover::{EpochSolvencyReceipt, UserZkSolvencyBundle};

/// Deserializes and prepares a BN254 Groth16 verifying key from canonical compressed bytes.
pub fn deserialize_prepared_vk(vk_bytes: &[u8]) -> SolvencyResult<PreparedVerifyingKey<Bn254>> {
    let vk = VerifyingKey::<Bn254>::deserialize_compressed(vk_bytes)
        .map_err(|e| SolvencyError::SerializationError(format!("invalid Groth16 VK: {e}")))?;
    Ok(prepare_verifying_key(&vk))
}

/// Verifies a standalone Merkle-Sum Inclusion Proof (both algebraic Poseidon over BN254 AND
/// symmetric Blake3 hybrid digests, plus per-level non-overflow sum bounds) in `< 0.2 ms`.
///
/// Returns the reconstructed `root_sum_fp` (`total_liabilities_fp` along the verified root) on success.
pub fn verify_msst_inclusion_proof(proof: &MsstInclusionProof) -> SolvencyResult<u128> {
    if proof.tree_depth == 0 || proof.tree_depth > MAX_MSST_DEPTH {
        return Err(SolvencyError::InvalidTreeDepth {
            depth: proof.tree_depth,
            max_depth: MAX_MSST_DEPTH,
        });
    }
    if proof.path.len() != proof.tree_depth {
        return Err(SolvencyError::InvalidPathLength {
            expected: proof.tree_depth,
            actual: proof.path.len(),
        });
    }
    let capacity = 1usize << proof.tree_depth;
    if proof.leaf_index >= capacity {
        return Err(SolvencyError::LeafIndexOutOfBounds {
            index: proof.leaf_index,
            capacity,
        });
    }

    let leaf_salt = fr_from_bytes_le(&proof.leaf_salt_bytes)?;
    let user_blinding = fr_from_bytes_le(&proof.user_blinding_bytes)?;

    // 1. Verify leaf Poseidon commitment & Blake3 hybrid digest.
    let expected_leaf_p = poseidon_leaf_hash(proof.account_id, proof.balance_fp, leaf_salt);
    if fr_to_bytes_le(expected_leaf_p) != proof.leaf_poseidon_bytes {
        return Err(SolvencyError::VerificationFailed {
            reason: "leaf Poseidon hash mismatch".to_string(),
        });
    }
    let expected_leaf_b3 = blake3_leaf_digest(expected_leaf_p, proof.account_id, proof.balance_fp);
    if expected_leaf_b3 != proof.leaf_blake3 {
        return Err(SolvencyError::VerificationFailed {
            reason: "leaf Blake3 digest mismatch".to_string(),
        });
    }

    // 2. Verify blinded user commitment matches `(account_id, balance_fp, user_blinding)`.
    let expected_user_commit =
        poseidon_user_commitment(proof.account_id, proof.balance_fp, user_blinding);
    if fr_to_bytes_le(expected_user_commit) != proof.user_commitment_bytes {
        return Err(SolvencyError::VerificationFailed {
            reason: "user commitment mismatch".to_string(),
        });
    }

    // 3. Walk binary path from level `0` to `tree_depth`, enforcing index bit consistency,
    //    `(64 + level)`-bit sibling sum bound, overflow-free `u128` addition, Poseidon & Blake3 hashes.
    let mut cur_p = expected_leaf_p;
    let mut cur_b3 = expected_leaf_b3;
    let mut cur_sum = u128::from(proof.balance_fp);
    let mut idx = proof.leaf_index;

    for (level, step) in proof.path.iter().enumerate() {
        let expected_is_right = (idx & 1) == 1;
        if step.is_right_child != expected_is_right {
            return Err(SolvencyError::VerificationFailed {
                reason: format!(
                    "path direction bit mismatch at level {level}: expected {expected_is_right}, got {}",
                    step.is_right_child
                ),
            });
        }

        let max_bits = (64 + level).min(120);
        if max_bits < 128 && step.sibling_sum_fp >= (1u128 << max_bits) {
            return Err(SolvencyError::RangeConstraintViolation {
                context: "sibling_sum_fp",
                max_bits,
            });
        }

        let sib_p = step.sibling_poseidon_fr()?;
        let (left_p, left_b3, left_sum, right_p, right_b3, right_sum) = if step.is_right_child {
            (
                sib_p,
                step.sibling_blake3,
                step.sibling_sum_fp,
                cur_p,
                cur_b3,
                cur_sum,
            )
        } else {
            (
                cur_p,
                cur_b3,
                cur_sum,
                sib_p,
                step.sibling_blake3,
                step.sibling_sum_fp,
            )
        };

        let next_sum =
            left_sum
                .checked_add(right_sum)
                .ok_or(SolvencyError::LiabilitySumOverflow {
                    level,
                    left: left_sum,
                    right: right_sum,
                })?;

        let next_p = poseidon_node_hash(left_p, Fr::from(left_sum), right_p, Fr::from(right_sum));
        let next_b3 = blake3_node_digest(next_p, &left_b3, left_sum, &right_b3, right_sum);

        cur_p = next_p;
        cur_b3 = next_b3;
        cur_sum = next_sum;
        idx >>= 1;
    }

    if fr_to_bytes_le(cur_p) != proof.root_poseidon_bytes {
        return Err(SolvencyError::VerificationFailed {
            reason: "reconstructed MSST Poseidon root mismatch".to_string(),
        });
    }
    if cur_b3 != proof.root_blake3 {
        return Err(SolvencyError::VerificationFailed {
            reason: "reconstructed MSST Blake3 root mismatch".to_string(),
        });
    }

    Ok(cur_sum)
}

/// Verifies an `EpochSolvencyReceipt` using a pre-processed `PreparedVerifyingKey<Bn254>` (`~1.5 ms`).
pub fn verify_epoch_solvency_with_pvk<const ARITY_LOG2: usize>(
    pvk: &PreparedVerifyingKey<Bn254>,
    receipt: &EpochSolvencyReceipt,
) -> SolvencyResult<()> {
    if receipt.groth16_proof_bytes.len() != 128 {
        return Err(SolvencyError::VerificationFailed {
            reason: format!(
                "invalid BN254 Groth16 compressed proof length: expected 128B, got {}B",
                receipt.groth16_proof_bytes.len()
            ),
        });
    }

    let proof = Proof::<Bn254>::deserialize_compressed(&receipt.groth16_proof_bytes[..]).map_err(
        |e| SolvencyError::VerificationFailed {
            reason: format!("malformed or off-curve Groth16 proof point: {e}"),
        },
    )?;

    let root_hash = fr_from_bytes_le(&receipt.root_poseidon_bytes)?;
    let root_commit = fr_from_bytes_le(&receipt.root_commitment_bytes)?;
    let public_inputs = EpochBatchSolvencyCircuit::<ARITY_LOG2>::public_inputs_vec(
        root_hash,
        root_commit,
        Fr::from(receipt.verified_reserves_fp),
        Fr::from(receipt.epoch_id),
    );

    let valid = Groth16::<Bn254>::verify_proof(pvk, &proof, &public_inputs).map_err(|e| {
        SolvencyError::VerificationFailed {
            reason: format!("Groth16 pairing check error: {e}"),
        }
    })?;

    if !valid {
        return Err(SolvencyError::VerificationFailed {
            reason: "Groth16 epoch solvency pairing equation rejected proof".to_string(),
        });
    }
    Ok(())
}

/// Verifies a per-user `UserZkSolvencyBundle` (both hybrid MSST path AND 128-byte Groth16 ZK proof
/// of global solvency + path non-negativity) in `< 2.5 ms`.
pub fn verify_user_zk_bundle_with_pvk<const DEPTH: usize>(
    pvk: &PreparedVerifyingKey<Bn254>,
    bundle: &UserZkSolvencyBundle,
) -> SolvencyResult<()> {
    // 1. Verify hybrid Poseidon + Blake3 MSST inclusion path AND root sum inequality.
    let reconstructed_total_liabilities = verify_msst_inclusion_proof(&bundle.inclusion)?;
    if bundle.verified_reserves_fp < reconstructed_total_liabilities {
        return Err(SolvencyError::InsufficientReserves {
            liabilities_fp: reconstructed_total_liabilities,
            reserves_fp: bundle.verified_reserves_fp,
            deficit_fp: reconstructed_total_liabilities - bundle.verified_reserves_fp,
        });
    }

    // 2. Verify BN254 Groth16 ZK proof (with curve & prime-order subgroup validation).
    if bundle.groth16_proof_bytes.len() != 128 {
        return Err(SolvencyError::VerificationFailed {
            reason: format!(
                "invalid BN254 Groth16 compressed proof length: expected 128B, got {}B",
                bundle.groth16_proof_bytes.len()
            ),
        });
    }
    let proof = Proof::<Bn254>::deserialize_compressed(&bundle.groth16_proof_bytes[..]).map_err(
        |e| SolvencyError::VerificationFailed {
            reason: format!("malformed or off-curve Groth16 proof point: {e}"),
        },
    )?;

    let root_hash = fr_from_bytes_le(&bundle.inclusion.root_poseidon_bytes)?;
    let root_commit = fr_from_bytes_le(&bundle.inclusion.root_commitment_bytes)?;
    let user_commit = fr_from_bytes_le(&bundle.inclusion.user_commitment_bytes)?;
    let public_inputs = MsstInclusionSolvencyCircuit::<DEPTH>::public_inputs_vec(
        root_hash,
        root_commit,
        Fr::from(bundle.verified_reserves_fp),
        user_commit,
        Fr::from(bundle.inclusion.epoch_id),
    );

    let valid = Groth16::<Bn254>::verify_proof(pvk, &proof, &public_inputs).map_err(|e| {
        SolvencyError::VerificationFailed {
            reason: format!("Groth16 pairing check error: {e}"),
        }
    })?;

    if !valid {
        return Err(SolvencyError::VerificationFailed {
            reason: "Groth16 user solvency & inclusion pairing equation rejected proof".to_string(),
        });
    }
    Ok(())
}

/// Verifies ONLY the zero-knowledge Groth16 SNARK proof + public commitments for third-party
/// observers (without requiring private `account_id`, `balance_fp`, or `MsstInclusionProof` witness!).
pub fn verify_zero_knowledge_inclusion_snark<const DEPTH: usize>(
    pvk: &PreparedVerifyingKey<Bn254>,
    groth16_proof_bytes: &[u8],
    root_poseidon_bytes: &[u8; 32],
    root_commitment_bytes: &[u8; 32],
    verified_reserves_fp: u128,
    user_commitment_bytes: &[u8; 32],
    epoch_id: u64,
) -> SolvencyResult<()> {
    if groth16_proof_bytes.len() != 128 {
        return Err(SolvencyError::VerificationFailed {
            reason: format!(
                "invalid BN254 Groth16 compressed proof length: expected 128B, got {}B",
                groth16_proof_bytes.len()
            ),
        });
    }
    let proof = Proof::<Bn254>::deserialize_compressed(groth16_proof_bytes).map_err(|e| {
        SolvencyError::VerificationFailed {
            reason: format!("malformed or off-curve Groth16 proof point: {e}"),
        }
    })?;

    let root_hash = fr_from_bytes_le(root_poseidon_bytes)?;
    let root_commit = fr_from_bytes_le(root_commitment_bytes)?;
    let user_commit = fr_from_bytes_le(user_commitment_bytes)?;
    let public_inputs = MsstInclusionSolvencyCircuit::<DEPTH>::public_inputs_vec(
        root_hash,
        root_commit,
        Fr::from(verified_reserves_fp),
        user_commit,
        Fr::from(epoch_id),
    );

    let valid = Groth16::<Bn254>::verify_proof(pvk, &proof, &public_inputs).map_err(|e| {
        SolvencyError::VerificationFailed {
            reason: format!("Groth16 pairing check error: {e}"),
        }
    })?;

    if !valid {
        return Err(SolvencyError::VerificationFailed {
            reason: "Groth16 pairing equation rejected proof".to_string(),
        });
    }
    Ok(())
}
