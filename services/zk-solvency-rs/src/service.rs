use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ark_bn254::{Bn254, Fr};
use ark_groth16::{Groth16, Proof};
use ark_serialize::CanonicalDeserialize;
use hyperion_omega_proto::{
    zk_solvency_service_server::ZkSolvencyService, AssetSolvencySummary,
    GenerateEpochSolvencyRequest, GetInclusionSumProofRequest, InclusionSumProof,
    MerkleSumRootAnnouncement, MerkleSumSiblingNode, MsstHashScheme,
    VerifyZkSolvencyReceiptRequest, VerifyZkSolvencyReceiptResponse, ZkProofSystem,
    ZkSolvencyReceipt,
};
use hyperion_proto::U128;
use tonic::{Request, Response, Status};

use crate::error::SolvencyError;
use crate::msst::{AccountLeafInput, MsstTree};
use crate::poseidon_params::{fr_from_bytes_le, fr_to_bytes_le};
use crate::prover::{prove_epoch_solvency, EpochBatchKeyMaterial, EpochSolvencyReceipt};
use crate::verifier::{verify_epoch_solvency_with_pvk, verify_msst_inclusion_proof};

/// Canonical top-subtree arity (`2^5 = 32` partition subtrees) for Epoch Groth16 SNARKs.
pub const SERVICE_EPOCH_ARITY_LOG2: usize = 5;
/// Minimum MSST depth for service snapshots (`>= SERVICE_EPOCH_ARITY_LOG2`).
pub const SERVICE_MIN_MSST_DEPTH: usize = 10;

#[derive(Clone)]
pub struct SnapshotRecord {
    pub epoch_id: u64,
    pub snapshot_engine_seq: u64,
    pub snapshot_raft_index: u64,
    pub asset: String,
    pub tree: Arc<MsstTree>,
}

pub struct SolvencyMetrics {
    pub epochs_proven_total: AtomicU64,
    pub inclusion_proofs_served_total: AtomicU64,
    pub verifications_total: AtomicU64,
    pub verifications_failed_total: AtomicU64,
    pub last_prove_latency_ms: AtomicU64,
    pub last_verify_latency_us: AtomicU64,
}

impl Default for SolvencyMetrics {
    fn default() -> Self {
        Self {
            epochs_proven_total: AtomicU64::new(0),
            inclusion_proofs_served_total: AtomicU64::new(0),
            verifications_total: AtomicU64::new(0),
            verifications_failed_total: AtomicU64::new(0),
            last_prove_latency_ms: AtomicU64::new(0),
            last_verify_latency_us: AtomicU64::new(0),
        }
    }
}

impl SolvencyMetrics {
    pub fn render_prometheus(&self) -> String {
        format!(
            "# HELP hyperion_zk_epochs_proven_total Total epoch ZK solvency proofs generated\n\
             # TYPE hyperion_zk_epochs_proven_total counter\n\
             hyperion_zk_epochs_proven_total {}\n\
             # HELP hyperion_zk_inclusion_proofs_served_total Total MSST inclusion proofs served\n\
             # TYPE hyperion_zk_inclusion_proofs_served_total counter\n\
             hyperion_zk_inclusion_proofs_served_total {}\n\
             # HELP hyperion_zk_verifications_total Total ZK receipt verifications executed\n\
             # TYPE hyperion_zk_verifications_total counter\n\
             hyperion_zk_verifications_total {}\n\
             # HELP hyperion_zk_verifications_failed_total Total ZK receipt verifications rejected\n\
             # TYPE hyperion_zk_verifications_failed_total counter\n\
             hyperion_zk_verifications_failed_total {}\n\
             # HELP hyperion_zk_last_prove_latency_ms Latency of last epoch Groth16 proof in ms\n\
             # TYPE hyperion_zk_last_prove_latency_ms gauge\n\
             hyperion_zk_last_prove_latency_ms {}\n\
             # HELP hyperion_zk_last_verify_latency_us Latency of last Groth16 verification in us\n\
             # TYPE hyperion_zk_last_verify_latency_us gauge\n\
             hyperion_zk_last_verify_latency_us {}\n",
            self.epochs_proven_total.load(Ordering::Relaxed),
            self.inclusion_proofs_served_total.load(Ordering::Relaxed),
            self.verifications_total.load(Ordering::Relaxed),
            self.verifications_failed_total.load(Ordering::Relaxed),
            self.last_prove_latency_ms.load(Ordering::Relaxed),
            self.last_verify_latency_us.load(Ordering::Relaxed),
        )
    }
}

pub struct ZkSolvencyEngine {
    epoch_keys: Arc<EpochBatchKeyMaterial<SERVICE_EPOCH_ARITY_LOG2>>,
    vk_blake3_digest: [u8; 32],
    master_seed: [u8; 32],
    snapshots: RwLock<HashMap<(u64, String), SnapshotRecord>>,
    receipts: RwLock<HashMap<(u64, String), ZkSolvencyReceipt>>,
    pub metrics: Arc<SolvencyMetrics>,
}

impl ZkSolvencyEngine {
    pub fn new(master_seed: [u8; 32]) -> Result<Self, SolvencyError> {
        let epoch_keys = Arc::new(
            EpochBatchKeyMaterial::<SERVICE_EPOCH_ARITY_LOG2>::setup_deterministic(
                false,
                0x4859_504F_4D45_4741,
            )?,
        );
        let vk_blake3_digest = *blake3::hash(&epoch_keys.vk_bytes).as_bytes();
        Ok(Self {
            epoch_keys,
            vk_blake3_digest,
            master_seed,
            snapshots: RwLock::new(HashMap::new()),
            receipts: RwLock::new(HashMap::new()),
            metrics: Arc::new(SolvencyMetrics::default()),
        })
    }

    #[inline]
    pub fn vk_bytes(&self) -> &[u8] {
        &self.epoch_keys.vk_bytes
    }

    #[inline]
    pub fn vk_digest(&self) -> [u8; 32] {
        self.vk_blake3_digest
    }

    /// Registers a ledger balance snapshot and constructs its parallel Poseidon+Blake3 MSST.
    pub fn register_ledger_snapshot(
        &self,
        epoch_id: u64,
        snapshot_engine_seq: u64,
        snapshot_raft_index: u64,
        asset: &str,
        accounts: &[AccountLeafInput],
    ) -> Result<SnapshotRecord, SolvencyError> {
        let depth = MsstTree::required_depth(accounts.len()).max(SERVICE_MIN_MSST_DEPTH);
        let tree = Arc::new(MsstTree::build_parallel(
            accounts,
            depth,
            epoch_id,
            &self.master_seed,
        )?);
        let rec = SnapshotRecord {
            epoch_id,
            snapshot_engine_seq,
            snapshot_raft_index,
            asset: asset.to_string(),
            tree,
        };
        if let Ok(mut guard) = self.snapshots.write() {
            guard.insert((epoch_id, asset.to_string()), rec.clone());
        }
        Ok(rec)
    }

    /// Returns the most recently generated `ZkSolvencyReceipt` for `(epoch_id, asset)` if cached.
    pub fn cached_receipt(&self, epoch_id: u64, asset: &str) -> Option<ZkSolvencyReceipt> {
        self.receipts
            .read()
            .ok()
            .and_then(|g| g.get(&(epoch_id, asset.to_string())).cloned())
    }

    fn now_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }

    /// Integer-exact collateralization ratio in parts-per-million (`1_000_000 = 100.0000%`).
    /// Zero floating-point arithmetic (`00-hyperion-contract.md` §5.1).
    pub fn compute_reserve_ratio_ppm(reserves_fp: u128, liabilities_fp: u128) -> u64 {
        if liabilities_fp == 0 {
            return 1_000_000;
        }
        let scaled = reserves_fp.saturating_mul(1_000_000);
        let ratio = scaled / liabilities_fp;
        ratio.min(u128::from(u64::MAX)) as u64
    }

    /// Direct Rust verification helper for `hyperion.omega.v1.ZKSolvencyReceipt`.
    pub fn verify_proto_receipt_inner(&self, rcpt: &ZkSolvencyReceipt) -> Result<(), String> {
        if rcpt.proof_system != ZkProofSystem::Groth16Bn254 as i32 {
            return Err(format!("unsupported proof_system {}", rcpt.proof_system));
        }
        if rcpt.verifying_key_digest.as_slice() != self.vk_blake3_digest {
            return Err("verifying_key_digest mismatch against anchored VK".to_string());
        }
        if rcpt.zk_proof_bytes.len() != 128 {
            return Err(format!(
                "invalid BN254 Groth16 proof byte length: expected 128, got {}",
                rcpt.zk_proof_bytes.len()
            ));
        }
        if rcpt.public_inputs_le.len() != 4 {
            return Err(format!(
                "expected 4 public inputs, got {}",
                rcpt.public_inputs_le.len()
            ));
        }

        let ann = rcpt
            .root_announcement
            .as_ref()
            .ok_or_else(|| "missing root_announcement".to_string())?;
        if ann.epoch_id != rcpt.epoch_id {
            return Err("epoch_id mismatch between receipt and root_announcement".to_string());
        }
        if ann.root_hash != rcpt.public_inputs_le[0] {
            return Err("root_hash mismatch against public_inputs_le[0]".to_string());
        }

        let liab_fp = ann
            .root_liability_sum_fp
            .map(u128::from)
            .ok_or_else(|| "missing root_liability_sum_fp".to_string())?;
        let res_fp = ann
            .onchain_reserve_sum_fp
            .map(u128::from)
            .ok_or_else(|| "missing onchain_reserve_sum_fp".to_string())?;

        if res_fp < liab_fp {
            return Err(format!(
                "declared reserves ({res_fp}) < root liabilities ({liab_fp})"
            ));
        }

        let expected_res_bytes = fr_to_bytes_le(Fr::from(res_fp));
        if rcpt.public_inputs_le[2].as_slice() != expected_res_bytes {
            return Err("onchain_reserve_sum_fp mismatch against public_inputs_le[2]".to_string());
        }
        let expected_epoch_bytes = fr_to_bytes_le(Fr::from(rcpt.epoch_id));
        if rcpt.public_inputs_le[3].as_slice() != expected_epoch_bytes {
            return Err("epoch_id mismatch against public_inputs_le[3]".to_string());
        }

        let mut pub_frs = Vec::with_capacity(4);
        for chunk in &rcpt.public_inputs_le {
            let arr: [u8; 32] = chunk
                .as_slice()
                .try_into()
                .map_err(|_| "public input chunk is not 32 bytes".to_string())?;
            pub_frs.push(fr_from_bytes_le(&arr).map_err(|e| e.to_string())?);
        }

        let proof = Proof::<Bn254>::deserialize_compressed(&rcpt.zk_proof_bytes[..])
            .map_err(|e| format!("invalid Groth16 curve point: {e}"))?;

        let ok = Groth16::<Bn254>::verify_proof(&self.epoch_keys.pvk, &proof, &pub_frs)
            .map_err(|e| format!("Groth16 pairing error: {e}"))?;
        if !ok {
            return Err("Groth16 pairing equation rejected proof".to_string());
        }
        Ok(())
    }
}

/// Thin cloneable Tonic gRPC handler wrapper satisfying Rust's trait coherence (orphan) rules.
#[derive(Clone)]
pub struct ZkSolvencyGrpcHandler(pub Arc<ZkSolvencyEngine>);

impl ZkSolvencyGrpcHandler {
    pub fn new(engine: Arc<ZkSolvencyEngine>) -> Self {
        Self(engine)
    }
}

#[tonic::async_trait]
impl ZkSolvencyService for ZkSolvencyGrpcHandler {
    async fn generate_epoch_solvency_proof(
        &self,
        request: Request<GenerateEpochSolvencyRequest>,
    ) -> Result<Response<ZkSolvencyReceipt>, Status> {
        let engine_ref = &self.0;
        let req = request.into_inner();
        let asset = if req.asset.is_empty() {
            "USDT".to_string()
        } else {
            req.asset.clone()
        };

        let verified_reserves_fp = req
            .verified_onchain_reserves_fp
            .map(u128::from)
            .ok_or_else(|| Status::invalid_argument("missing verified_onchain_reserves_fp"))?;

        let snapshot = {
            let guard = engine_ref
                .snapshots
                .read()
                .map_err(|_| Status::internal("snapshot RwLock poisoned"))?;
            guard.get(&(req.epoch_id, asset.clone())).cloned()
        }
        .ok_or_else(|| {
            Status::not_found(format!(
                "ledger snapshot for epoch_id={} asset={} not registered",
                req.epoch_id, asset
            ))
        })?;

        let engine = Arc::clone(engine_ref);
        let asset_clone = asset.clone();
        let l2_block_hash = req.l2_block_hash.clone();
        let trace = req.trace.clone();

        let receipt = tokio::task::spawn_blocking(move || -> Result<ZkSolvencyReceipt, Box<Status>> {
            let t0 = Instant::now();
            let mut entropy = [0u8; 32];
            entropy[..8].copy_from_slice(&req.epoch_id.to_le_bytes());
            entropy[8..16].copy_from_slice(&req.snapshot_engine_seq.to_le_bytes());
            entropy[16..32].copy_from_slice(&engine.master_seed[..16]);

            let ep_receipt: EpochSolvencyReceipt = prove_epoch_solvency::<SERVICE_EPOCH_ARITY_LOG2>(
                &engine.epoch_keys,
                &snapshot.tree,
                verified_reserves_fp,
                entropy,
            )
            .map_err(|err| {
                Box::new(match err {
                    SolvencyError::InsufficientReserves { .. } => {
                        Status::failed_precondition(err.to_string())
                    }
                    other => Status::internal(other.to_string()),
                })
            })?;

            let prove_ms = t0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

            let self_ok = verify_epoch_solvency_with_pvk::<SERVICE_EPOCH_ARITY_LOG2>(
                &engine.epoch_keys.pvk,
                &ep_receipt,
            )
            .is_ok();
            if !self_ok {
                return Err(Box::new(Status::internal(
                    "self-verification of Groth16 proof failed",
                )));
            }

            let total_liab = snapshot.tree.total_liabilities_fp();
            let surplus = verified_reserves_fp - total_liab;
            let ratio_ppm =
                ZkSolvencyEngine::compute_reserve_ratio_ppm(verified_reserves_fp, total_liab);
            let now_ns = ZkSolvencyEngine::now_ns();

            let announcement = MerkleSumRootAnnouncement {
                epoch_id: req.epoch_id,
                snapshot_engine_seq: snapshot.snapshot_engine_seq.max(req.snapshot_engine_seq),
                snapshot_raft_index: snapshot.snapshot_raft_index,
                asset: asset_clone.clone(),
                tree_height: snapshot.tree.depth as u32,
                active_account_count: snapshot.tree.active_accounts as u64,
                hash_scheme: MsstHashScheme::HybridPoseidonBlake3 as i32,
                root_hash: ep_receipt.root_poseidon_bytes.to_vec(),
                root_liability_sum_fp: Some(U128::from(total_liab)),
                onchain_reserve_sum_fp: Some(U128::from(verified_reserves_fp)),
                l2_reference_block_number: req.l2_block_number,
                l2_reference_block_hash: l2_block_hash,
                announced_ts_ns: now_ns,
                trace: trace.clone(),
            };

            let summary = AssetSolvencySummary {
                asset: asset_clone.clone(),
                total_liabilities_fp: Some(U128::from(total_liab)),
                verified_reserves_fp: Some(U128::from(verified_reserves_fp)),
                surplus_equity_fp: Some(U128::from(surplus)),
                reserve_ratio_ppm: ratio_ppm,
            };

            let public_inputs_le = vec![
                ep_receipt.root_poseidon_bytes.to_vec(),
                ep_receipt.root_commitment_bytes.to_vec(),
                fr_to_bytes_le(Fr::from(verified_reserves_fp)).to_vec(),
                fr_to_bytes_le(Fr::from(req.epoch_id)).to_vec(),
            ];

            let proof_len = ep_receipt.groth16_proof_bytes.len() as u32;
            let proto_receipt = ZkSolvencyReceipt {
                receipt_id: format!("zk-solvency-{}-{asset_clone}", req.epoch_id),
                epoch_id: req.epoch_id,
                snapshot_engine_seq: announcement.snapshot_engine_seq,
                proof_system: ZkProofSystem::Groth16Bn254 as i32,
                hash_scheme: MsstHashScheme::HybridPoseidonBlake3 as i32,
                root_announcement: Some(announcement),
                asset_summaries: vec![summary],
                zk_proof_bytes: ep_receipt.groth16_proof_bytes,
                public_inputs_le,
                verifying_key_digest: engine.vk_blake3_digest.to_vec(),
                proving_latency_ms: prove_ms,
                proof_size_bytes: proof_len,
                self_verified_ok: true,
                created_ts_ns: now_ns,
                trace,
            };

            engine
                .metrics
                .epochs_proven_total
                .fetch_add(1, Ordering::Relaxed);
            engine
                .metrics
                .last_prove_latency_ms
                .store(prove_ms, Ordering::Relaxed);

            if let Ok(mut guard) = engine.receipts.write() {
                guard.insert((req.epoch_id, asset_clone), proto_receipt.clone());
            }

            Ok(proto_receipt)
        })
        .await
        .map_err(|e| Status::internal(format!("prover join error: {e}")))?
        .map_err(|boxed| *boxed)?;

        Ok(Response::new(receipt))
    }

    async fn get_inclusion_sum_proof(
        &self,
        request: Request<GetInclusionSumProofRequest>,
    ) -> Result<Response<InclusionSumProof>, Status> {
        let engine_ref = &self.0;
        let req = request.into_inner();
        let asset = if req.asset.is_empty() {
            "USDT".to_string()
        } else {
            req.asset
        };

        let snapshot = {
            let guard = engine_ref
                .snapshots
                .read()
                .map_err(|_| Status::internal("snapshot RwLock poisoned"))?;
            guard.get(&(req.epoch_id, asset.clone())).cloned()
        }
        .ok_or_else(|| {
            Status::not_found(format!(
                "snapshot for epoch_id={} asset={} not found",
                req.epoch_id, asset
            ))
        })?;

        let msst_proof = snapshot
            .tree
            .generate_inclusion_proof(req.account_id)
            .map_err(|e| match e {
                SolvencyError::AccountNotFound { .. } => Status::not_found(e.to_string()),
                other => Status::internal(other.to_string()),
            })?;

        let verified_sum = verify_msst_inclusion_proof(&msst_proof)
            .map_err(|e| Status::internal(format!("internal MSST proof check failed: {e}")))?;

        let siblings: Vec<MerkleSumSiblingNode> = msst_proof
            .path
            .iter()
            .enumerate()
            .map(|(lv, step)| MerkleSumSiblingNode {
                level: lv as u32,
                sibling_is_left: step.is_right_child,
                sibling_hash: step.sibling_poseidon_bytes.to_vec(),
                sibling_sum_fp: Some(U128::from(step.sibling_sum_fp)),
            })
            .collect();

        engine_ref
            .metrics
            .inclusion_proofs_served_total
            .fetch_add(1, Ordering::Relaxed);

        Ok(Response::new(InclusionSumProof {
            epoch_id: req.epoch_id,
            snapshot_engine_seq: snapshot.snapshot_engine_seq,
            asset,
            blinded_account_leaf_id: msst_proof.user_commitment_bytes.to_vec(),
            user_balance_fp: msst_proof.balance_fp,
            leaf_salt_commitment: msst_proof.leaf_salt_bytes.to_vec(),
            leaf_hash: msst_proof.leaf_poseidon_bytes.to_vec(),
            siblings,
            expected_root_hash: msst_proof.root_poseidon_bytes.to_vec(),
            expected_root_sum_fp: Some(U128::from(verified_sum)),
            hash_scheme: MsstHashScheme::HybridPoseidonBlake3 as i32,
            generated_ts_ns: ZkSolvencyEngine::now_ns(),
        }))
    }

    async fn verify_zk_solvency_receipt(
        &self,
        request: Request<VerifyZkSolvencyReceiptRequest>,
    ) -> Result<Response<VerifyZkSolvencyReceiptResponse>, Status> {
        let engine_ref = &self.0;
        let t0 = Instant::now();
        engine_ref
            .metrics
            .verifications_total
            .fetch_add(1, Ordering::Relaxed);

        let Some(rcpt) = request.into_inner().receipt else {
            engine_ref
                .metrics
                .verifications_failed_total
                .fetch_add(1, Ordering::Relaxed);
            return Ok(Response::new(VerifyZkSolvencyReceiptResponse {
                valid: false,
                verification_latency_us: t0.elapsed().as_micros() as u64,
                failure_reason: "missing ZKSolvencyReceipt".to_string(),
            }));
        };

        let verdict = engine_ref.verify_proto_receipt_inner(&rcpt);
        let elapsed_us = t0.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        engine_ref
            .metrics
            .last_verify_latency_us
            .store(elapsed_us, Ordering::Relaxed);

        match verdict {
            Ok(()) => Ok(Response::new(VerifyZkSolvencyReceiptResponse {
                valid: true,
                verification_latency_us: elapsed_us,
                failure_reason: String::new(),
            })),
            Err(reason) => {
                engine_ref
                    .metrics
                    .verifications_failed_total
                    .fetch_add(1, Ordering::Relaxed);
                Ok(Response::new(VerifyZkSolvencyReceiptResponse {
                    valid: false,
                    verification_latency_us: elapsed_us,
                    failure_reason: reason,
                }))
            }
        }
    }
}
