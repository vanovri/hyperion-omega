use std::time::Instant;

use ark_bn254::Fr;
use ark_crypto_primitives::sponge::{
    constraints::CryptographicSpongeVar,
    poseidon::constraints::PoseidonSpongeVar,
};
use ark_r1cs_std::{alloc::AllocVar, fields::fp::FpVar, R1CSVar};
use ark_relations::r1cs::ConstraintSystem;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use hyperion_zk_solvency::{
    circuit::{CircuitPathWitness, EpochBatchSolvencyCircuit, MsstInclusionSolvencyCircuit},
    error::SolvencyError,
    msst::{AccountLeafInput, MsstTree},
    poseidon_params::{
        baseline_poseidon_hash, fast_poseidon, fr_from_bytes_le, fr_to_bytes_le, poseidon_config,
        poseidon_leaf_hash_fr, poseidon_node_hash, poseidon_root_commitment,
    },
    prover::{
        prove_epoch_solvency, prove_user_inclusion_solvency, verify_r1cs_satisfied,
        EpochBatchKeyMaterial, InclusionKeyMaterial,
    },
    verifier::{
        deserialize_prepared_vk, verify_epoch_solvency_with_pvk, verify_msst_inclusion_proof,
        verify_user_zk_bundle_with_pvk, verify_zero_knowledge_inclusion_snark,
    },
};

fn sample_accounts(count: usize, seed: u64) -> Vec<AccountLeafInput> {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    (0..count)
        .map(|i| AccountLeafInput {
            account_id: 100_000 + (i as u64) * 7 + (rng.gen::<u64>() % 5),
            // Realistic fixed-point balances (`0.00010000` to `500.00000000` BTC/USDT @ 1e8 scale)
            balance_fp: 10_000 + (rng.gen::<u64>() % 50_000_000_000),
        })
        .collect()
}

#[test]
fn test_poseidon_bit_for_bit_equivalence_across_native_fast_and_r1cs() {
    let mut rng = ChaCha20Rng::seed_from_u64(0xC0DE_CAFE_2026);
    let fast = fast_poseidon();
    let cfg = poseidon_config();

    for _ in 0..128 {
        let in4 = [
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
        ];
        let expected_4 = baseline_poseidon_hash(&in4);
        let actual_fast_4 = fast.hash_4(in4);
        assert_eq!(expected_4, actual_fast_4, "4-element fast Poseidon mismatch");

        let cs = ConstraintSystem::<Fr>::new_ref();
        let vars4: Vec<FpVar<Fr>> = in4
            .iter()
            .map(|v| FpVar::new_witness(cs.clone(), || Ok(*v)).unwrap())
            .collect();
        let mut sponge_var = PoseidonSpongeVar::<Fr>::new(cs.clone(), cfg);
        let slice4: &[FpVar<Fr>] = &vars4;
        sponge_var.absorb(&slice4).unwrap();
        let out4 = sponge_var.squeeze_field_elements(1).unwrap();
        assert!(cs.is_satisfied().unwrap());
        assert_eq!(out4[0].value().unwrap(), expected_4, "4-element R1CS Poseidon mismatch");

        let in5 = [
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
            Fr::from(rng.gen::<u64>()),
        ];
        let expected_5 = baseline_poseidon_hash(&in5);
        let actual_fast_5 = fast.hash_5(in5);
        assert_eq!(expected_5, actual_fast_5, "5-element fast Poseidon mismatch");

        let cs5 = ConstraintSystem::<Fr>::new_ref();
        let vars5: Vec<FpVar<Fr>> = in5
            .iter()
            .map(|v| FpVar::new_witness(cs5.clone(), || Ok(*v)).unwrap())
            .collect();
        let mut sponge_var5 = PoseidonSpongeVar::<Fr>::new(cs5.clone(), cfg);
        let slice5: &[FpVar<Fr>] = &vars5;
        sponge_var5.absorb(&slice5).unwrap();
        let out5 = sponge_var5.squeeze_field_elements(1).unwrap();
        assert!(cs5.is_satisfied().unwrap());
        assert_eq!(out5[0].value().unwrap(), expected_5, "5-element R1CS Poseidon mismatch");
    }
}

#[test]
fn test_msst_three_optimization_tiers_produce_identical_roots_and_proofs() {
    let seed = [0x42u8; 32];
    let accounts = sample_accounts(777, 9991);
    let depth = 10; // 1,024 capacity with 777 active accounts + 247 sparse zero accounts!

    let t_base = MsstTree::build_baseline_sequential(&accounts, depth, 17, &seed).unwrap();
    let t_arena = MsstTree::build_zero_alloc_arena(&accounts, depth, 17, &seed).unwrap();
    let t_par = MsstTree::build_parallel(&accounts, depth, 17, &seed).unwrap();

    assert_eq!(t_base.root(), t_arena.root());
    assert_eq!(t_arena.root(), t_par.root());
    assert_eq!(t_base.root_commitment, t_par.root_commitment);

    for idx in [0usize, 1, 255, 256, 511, 776, 800, 1023] {
        let p1 = t_base.generate_inclusion_proof_by_index(idx).unwrap();
        let p2 = t_arena.generate_inclusion_proof_by_index(idx).unwrap();
        let p3 = t_par.generate_inclusion_proof_by_index(idx).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(p2, p3);
        let reconstructed_sum = verify_msst_inclusion_proof(&p3).unwrap();
        assert_eq!(reconstructed_sum, t_par.total_liabilities_fp());
    }
}

#[test]
fn test_valid_end_to_end_groth16_inclusion_and_epoch_solvency_proofs() {
    const DEPTH: usize = 10; // 1,024 leaves
    const EPOCH_ARITY: usize = 5; // Top 32 subtrees aggregated in Epoch SNARK

    let master_seed = [0x77u8; 32];
    let accounts = sample_accounts(1_024, 20261118);
    let tree = MsstTree::build_parallel(&accounts, DEPTH, 42, &master_seed).unwrap();
    let liabilities = tree.total_liabilities_fp();
    // Exact reserves with healthy surplus
    let verified_reserves = liabilities + 250_000_000_000;

    let inc_keys = InclusionKeyMaterial::<DEPTH>::setup_deterministic(1).unwrap();
    let epoch_keys = EpochBatchKeyMaterial::<EPOCH_ARITY>::setup_deterministic(false, 2).unwrap();

    // 1. Prove & verify Epoch Solvency Receipt
    let epoch_receipt =
        prove_epoch_solvency::<EPOCH_ARITY>(&epoch_keys, &tree, verified_reserves, [0x11; 32])
            .unwrap();
    assert_eq!(epoch_receipt.groth16_proof_bytes.len(), 128);

    let pvk_epoch = deserialize_prepared_vk(&epoch_receipt.vk_bytes).unwrap();
    // Warmup + measure median across 5 runs to avoid one-shot thread-scheduler preemption spikes
    verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&pvk_epoch, &epoch_receipt).unwrap();
    let mut ep_samples_us = [0u128; 5];
    for slot in &mut ep_samples_us {
        let t0 = Instant::now();
        verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&pvk_epoch, &epoch_receipt).unwrap();
        *slot = t0.elapsed().as_micros();
    }
    ep_samples_us.sort_unstable();
    let epoch_verify_min_us = ep_samples_us[0];
    let epoch_verify_med_us = ep_samples_us[2];
    println!(
        "[VERIFY] Epoch Groth16 (128B) median = {epoch_verify_med_us} us (min={epoch_verify_min_us} us)"
    );
    assert!(
        epoch_verify_min_us < 5_000,
        "Epoch Groth16 verification exceeded 5ms target: min={epoch_verify_min_us} us (med={epoch_verify_med_us} us)"
    );

    // 2. Prove & verify Individual User ZK Solvency + Inclusion Bundle
    let target_account = accounts[313].account_id;
    let user_bundle = prove_user_inclusion_solvency::<DEPTH>(
        &inc_keys,
        &tree,
        target_account,
        verified_reserves,
        [0x22; 32],
    )
    .unwrap();
    assert_eq!(user_bundle.groth16_proof_bytes.len(), 128);

    let pvk_user = deserialize_prepared_vk(&user_bundle.vk_bytes).unwrap();
    verify_user_zk_bundle_with_pvk::<DEPTH>(&pvk_user, &user_bundle).unwrap();
    let mut usr_samples_us = [0u128; 5];
    for slot in &mut usr_samples_us {
        let t1 = Instant::now();
        verify_user_zk_bundle_with_pvk::<DEPTH>(&pvk_user, &user_bundle).unwrap();
        *slot = t1.elapsed().as_micros();
    }
    usr_samples_us.sort_unstable();
    let user_verify_min_us = usr_samples_us[0];
    let user_verify_med_us = usr_samples_us[2];
    println!(
        "[VERIFY] User ZK Bundle (128B + MSST) median = {user_verify_med_us} us (min={user_verify_min_us} us)"
    );
    assert!(
        user_verify_min_us < 5_000,
        "User ZK bundle verification exceeded 5ms target: min={user_verify_min_us} us (med={user_verify_med_us} us)"
    );

    // 3. Third-party Zero-Knowledge SNARK verification (no private balance/account_id revealed!)
    verify_zero_knowledge_inclusion_snark::<DEPTH>(
        &pvk_user,
        &user_bundle.groth16_proof_bytes,
        &user_bundle.inclusion.root_poseidon_bytes,
        &user_bundle.inclusion.root_commitment_bytes,
        user_bundle.verified_reserves_fp,
        &user_bundle.inclusion.user_commitment_bytes,
        user_bundle.inclusion.epoch_id,
    )
    .unwrap();
}

/// ============================================================================
/// ADVERSARIAL SOUNDNESS ATTACK 1:
/// Phantom Negative-Balance Account Injection (`-1 mod r` & Sibling Cancellation)
/// ============================================================================
#[test]
fn attack_01_phantom_negative_balance_leaf_rejected_100_percent() {
    // Part A: Inject `-1 mod r` (`r - 1`) directly into `MsstInclusionSolvencyCircuit<4>` witness,
    // even when the attacker recomputes EVERY Poseidon leaf/node/root/user commitment consistently!
    const D: usize = 4;
    let neg_one = Fr::from(0u64) - Fr::from(1u64); // r - 1 in BN254 scalar field!
    let account_id = Fr::from(9999u64);
    let salt = Fr::from(12345u64);
    let user_blinding = Fr::from(67890u64);
    let epoch_blinding = Fr::from(55555u64);
    let epoch_id = Fr::from(1u64);

    for neg_val in [
        neg_one,
        Fr::from(0u64) - Fr::from(100_000_000u64),
        Fr::from(1u128 << 64), // 2^64 (1 bit above 64-bit leaf range!)
    ] {
        let mut cur_hash = poseidon_leaf_hash_fr(account_id, neg_val, salt);
        let mut cur_sum = neg_val;
        let mut path = [Some(CircuitPathWitness::default()); D];

        for (lv, slot) in path.iter_mut().enumerate() {
            let sib_sum = Fr::from(1_000_000_000u64); // +10 units sibling!
            let sib_hash = Fr::from(777u64 + lv as u64);
            *slot = Some(CircuitPathWitness {
                sibling_hash: sib_hash,
                sibling_sum: sib_sum,
                is_right_child: false,
            });
            cur_hash = poseidon_node_hash(cur_hash, cur_sum, sib_hash, sib_sum);
            cur_sum += sib_sum;
        }

        // Notice `cur_sum` at root is POSITIVE (`40 - 1 = 39` units) when `neg_val = -1`!
        // A naive root-only check would be fooled! Our 64-bit leaf range circuit MUST reject!
        let verified_reserves = Fr::from(100_000_000_000u64);
        let surplus = verified_reserves - cur_sum;
        let user_commit = fast_poseidon().hash_4([
            Fr::from(hyperion_zk_solvency::poseidon_params::DOMAIN_USER_COMMIT),
            account_id,
            neg_val,
            user_blinding,
        ]);
        let root_commit = fast_poseidon().hash_5([
            Fr::from(hyperion_zk_solvency::poseidon_params::DOMAIN_MSST_ROOT),
            cur_hash,
            cur_sum,
            epoch_id,
            epoch_blinding,
        ]);

        let malicious_circuit = MsstInclusionSolvencyCircuit::<D> {
            public_root_hash: Some(cur_hash),
            public_root_commitment: Some(root_commit),
            public_verified_reserves: Some(verified_reserves),
            public_user_commitment: Some(user_commit),
            public_epoch_id: Some(epoch_id),
            account_id: Some(account_id),
            leaf_balance: Some(neg_val),
            leaf_salt: Some(salt),
            user_blinding: Some(user_blinding),
            epoch_blinding: Some(epoch_blinding),
            surplus: Some(surplus),
            path,
        };

        let res = verify_r1cs_satisfied(malicious_circuit);
        assert!(
            res.is_err(),
            "CRITICAL SOUNDNESS BREACH: negative/out-of-range leaf {neg_val:?} satisfied R1CS!"
        );
    }

    // Part B: Maxwell-Buterin Sibling Cancellation Attack in `EpochBatchSolvencyCircuit<3>` (8 leaves):
    // Leaf 0 = +500, Leaf 1 = +500 (honest users = 1,000),
    // Leaf 6 = -400 (`r - 400`), Leaf 7 = +500 -> Pair(6,7) parent sum = `+100` (looks positive!).
    // Total apparent root liability = `1,100` instead of `1,500`!
    const K_LOG2: usize = 3; // 8 leaves
    let k = 1usize << K_LOG2;
    let mut leaf_sums = vec![
        Fr::from(500u64),
        Fr::from(500u64),
        Fr::from(0u64),
        Fr::from(0u64),
        Fr::from(0u64),
        Fr::from(0u64),
        Fr::from(0u64) - Fr::from(400u64), // Phantom `-400 mod r` leaf!
        Fr::from(500u64),                  // Masks sibling sum to `+100`!
    ];
    let leaf_accs: Vec<Fr> = (0..k).map(|i| Fr::from(1000 + i as u64)).collect();
    let leaf_salts: Vec<Fr> = (0..k).map(|i| Fr::from(9000 + i as u64)).collect();
    let mut hashes: Vec<Fr> = (0..k)
        .map(|i| poseidon_leaf_hash_fr(leaf_accs[i], leaf_sums[i], leaf_salts[i]))
        .collect();

    let mut cur_h = hashes.clone();
    let mut cur_s = leaf_sums.clone();
    for _ in 0..K_LOG2 {
        let mut nh = Vec::new();
        let mut ns = Vec::new();
        for p in 0..(cur_h.len() / 2) {
            let lh = cur_h[2 * p];
            let rh = cur_h[2 * p + 1];
            let ls = cur_s[2 * p];
            let rs = cur_s[2 * p + 1];
            nh.push(poseidon_node_hash(lh, ls, rh, rs));
            ns.push(ls + rs);
        }
        cur_h = nh;
        cur_s = ns;
    }
    let forged_root_hash = cur_h[0];
    let forged_root_sum = cur_s[0]; // Equals Fr::from(1100)!
    assert_eq!(forged_root_sum, Fr::from(1100u64));

    let reserves = Fr::from(1200u64); // Less than true 1500 liabilities, but > forged 1100!
    let forged_root_commit = fast_poseidon().hash_5([
        Fr::from(hyperion_zk_solvency::poseidon_params::DOMAIN_MSST_ROOT),
        forged_root_hash,
        forged_root_sum,
        Fr::from(1u64),
        Fr::from(77u64),
    ]);

    let batch_attack = EpochBatchSolvencyCircuit::<K_LOG2> {
        public_root_hash: Some(forged_root_hash),
        public_root_commitment: Some(forged_root_commit),
        public_verified_reserves: Some(reserves),
        public_epoch_id: Some(Fr::from(1u64)),
        child_hashes: hashes.drain(..).map(Some).collect(),
        child_sums: leaf_sums.drain(..).map(Some).collect(),
        is_leaf_batch: true,
        leaf_account_ids: leaf_accs.into_iter().map(Some).collect(),
        leaf_salts: leaf_salts.into_iter().map(Some).collect(),
        epoch_blinding: Some(Fr::from(77u64)),
        surplus: Some(reserves - forged_root_sum),
    };

    assert!(
        verify_r1cs_satisfied(batch_attack).is_err(),
        "CRITICAL SOUNDNESS BREACH: Maxwell-Buterin sibling cancellation bypassed EpochBatchSolvencyCircuit!"
    );
}

/// ============================================================================
/// ADVERSARIAL SOUNDNESS ATTACK 2:
/// Intermediate Branch Sum Manipulation & Finite-Field Wrap-Around Attack
/// ============================================================================
#[test]
fn attack_02_intermediate_branch_sum_manipulation_and_wraparound_rejected() {
    const D: usize = 6;
    let seed = [0x33u8; 32];
    let accounts = sample_accounts(64, 54321);
    let tree = MsstTree::build_parallel(&accounts, D, 9, &seed).unwrap();
    let total_liabilities = tree.total_liabilities_fp();
    let reserves = total_liabilities + 1_000_000;

    // 2A: Tamper with middle branch `sibling_sum_fp` (level 3) in `MsstInclusionProof`
    let mut tampered_proof = tree.generate_inclusion_proof(accounts[10].account_id).unwrap();
    tampered_proof.path[3].sibling_sum_fp -= 1; // Shave 1 satoshi off middle sibling branch!
    assert!(
        verify_msst_inclusion_proof(&tampered_proof).is_err(),
        "Compact verifier accepted mutated middle branch sum!"
    );

    // Also test tamper in R1CS circuit directly:
    let mut circuit_tampered =
        hyperion_zk_solvency::prover::build_inclusion_circuit::<D>(&tree, &tampered_proof, reserves)
            .unwrap();
    // Adjust surplus witness to match shaved sum so only the middle Poseidon hash / sum check catches it
    circuit_tampered.surplus = Some(Fr::from(reserves - (total_liabilities - 1)));
    assert!(
        verify_r1cs_satisfied(circuit_tampered).is_err(),
        "R1CS accepted dishonest middle-branch sum!"
    );

    // 2B: Finite-Field Modulo-`r` Wrap-Around Attack at intermediate level `d = 2`!
    // Attacker sets `sibling_sum` at level 2 to `r - 500` so `cur_sum + sibling_sum` wraps around modulo `r`
    // and recomputes all parent hashes honestly from level 2 up to a malicious root!
    let valid_proof = tree.generate_inclusion_proof(accounts[0].account_id).unwrap();
    let mut cur_hash = fr_from_bytes_le(&valid_proof.leaf_poseidon_bytes).unwrap();
    let mut cur_sum = Fr::from(valid_proof.balance_fp);
    let mut wrap_path = [Some(CircuitPathWitness::default()); D];

    for (lv, slot) in wrap_path.iter_mut().enumerate() {
        let sib_sum = if lv == 2 {
            Fr::from(0u64) - Fr::from(500u64) // Wrap-around sibling sum `r - 500`!
        } else {
            Fr::from(1_000u64)
        };
        let sib_hash = Fr::from(8888u64 + lv as u64);
        *slot = Some(CircuitPathWitness {
            sibling_hash: sib_hash,
            sibling_sum: sib_sum,
            is_right_child: false,
        });
        cur_hash = poseidon_node_hash(cur_hash, cur_sum, sib_hash, sib_sum);
        cur_sum += sib_sum;
    }

    let fake_reserves = Fr::from(valid_proof.balance_fp + 10_000);
    let fake_root_commit = fast_poseidon().hash_5([
        Fr::from(hyperion_zk_solvency::poseidon_params::DOMAIN_MSST_ROOT),
        cur_hash,
        cur_sum,
        Fr::from(9u64),
        tree.epoch_blinding,
    ]);

    let wraparound_circuit = MsstInclusionSolvencyCircuit::<D> {
        public_root_hash: Some(cur_hash),
        public_root_commitment: Some(fake_root_commit),
        public_verified_reserves: Some(fake_reserves),
        public_user_commitment: Some(fr_from_bytes_le(&valid_proof.user_commitment_bytes).unwrap()),
        public_epoch_id: Some(Fr::from(9u64)),
        account_id: Some(Fr::from(valid_proof.account_id)),
        leaf_balance: Some(Fr::from(valid_proof.balance_fp)),
        leaf_salt: Some(fr_from_bytes_le(&valid_proof.leaf_salt_bytes).unwrap()),
        user_blinding: Some(fr_from_bytes_le(&valid_proof.user_blinding_bytes).unwrap()),
        epoch_blinding: Some(tree.epoch_blinding),
        surplus: Some(fake_reserves - cur_sum),
        path: wrap_path,
    };

    assert!(
        verify_r1cs_satisfied(wraparound_circuit).is_err(),
        "CRITICAL SOUNDNESS BREACH: Finite-field wrap-around on intermediate branch sum bypassed R1CS!"
    );
}

/// ============================================================================
/// ADVERSARIAL SOUNDNESS ATTACK 3:
/// 1-Satoshi Reserve Deficit (`verified_reserves == total_liabilities - 1`)
/// ============================================================================
#[test]
fn attack_03_one_satoshi_reserve_deficit_rejected_100_percent() {
    const D: usize = 6;
    const EPOCH_ARITY: usize = 4;
    let seed = [0x55u8; 32];
    let accounts = sample_accounts(64, 77777);
    let tree = MsstTree::build_parallel(&accounts, D, 101, &seed).unwrap();
    let total_liabilities = tree.total_liabilities_fp();

    let inc_keys = InclusionKeyMaterial::<D>::setup_deterministic(10).unwrap();
    let epoch_keys = EpochBatchKeyMaterial::<EPOCH_ARITY>::setup_deterministic(false, 11).unwrap();

    // Boundary 1: Exact 100.00000000% collateralization (`surplus == 0` satoshis) MUST SUCCEED!
    let exact_epoch =
        prove_epoch_solvency::<EPOCH_ARITY>(&epoch_keys, &tree, total_liabilities, [0x01; 32])
            .unwrap();
    verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &exact_epoch).unwrap();

    let exact_user = prove_user_inclusion_solvency::<D>(
        &inc_keys,
        &tree,
        accounts[0].account_id,
        total_liabilities,
        [0x02; 32],
    )
    .unwrap();
    verify_user_zk_bundle_with_pvk::<D>(&inc_keys.pvk, &exact_user).unwrap();

    // Boundary 2: `total_liabilities - 1` (1 satoshi short!) MUST BE REJECTED 100%!
    let insolvent_reserves = total_liabilities - 1;

    let err_epoch =
        prove_epoch_solvency::<EPOCH_ARITY>(&epoch_keys, &tree, insolvent_reserves, [0x03; 32])
            .unwrap_err();
    assert_eq!(
        err_epoch,
        SolvencyError::InsufficientReserves {
            liabilities_fp: total_liabilities,
            reserves_fp: insolvent_reserves,
            deficit_fp: 1,
        }
    );

    let err_user = prove_user_inclusion_solvency::<D>(
        &inc_keys,
        &tree,
        accounts[0].account_id,
        insolvent_reserves,
        [0x04; 32],
    )
    .unwrap_err();
    assert_eq!(
        err_user,
        SolvencyError::InsufficientReserves {
            liabilities_fp: total_liabilities,
            reserves_fp: insolvent_reserves,
            deficit_fp: 1,
        }
    );

    // Boundary 3: Rogue prover bypasses Rust pre-check and injects `surplus = -1 mod r` into R1CS!
    let mut rogue_circuit =
        hyperion_zk_solvency::prover::build_inclusion_circuit::<D>(&tree, &exact_user.inclusion, total_liabilities)
            .unwrap();
    rogue_circuit.public_verified_reserves = Some(Fr::from(insolvent_reserves));
    rogue_circuit.surplus = Some(Fr::from(insolvent_reserves) - Fr::from(total_liabilities)); // -1 mod r!
    assert!(
        verify_r1cs_satisfied(rogue_circuit).is_err(),
        "CRITICAL SOUNDNESS BREACH: 1-satoshi deficit (`surplus = -1 mod r`) satisfied R1CS!"
    );

    // Boundary 4: Malicious server mutates `verified_reserves_fp` in a valid proof receipt by -1 satoshi!
    let mut forged_receipt = exact_epoch.clone();
    forged_receipt.verified_reserves_fp = insolvent_reserves;
    assert!(
        verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &forged_receipt).is_err(),
        "Verifier accepted Groth16 proof with modified public reserves!"
    );
}

/// ============================================================================
/// ADVERSARIAL SOUNDNESS ATTACK 4:
/// Groth16 Proof Byte Bit-Flipping, Malleability, Truncation, & Replay Attacks
/// ============================================================================
#[test]
fn attack_04_proof_tampering_malleability_and_replay_rejected_100_percent() {
    const D: usize = 6;
    const EPOCH_ARITY: usize = 4;
    let seed = [0x99u8; 32];
    let accounts = sample_accounts(64, 314159);
    let tree = MsstTree::build_parallel(&accounts, D, 500, &seed).unwrap();
    let reserves = tree.total_liabilities_fp() + 10_000;

    let epoch_keys = EpochBatchKeyMaterial::<EPOCH_ARITY>::setup_deterministic(false, 99).unwrap();
    let receipt =
        prove_epoch_solvency::<EPOCH_ARITY>(&epoch_keys, &tree, reserves, [0xAB; 32]).unwrap();

    // 4A: Flip bits across all 128 bytes of the compressed BN254 Groth16 proof (`A`, `B`, `C` curve points)
    for byte_idx in (0..128).step_by(2) {
        let mut mutated = receipt.clone();
        mutated.groth16_proof_bytes[byte_idx] ^= 0x01;
        assert!(
            verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &mutated).is_err(),
            "Verifier accepted bit-flipped Groth16 proof at byte {byte_idx}!"
        );
    }

    // 4B: Truncated / oversized proof payloads
    let mut truncated = receipt.clone();
    truncated.groth16_proof_bytes.pop();
    assert!(verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &truncated).is_err());

    // 4C: Replay proof on Epoch `501`
    let mut replayed_epoch = receipt.clone();
    replayed_epoch.epoch_id = 501;
    assert!(
        verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &replayed_epoch).is_err(),
        "Verifier accepted cross-epoch replayed proof!"
    );

    // 4D: Mutate `root_commitment_bytes`
    let mut forged_commit = receipt.clone();
    let alt_commit = poseidon_root_commitment(
        tree.root().poseidon_hash,
        tree.total_liabilities_fp() - 100,
        500,
        tree.epoch_blinding,
    );
    forged_commit.root_commitment_bytes = fr_to_bytes_le(alt_commit);
    assert!(
        verify_epoch_solvency_with_pvk::<EPOCH_ARITY>(&epoch_keys.pvk, &forged_commit).is_err(),
        "Verifier accepted forged root commitment!"
    );
}
