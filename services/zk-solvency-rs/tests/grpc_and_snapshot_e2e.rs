use std::sync::Arc;

use hyperion_omega_proto::{
    zk_solvency_service_client::ZkSolvencyServiceClient,
    zk_solvency_service_server::ZkSolvencyServiceServer, GenerateEpochSolvencyRequest,
    GetInclusionSumProofRequest, MsstHashScheme, VerifyZkSolvencyReceiptRequest, ZkProofSystem,
};
use hyperion_proto::U128;
use hyperion_zk_solvency::{
    msst::{AccountLeafInput, MsstTree},
    prover::{
        prove_epoch_solvency, prove_user_inclusion_solvency, EpochBatchKeyMaterial,
        InclusionKeyMaterial,
    },
    service::{ZkSolvencyEngine, ZkSolvencyGrpcHandler},
    snapshot_loader::{
        compute_snapshot_crc32c, parse_hypsnp01_bytes, SNAPSHOT_HEADER_LEN, SNAPSHOT_MAGIC,
        SNAPSHOT_VERSION,
    },
};
use hyperion_zk_verifier_wasm::{
    verify_epoch_solvency_json, verify_msst_inclusion_json, verify_user_solvency_bundle_json,
    WasmVerifyVerdict,
};
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::test]
async fn test_live_grpc_zk_solvency_service_end_to_end() {
    let engine = Arc::new(ZkSolvencyEngine::new([0x42u8; 32]).unwrap());
    let accounts: Vec<AccountLeafInput> = (0..1_024u64)
        .map(|i| AccountLeafInput {
            account_id: 50_000 + i,
            balance_fp: (i + 1) * 10_000_000,
        })
        .collect();
    let snap = engine
        .register_ledger_snapshot(7, 999_001, 420_000, "USDT", &accounts)
        .unwrap();
    let total_liab = snap.tree.total_liabilities_fp();
    let reserves = total_liab + 500_000_000_000;

    // Bind ephemeral localhost TCP port so parallel tests never collide on :50162
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = tokio::net::TcpListener::from_std(listener.into_std().unwrap()).unwrap();

    let server_engine = Arc::clone(&engine);
    let server_handle = tokio::spawn(async move {
        let incoming_stream = tonic::transport::server::TcpIncoming::from_listener(
            incoming,
            true,
            None,
        )
        .unwrap();
        Server::builder()
            .add_service(ZkSolvencyServiceServer::new(ZkSolvencyGrpcHandler::new(server_engine)))
            .serve_with_incoming(incoming_stream)
            .await
            .unwrap();
    });

    let endpoint = format!("http://{addr}");
    let mut client = ZkSolvencyServiceClient::connect(endpoint).await.unwrap();

    // 1. Reject insolvent request (`reserves = total_liab - 1`) with FAILED_PRECONDITION
    let insolvent_err = client
        .generate_epoch_solvency_proof(GenerateEpochSolvencyRequest {
            epoch_id: 7,
            snapshot_engine_seq: 999_001,
            asset: "USDT".to_string(),
            verified_onchain_reserves_fp: Some(U128::from(total_liab - 1)),
            l2_block_number: 1_234_567,
            l2_block_hash: vec![0xEE; 32],
            trace: None,
        })
        .await
        .unwrap_err();
    assert_eq!(insolvent_err.code(), tonic::Code::FailedPrecondition);

    // 2. Generate valid Epoch Solvency Receipt (`128`-byte BN254 Groth16 SNARK)
    let receipt = client
        .generate_epoch_solvency_proof(GenerateEpochSolvencyRequest {
            epoch_id: 7,
            snapshot_engine_seq: 999_001,
            asset: "USDT".to_string(),
            verified_onchain_reserves_fp: Some(U128::from(reserves)),
            l2_block_number: 1_234_567,
            l2_block_hash: vec![0xEE; 32],
            trace: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(receipt.proof_system, ZkProofSystem::Groth16Bn254 as i32);
    assert_eq!(
        receipt.hash_scheme,
        MsstHashScheme::HybridPoseidonBlake3 as i32
    );
    assert_eq!(receipt.proof_size_bytes, 128);
    assert_eq!(receipt.zk_proof_bytes.len(), 128);
    assert!(receipt.self_verified_ok);

    // 3. Verify authentic receipt via gRPC `VerifyZKSolvencyReceipt`
    let verify_ok = client
        .verify_zk_solvency_receipt(VerifyZkSolvencyReceiptRequest {
            receipt: Some(receipt.clone()),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(verify_ok.valid, "valid receipt rejected: {}", verify_ok.failure_reason);

    // 4. Tampered receipt (`zk_proof_bytes[0] ^= 1`) MUST be rejected (`valid == false`)
    let mut tampered = receipt.clone();
    tampered.zk_proof_bytes[0] ^= 0x01;
    let verify_bad = client
        .verify_zk_solvency_receipt(VerifyZkSolvencyReceiptRequest {
            receipt: Some(tampered),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!verify_bad.valid);
    assert!(!verify_bad.failure_reason.is_empty());

    // 5. Fetch individual `InclusionSumProof` for account `50_512`
    let inc = client
        .get_inclusion_sum_proof(GetInclusionSumProofRequest {
            epoch_id: 7,
            account_id: 50_512,
            asset: "USDT".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(inc.user_balance_fp, 513 * 10_000_000);
    assert_eq!(inc.siblings.len(), 10);
    assert_eq!(u128::from(inc.expected_root_sum_fp.unwrap()), total_liab);

    server_handle.abort();
}

#[test]
fn test_hypsnp01_channel02_snapshot_ingestion_and_crc_tamper_detection() {
    let state_root = [7u8; 32];
    let payload_json = serde_json::json!({
        "engine_seq": 88_001u64,
        "ts_ns": 1_795_020_000_000_000_000u64,
        "state_root_blake3": state_root,
        "accounts": [
            {
                "account_id": 1001u64,
                "balances": [
                    { "asset": "USDT", "available_fp": "150000000000", "locked_fp": "50000000000" },
                    { "asset": "BTC",  "available_fp": "100000000",    "locked_fp": "0" }
                ]
            },
            {
                "account_id": 1002u64,
                "balances": [
                    { "asset": "USDT", "available_fp": "300000000000", "locked_fp": "0" }
                ]
            }
        ]
    });
    let payload = serde_json::to_vec(&payload_json).unwrap();
    let engine_seq = 88_001u64;
    let ts_ns = 1_795_020_000_000_000_000u64;
    let crc = compute_snapshot_crc32c(engine_seq, ts_ns, &state_root, &payload);

    let mut raw = vec![0u8; SNAPSHOT_HEADER_LEN + payload.len()];
    raw[0..8].copy_from_slice(SNAPSHOT_MAGIC);
    raw[8..12].copy_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
    raw[12..16].copy_from_slice(&crc.to_le_bytes());
    raw[16..24].copy_from_slice(&engine_seq.to_le_bytes());
    raw[24..32].copy_from_slice(&ts_ns.to_le_bytes());
    raw[32..64].copy_from_slice(&state_root);
    raw[64..72].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    raw[SNAPSHOT_HEADER_LEN..].copy_from_slice(&payload);

    let parsed = parse_hypsnp01_bytes(&raw, "USDT").unwrap();
    assert_eq!(parsed.engine_seq, engine_seq);
    assert_eq!(parsed.accounts.len(), 2);
    assert_eq!(parsed.accounts[0].balance_fp, 200_000_000_000);
    assert_eq!(parsed.accounts[1].balance_fp, 300_000_000_000);

    // Build MSST directly from parsed Channel 02 snapshot!
    let tree = MsstTree::build_parallel(&parsed.accounts, 4, 1, &[0x11; 32]).unwrap();
    assert_eq!(tree.total_liabilities_fp(), 500_000_000_000);

    // 1-bit corruption in payload MUST fail CRC-32C check!
    let mut corrupted = raw.clone();
    corrupted[SNAPSHOT_HEADER_LEN + 5] ^= 0x01;
    assert!(parse_hypsnp01_bytes(&corrupted, "USDT").is_err());
}

#[test]
fn test_cross_crate_wasm_verifier_json_compatibility() {
    const D: usize = 6;
    const ARITY: usize = 4;
    let accounts: Vec<AccountLeafInput> = (0..64u64)
        .map(|i| AccountLeafInput {
            account_id: 7000 + i,
            balance_fp: (i + 1) * 50_000_000,
        })
        .collect();
    let tree = MsstTree::build_parallel(&accounts, D, 21, &[0x5A; 32]).unwrap();
    let reserves = tree.total_liabilities_fp() + 100_000_000;

    let ep_keys = EpochBatchKeyMaterial::<ARITY>::setup_deterministic(false, 401).unwrap();
    let inc_keys = InclusionKeyMaterial::<D>::setup_deterministic(402).unwrap();

    let ep_receipt = prove_epoch_solvency::<ARITY>(&ep_keys, &tree, reserves, [0x11; 32]).unwrap();
    let usr_bundle =
        prove_user_inclusion_solvency::<D>(&inc_keys, &tree, 7019, reserves, [0x22; 32]).unwrap();

    // Verify Epoch Receipt JSON via Wasm Verifier crate
    let ep_json = serde_json::to_string(&ep_receipt).unwrap();
    let ep_verdict: WasmVerifyVerdict =
        serde_json::from_str(&verify_epoch_solvency_json(&ep_json)).unwrap();
    assert!(ep_verdict.valid, "Wasm epoch verify failed: {:?}", ep_verdict.error);

    // Verify User ZK Bundle JSON via Wasm Verifier crate
    let usr_json = serde_json::to_string(&usr_bundle).unwrap();
    let usr_verdict: WasmVerifyVerdict =
        serde_json::from_str(&verify_user_solvency_bundle_json(&usr_json)).unwrap();
    assert!(usr_verdict.valid, "Wasm user bundle verify failed: {:?}", usr_verdict.error);
    assert_eq!(
        usr_verdict.reconstructed_liabilities_fp.as_deref(),
        Some(tree.total_liabilities_fp().to_string().as_str())
    );

    // Verify standalone MSST Inclusion JSON via Wasm Verifier crate
    let inc_json = serde_json::to_string(&usr_bundle.inclusion).unwrap();
    let inc_verdict: WasmVerifyVerdict =
        serde_json::from_str(&verify_msst_inclusion_json(&inc_json)).unwrap();
    assert!(inc_verdict.valid);

    // Tampered bundle MUST be rejected by Wasm Verifier!
    let mut bad_bundle = usr_bundle.clone();
    bad_bundle.verified_reserves_fp = tree.total_liabilities_fp() - 1;
    let bad_verdict: WasmVerifyVerdict = serde_json::from_str(&verify_user_solvency_bundle_json(
        &serde_json::to_string(&bad_bundle).unwrap(),
    ))
    .unwrap();
    assert!(!bad_verdict.valid);
}
