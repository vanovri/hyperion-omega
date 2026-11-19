use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;

pub const FP_SCALE: u128 = 100_000_000;
pub const MAX_LEAF_BALANCE_FP: i128 = u64::MAX as i128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostTelemetrySnapshot {
    pub os_arch: String,
    pub active_windows: usize,
    pub fixed_point_scale: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeMsstSibling {
    pub is_left: bool,
    pub sibling_hash_hex: String,
    pub sibling_sum_fp: i128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeMsstVerifyResult {
    pub valid: bool,
    pub computed_root_hex: String,
    pub total_liabilities_fp: i128,
    pub surplus_fp: i128,
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Pure Rust native Merkle-Sum Sparse Tree + Solvency verifier exposed over Tauri v2 IPC.
pub fn verify_msst_inclusion_pure(
    account_id: u64,
    asset: &str,
    leaf_balance_fp: i128,
    salt_hex: &str,
    path: &[NativeMsstSibling],
    expected_root_hex: &str,
    vault_reserves_fp: i128,
) -> NativeMsstVerifyResult {
    if !(0..=MAX_LEAF_BALANCE_FP).contains(&leaf_balance_fp) {
        return NativeMsstVerifyResult {
            valid: false,
            computed_root_hex: String::new(),
            total_liabilities_fp: leaf_balance_fp,
            surplus_fp: -1,
        };
    }

    let mut cur_hash = sha256_hex(&format!(
        "HYPERION_MSST_LEAF_V1|{account_id}|{asset}|{leaf_balance_fp}|{}",
        salt_hex.to_ascii_lowercase()
    ));
    let mut cur_sum = leaf_balance_fp;

    for step in path {
        if step.sibling_sum_fp < 0 {
            return NativeMsstVerifyResult {
                valid: false,
                computed_root_hex: cur_hash,
                total_liabilities_fp: cur_sum,
                surplus_fp: -1,
            };
        }
        let next_sum = match cur_sum.checked_add(step.sibling_sum_fp) {
            Some(s) => s,
            None => {
                return NativeMsstVerifyResult {
                    valid: false,
                    computed_root_hex: cur_hash,
                    total_liabilities_fp: cur_sum,
                    surplus_fp: -1,
                }
            }
        };
        let (l_hash, l_sum, r_hash, r_sum) = if step.is_left {
            (
                step.sibling_hash_hex.to_ascii_lowercase(),
                step.sibling_sum_fp,
                cur_hash,
                cur_sum,
            )
        } else {
            (
                cur_hash,
                cur_sum,
                step.sibling_hash_hex.to_ascii_lowercase(),
                step.sibling_sum_fp,
            )
        };
        cur_hash = sha256_hex(&format!(
            "HYPERION_MSST_NODE_V1|{l_hash}|{l_sum}|{r_hash}|{r_sum}"
        ));
        cur_sum = next_sum;
    }

    let root_matches = cur_hash.eq_ignore_ascii_case(expected_root_hex);
    let solvent = vault_reserves_fp >= cur_sum;
    NativeMsstVerifyResult {
        valid: root_matches && solvent,
        computed_root_hex: cur_hash,
        total_liabilities_fp: cur_sum,
        surplus_fp: vault_reserves_fp.saturating_sub(cur_sum),
    }
}

#[tauri::command]
fn host_telemetry_snapshot(app: tauri::AppHandle) -> HostTelemetrySnapshot {
    HostTelemetrySnapshot {
        os_arch: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        active_windows: app.webview_windows().len(),
        fixed_point_scale: FP_SCALE,
    }
}

#[tauri::command]
fn verify_merkle_sum_native(
    account_id: u64,
    asset: String,
    leaf_balance_fp: i128,
    salt_hex: String,
    path: Vec<NativeMsstSibling>,
    expected_root_hex: String,
    vault_reserves_fp: i128,
) -> NativeMsstVerifyResult {
    verify_msst_inclusion_pure(
        account_id,
        &asset,
        leaf_balance_fp,
        &salt_hex,
        &path,
        &expected_root_hex,
        vault_reserves_fp,
    )
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            host_telemetry_snapshot,
            verify_merkle_sum_native
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hyperion Sovereign Studio Tauri v2 desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_native_msst_verifier_soundness() {
        let leaf_a = sha256_hex("HYPERION_MSST_LEAF_V1|1001|USDT|50000000000|aa11");
        let leaf_b = sha256_hex("HYPERION_MSST_LEAF_V1|1002|USDT|30000000000|bb22");
        let root = sha256_hex(&format!(
            "HYPERION_MSST_NODE_V1|{leaf_a}|50000000000|{leaf_b}|30000000000"
        ));

        let ok = verify_msst_inclusion_pure(
            1001,
            "USDT",
            50_000_000_000,
            "aa11",
            &[NativeMsstSibling {
                is_left: false,
                sibling_hash_hex: leaf_b.clone(),
                sibling_sum_fp: 30_000_000_000,
            }],
            &root,
            90_000_000_000,
        );
        assert!(ok.valid);
        assert_eq!(ok.total_liabilities_fp, 80_000_000_000);
        assert_eq!(ok.surplus_fp, 10_000_000_000);

        // Negative ghost sibling must be rejected
        let bad_neg = verify_msst_inclusion_pure(
            1001,
            "USDT",
            50_000_000_000,
            "aa11",
            &[NativeMsstSibling {
                is_left: false,
                sibling_hash_hex: leaf_b,
                sibling_sum_fp: -1,
            }],
            &root,
            90_000_000_000,
        );
        assert!(!bad_neg.valid);
    }
}
