#![forbid(unsafe_code)]

use margin_risk_rs::omega_wire::pb::margin_risk_service_server::MarginRiskServiceServer;
use margin_risk_rs::service::{
    build_http_router, MarginRiskEngineState, MarginRiskGrpcService, DEFAULT_GRPC_PORT,
    DEFAULT_HTTP_PORT,
};
use std::net::SocketAddr;
use tonic::transport::Server;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "margin_risk_rs=info".into()),
        )
        .init();

    let grpc_addr: SocketAddr = std::env::var("MARGIN_GRPC_ADDR")
        .unwrap_or_else(|_| format!("0.0.0.0:{DEFAULT_GRPC_PORT}"))
        .parse()?;
    let http_addr: SocketAddr = std::env::var("MARGIN_HTTP_ADDR")
        .unwrap_or_else(|_| format!("0.0.0.0:{DEFAULT_HTTP_PORT}"))
        .parse()?;

    let state = MarginRiskEngineState::new_with_canonical_defaults();
    let grpc_svc = MarginRiskGrpcService {
        state: state.clone(),
    };
    let http_router = build_http_router(state);

    let http_listener = tokio::net::TcpListener::bind(http_addr).await?;
    info!(%grpc_addr, %http_addr, "Hyperion CH14 margin-risk-rs online");

    let http_task = tokio::spawn(async move {
        if let Err(err) = axum::serve(http_listener, http_router).await {
            tracing::error!(error = %err, "HTTP server terminated with error");
        }
    });

    Server::builder()
        .add_service(MarginRiskServiceServer::new(grpc_svc))
        .serve_with_shutdown(grpc_addr, async {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received for margin-risk-rs");
        })
        .await?;

    http_task.abort();
    Ok(())
}
