//! Aegis SRE Cryptographic Mitigation Bridge (`Ed25519` + `Blake3` Hash Chain + Sub-ms RCU Swap).
//!
//! Consumes emergency `RiskMitigationCommand` messages from Channel 06 (`aegis-agent`) via:
//!   1. gRPC `NetGuardControl::ApplyMitigation` (port `50165`)
//!   2. HTTP REST `/api/v1/netguard/mitigations` (port `8095`)
//!   3. Redpanda topic `hyperion.risk.mitigations.v1` (Pandaproxy / Kafka wire consumer)
//!
//! Publishes structured XDP drop + RTT histogram snapshots to `hyperion.omega.netguard.drops.v1`.

use crate::lpm::{parse_cidr_spec, CidrEntry};
use crate::pipeline::NetGuardSharedState;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use hyperion_proto::{MitigationActionType, RiskMitigationCommand};
use netguard_common::{BlockRuleVal, RuleAction};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Canonical Redpanda topics (`00-hyperion-contract.md` & `01-hyperion-omega.md`).
pub const TOPIC_RISK_MITIGATIONS: &str = "hyperion.risk.mitigations.v1";
pub const TOPIC_NETGUARD_DROPS: &str = "hyperion.omega.netguard.drops.v1";

/// Derives the exact default Channel 06 (`services/aegis-agent/src/crypto/signer.ts:94`)
/// Ed25519 public verifying key (`blake3("hyperion-aegis-ch06-default-ed25519-seed-v1")`).
pub fn default_aegis_ch06_verifying_key() -> VerifyingKey {
    let seed = *blake3::hash(b"hyperion-aegis-ch06-default-ed25519-seed-v1").as_bytes();
    SigningKey::from_bytes(&seed).verifying_key()
}

/// Computes Aegis CH06's exact canonical unsigned Protobuf wire payload (`signer.ts:298-310`):
/// encodes `RiskMitigationCommand` with `ed25519_signature` cleared to empty.
pub fn aegis_ch06_unsigned_protobuf_bytes(cmd: &RiskMitigationCommand) -> Vec<u8> {
    let mut unsigned = cmd.clone();
    unsigned.ed25519_signature.clear();
    unsigned.encode_to_vec()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CidrRuleInput {
    pub cidr: String,
    pub rule_action: u32,
    pub rate_override_rps: u32,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitigationOutcome {
    pub applied: bool,
    pub dry_run: bool,
    pub rcu_generation: u64,
    pub apply_latency_ns: u64,
    pub rollback_token: String,
    pub active_cidr_rules: u32,
    pub active_account_rules: u32,
    pub preserved_established_sessions: u64,
    pub audit_blake3_hex: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub enum AegisAuthError {
    ExpiredCommand { expires_ts_ns: u64, now_ns: u64 },
    ReplayCommandId(String),
    RequiresHumanApproval(String),
    InvalidSignatureLength(usize),
    SignatureVerificationFailed,
    AuditBlake3Mismatch,
    InvalidProtobuf(String),
    InvalidCidr(String),
}

impl core::fmt::Display for AegisAuthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ExpiredCommand {
                expires_ts_ns,
                now_ns,
            } => write!(f, "command expired ({expires_ts_ns} <= {now_ns})"),
            Self::ReplayCommandId(id) => write!(f, "duplicate command_id replay: {id}"),
            Self::RequiresHumanApproval(id) => {
                write!(f, "command {id} requires human-in-the-loop approval")
            }
            Self::InvalidSignatureLength(n) => write!(f, "invalid Ed25519 signature len: {n}"),
            Self::SignatureVerificationFailed => write!(f, "Ed25519 signature verification failed"),
            Self::AuditBlake3Mismatch => write!(f, "Blake3 audit chain digest mismatch"),
            Self::InvalidProtobuf(e) => write!(f, "invalid RiskMitigationCommand protobuf: {e}"),
            Self::InvalidCidr(e) => write!(f, "invalid CIDR specification: {e}"),
        }
    }
}

impl std::error::Error for AegisAuthError {}

/// Computes canonical preimage bytes for `RiskMitigationCommand` Ed25519 signing & Blake3 chaining.
pub fn canonical_risk_command_preimage(cmd: &RiskMitigationCommand) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(b"HYPERION_RISK_MITIGATION_V1\n");
    buf.extend_from_slice(cmd.command_id.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(cmd.incident_id.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(&cmd.action.to_le_bytes());
    for &acct in &cmd.target_account_ids {
        buf.extend_from_slice(&acct.to_le_bytes());
    }
    buf.extend_from_slice(cmd.target_symbol.as_bytes());
    buf.push(b'\n');
    buf.extend_from_slice(&cmd.rate_limit_override_rps.to_le_bytes());
    buf.extend_from_slice(&cmd.cooldown_duration_ms.to_le_bytes());
    buf.push(u8::from(cmd.dry_run));
    buf.extend_from_slice(cmd.rollback_token.as_bytes());
    buf.extend_from_slice(&cmd.issued_ts_ns.to_le_bytes());
    buf.extend_from_slice(&cmd.expires_ts_ns.to_le_bytes());
    buf
}

/// Computes chained Blake3 audit digest `blake3(prev_digest || canonical_preimage)`.
pub fn compute_chained_audit_blake3(prev_digest: &[u8; 32], preimage: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(prev_digest);
    hasher.update(preimage);
    *hasher.finalize().as_bytes()
}

#[inline]
pub fn now_unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1_700_000_000_000_000_000)
}

/// Stateful cryptographic verifier & sub-millisecond RCU mitigation executor.
pub struct AegisMitigationBridge {
    shared: Arc<NetGuardSharedState>,
    verifying_key: Option<VerifyingKey>,
    seen_command_ids: Mutex<HashSet<String>>,
    audit_head: Mutex<[u8; 32]>,
    outbox_drops_json: Mutex<Vec<(String, String)>>,
}

impl AegisMitigationBridge {
    pub fn new(shared: Arc<NetGuardSharedState>, verifying_key: Option<VerifyingKey>) -> Arc<Self> {
        Arc::new(Self {
            shared,
            verifying_key,
            seen_command_ids: Mutex::new(HashSet::new()),
            audit_head: Mutex::new([0_u8; 32]),
            outbox_drops_json: Mutex::new(Vec::new()),
        })
    }

    pub fn current_audit_head_hex(&self) -> String {
        let head = *self.audit_head.lock().unwrap_or_else(|e| e.into_inner());
        hex::encode(head)
    }

    /// Verifies and applies a serialized `hyperion.v1.RiskMitigationCommand` from Aegis SRE.
    pub fn apply_signed_protobuf_command(
        &self,
        pb_bytes: &[u8],
        extra_cidrs: &[CidrRuleInput],
        preserve_established: bool,
        now_ns: u64,
    ) -> Result<MitigationOutcome, AegisAuthError> {
        let cmd = RiskMitigationCommand::decode(pb_bytes)
            .map_err(|e| AegisAuthError::InvalidProtobuf(e.to_string()))?;
        self.apply_risk_command(&cmd, extra_cidrs, preserve_established, now_ns)
    }

    /// Verifies Ed25519 signature + anti-replay + TTL + Human-in-the-Loop gate, then atomically
    /// swaps the RCU BPF policy snapshot in sub-millisecond latency.
    pub fn apply_risk_command(
        &self,
        cmd: &RiskMitigationCommand,
        extra_cidrs: &[CidrRuleInput],
        preserve_established: bool,
        now_ns: u64,
    ) -> Result<MitigationOutcome, AegisAuthError> {
        let t0 = Instant::now();

        if cmd.requires_human_approval {
            return Err(AegisAuthError::RequiresHumanApproval(
                cmd.command_id.clone(),
            ));
        }
        if cmd.expires_ts_ns != 0 && cmd.expires_ts_ns <= now_ns {
            return Err(AegisAuthError::ExpiredCommand {
                expires_ts_ns: cmd.expires_ts_ns,
                now_ns,
            });
        }

        let preimage = canonical_risk_command_preimage(cmd);
        let unsigned_pb = aegis_ch06_unsigned_protobuf_bytes(cmd);

        // Enforce Ed25519 cryptographic authenticity if a verifying key is configured:
        // accepts BOTH Aegis CH06 canonical unsigned-Protobuf signatures (`signer.ts:309`)
        // AND `canonical_risk_command_preimage` signatures.
        let mut verified_ch06_wire = false;
        if let Some(vk) = &self.verifying_key {
            let sig_bytes: [u8; 64] = cmd
                .ed25519_signature
                .as_slice()
                .try_into()
                .map_err(|_| AegisAuthError::InvalidSignatureLength(cmd.ed25519_signature.len()))?;
            let sig = Signature::from_bytes(&sig_bytes);
            if vk.verify(&unsigned_pb, &sig).is_ok() {
                verified_ch06_wire = true;
            } else {
                vk.verify(&preimage, &sig)
                    .map_err(|_| AegisAuthError::SignatureVerificationFailed)?;
            }
        }

        // Verify optional Blake3 audit digest if provided by caller (when signed via CH06 wire,
        // `audit_chain_blake3` carries Aegis's upstream chain digest, which we link into ours!)
        let next_audit = {
            let head = *self.audit_head.lock().unwrap_or_else(|e| e.into_inner());
            let expected = compute_chained_audit_blake3(&head, &preimage);
            if !verified_ch06_wire
                && !cmd.audit_chain_blake3.is_empty()
                && cmd.audit_chain_blake3.as_slice() != expected.as_slice()
            {
                return Err(AegisAuthError::AuditBlake3Mismatch);
            }
            if verified_ch06_wire && !cmd.audit_chain_blake3.is_empty() {
                let mut h = blake3::Hasher::new();
                h.update(&head);
                h.update(&cmd.audit_chain_blake3);
                h.update(&unsigned_pb);
                *h.finalize().as_bytes()
            } else {
                expected
            }
        };

        // Anti-replay check on `command_id`
        if !cmd.command_id.is_empty() {
            let mut seen = self
                .seen_command_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !seen.insert(cmd.command_id.clone()) {
                return Err(AegisAuthError::ReplayCommandId(cmd.command_id.clone()));
            }
        }

        let rollback_token = if cmd.rollback_token.is_empty() {
            format!("rb-{}", cmd.command_id)
        } else {
            cmd.rollback_token.clone()
        };

        let expires_ts_ns = if cmd.expires_ts_ns != 0 {
            cmd.expires_ts_ns
        } else if cmd.cooldown_duration_ms > 0 {
            now_ns.saturating_add(cmd.cooldown_duration_ms.saturating_mul(1_000_000))
        } else {
            0
        };

        let action_enum = MitigationActionType::try_from(cmd.action)
            .unwrap_or(MitigationActionType::Unspecified);

        // Handle RESTORE_NORMAL as atomic rollback
        if action_enum == MitigationActionType::RestoreNormal && !cmd.dry_run {
            let (_, snap) = self.shared.policy_store.revoke(
                &rollback_token,
                &[],
                &cmd.target_account_ids,
            );
            *self.audit_head.lock().unwrap_or_else(|e| e.into_inner()) = next_audit;
            let elapsed_ns = t0.elapsed().as_nanos() as u64;
            return Ok(MitigationOutcome {
                applied: true,
                dry_run: false,
                rcu_generation: snap.generation,
                apply_latency_ns: elapsed_ns,
                rollback_token,
                active_cidr_rules: snap.ipv4_rule_count + snap.ipv6_rule_count,
                active_account_rules: snap.account_rules.len() as u32,
                preserved_established_sessions: self.shared.conntrack.active_count(),
                audit_blake3_hex: hex::encode(next_audit),
                detail: "restored normal policy via atomic RCU swap".to_string(),
            });
        }

        let acct_rule_action = match action_enum {
            MitigationActionType::FreezeAccount => RuleAction::HardDrop,
            MitigationActionType::ThrottleRateLimit => RuleAction::ThrottleRps,
            MitigationActionType::EnableVolatilityGuard => RuleAction::DropNewSynOnly,
            _ => RuleAction::ThrottleRps,
        };

        let mut cidr_entries = Vec::with_capacity(extra_cidrs.len());
        for c in extra_cidrs {
            let (ip, prefix_len) =
                parse_cidr_spec(&c.cidr).map_err(AegisAuthError::InvalidCidr)?;
            let mut r_act = match c.rule_action {
                1 => RuleAction::HardDrop,
                2 => RuleAction::DropNewSynOnly,
                3 => RuleAction::ThrottleRps,
                4 => RuleAction::SynCookieTx,
                _ => RuleAction::HardDrop,
            };
            if preserve_established
                && r_act == RuleAction::HardDrop
                && ((ip.is_ipv4() && prefix_len < 32) || (ip.is_ipv6() && prefix_len < 128))
            {
                // Convert subnet quarantine into graceful `DropNewSynOnly` so active legitimate
                // connections are never severed mid-transaction!
                r_act = RuleAction::DropNewSynOnly;
            }
            let c_exp = if c.duration_ms > 0 {
                now_ns.saturating_add(c.duration_ms.saturating_mul(1_000_000))
            } else {
                expires_ts_ns
            };
            cidr_entries.push(CidrEntry {
                cidr_text: c.cidr.clone(),
                ip,
                prefix_len,
                rule: BlockRuleVal {
                    action: r_act,
                    rate_override_rps: c.rate_override_rps,
                    expires_ts_ns: c_exp,
                    rule_generation: 0,
                },
                rollback_token: rollback_token.clone(),
            });
        }

        let mut acct_entries = Vec::with_capacity(cmd.target_account_ids.len());
        for &acct_id in &cmd.target_account_ids {
            acct_entries.push((
                acct_id,
                BlockRuleVal {
                    action: acct_rule_action,
                    rate_override_rps: cmd.rate_limit_override_rps,
                    expires_ts_ns,
                    rule_generation: 0,
                },
                rollback_token.clone(),
            ));
        }

        let preserved = self.shared.conntrack.active_count();
        if cmd.dry_run {
            let snap = self.shared.policy_store.load_snapshot();
            let elapsed_ns = t0.elapsed().as_nanos() as u64;
            return Ok(MitigationOutcome {
                applied: false,
                dry_run: true,
                rcu_generation: snap.generation,
                apply_latency_ns: elapsed_ns,
                rollback_token,
                active_cidr_rules: snap.ipv4_rule_count + snap.ipv6_rule_count,
                active_account_rules: snap.account_rules.len() as u32,
                preserved_established_sessions: preserved,
                audit_blake3_hex: hex::encode(next_audit),
                detail: "dry-run validated signature and RCU delta without mutating live maps"
                    .to_string(),
            });
        }

        let snap = self.shared.policy_store.upsert_batch(cidr_entries, acct_entries);
        *self.audit_head.lock().unwrap_or_else(|e| e.into_inner()) = next_audit;
        let elapsed_ns = t0.elapsed().as_nanos() as u64;

        Ok(MitigationOutcome {
            applied: true,
            dry_run: false,
            rcu_generation: snap.generation,
            apply_latency_ns: elapsed_ns,
            rollback_token,
            active_cidr_rules: snap.ipv4_rule_count + snap.ipv6_rule_count,
            active_account_rules: snap.account_rules.len() as u32,
            preserved_established_sessions: preserved,
            audit_blake3_hex: hex::encode(next_audit),
            detail: format!("atomic RCU generation {} committed", snap.generation),
        })
    }

    /// Publishes a structured JSON telemetry & XDP drop snapshot keyed by `iface:<interface_name>`
    /// for Redpanda topic `hyperion.omega.netguard.drops.v1` (`01-hyperion-omega.md` §3).
    pub fn stage_redpanda_drop_snapshot(&self, now_ns: u64) -> (String, String) {
        let snap = self.shared.policy_store.load_snapshot();
        let report = self.shared.telemetry.snapshot(
            now_ns,
            snap.generation,
            snap.ipv4_rule_count,
            snap.ipv6_rule_count,
            snap.account_rules.len() as u32,
            self.shared.conntrack.active_count(),
        );
        let key = format!("iface:{}", report.interface_name);
        let payload = serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());
        let mut outbox = self
            .outbox_drops_json
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if outbox.len() >= 256 {
            outbox.remove(0);
        }
        outbox.push((key.clone(), payload.clone()));
        (key, payload)
    }

    pub fn drain_staged_redpanda_messages(&self) -> Vec<(String, String)> {
        let mut outbox = self
            .outbox_drops_json
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        core::mem::take(&mut *outbox)
    }
}
