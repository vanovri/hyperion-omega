use ledger_rs::engine::{DeterministicExchangeEngine, EngineSizing};
use ledger_rs::grpc::{
    spawn_health_and_metrics_http_server, HyperionOrderbookGrpcService, LmaxDisruptorHandle,
};
use ledger_rs::publisher::{spawn_async_redpanda_publisher, PublisherMetrics};
use ledger_rs::ring::spsc_channel;
use ledger_rs::wal::WalSyncMode;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tonic::transport::Server;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let grpc_addr: std::net::SocketAddr = std::env::var("HYPERION_LEDGER_GRPC_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50151".to_string())
        .parse()?;
    let metrics_port =
        std::env::var("HYPERION_LEDGER_METRICS_PORT").unwrap_or_else(|_| "8082".to_string());
    let metrics_addr = format!("0.0.0.0:{metrics_port}");
    let data_dir = PathBuf::from(
        std::env::var("HYPERION_DATA_DIR").unwrap_or_else(|_| "./data/ledger".to_string()),
    );
    let redpanda_broker = std::env::var("HYPERION_REDPANDA_BROKERS").ok();

    let (spsc_prod, spsc_cons) = spsc_channel(65_536);
    let sizing = EngineSizing {
        max_accounts: 32_768,
        max_orders_per_book: 262_144,
        max_levels_per_side: 32_768,
        snapshot_every_n_events: 25_000,
    };

    let (engine, replayed, truncated) = DeterministicExchangeEngine::recover_from_data_dir(
        sizing,
        &data_dir,
        WalSyncMode::GroupCommit {
            flush_every_bytes: 65_536,
        },
        Some(spsc_prod),
    )?;

    tracing::info!(
        grpc = %grpc_addr,
        http_metrics = %metrics_addr,
        data_dir = %data_dir.display(),
        replayed_wal_frames = replayed,
        healed_corrupt_tail = truncated,
        engine_seq = engine.engine_seq(),
        "HYPERION ledger-rs single-writer LMAX engine initialized"
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let pub_metrics = Arc::new(PublisherMetrics::default());
    let (stream_tx, _) = broadcast::channel(8_192);

    let _pub_task = spawn_async_redpanda_publisher(
        spsc_cons,
        redpanda_broker,
        None,
        stream_tx.clone(),
        Arc::clone(&pub_metrics),
        Arc::clone(&shutdown),
    );

    let handle = LmaxDisruptorHandle::spawn(
        engine,
        32_768,
        pub_metrics,
        stream_tx,
        Arc::clone(&shutdown),
    );
    let _http_task =
        spawn_health_and_metrics_http_server(metrics_addr, handle.clone(), Arc::clone(&shutdown));

    let svc = HyperionOrderbookGrpcService::new(handle.clone()).into_server();
    Server::builder()
        .tcp_nodelay(true)
        .add_service(svc)
        .serve_with_shutdown(grpc_addr, async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Shutting down HYPERION ledger-rs...");
            handle.shutdown().await;
        })
        .await?;

    Ok(())
}
