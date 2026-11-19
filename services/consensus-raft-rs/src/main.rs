#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use consensus_raft_rs::rpc_service::LiveConsensusRuntime;
use std::env;
use std::net::SocketAddr;
use tonic::transport::Server;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "consensus_raft_rs=info".into()),
        )
        .init();

    let grpc_port: u16 = env::var("GRPC_PORT")
        .or_else(|_| env::var("HYPERION_RAFT_GRPC_PORT"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50161);
    let http_port: u16 = env::var("HTTP_PORT")
        .or_else(|_| env::var("HYPERION_RAFT_HTTP_PORT"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8091);
    let seed: u64 = env::var("HYPERION_RAFT_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x4859_5045_5249_4F4E);

    let runtime = LiveConsensusRuntime::bootstrap_five_node(1, 1, seed);
    let grpc_addr: SocketAddr = format!("0.0.0.0:{grpc_port}").parse()?;
    let http_addr: SocketAddr = format!("0.0.0.0:{http_port}").parse()?;

    info!(
        "HYPERION CH12 consensus-raft-rs online | gRPC={} | HTTP={} (peers 50201-50205)",
        grpc_addr, http_addr
    );

    let http_router = runtime.clone().build_http_router();
    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    let http_task = tokio::spawn(async move {
        let _ = axum::serve(http_listener, http_router).await;
    });

    Server::builder()
        .add_service(runtime.into_grpc_server())
        .serve_with_shutdown(grpc_addr, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    http_task.abort();
    Ok(())
}
