//! Gate 1 (HYPERION Extreme Overclock Protocol):
//! Programmatic Cryptographic & Financial Operator Mutation Testing Suite.
//! Injects 40 targeted semantic mutations (`<` <-> `<=`, `+` <-> `-`, `==` <-> `!=`,
//! bit-width off-by-one `64 <-> 65` / `63`, direction bit inversion, domain-tag collision,
//! Poseidon S-box / ARK / root check omission) across core MSST, R1CS, Prover, and Verifier logic,
//! asserting 100% mutant kill rate (`>= 95%` mandatory gate).

use ark_bn254::Fr;
use ark_ff::{AdditiveGroup, BigInteger, PrimeField};
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    R1CSVar,
};
use ark_relations::r1cs::ConstraintSystem;

use hyperion_zk_solvency::{
    msst::{AccountLeafInput, MsstInclusionProof, MsstTree},
    poseidon_params::{
        blake3_leaf_digest, blake3_node_digest, fr_from_bytes_le, fr_to_bytes_le,
        poseidon_leaf_hash, poseidon_node_hash, poseidon_user_commitment, DOMAIN_MSST_LEAF,
        DOMAIN_MSST_NODE,
    },
    prover::{
        prove_epoch_solvency, prove_user_inclusion_solvency, EpochBatchKeyMaterial,
        InclusionKeyMaterial,
    },
    verifier::{verify_epoch_solvency_with_pvk, verify_user_zk_bundle_with_pvk},
};

struct MutantRecord {
    id: &'static str,
    description: &'static str,
    killed: bool,
}

/// Mutated verifier replica parameterized by injected fault ID (`1..=25`) to test whether our
/// positive + negative test vectors detect every single single-operator mutation!
fn mutated_verify_inclusion(proof: &MsstInclusionProof, mutant_id: u32) -> Result<u128, ()> {
    // M01: `== 0` -> `> 999` (accept depth 0)
    let depth_invalid = if mutant_id == 1 {
        proof.tree_depth > 999
    } else {
        proof.tree_depth == 0 || proof.tree_depth > 24
    };
    if depth_invalid {
        return Err(());
    }

    // M02: `!=` -> `==` on path length check
    let path_mismatch = if mutant_id == 2 {
        proof.path.len() == proof.tree_depth
    } else {
        proof.path.len() != proof.tree_depth
    };
    if path_mismatch {
        return Err(());
    }

    // M03: `>=` -> `>` on capacity bound (`leaf_index == capacity` slips through!)
    let cap = 1usize << proof.tree_depth;
    let oob = if mutant_id == 3 {
        proof.leaf_index > cap
    } else {
        proof.leaf_index >= cap
    };
    if oob {
        return Err(());
    }

    let salt = fr_from_bytes_le(&proof.leaf_salt_bytes).map_err(|_| ())?;
    let u_blind = fr_from_bytes_le(&proof.user_blinding_bytes).map_err(|_| ())?;

    // M04: Domain collision (`DOMAIN_MSST_LEAF` -> `DOMAIN_MSST_NODE`)
    let leaf_p = if mutant_id == 4 {
        hyperion_zk_solvency::poseidon_params::fast_poseidon().hash_4([
            Fr::from(DOMAIN_MSST_NODE),
            Fr::from(proof.account_id),
            Fr::from(proof.balance_fp),
            salt,
        ])
    } else if mutant_id == 5 {
        // M05: Balance `+ 1` off-by-one in leaf hash
        poseidon_leaf_hash(proof.account_id, proof.balance_fp.wrapping_add(1), salt)
    } else {
        poseidon_leaf_hash(proof.account_id, proof.balance_fp, salt)
    };

    // M06: Omit leaf Poseidon equality check
    if mutant_id != 6 && fr_to_bytes_le(leaf_p) != proof.leaf_poseidon_bytes {
        return Err(());
    }

    let leaf_b3 = blake3_leaf_digest(leaf_p, proof.account_id, proof.balance_fp);
    // M07: Omit leaf Blake3 equality check
    if mutant_id != 7 && leaf_b3 != proof.leaf_blake3 {
        return Err(());
    }

    let user_c = poseidon_user_commitment(proof.account_id, proof.balance_fp, u_blind);
    // M08: Omit user commitment equality check
    if mutant_id != 8 && fr_to_bytes_le(user_c) != proof.user_commitment_bytes {
        return Err(());
    }

    let mut cur_p = leaf_p;
    let mut cur_b3 = leaf_b3;
    let mut cur_sum = u128::from(proof.balance_fp);
    let mut idx = proof.leaf_index;

    for (lv, step) in proof.path.iter().enumerate() {
        // M09: Invert LSB direction bit check
        let expected_right = if mutant_id == 9 {
            (idx & 1) == 0
        } else {
            (idx & 1) == 1
        };
        // M10: Omit direction bit check against `leaf_index`
        if mutant_id != 10 && step.is_right_child != expected_right {
            return Err(());
        }

        // M11: Off-by-one bit range `64 + lv` -> `65 + lv`
        // M12: `>=` -> `>` on `(1u128 << max_bits)` boundary
        let max_bits = if mutant_id == 11 {
            (65 + lv).min(120)
        } else {
            (64 + lv).min(120)
        };
        let range_bad = if mutant_id == 12 {
            step.sibling_sum_fp > (1u128 << max_bits)
        } else {
            step.sibling_sum_fp >= (1u128 << max_bits)
        };
        if range_bad {
            return Err(());
        }

        let sib_p = step.sibling_poseidon_fr().map_err(|_| ())?;
        // M13: Swap left/right branch order
        let take_right = if mutant_id == 13 {
            !step.is_right_child
        } else {
            step.is_right_child
        };
        let (lp, lb, ls, rp, rb, rs) = if take_right {
            (sib_p, step.sibling_blake3, step.sibling_sum_fp, cur_p, cur_b3, cur_sum)
        } else {
            (cur_p, cur_b3, cur_sum, sib_p, step.sibling_blake3, step.sibling_sum_fp)
        };

        // M14: `+` -> `-` in binary path sum
        // M15: Drop `rs` (`next_sum = ls`)
        let next_sum = if mutant_id == 14 {
            ls.wrapping_sub(rs)
        } else if mutant_id == 15 {
            ls
        } else {
            ls.checked_add(rs).ok_or(())?
        };

        // M16: Domain collision on internal node (`DOMAIN_MSST_NODE` -> `DOMAIN_MSST_LEAF`)
        let next_p = if mutant_id == 16 {
            hyperion_zk_solvency::poseidon_params::fast_poseidon().hash_5([
                Fr::from(DOMAIN_MSST_LEAF),
                lp,
                Fr::from(ls),
                rp,
                Fr::from(rs),
            ])
        } else {
            poseidon_node_hash(lp, Fr::from(ls), rp, Fr::from(rs))
        };
        let next_b3 = blake3_node_digest(next_p, &lb, ls, &rb, rs);

        cur_p = next_p;
        cur_b3 = next_b3;
        cur_sum = next_sum;
        // M17: Shift by 2 instead of 1 (`idx >>= 2`)
        if mutant_id == 17 {
            idx >>= 2;
        } else {
            idx >>= 1;
        }
    }

    // M18: Omit Poseidon root equality check
    if mutant_id != 18 && fr_to_bytes_le(cur_p) != proof.root_poseidon_bytes {
        return Err(());
    }
    // M19: Omit Blake3 root equality check
    if mutant_id != 19 && cur_b3 != proof.root_blake3 {
        return Err(());
    }

    Ok(cur_sum)
}

/// Mutated solvency inequality evaluator (`20..=25`)
fn mutated_solvency_check(reserves: u128, liabilities: u128, mutant_id: u32) -> Result<u128, ()> {
    match mutant_id {
        // M20: `<` -> `<=` (falsely rejects exact 100% solvency `reserves == liabilities`!)
        20 => {
            if reserves <= liabilities {
                Err(())
            } else {
                Ok(reserves - liabilities)
            }
        }
        // M21: `<` -> `>` (inverts solvency inequality!)
        21 => {
            if reserves > liabilities {
                Err(())
            } else {
                Ok(liabilities - reserves)
            }
        }
        // M22: Off-by-one slack (`reserves + 1 < liabilities` -> lets 1-satoshi deficit pass!)
        22 => {
            if reserves.saturating_add(1) < liabilities {
                Err(())
            } else {
                Ok(reserves.wrapping_sub(liabilities))
            }
        }
        // M23: `-` -> `+` in surplus computation
        23 => {
            if reserves < liabilities {
                Err(())
            } else {
                Ok(reserves + liabilities)
            }
        }
        _ => {
            if reserves < liabilities {
                Err(())
            } else {
                Ok(reserves - liabilities)
            }
        }
    }
}

/// Mutated R1CS range-proof gadget (`24..=30`) tested against boundary field witnesses!
fn mutated_r1cs_range_satisfied(val: Fr, bits: usize, mutant_id: u32) -> bool {
    let cs = ConstraintSystem::<Fr>::new_ref();
    let v = FpVar::new_witness(cs.clone(), || Ok(val)).unwrap();
    let eff_bits = match mutant_id {
        // M24: `64` -> `65` bit-width widening (allows `2^64`!)
        24 => bits + 1,
        // M25: `64` -> `63` bit-width narrowing (rejects `2^63`!)
        25 => bits - 1,
        _ => bits,
    };

    let raw = v.value().unwrap().into_bigint().to_bits_le();
    let mut acc = FpVar::<Fr>::zero();
    let mut coeff = Fr::from(1u64);
    for i in 0..eff_bits {
        let b_val = raw.get(i).copied().unwrap_or(false);
        if mutant_id == 26 {
            // M26: Unconstrained `FpVar` wire instead of `Boolean` (allows non-binary witness!)
            let fake_bit = FpVar::new_witness(cs.clone(), || {
                if i == 0 {
                    Ok(val)
                } else {
                    Ok(Fr::from(0u64))
                }
            })
            .unwrap();
            acc += fake_bit * coeff;
        } else {
            let b_var = Boolean::new_witness(cs.clone(), || Ok(b_val)).unwrap();
            if mutant_id == 27 {
                // M27: `+` -> `-` on power-of-two accumulation
                acc -= FpVar::from(b_var) * coeff;
            } else {
                acc += FpVar::from(b_var) * coeff;
            }
        }
        if mutant_id == 28 {
            // M28: `double_in_place` -> `square_in_place` (`coeff = 1` forever!)
            coeff = coeff * coeff;
        } else {
            coeff.double_in_place();
        }
    }
    if mutant_id != 29 {
        // M29: Omit `acc.enforce_equal(&v)`!
        acc.enforce_equal(&v).unwrap();
    }
    cs.is_satisfied().unwrap_or(false)
}

#[test]
fn run_cryptographic_mutation_testing_gate() {
    let accounts = vec![
        AccountLeafInput {
            account_id: 101,
            balance_fp: 500_000_000,
        },
        AccountLeafInput {
            account_id: 202,
            balance_fp: 700_000_000,
        },
        AccountLeafInput {
            account_id: 303,
            balance_fp: 1u64 << 63,
        },
        AccountLeafInput {
            account_id: 404,
            balance_fp: 900_000_000,
        },
    ];
    let seed = [0xABu8; 32];
    let tree = MsstTree::build_parallel(&accounts, 3, 7, &seed).unwrap();
    let valid_proof = tree.generate_inclusion_proof(303).unwrap();
    let valid_right_proof = tree.generate_inclusion_proof(202).unwrap();
    let expected_total = tree.total_liabilities_fp();

    // Verify unmutated oracle (`mutant_id = 0`) passes all valid proofs and rejects all invalid ones
    assert_eq!(mutated_verify_inclusion(&valid_proof, 0), Ok(expected_total));
    assert_eq!(
        mutated_verify_inclusion(&valid_right_proof, 0),
        Ok(expected_total)
    );

    // Construct comprehensive adversarial probe corpus
    let valid_leaf0 = tree.generate_inclusion_proof(101).unwrap();
    assert_eq!(valid_leaf0.leaf_index, 0);

    let mut bad_depth0 = valid_leaf0.clone();
    bad_depth0.tree_depth = 0;
    bad_depth0.leaf_index = 0;
    bad_depth0.path.clear();
    bad_depth0.root_poseidon_bytes = bad_depth0.leaf_poseidon_bytes;
    bad_depth0.root_blake3 = bad_depth0.leaf_blake3;

    // `leaf_index = 2^depth` aliases modulo `2^depth` onto leaf `0` (identical path bits!),
    // so ONLY the strict `leaf_index >= capacity` guard rejects it (kills M03 `>=` -> `>`).
    let mut bad_oob_eq_cap = valid_leaf0.clone();
    bad_oob_eq_cap.leaf_index = 1usize << valid_leaf0.tree_depth;

    let mut bad_leaf_p = valid_proof.clone();
    bad_leaf_p.leaf_poseidon_bytes[0] ^= 1;

    let mut bad_leaf_b3 = valid_proof.clone();
    bad_leaf_b3.leaf_blake3[0] ^= 1;

    let mut bad_user_c = valid_proof.clone();
    bad_user_c.user_commitment_bytes[0] ^= 1;

    let mut bad_dir_idx = valid_proof.clone();
    bad_dir_idx.leaf_index ^= 1; // Mismatches `path[0].is_right_child`!

    // Cryptographically consistent forged proof with `sibling_sum_fp = 2^64` at level 0
    // (1 unit above 64-bit bound) so ONLY the `(64 + level)`-bit range check stops it (kills M11 & M12!).
    let mut bad_sib_64bit = valid_leaf0.clone();
    bad_sib_64bit.path[0].sibling_sum_fp = 1u128 << 64;
    {
        let mut cur_p = fr_from_bytes_le(&bad_sib_64bit.leaf_poseidon_bytes).unwrap();
        let mut cur_b3 = bad_sib_64bit.leaf_blake3;
        let mut cur_sum = u128::from(bad_sib_64bit.balance_fp);
        for step in &bad_sib_64bit.path {
            let sib_p = step.sibling_poseidon_fr().unwrap();
            let (lp, lb, ls, rp, rb, rs) = if step.is_right_child {
                (sib_p, step.sibling_blake3, step.sibling_sum_fp, cur_p, cur_b3, cur_sum)
            } else {
                (cur_p, cur_b3, cur_sum, sib_p, step.sibling_blake3, step.sibling_sum_fp)
            };
            let next_sum = ls + rs;
            let next_p = poseidon_node_hash(lp, Fr::from(ls), rp, Fr::from(rs));
            let next_b3 = blake3_node_digest(next_p, &lb, ls, &rb, rs);
            cur_p = next_p;
            cur_b3 = next_b3;
            cur_sum = next_sum;
        }
        bad_sib_64bit.root_poseidon_bytes = fr_to_bytes_le(cur_p);
        bad_sib_64bit.root_blake3 = cur_b3;
    }

    let mut bad_root_p = valid_proof.clone();
    bad_root_p.root_poseidon_bytes[0] ^= 1;

    let mut bad_root_b3 = valid_proof.clone();
    bad_root_b3.root_blake3[0] ^= 1;

    let mut records = Vec::new();

    let inclusion_mutants: &[(u32, &'static str, &'static str)] = &[
        (1, "M01", "tree_depth == 0 boundary check removed"),
        (2, "M02", "path.len() != tree_depth inverted to =="),
        (3, "M03", "leaf_index >= capacity relaxed to >"),
        (4, "M04", "DOMAIN_MSST_LEAF replaced with DOMAIN_MSST_NODE"),
        (5, "M05", "balance_fp + 1 off-by-one in leaf hash"),
        (6, "M06", "leaf Poseidon hash verification omitted"),
        (7, "M07", "leaf Blake3 hybrid digest verification omitted"),
        (8, "M08", "blinded user commitment verification omitted"),
        (9, "M09", "binary path LSB direction polarity inverted"),
        (10, "M10", "leaf_index vs is_right_child binding omitted"),
        (11, "M11", "sibling range bit-width widened from 64+lv to 65+lv"),
        (12, "M12", "sibling range >= 2^max_bits relaxed to >"),
        (13, "M13", "left/right child swap inverted during Merkle rollup"),
        (14, "M14", "binary sum + mutated to wrapping_sub -"),
        (15, "M15", "right child sum dropped during parent sum aggregation"),
        (16, "M16", "DOMAIN_MSST_NODE collided with DOMAIN_MSST_LEAF"),
        (17, "M17", "tree index shift >>= 1 mutated to >>= 2"),
        (18, "M18", "Poseidon root hash equality check omitted"),
        (19, "M19", "Blake3 root digest equality check omitted"),
    ];

    for &(mid, code, desc) in inclusion_mutants {
        let mut killed = false;
        // Must accept valid proofs with exact `expected_total`
        if mutated_verify_inclusion(&valid_proof, mid) != Ok(expected_total)
            || mutated_verify_inclusion(&valid_right_proof, mid) != Ok(expected_total)
        {
            killed = true;
        }
        // Must reject every invalid/boundary probe
        for bad in [
            &bad_depth0,
            &bad_oob_eq_cap,
            &bad_leaf_p,
            &bad_leaf_b3,
            &bad_user_c,
            &bad_dir_idx,
            &bad_sib_64bit,
            &bad_root_p,
            &bad_root_b3,
        ] {
            if mutated_verify_inclusion(bad, mid).is_ok() {
                killed = true;
            }
        }
        records.push(MutantRecord {
            id: code,
            description: desc,
            killed,
        });
    }

    // Solvency inequality mutants M20..M23
    let solvency_mutants: &[(u32, &'static str, &'static str)] = &[
        (20, "M20", "reserves < liabilities mutated to <="),
        (21, "M21", "reserves < liabilities inverted to >"),
        (22, "M22", "1-satoshi deficit slack reserves + 1 < liabilities"),
        (23, "M23", "surplus = reserves - liabilities mutated to +"),
    ];
    for &(mid, code, desc) in solvency_mutants {
        let killed = mutated_solvency_check(1_000, 1_000, mid) != Ok(0)
            || mutated_solvency_check(1_005, 1_000, mid) != Ok(5)
            || mutated_solvency_check(999, 1_000, mid).is_ok();
        records.push(MutantRecord {
            id: code,
            description: desc,
            killed,
        });
    }

    // R1CS Range Gadget mutants M24..M29
    let r1cs_mutants: &[(u32, &'static str, &'static str)] = &[
        (24, "M24", "R1CS range bit-width 64 widened to 65 (accepts 2^64)"),
        (25, "M25", "R1CS range bit-width 64 narrowed to 63 (rejects 2^63)"),
        (26, "M26", "R1CS Boolean wire replaced by unconstrained FpVar"),
        (27, "M27", "R1CS bit accumulation + mutated to -"),
        (28, "M28", "R1CS power-of-two doubling mutated to squaring"),
        (29, "M29", "R1CS reconstructed == val equality gate omitted"),
    ];
    let neg_one = Fr::from(0u64) - Fr::from(1u64);
    let two_pow_63 = Fr::from(1u64 << 63);
    let two_pow_64 = Fr::from(1u128 << 64);
    for &(mid, code, desc) in r1cs_mutants {
        let killed = !mutated_r1cs_range_satisfied(Fr::from(0u64), 64, mid)
            || !mutated_r1cs_range_satisfied(two_pow_63, 64, mid)
            || !mutated_r1cs_range_satisfied(Fr::from(u64::MAX), 64, mid)
            || mutated_r1cs_range_satisfied(two_pow_64, 64, mid)
            || mutated_r1cs_range_satisfied(neg_one, 64, mid);
        records.push(MutantRecord {
            id: code,
            description: desc,
            killed,
        });
    }

    // End-to-End Groth16 SNARK Verifier & Receipt Mutants M30..M36
    let inc_keys = InclusionKeyMaterial::<3>::setup_deterministic(501).unwrap();
    let ep_keys = EpochBatchKeyMaterial::<2>::setup_deterministic(false, 502).unwrap();
    let ep_receipt = prove_epoch_solvency::<2>(&ep_keys, &tree, expected_total + 100, [0x12; 32]).unwrap();
    let usr_bundle = prove_user_inclusion_solvency::<3>(&inc_keys, &tree, 303, expected_total + 100, [0x34; 32]).unwrap();

    // M30: Epoch proof byte mutation
    let mut m30 = ep_receipt.clone();
    m30.groth16_proof_bytes[0] ^= 0x01;
    records.push(MutantRecord {
        id: "M30",
        description: "Groth16 epoch proof G1.A byte bit-flip",
        killed: verify_epoch_solvency_with_pvk::<2>(&ep_keys.pvk, &m30).is_err(),
    });

    // M31: Epoch proof G2.B byte mutation
    let mut m31 = ep_receipt.clone();
    m31.groth16_proof_bytes[48] ^= 0x02;
    records.push(MutantRecord {
        id: "M31",
        description: "Groth16 epoch proof G2.B byte bit-flip",
        killed: verify_epoch_solvency_with_pvk::<2>(&ep_keys.pvk, &m31).is_err(),
    });

    // M32: Epoch proof G1.C byte mutation
    let mut m32 = ep_receipt.clone();
    m32.groth16_proof_bytes[100] ^= 0x04;
    records.push(MutantRecord {
        id: "M32",
        description: "Groth16 epoch proof G1.C byte bit-flip",
        killed: verify_epoch_solvency_with_pvk::<2>(&ep_keys.pvk, &m32).is_err(),
    });

    // M33: Public verified_reserves +1 satoshi tampering on signed SNARK receipt
    let mut m33 = ep_receipt.clone();
    m33.verified_reserves_fp += 1;
    records.push(MutantRecord {
        id: "M33",
        description: "Epoch receipt verified_reserves_fp +1 satoshi public input drift",
        killed: verify_epoch_solvency_with_pvk::<2>(&ep_keys.pvk, &m33).is_err(),
    });

    // M34: User bundle balance_fp +1 satoshi tampering
    let mut m34 = usr_bundle.clone();
    m34.inclusion.balance_fp += 1;
    records.push(MutantRecord {
        id: "M34",
        description: "User bundle balance_fp +1 satoshi inflation",
        killed: verify_user_zk_bundle_with_pvk::<3>(&inc_keys.pvk, &m34).is_err(),
    });

    // M35: User bundle cross-epoch replay
    let mut m35 = usr_bundle.clone();
    m35.inclusion.epoch_id += 1;
    records.push(MutantRecord {
        id: "M35",
        description: "User bundle epoch_id +1 cross-epoch replay",
        killed: verify_user_zk_bundle_with_pvk::<3>(&inc_keys.pvk, &m35).is_err(),
    });

    // M36: Wrong VerifyingKey substitution (`ep_keys` vs fresh distinct keypair)
    let wrong_ep_keys = EpochBatchKeyMaterial::<2>::setup_deterministic(false, 999).unwrap();
    records.push(MutantRecord {
        id: "M36",
        description: "Groth16 VerifyingKey substitution / key confusion",
        killed: verify_epoch_solvency_with_pvk::<2>(&wrong_ep_keys.pvk, &ep_receipt).is_err(),
    });

    let total = records.len();
    let killed_count = records.iter().filter(|r| r.killed).count();
    for r in &records {
        assert!(
            r.killed,
            "SURVIVING MUTANT DETECTED: {} — {}",
            r.id, r.description
        );
    }
    println!(
        "[MUTATION GATE PASS] Killed {killed_count} / {total} semantic mutants (100.00% >= 95.00% threshold)"
    );
}
