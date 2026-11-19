#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::sync::OnceLock;

use ark_bn254::{Bn254, Fr};
use ark_crypto_primitives::sponge::poseidon::find_poseidon_ark_and_mds;
use ark_ff::{Field, PrimeField};
use ark_groth16::{prepare_verifying_key, Groth16, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

pub const POSEIDON_RATE: usize = 5;
pub const POSEIDON_CAPACITY: usize = 1;
pub const POSEIDON_WIDTH: usize = 6;
pub const POSEIDON_FULL_ROUNDS: usize = 8;
pub const POSEIDON_PARTIAL_ROUNDS: usize = 57;
pub const POSEIDON_TOTAL_ROUNDS: usize = 65;

pub const DOMAIN_MSST_LEAF: u64 = 0x4859_505F_4C45_4146;
pub const DOMAIN_MSST_NODE: u64 = 0x4859_505F_4E4F_4445;
pub const DOMAIN_USER_COMMIT: u64 = 0x4859_505F_5553_4552;

const BLAKE3_LEAF_CONTEXT: &[u8; 8] = b"HYPMSSTL";
const BLAKE3_NODE_CONTEXT: &[u8; 8] = b"HYPMSSTN";
const MAX_MSST_DEPTH: usize = 24;

/// Sparse-MDS factored Poseidon permutation (`Grassi et al., ePrint 2019/458 Appendix B`)
/// matching native `FastPoseidon6` and R1CS `PoseidonSpongeVar` bit-for-bit (`-69.4%` partial muls).
#[repr(C, align(64))]
struct WasmPoseidon6 {
    ark_init: [[Fr; POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2],
    ark_final: [[Fr; POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2],
    partial_c0: [Fr; POSEIDON_PARTIAL_ROUNDS],
    mds: [[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
    pre_sparse_mds: [[Fr; POSEIDON_WIDTH]; POSEIDON_WIDTH],
    sparse_w: [[Fr; POSEIDON_WIDTH]; POSEIDON_PARTIAL_ROUNDS],
    sparse_v: [[Fr; POSEIDON_WIDTH - 1]; POSEIDON_PARTIAL_ROUNDS],
}

static WASM_POSEIDON: OnceLock<WasmPoseidon6> = OnceLock::new();

fn mat_mul_6x6(a: &[[Fr; 6]; 6], b: &[[Fr; 6]; 6]) -> [[Fr; 6]; 6] {
    let mut out = [[Fr::from(0u64); 6]; 6];
    for i in 0..6 {
        for j in 0..6 {
            let mut sum = Fr::from(0u64);
            for k in 0..6 {
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

fn wasm_poseidon() -> &'static WasmPoseidon6 {
    WASM_POSEIDON.get_or_init(|| {
        let (ark_v, mds_v) = find_poseidon_ark_and_mds::<Fr>(
            Fr::MODULUS_BIT_SIZE as u64,
            POSEIDON_RATE,
            POSEIDON_FULL_ROUNDS as u64,
            POSEIDON_PARTIAL_ROUNDS as u64,
            0,
        );
        let half = POSEIDON_FULL_ROUNDS / 2;
        let mut work_ark = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_TOTAL_ROUNDS];
        for (r, row) in ark_v.iter().enumerate() {
            for (c, val) in row.iter().enumerate() {
                work_ark[r][c] = *val;
            }
        }
        let mut mds = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_WIDTH];
        for (r, row) in mds_v.iter().enumerate() {
            for (c, val) in row.iter().enumerate() {
                mds[r][c] = *val;
            }
        }

        let mut partial_c0 = [Fr::from(0u64); POSEIDON_PARTIAL_ROUNDS];
        for (p, c0_slot) in partial_c0.iter_mut().enumerate() {
            let r = half + p;
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
                work_ark[r + 1][row_idx] += m_row[1] * tail[1]
                    + m_row[2] * tail[2]
                    + m_row[3] * tail[3]
                    + m_row[4] * tail[4]
                    + m_row[5] * tail[5];
            }
        }

        let mut sparse_w = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_PARTIAL_ROUNDS];
        let mut sparse_v = [[Fr::from(0u64); POSEIDON_WIDTH - 1]; POSEIDON_PARTIAL_ROUNDS];
        let mut cur_mat = mds;
        for p in (0..POSEIDON_PARTIAL_ROUNDS).rev() {
            let mut a_hat = [[Fr::from(0u64); 5]; 5];
            for i in 0..5 {
                a_hat[i].copy_from_slice(&cur_mat[i + 1][1..6]);
            }
            let a_hat_inv = invert_5x5(&a_hat);
            sparse_w[p][0] = cur_mat[0][0];
            for j in 0..5 {
                let mut acc = Fr::from(0u64);
                for k in 0..5 {
                    acc += cur_mat[0][k + 1] * a_hat_inv[k][j];
                }
                sparse_w[p][j + 1] = acc;
            }
            for i in 0..5 {
                sparse_v[p][i] = cur_mat[i + 1][0];
            }
            let mut m_tail = [[Fr::from(0u64); 6]; 6];
            m_tail[0][0] = Fr::from(1u64);
            for i in 0..5 {
                m_tail[i + 1][1..6].copy_from_slice(&a_hat[i]);
            }
            cur_mat = mat_mul_6x6(&m_tail, &mds);
        }

        let mut ark_init = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2];
        ark_init.copy_from_slice(&work_ark[0..half]);
        let mut ark_final = [[Fr::from(0u64); POSEIDON_WIDTH]; POSEIDON_FULL_ROUNDS / 2];
        ark_final.copy_from_slice(&work_ark[(half + POSEIDON_PARTIAL_ROUNDS)..POSEIDON_TOTAL_ROUNDS]);

        WasmPoseidon6 {
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
    x2.square() * x
}

impl WasmPoseidon6 {
    fn permute(&self, state: &mut [Fr; POSEIDON_WIDTH]) {
        for rk in &self.ark_init[..3] {
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);
            for (st, m) in state.iter_mut().zip(&self.mds) {
                *st = s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }
        {
            let rk = &self.ark_init[3];
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);
            for (st, m) in state.iter_mut().zip(&self.pre_sparse_mds) {
                *st = s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }
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
        for rk in &self.ark_final {
            let s0 = sbox_alpha5(state[0] + rk[0]);
            let s1 = sbox_alpha5(state[1] + rk[1]);
            let s2 = sbox_alpha5(state[2] + rk[2]);
            let s3 = sbox_alpha5(state[3] + rk[3]);
            let s4 = sbox_alpha5(state[4] + rk[4]);
            let s5 = sbox_alpha5(state[5] + rk[5]);
            for (st, m) in state.iter_mut().zip(&self.mds) {
                *st = s0 * m[0] + s1 * m[1] + s2 * m[2] + s3 * m[3] + s4 * m[4] + s5 * m[5];
            }
        }
    }

    fn hash_4(&self, inputs: [Fr; 4]) -> Fr {
        let mut st = [
            Fr::from(0u64),
            inputs[0],
            inputs[1],
            inputs[2],
            inputs[3],
            Fr::from(0u64),
        ];
        self.permute(&mut st);
        st[POSEIDON_CAPACITY]
    }

    fn hash_5(&self, inputs: [Fr; 5]) -> Fr {
        let mut st = [
            Fr::from(0u64),
            inputs[0],
            inputs[1],
            inputs[2],
            inputs[3],
            inputs[4],
        ];
        self.permute(&mut st);
        st[POSEIDON_CAPACITY]
    }
}

#[inline(always)]
fn fr_to_bytes_le(val: Fr) -> [u8; 32] {
    let limbs = val.into_bigint().0;
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&limbs[0].to_le_bytes());
    out[8..16].copy_from_slice(&limbs[1].to_le_bytes());
    out[16..24].copy_from_slice(&limbs[2].to_le_bytes());
    out[24..32].copy_from_slice(&limbs[3].to_le_bytes());
    out
}

fn fr_from_bytes_le(bytes: &[u8; 32]) -> Result<Fr, String> {
    Fr::deserialize_compressed(&bytes[..]).map_err(|e| format!("invalid canonical Fr: {e}"))
}

fn blake3_leaf_digest(poseidon_hash: Fr, account_id: u64, balance_fp: u64) -> [u8; 32] {
    let mut buf = [0u8; 56];
    buf[0..8].copy_from_slice(BLAKE3_LEAF_CONTEXT);
    buf[8..40].copy_from_slice(&fr_to_bytes_le(poseidon_hash));
    buf[40..48].copy_from_slice(&account_id.to_le_bytes());
    buf[48..56].copy_from_slice(&balance_fp.to_le_bytes());
    *blake3::hash(&buf).as_bytes()
}

fn blake3_node_digest(
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmPathStep {
    pub sibling_poseidon_bytes: [u8; 32],
    pub sibling_blake3: [u8; 32],
    pub sibling_sum_fp: u128,
    pub is_right_child: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmInclusionProof {
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
    pub path: Vec<WasmPathStep>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmEpochReceipt {
    pub epoch_id: u64,
    pub tree_depth: usize,
    pub active_accounts: usize,
    pub verified_reserves_fp: u128,
    pub root_poseidon_bytes: [u8; 32],
    pub root_blake3: [u8; 32],
    pub root_commitment_bytes: [u8; 32],
    pub groth16_proof_bytes: Vec<u8>,
    pub vk_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmUserBundle {
    pub inclusion: WasmInclusionProof,
    pub verified_reserves_fp: u128,
    pub groth16_proof_bytes: Vec<u8>,
    pub vk_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WasmVerifyVerdict {
    pub valid: bool,
    pub epoch_id: u64,
    pub reconstructed_liabilities_fp: Option<String>,
    pub verified_reserves_fp: Option<String>,
    pub surplus_fp: Option<String>,
    pub error: Option<String>,
}

pub fn verify_inclusion_native(proof: &WasmInclusionProof) -> Result<u128, String> {
    if proof.tree_depth == 0 || proof.tree_depth > MAX_MSST_DEPTH {
        return Err(format!("invalid tree_depth {}", proof.tree_depth));
    }
    if proof.path.len() != proof.tree_depth {
        return Err("path length mismatch".to_string());
    }
    if proof.leaf_index >= (1usize << proof.tree_depth) {
        return Err("leaf_index out of bounds".to_string());
    }

    let pos = wasm_poseidon();
    let salt = fr_from_bytes_le(&proof.leaf_salt_bytes)?;
    let u_blind = fr_from_bytes_le(&proof.user_blinding_bytes)?;

    let leaf_p = pos.hash_4([
        Fr::from(DOMAIN_MSST_LEAF),
        Fr::from(proof.account_id),
        Fr::from(proof.balance_fp),
        salt,
    ]);
    if fr_to_bytes_le(leaf_p) != proof.leaf_poseidon_bytes {
        return Err("leaf Poseidon hash mismatch".to_string());
    }
    let leaf_b3 = blake3_leaf_digest(leaf_p, proof.account_id, proof.balance_fp);
    if leaf_b3 != proof.leaf_blake3 {
        return Err("leaf Blake3 digest mismatch".to_string());
    }

    let user_c = pos.hash_4([
        Fr::from(DOMAIN_USER_COMMIT),
        Fr::from(proof.account_id),
        Fr::from(proof.balance_fp),
        u_blind,
    ]);
    if fr_to_bytes_le(user_c) != proof.user_commitment_bytes {
        return Err("user commitment mismatch".to_string());
    }

    let mut cur_p = leaf_p;
    let mut cur_b3 = leaf_b3;
    let mut cur_sum = u128::from(proof.balance_fp);
    let mut idx = proof.leaf_index;

    for (lv, step) in proof.path.iter().enumerate() {
        if step.is_right_child != ((idx & 1) == 1) {
            return Err(format!("direction bit mismatch at level {lv}"));
        }
        let max_bits = (64 + lv).min(120);
        if step.sibling_sum_fp >= (1u128 << max_bits) {
            return Err(format!("sibling sum exceeds {max_bits}-bit range at level {lv}"));
        }
        let sib_p = fr_from_bytes_le(&step.sibling_poseidon_bytes)?;
        let (lp, lb, ls, rp, rb, rs) = if step.is_right_child {
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
        let next_sum = ls
            .checked_add(rs)
            .ok_or_else(|| format!("u128 overflow at level {lv}"))?;
        let next_p = pos.hash_5([
            Fr::from(DOMAIN_MSST_NODE),
            lp,
            Fr::from(ls),
            rp,
            Fr::from(rs),
        ]);
        let next_b3 = blake3_node_digest(next_p, &lb, ls, &rb, rs);
        cur_p = next_p;
        cur_b3 = next_b3;
        cur_sum = next_sum;
        idx >>= 1;
    }

    if fr_to_bytes_le(cur_p) != proof.root_poseidon_bytes {
        return Err("MSST Poseidon root mismatch".to_string());
    }
    if cur_b3 != proof.root_blake3 {
        return Err("MSST Blake3 root mismatch".to_string());
    }
    Ok(cur_sum)
}

pub fn verify_epoch_receipt_native(receipt: &WasmEpochReceipt) -> Result<(), String> {
    if receipt.groth16_proof_bytes.len() != 128 {
        return Err("Groth16 proof must be 128 compressed bytes".to_string());
    }
    let vk = VerifyingKey::<Bn254>::deserialize_compressed(&receipt.vk_bytes[..])
        .map_err(|e| format!("invalid VK: {e}"))?;
    let pvk = prepare_verifying_key(&vk);
    let proof = Proof::<Bn254>::deserialize_compressed(&receipt.groth16_proof_bytes[..])
        .map_err(|e| format!("invalid Groth16 proof point: {e}"))?;

    let root_hash = fr_from_bytes_le(&receipt.root_poseidon_bytes)?;
    let root_commit = fr_from_bytes_le(&receipt.root_commitment_bytes)?;
    let pub_inputs = [
        root_hash,
        root_commit,
        Fr::from(receipt.verified_reserves_fp),
        Fr::from(receipt.epoch_id),
    ];

    let ok = Groth16::<Bn254>::verify_proof(&pvk, &proof, &pub_inputs)
        .map_err(|e| format!("pairing error: {e}"))?;
    if !ok {
        return Err("Groth16 pairing check failed".to_string());
    }
    Ok(())
}

pub fn verify_user_bundle_native(bundle: &WasmUserBundle) -> Result<(u128, u128), String> {
    let total_liabilities = verify_inclusion_native(&bundle.inclusion)?;
    if bundle.verified_reserves_fp < total_liabilities {
        return Err(format!(
            "insolvent: liabilities {total_liabilities} > reserves {}",
            bundle.verified_reserves_fp
        ));
    }
    let surplus = bundle.verified_reserves_fp - total_liabilities;

    if bundle.groth16_proof_bytes.len() != 128 {
        return Err("Groth16 proof must be 128 compressed bytes".to_string());
    }
    let vk = VerifyingKey::<Bn254>::deserialize_compressed(&bundle.vk_bytes[..])
        .map_err(|e| format!("invalid VK: {e}"))?;
    let pvk = prepare_verifying_key(&vk);
    let proof = Proof::<Bn254>::deserialize_compressed(&bundle.groth16_proof_bytes[..])
        .map_err(|e| format!("invalid Groth16 proof point: {e}"))?;

    let root_hash = fr_from_bytes_le(&bundle.inclusion.root_poseidon_bytes)?;
    let root_commit = fr_from_bytes_le(&bundle.inclusion.root_commitment_bytes)?;
    let user_commit = fr_from_bytes_le(&bundle.inclusion.user_commitment_bytes)?;
    let pub_inputs = [
        root_hash,
        root_commit,
        Fr::from(bundle.verified_reserves_fp),
        user_commit,
        Fr::from(bundle.inclusion.epoch_id),
    ];

    let ok = Groth16::<Bn254>::verify_proof(&pvk, &proof, &pub_inputs)
        .map_err(|e| format!("pairing error: {e}"))?;
    if !ok {
        return Err("Groth16 user solvency pairing check failed".to_string());
    }
    Ok((total_liabilities, surplus))
}

#[wasm_bindgen]
pub fn verify_epoch_solvency_json(receipt_json: &str) -> String {
    let parsed: Result<WasmEpochReceipt, _> = serde_json::from_str(receipt_json);
    let verdict = match parsed {
        Err(e) => WasmVerifyVerdict {
            valid: false,
            epoch_id: 0,
            reconstructed_liabilities_fp: None,
            verified_reserves_fp: None,
            surplus_fp: None,
            error: Some(format!("JSON parse error: {e}")),
        },
        Ok(r) => match verify_epoch_receipt_native(&r) {
            Ok(()) => WasmVerifyVerdict {
                valid: true,
                epoch_id: r.epoch_id,
                reconstructed_liabilities_fp: None,
                verified_reserves_fp: Some(r.verified_reserves_fp.to_string()),
                surplus_fp: None,
                error: None,
            },
            Err(err) => WasmVerifyVerdict {
                valid: false,
                epoch_id: r.epoch_id,
                reconstructed_liabilities_fp: None,
                verified_reserves_fp: Some(r.verified_reserves_fp.to_string()),
                surplus_fp: None,
                error: Some(err),
            },
        },
    };
    serde_json::to_string(&verdict)
        .unwrap_or_else(|_| "{\"valid\":false,\"error\":\"serialize\"}".to_string())
}

#[wasm_bindgen]
pub fn verify_user_solvency_bundle_json(bundle_json: &str) -> String {
    let parsed: Result<WasmUserBundle, _> = serde_json::from_str(bundle_json);
    let verdict = match parsed {
        Err(e) => WasmVerifyVerdict {
            valid: false,
            epoch_id: 0,
            reconstructed_liabilities_fp: None,
            verified_reserves_fp: None,
            surplus_fp: None,
            error: Some(format!("JSON parse error: {e}")),
        },
        Ok(b) => {
            let epoch_id = b.inclusion.epoch_id;
            let res_str = b.verified_reserves_fp.to_string();
            match verify_user_bundle_native(&b) {
                Ok((liab, surplus)) => WasmVerifyVerdict {
                    valid: true,
                    epoch_id,
                    reconstructed_liabilities_fp: Some(liab.to_string()),
                    verified_reserves_fp: Some(res_str),
                    surplus_fp: Some(surplus.to_string()),
                    error: None,
                },
                Err(err) => WasmVerifyVerdict {
                    valid: false,
                    epoch_id,
                    reconstructed_liabilities_fp: None,
                    verified_reserves_fp: Some(res_str),
                    surplus_fp: None,
                    error: Some(err),
                },
            }
        }
    };
    serde_json::to_string(&verdict)
        .unwrap_or_else(|_| "{\"valid\":false,\"error\":\"serialize\"}".to_string())
}

#[wasm_bindgen]
pub fn verify_msst_inclusion_json(inclusion_json: &str) -> String {
    let parsed: Result<WasmInclusionProof, _> = serde_json::from_str(inclusion_json);
    let verdict = match parsed {
        Err(e) => WasmVerifyVerdict {
            valid: false,
            epoch_id: 0,
            reconstructed_liabilities_fp: None,
            verified_reserves_fp: None,
            surplus_fp: None,
            error: Some(format!("JSON parse error: {e}")),
        },
        Ok(inc) => {
            let epoch_id = inc.epoch_id;
            match verify_inclusion_native(&inc) {
                Ok(liab) => WasmVerifyVerdict {
                    valid: true,
                    epoch_id,
                    reconstructed_liabilities_fp: Some(liab.to_string()),
                    verified_reserves_fp: None,
                    surplus_fp: None,
                    error: None,
                },
                Err(err) => WasmVerifyVerdict {
                    valid: false,
                    epoch_id,
                    reconstructed_liabilities_fp: None,
                    verified_reserves_fp: None,
                    surplus_fp: None,
                    error: Some(err),
                },
            }
        }
    };
    serde_json::to_string(&verdict)
        .unwrap_or_else(|_| "{\"valid\":false,\"error\":\"serialize\"}".to_string())
}
