use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{SolvencyError, SolvencyResult};
use crate::msst::AccountLeafInput;

/// Exact 8-byte snapshot file magic per `.agents/rules/00-hyperion-contract.md` §6.1 (`HYPSNP01`).
pub const SNAPSHOT_MAGIC: &[u8; 8] = b"HYPSNP01";
pub const SNAPSHOT_VERSION: u32 = 1;
pub const SNAPSHOT_HEADER_LEN: usize = 72;

#[derive(Debug, Clone, Deserialize)]
pub struct LedgerSnapshotPayloadV1 {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub state_root_blake3: [u8; 32],
    pub accounts: Vec<LedgerSnapshotAccountV1>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LedgerSnapshotAccountV1 {
    pub account_id: u64,
    pub balances: Vec<LedgerSnapshotAssetBalanceV1>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LedgerSnapshotAssetBalanceV1 {
    pub asset: String,
    pub available_fp: String,
    pub locked_fp: String,
}

/// Pure-safe Castagnoli CRC-32C (`0x82F63B78` reflected polynomial) matching `crc32c` crate
/// bit-for-bit without requiring `unsafe` intrinsics.
fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
    crc = !crc;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78 & mask);
        }
    }
    !crc
}

pub fn compute_snapshot_crc32c(
    engine_seq: u64,
    ts_ns: u64,
    state_root_blake3: &[u8; 32],
    payload: &[u8],
) -> u32 {
    let mut crc = 0u32;
    crc = crc32c_update(crc, &engine_seq.to_le_bytes());
    crc = crc32c_update(crc, &ts_ns.to_le_bytes());
    crc = crc32c_update(crc, state_root_blake3);
    crc32c_update(crc, payload)
}

/// Parsed snapshot summary ready for MSST construction.
#[derive(Debug, Clone)]
pub struct ParsedLedgerAssetSnapshot {
    pub engine_seq: u64,
    pub ts_ns: u64,
    pub state_root_blake3: [u8; 32],
    pub asset: String,
    pub accounts: Vec<AccountLeafInput>,
}

/// Parses and verifies a Channel 02 (`services/ledger-rs`) `HYPSNP01` binary snapshot from bytes,
/// verifying header magic, format version, CRC-32C integrity, `state_root_blake3` header consistency,
/// and strict 64-bit non-negative fixed-point balances (`available_fp + locked_fp`) for `target_asset`.
pub fn parse_hypsnp01_bytes(
    raw: &[u8],
    target_asset: &str,
) -> SolvencyResult<ParsedLedgerAssetSnapshot> {
    if raw.len() < SNAPSHOT_HEADER_LEN || &raw[0..8] != SNAPSHOT_MAGIC {
        return Err(SolvencyError::SerializationError(
            "invalid HYPSNP01 snapshot magic or truncated header".to_string(),
        ));
    }
    let version = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
    if version != SNAPSHOT_VERSION {
        return Err(SolvencyError::SerializationError(format!(
            "unsupported HYPSNP01 version {version}"
        )));
    }
    let stored_crc = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
    let engine_seq = u64::from_le_bytes([
        raw[16], raw[17], raw[18], raw[19], raw[20], raw[21], raw[22], raw[23],
    ]);
    let ts_ns = u64::from_le_bytes([
        raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30], raw[31],
    ]);
    let mut state_root = [0u8; 32];
    state_root.copy_from_slice(&raw[32..64]);
    let payload_len = u64::from_le_bytes([
        raw[64], raw[65], raw[66], raw[67], raw[68], raw[69], raw[70], raw[71],
    ]) as usize;

    if SNAPSHOT_HEADER_LEN.saturating_add(payload_len) != raw.len() {
        return Err(SolvencyError::SerializationError(
            "HYPSNP01 payload length mismatch".to_string(),
        ));
    }

    let payload = &raw[SNAPSHOT_HEADER_LEN..];
    let calc_crc = compute_snapshot_crc32c(engine_seq, ts_ns, &state_root, payload);
    if calc_crc != stored_crc {
        return Err(SolvencyError::SerializationError(format!(
            "HYPSNP01 CRC-32C mismatch: stored={stored_crc:#010x}, computed={calc_crc:#010x}"
        )));
    }

    let decoded: LedgerSnapshotPayloadV1 = serde_json::from_slice(payload)
        .map_err(|e| SolvencyError::SerializationError(format!("HYPSNP01 JSON decode: {e}")))?;

    if decoded.engine_seq != engine_seq || decoded.state_root_blake3 != state_root {
        return Err(SolvencyError::SerializationError(
            "HYPSNP01 header/payload engine_seq or state_root_blake3 mismatch".to_string(),
        ));
    }

    let mut leaves = Vec::with_capacity(decoded.accounts.len());
    for acc in decoded.accounts {
        let mut total_i128: i128 = 0;
        for bal in &acc.balances {
            if bal.asset == target_asset {
                let avail: i128 = bal.available_fp.parse().map_err(|_| {
                    SolvencyError::SerializationError(format!(
                        "invalid available_fp string for account {}",
                        acc.account_id
                    ))
                })?;
                let locked: i128 = bal.locked_fp.parse().map_err(|_| {
                    SolvencyError::SerializationError(format!(
                        "invalid locked_fp string for account {}",
                        acc.account_id
                    ))
                })?;
                if avail < 0 || locked < 0 {
                    return Err(SolvencyError::RangeConstraintViolation {
                        context: "ledger_snapshot_negative_balance",
                        max_bits: 64,
                    });
                }
                total_i128 = total_i128
                    .checked_add(avail)
                    .and_then(|v| v.checked_add(locked))
                    .ok_or(SolvencyError::LiabilitySumOverflow {
                        level: 0,
                        left: total_i128 as u128,
                        right: avail as u128,
                    })?;
            }
        }
        if total_i128 < 0 || total_i128 > i128::from(u64::MAX) {
            return Err(SolvencyError::RangeConstraintViolation {
                context: "ledger_snapshot_u64_balance",
                max_bits: 64,
            });
        }
        leaves.push(AccountLeafInput {
            account_id: acc.account_id,
            balance_fp: total_i128 as u64,
        });
    }

    Ok(ParsedLedgerAssetSnapshot {
        engine_seq,
        ts_ns,
        state_root_blake3: state_root,
        asset: target_asset.to_string(),
        accounts: leaves,
    })
}

/// Reads and verifies a `HYPSNP01` snapshot file directly from disk.
pub fn load_hypsnp01_file(
    path: impl AsRef<Path>,
    target_asset: &str,
) -> SolvencyResult<ParsedLedgerAssetSnapshot> {
    let raw = fs::read(path.as_ref())
        .map_err(|e| SolvencyError::SerializationError(format!("read HYPSNP01 file: {e}")))?;
    parse_hypsnp01_bytes(&raw, target_asset)
}
