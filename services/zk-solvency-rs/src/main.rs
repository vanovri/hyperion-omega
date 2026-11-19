#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::net::SocketAddr;
use std::sync::Arc;

use hyperion_omega_proto::zk_solvency_service_server::ZkSolvencyServiceServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tonic::transport::Server;

use hyperion_zk_solvency::{
    msst::AccountLeafInput,
    service::{ZkSolvencyEngine, ZkSolvencyGrpcHandler},
};

async fn serve_http_observability(
    addr: SocketAddr,
    engine: Arc<ZkSolvencyEngine>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        let eng = Arc::clone(&engine);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req_head = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_head.lines().next().unwrap_or("");

            let (status, content_type, body) = if first_line.starts_with("GET /healthz") {
                (
                    "200 OK",
                    "application/json",
                    r#"{"status":"healthy","service":"zk-solvency-rs","grpc_port":50162,"http_port":8092,"proof_system":"GROTH16_BN254","hash_scheme":"HYBRID_POSEIDON_BLAKE3"}"#.to_string(),
                )
            } else if first_line.starts_with("GET /metrics") {
                (
                    "200 OK",
                    "text/plain; version=0.0.4",
                    eng.metrics.render_prometheus(),
                )
            } else if first_line.starts_with("GET /api/v1/zk/vk") {
                let vk_hex = hex::encode(eng.vk_bytes());
                let digest_hex = hex::encode(eng.vk_digest());
                (
                    "200 OK",
                    "application/json",
                    format!(
                        r#"{{"curve":"BN254","proof_system":"Groth16","verifying_key_digest_blake3":"{digest_hex}","verifying_key_compressed_hex":"{vk_hex}"}}"#
                    ),
                )
            } else {
                (
                    "404 Not Found",
                    "application/json",
                    r#"{"error":"not_found"}"#.to_string(),
                )
            };

            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
        });
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let grpc_port: u16 = std::env::var("ZK_SOLVENCY_GRPC_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50162);
    let http_port: u16 = std::env::var("ZK_SOLVENCY_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8092);

    let mut master_seed = [0x48u8; 32];
    if let Ok(seed_hex) = std::env::var("ZK_SOLVENCY_MASTER_SEED_HEX") {
        if let Ok(decoded) = hex::decode(seed_hex) {
            if decoded.len() == 32 {
                master_seed.copy_from_slice(&decoded);
            }
        }
    }

    let engine = Arc::new(ZkSolvencyEngine::new(master_seed)?);

    // Seed genesis epoch `1` (`1,024` deterministic accounts) for immediate readiness & E2E verification.
    let genesis_accounts: Vec<AccountLeafInput> = (0..1_024u64)
        .map(|i| AccountLeafInput {
            account_id: 10_000 + i,
            balance_fp: (i + 1) * 100_000_000,
        })
        .collect();
    engine.register_ledger_snapshot(1, 1, 1, "USDT", &genesis_accounts)?;

    let grpc_addr: SocketAddr = ([0, 0, 0, 0], grpc_port).into();
    let http_addr: SocketAddr = ([0, 0, 0, 0], http_port).into();

    let http_engine = Arc::clone(&engine);
    tokio::spawn(async move {
        if let Err(err) = serve_http_observability(http_addr, http_engine).await {
            eprintln!("zk-solvency HTTP observability server error: {err}");
        }
    });

    println!(
        "HYPERION CH13 zk-solvency-rs online | gRPC={grpc_addr} | HTTP={http_addr} | VK_BLAKE3={}",
        hex::encode(engine.vk_digest())
    );

    Server::builder()
        .add_service(ZkSolvencyServiceServer::new(ZkSolvencyGrpcHandler::new(engine)))
        .serve(grpc_addr)
        .await?;

    Ok(())
}
