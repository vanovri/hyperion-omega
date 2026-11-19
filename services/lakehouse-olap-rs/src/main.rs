#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::sync::Arc;
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing::info;

use lakehouse_olap_rs::compactor::BackgroundCompactorHandle;
use lakehouse_olap_rs::config::LakehouseConfig;
use lakehouse_olap_rs::engine::LakehouseEngine;
use lakehouse_olap_rs::server_flight::LakehouseFlightService;
use lakehouse_olap_rs::server_http::build_http_router;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lakehouse_olap_rs=info,info".into()),
        )
        .init();

    let cfg = LakehouseConfig::from_env();
    let http_addr = cfg.http_bind_addr;
    let flight_addr = cfg.flight_bind_addr;

    let engine = LakehouseEngine::try_new(cfg.clone()).await?;
    let compactor = BackgroundCompactorHandle::spawn(cfg, None);

    let http_router = build_http_router(Arc::clone(&engine));
    let http_listener = TcpListener::bind(http_addr).await?;
    info!(%http_addr, "Channel 17 Lakehouse HTTP (JSON + Arrow IPC) server listening");

    let flight_svc = LakehouseFlightService::into_server(Arc::clone(&engine));
    info!(%flight_addr, "Channel 17 Lakehouse gRPC Arrow Flight server listening");

    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_router).await
    });

    let flight_task = tokio::spawn(async move {
        TonicServer::builder()
            .add_service(flight_svc)
            .serve(flight_addr)
            .await
    });

    tokio::select! {
        res = http_task => {
            res??;
        }
        res = flight_task => {
            res??;
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received; stopping Lakehouse OLAP engine");
        }
    }

    compactor.shutdown().await;
    Ok(())
}
