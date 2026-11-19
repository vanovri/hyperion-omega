#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use anyhow::Context;
use hyperion_stream_aggregator::api::grpc::StreamAnalyticsGrpcServer;
use hyperion_stream_aggregator::api::http::build_http_router;
use hyperion_stream_aggregator::api::QueryServiceState;
use hyperion_stream_aggregator::broker::redpanda::{
    run_redpanda_consumer_loop, RedpandaAggregatePublisher,
};
use hyperion_stream_aggregator::broker::ExactlyOnceCoordinator;
use hyperion_stream_aggregator::config::AggregatorConfig;
use hyperion_stream_aggregator::engine::StreamAggregatorEngine;
use hyperion_stream_aggregator::storage::redis_cache::{HotStateCache, RedisHotCache};
use hyperion_stream_aggregator::storage::timescale::TimescaleFlusher;
use hyperion_stream_aggregator::storage::{AnalyticsStorage, InMemoryTransactionalStore};
use parking_lot::RwLock;
use rskafka::client::ClientBuilder;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,hyperion_stream_aggregator=info")),
        )
        .init();

    let cfg = AggregatorConfig::from_env();
    tracing::info!(
        grpc_addr = %cfg.grpc_bind_addr,
        http_addr = %cfg.http_bind_addr,
        consumer_group = %cfg.consumer_group,
        "Starting HYPERION Channel 04 stream-aggregator"
    );

    let engine = Arc::new(RwLock::new(StreamAggregatorEngine::new(cfg.clone())));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Connect to TimescaleDB if reachable, otherwise fall back gracefully to in-memory transactional store
    // while logging degraded storage state so standalone smoke/dev runs remain responsive.
    let storage: Arc<dyn AnalyticsStorage> = if cfg.enable_external_io {
        match TimescaleFlusher::connect(&cfg.postgres_dsn).await {
            Ok(ts_flusher) => {
                tracing::info!("Connected to TimescaleDB with Binary COPY staging tables ready");
                let ts_arc = Arc::new(ts_flusher);

                let hot_cache: Option<Arc<dyn HotStateCache>> =
                    match RedisHotCache::connect(&cfg.redis_url).await {
                        Ok(rc) => {
                            tracing::info!("Connected to Redis 7 hot cache");
                            Some(Arc::new(rc))
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Redis 7 unavailable; continuing without L1 Redis mirror");
                            None
                        }
                    };

                match ClientBuilder::new(cfg.redpanda_brokers.clone()).build().await {
                    Ok(kafka_client) => {
                        let kafka_arc = Arc::new(kafka_client);
                        let publisher =
                            Arc::new(RedpandaAggregatePublisher::from_client(Arc::clone(&kafka_arc)));
                        let coordinator = ExactlyOnceCoordinator::from_shared_engine(
                            Arc::clone(&engine),
                            cfg.batch_flush_max_events,
                            Arc::clone(&ts_arc),
                            publisher,
                            hot_cache,
                        );
                        let worker_cfg = cfg.clone();
                        let worker_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = run_redpanda_consumer_loop(
                                worker_cfg,
                                kafka_arc,
                                coordinator,
                                worker_shutdown,
                            )
                            .await
                            {
                                tracing::error!(error = %e, "Redpanda consumer loop exited with error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Redpanda broker unavailable at startup; serving query API");
                    }
                }

                ts_arc
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "TimescaleDB unavailable; falling back to deterministic InMemoryTransactionalStore"
                );
                Arc::new(InMemoryTransactionalStore::new())
            }
        }
    } else {
        Arc::new(InMemoryTransactionalStore::new())
    };

    let query_state = QueryServiceState::new(Arc::clone(&engine), storage);

    let grpc_addr: SocketAddr = cfg
        .grpc_bind_addr
        .parse()
        .context("invalid HYPERION_AGGREGATOR_GRPC_ADDR")?;
    let http_addr: SocketAddr = cfg
        .http_bind_addr
        .parse()
        .context("invalid HYPERION_AGGREGATOR_HTTP_ADDR")?;

    let grpc_service = StreamAnalyticsGrpcServer::new(query_state.clone()).into_service();
    let http_router = build_http_router(query_state);
    let http_listener = TcpListener::bind(http_addr)
        .await
        .with_context(|| format!("failed to bind HTTP server on {http_addr}"))?;

    let mut grpc_shutdown = shutdown_rx.clone();
    let grpc_task = tokio::spawn(async move {
        TonicServer::builder()
            .add_service(grpc_service)
            .serve_with_shutdown(grpc_addr, async move {
                let _ = grpc_shutdown.changed().await;
            })
            .await
    });

    let mut http_shutdown = shutdown_rx.clone();
    let http_task = tokio::spawn(async move {
        axum::serve(http_listener, http_router)
            .with_graceful_shutdown(async move {
                let _ = http_shutdown.changed().await;
            })
            .await
    });

    tracing::info!(
        grpc = %grpc_addr,
        http = %http_addr,
        "stream-aggregator ready (gRPC StreamAnalyticsService + HTTP REST /api/v1/analytics/*)"
    );

    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received; draining watermarks & closing listeners");
    let _ = shutdown_tx.send(true);

    let _ = grpc_task.await;
    let _ = http_task.await;
    Ok(())
}
