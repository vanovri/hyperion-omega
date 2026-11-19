#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use chronos_replay::{
    build_default_demo_scenario, build_http_router, ChronosEngine, ChronosGrpcService,
    ChronosServiceState, ChronosWalStore, DEFAULT_SPARSE_INDEX_STRIDE,
};
use tonic::transport::Server as TonicServer;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let data_dir = std::env::var("HYPERION_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data/ledger"));

    let grpc_addr: SocketAddr = std::env::var("HYPERION_CHRONOS_REPLAY_GRPC_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50154".to_string())
        .parse()?;

    let http_port: u16 = std::env::var("HYPERION_CHRONOS_REPLAY_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8088);
    let http_addr = SocketAddr::from(([0, 0, 0, 0], http_port));

    let store = if data_dir.join("wal").exists() {
        let loaded = ChronosWalStore::open_data_dir(&data_dir, DEFAULT_SPARSE_INDEX_STRIDE)?;
        if loaded.max_engine_seq() > 0 {
            info!(
                data_dir = %data_dir.display(),
                max_seq = loaded.max_engine_seq(),
                snapshots = loaded.snapshots().len(),
                "Loaded verified Channel 02 Blake3 WAL & Snapshots from disk"
            );
            loaded
        } else {
            info!("WAL directory empty; bootstrapping deterministic demo scenario in memory");
            build_default_demo_scenario()?
        }
    } else {
        info!(
            data_dir = %data_dir.display(),
            "No existing WAL directory found; bootstrapping deterministic multi-regime demo scenario"
        );
        build_default_demo_scenario()?
    };

    let engine = Arc::new(ChronosEngine::new(store));
    let svc_state = ChronosServiceState::new(engine);

    let grpc_svc = ChronosGrpcService::new(Arc::clone(&svc_state)).into_server();
    let http_router = build_http_router(Arc::clone(&svc_state));

    info!(%grpc_addr, %http_addr, "HYPERION Chronos Time-Travel & Counterfactual Engine online");

    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_router).await
    });

    let grpc_task = tokio::spawn(async move {
        TonicServer::builder()
            .add_service(grpc_svc)
            .serve(grpc_addr)
            .await
    });

    tokio::select! {
        res = http_task => {
            res??;
        }
        res = grpc_task => {
            res??;
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received; stopping Chronos cleanly");
        }
    }

    Ok(())
}
