use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use hyperion_proto::stream_analytics_service_client::StreamAnalyticsServiceClient;
use hyperion_proto::{
    CandleInterval, GetCandlesRequest, GetLatestMicrostructureRequest, GetLiquidityHeatmapRequest,
    GetVolumeProfileRequest,
};
use hyperion_stream_aggregator::api::grpc::StreamAnalyticsGrpcServer;
use hyperion_stream_aggregator::api::http::{
    build_http_router, LiquidityHeatmapDto, MicrostructureDto, OhlcvCandleDto, VolumeProfileDto,
};
use hyperion_stream_aggregator::api::QueryServiceState;
use hyperion_stream_aggregator::config::AggregatorConfig;
use hyperion_stream_aggregator::engine::{CompactFill, CompactL3Delta, StreamAggregatorEngine};
use hyperion_stream_aggregator::storage::timescale::{
    encode_candles_pg_binary_copy, PGCOPY_HEADER_MAGIC,
};
use hyperion_stream_aggregator::storage::{AnalyticsStorage, InMemoryTransactionalStore};
use hyperion_stream_aggregator::synthetic::{
    generate_ordered_market_stream, SyntheticStreamConfig,
};
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_postgres::types::Type;
use tonic::transport::Server as TonicServer;
use tower::ServiceExt;

#[tokio::test]
async fn grpc_and_http_query_endpoints_and_pgcopy_wire_verification() {
    let cfg = AggregatorConfig {
        watermark_lateness_ns: 0,
        ..Default::default()
    };
    let mut engine = StreamAggregatorEngine::new(cfg);

    let stream = generate_ordered_market_stream(&SyntheticStreamConfig {
        symbol: "SOL-USDT".to_string(),
        seed: 777_888,
        event_count: 15_000,
        start_ts_ns: 1_795_040_000_000_000_000,
        step_ts_ns: 1_000_000,           // 15 seconds span
        anchor_price_fp: 18_500_000_000, // 185.00 USDT
    });
    for ev in &stream {
        engine.ingest_event(ev);
    }
    engine.flush_all_watermarks();

    let batch = engine.prepare_flush_batch().expect("prepare flush batch");
    assert!(!batch.candles.is_empty());

    // Verify PostgreSQL Binary COPY wire framing (`PGCOPY\n\xff\r\n\0` + trailer `0xFFFF`)
    let copy_bytes = encode_candles_pg_binary_copy(&batch.candles).expect("encode PGCOPY");
    assert_eq!(&copy_bytes[..11], PGCOPY_HEADER_MAGIC);
    assert_eq!(&copy_bytes[copy_bytes.len() - 2..], &[0xFF, 0xFF]);
    let _ = Type::NUMERIC; // type compatibility sanity

    let store = Arc::new(InMemoryTransactionalStore::new());
    store
        .commit_flush_batch(&batch)
        .await
        .expect("commit flush batch");

    let shared_engine = Arc::new(RwLock::new(engine));
    let query_state = QueryServiceState::new(shared_engine, store);

    // 1. Test HTTP REST Router (`/healthz`, `/metrics`, `/api/v1/analytics/*`)
    let app = build_http_router(query_state.clone());

    let health_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health_resp.status(), StatusCode::OK);

    let candles_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/analytics/candles?symbol=SOL-USDT&interval=1s&limit=50")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(candles_resp.status(), StatusCode::OK);
    let body_bytes = to_bytes(candles_resp.into_body(), 1_048_576).await.unwrap();
    let candle_dtos: Vec<OhlcvCandleDto> = serde_json::from_slice(&body_bytes).unwrap();
    assert!(!candle_dtos.is_empty());
    // Verify stringified fixed-point integers (> 0)
    assert!(candle_dtos[0].open_fp.parse::<u64>().unwrap() > 0);
    assert!(candle_dtos[0].base_volume_fp.parse::<u128>().unwrap() > 0);

    let micro_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/analytics/microstructure?symbol=SOL-USDT")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(micro_resp.status(), StatusCode::OK);
    let micro_bytes = to_bytes(micro_resp.into_body(), 65_536).await.unwrap();
    let micro_dto: MicrostructureDto = serde_json::from_slice(&micro_bytes).unwrap();
    assert_eq!(micro_dto.symbol, "SOL-USDT");
    assert!(micro_dto.rolling_vwap_fp.parse::<u64>().unwrap() > 0);

    let vp_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/analytics/volume-profile?symbol=SOL-USDT")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(vp_resp.status(), StatusCode::OK);
    let vp_bytes = to_bytes(vp_resp.into_body(), 262_144).await.unwrap();
    let vp_dto: VolumeProfileDto = serde_json::from_slice(&vp_bytes).unwrap();
    let poc = vp_dto.poc_price_fp.parse::<u64>().unwrap();
    let val = vp_dto.value_area_low_fp.parse::<u64>().unwrap();
    let vah = vp_dto.value_area_high_fp.parse::<u64>().unwrap();
    assert!(
        val <= poc && poc <= vah,
        "Value Area invariant VAL <= POC <= VAH must hold"
    );

    let hm_resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/analytics/heatmap?symbol=SOL-USDT")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hm_resp.status(), StatusCode::OK);
    let hm_bytes = to_bytes(hm_resp.into_body(), 524_288).await.unwrap();
    let hm_dto: LiquidityHeatmapDto = serde_json::from_slice(&hm_bytes).unwrap();
    assert!(!hm_dto.buckets.is_empty());

    // 2. Test Live Bound gRPC `StreamAnalyticsService` Server + Client
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    drop(listener); // let tonic bind cleanly on ephemeral port

    let grpc_svc = StreamAnalyticsGrpcServer::new(query_state).into_service();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let server_handle = tokio::spawn(async move {
        TonicServer::builder()
            .add_service(grpc_svc)
            .serve_with_shutdown(local_addr, async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let endpoint = format!("http://{local_addr}");
    let mut client = StreamAnalyticsServiceClient::connect(endpoint)
        .await
        .expect("connect gRPC client");

    let grpc_candles = client
        .get_candles(GetCandlesRequest {
            symbol: "SOL-USDT".to_string(),
            interval: CandleInterval::CandleInterval1s as i32,
            from_ts_ns: 0,
            to_ts_ns: 0,
            limit: 20,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!grpc_candles.candles.is_empty());

    let grpc_vp = client
        .get_volume_profile(GetVolumeProfileRequest {
            symbol: "SOL-USDT".to_string(),
            from_ts_ns: 0,
            to_ts_ns: 0,
            bucket_size_fp: 10_000_000,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(grpc_vp.poc_price_fp > 0);

    let grpc_hm = client
        .get_liquidity_heatmap(GetLiquidityHeatmapRequest {
            symbol: "SOL-USDT".to_string(),
            from_ts_ns: 0,
            to_ts_ns: 0,
            price_bucket_size_fp: 10_000_000,
            time_bucket_ns: 1_000_000_000,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!grpc_hm.buckets.is_empty());

    let grpc_micro = client
        .get_latest_microstructure(GetLatestMicrostructureRequest {
            symbol: "SOL-USDT".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(grpc_micro.metrics.unwrap().rolling_vwap_fp > 0);

    let _ = tx.send(());
    let _ = server_handle.await;
}

/// Overclock Governor Mandate #2: 500,000-cycle consecutive steady-state zero-leak soak test!
#[test]
fn soak_500k_cycles_zero_steady_state_memory_growth() {
    let cfg = AggregatorConfig {
        watermark_lateness_ns: 0,
        hot_candle_ring_capacity: 256,
        hot_heatmap_columns: 64,
        ..Default::default()
    };

    let mut engine = StreamAggregatorEngine::with_raw_collection(cfg, false);
    let base_ts = 1_795_050_000_000_000_000_u64;

    // 1. Warmup 60,000 cycles across ring wraparound boundaries
    let mut seq = 1_u64;
    let mut fill_id = 1_u64;
    for i in 0..60_000_u64 {
        let ts = base_ts + i * 20_000_000; // 20ms per step -> 1,200 seconds warmup (wraps 64s/256-slot rings!)
        let px = 6_850_000_000_000 + ((i % 32) * 1_000_000);
        if (i & 1) == 0 {
            engine.ingest_compact_fill(
                "BTC-USDT",
                CompactFill {
                    fill_id,
                    engine_seq: seq,
                    instrument_id: 1,
                    aggressor_side: if (i % 4) == 0 { 1 } else { 2 },
                    maker_order_id: 1,
                    taker_order_id: 2,
                    maker_account_id: 10,
                    taker_account_id: 20,
                    price_fp: px,
                    qty_fp: 10_000_000,
                    notional_fp: (px as u128 * 10_000_000) / 100_000_000,
                    maker_fee_fp: 0,
                    taker_fee_fp: 0,
                    exec_ts_ns: ts,
                    wal_blake3_hash: [0; 32],
                },
            );
            fill_id += 1;
        } else {
            engine.ingest_compact_l3(
                "BTC-USDT",
                CompactL3Delta {
                    engine_seq: seq,
                    prev_engine_seq: seq - 1,
                    instrument_id: 1,
                    op: if (i % 6) == 1 { 1 } else { 3 },
                    side: if (i % 4) == 1 { 1 } else { 2 },
                    order_id: (i % 128) + 1,
                    account_id: 10,
                    price_fp: px,
                    remaining_qty_fp: 5_000_000,
                    delta_qty_fp: if (i % 6) == 1 { 5_000_000 } else { -5_000_000 },
                    event_ts_ns: ts,
                },
            );
        }
        seq += 1;
    }

    let baseline_cap = engine.steady_state_capacity_units();

    // 2. Run 500,000 consecutive hot cycles and assert 0 capacity growth!
    for i in 60_000..560_000_u64 {
        let ts = base_ts + i * 20_000_000;
        let px = 6_850_000_000_000 + ((i % 32) * 1_000_000);
        if (i & 1) == 0 {
            engine.ingest_compact_fill(
                "BTC-USDT",
                CompactFill {
                    fill_id,
                    engine_seq: seq,
                    instrument_id: 1,
                    aggressor_side: if (i % 4) == 0 { 1 } else { 2 },
                    maker_order_id: 1,
                    taker_order_id: 2,
                    maker_account_id: 10,
                    taker_account_id: 20,
                    price_fp: px,
                    qty_fp: 10_000_000,
                    notional_fp: (px as u128 * 10_000_000) / 100_000_000,
                    maker_fee_fp: 0,
                    taker_fee_fp: 0,
                    exec_ts_ns: ts,
                    wal_blake3_hash: [0; 32],
                },
            );
            fill_id += 1;
        } else {
            engine.ingest_compact_l3(
                "BTC-USDT",
                CompactL3Delta {
                    engine_seq: seq,
                    prev_engine_seq: seq - 1,
                    instrument_id: 1,
                    op: if (i % 6) == 1 { 1 } else { 3 },
                    side: if (i % 4) == 1 { 1 } else { 2 },
                    order_id: (i % 128) + 1,
                    account_id: 10,
                    price_fp: px,
                    remaining_qty_fp: 5_000_000,
                    delta_qty_fp: if (i % 6) == 1 { 5_000_000 } else { -5_000_000 },
                    event_ts_ns: ts,
                },
            );
        }
        seq += 1;
    }

    let post_soak_cap = engine.steady_state_capacity_units();
    assert_eq!(
        post_soak_cap, baseline_cap,
        "steady-state memory leak detected over 500,000 cycles: before={baseline_cap}, after={post_soak_cap}"
    );
    assert_eq!(engine.total_accepted_events(), 560_000);
}
