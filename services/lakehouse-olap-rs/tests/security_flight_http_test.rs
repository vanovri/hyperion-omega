use std::io::Cursor;
use std::sync::Arc;

use arrow::array::Int64Array;
use arrow::ipc::reader::StreamReader;
use arrow_flight::flight_service_client::FlightServiceClient;
use arrow_flight::{Action, Criteria, FlightClient, FlightDescriptor, Ticket};
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use futures::TryStreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server as TonicServer};
use tonic::Code;
use tower::ServiceExt;

use lakehouse_olap_rs::ast_guard::validate_forensic_sql;
use lakehouse_olap_rs::config::{LakehouseConfig, FP_SCALE, NS_PER_HOUR};
use lakehouse_olap_rs::engine::{LakehouseEngine, QueryRequestOptions};
use lakehouse_olap_rs::error::LakehouseError;
use lakehouse_olap_rs::schema::{QuoteRecord, TradeRecord};
use lakehouse_olap_rs::server_flight::LakehouseFlightService;
use lakehouse_olap_rs::server_http::build_http_router;

#[test]
fn test_sql_ast_guard_blocks_all_ddl_dml_and_cross_join_attacks() {
    let malicious_queries = [
        "DROP TABLE trades",
        "DELETE FROM trades WHERE account_id = 1001",
        "INSERT INTO trades VALUES (1, 2, 3)",
        "UPDATE trades SET price_fp = 0 WHERE seq_id = 1",
        "TRUNCATE TABLE trades",
        "ALTER TABLE trades DROP COLUMN price_fp",
        "CREATE TABLE hacked AS SELECT * FROM trades",
        "SELECT * INTO hacked FROM trades",
        "COPY trades TO '/tmp/leak.parquet'",
        "SET statement_timeout = 0",
        "SELECT count(*) FROM trades; DROP TABLE trades;",
        "EXPLAIN ANALYZE SELECT * FROM trades",
        "WITH RECURSIVE r AS (SELECT 1 AS x UNION ALL SELECT x + 1 FROM r) SELECT * FROM r",
        "SELECT * FROM read_parquet('/etc/passwd')",
        "SELECT pg_sleep(30) FROM trades",
        "SELECT * FROM trades CROSS JOIN quotes",
        "SELECT * FROM trades, quotes",
        "SELECT * FROM trades t JOIN quotes q ON 1 = 1",
        "SELECT * FROM trades t INNER JOIN quotes q ON true",
        "SELECT * FROM trades t INNER JOIN quotes q ON t.seq_id = t.seq_id",
        "SELECT * FROM trades t INNER JOIN quotes q ON t.instrument_id = q.instrument_id OR 1 = 1",
        "SELECT * FROM trades NATURAL JOIN quotes",
    ];

    for bad_sql in malicious_queries {
        let res = validate_forensic_sql(bad_sql, 24);
        assert!(
            res.is_err(),
            "Malicious or explosive query MUST be rejected by SqlAstGuard: {bad_sql}"
        );
    }

    // Valid read-only forensic queries MUST pass
    let valid_queries = [
        "SELECT symbol, COUNT(*), SUM(notional_fp) FROM trades WHERE symbol = 'BTC-USDT' GROUP BY symbol",
        "WITH sniper AS (SELECT taker_account_id, SUM(qty_fp) AS v FROM trades GROUP BY taker_account_id) SELECT * FROM sniper ORDER BY v DESC LIMIT 10",
        "SELECT t.seq_id, q.mid_price_fp FROM trades t ASOF JOIN quotes q ON t.instrument_id = q.instrument_id AND t.timestamp_ns >= q.timestamp_ns WHERE t.symbol = 'BTC-USDT'",
        "SELECT t.seq_id, q.mid_price_fp FROM trades t INNER JOIN quotes q ON t.instrument_id = q.instrument_id AND t.seq_id = q.seq_id",
        "EXPLAIN SELECT COUNT(*) FROM trades WHERE symbol = 'BTC-USDT'",
    ];

    for ok_sql in valid_queries {
        validate_forensic_sql(ok_sql, 24)
            .unwrap_or_else(|e| panic!("Expected valid forensic SQL to pass '{ok_sql}': {e}"));
    }
}

#[tokio::test]
async fn test_memory_quota_http_ipc_and_arrow_flight_streaming() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = LakehouseConfig {
        storage_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let engine = LakehouseEngine::try_new(cfg).await.expect("engine");

    let base_ts = (498_620 * NS_PER_HOUR) as i64;
    let mut trades = Vec::with_capacity(4_000);
    for i in 0..4_000_u64 {
        trades.push(TradeRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: base_ts + (i as i64) * 1_000,
            seq_id: i + 1,
            fill_id: i + 1,
            maker_account_id: 100 + (i % 50),
            taker_account_id: 200 + (i % 50),
            is_buy_aggressor: (i & 1) == 0,
            price_fp: 6_850_000_000_000 + (i as i64) * 1_000_000,
            qty_fp: FP_SCALE,
            notional_fp: 6_850_000_000_000 + (i as i64) * 1_000_000,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            from_wal: false,
        });
    }
    engine.ingest_trades(&trades).unwrap();
    engine
        .ingest_quotes(&[QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: base_ts,
            seq_id: 1,
            best_bid_fp: 6_849_900_000_000,
            best_ask_fp: 6_850_100_000_000,
            mid_price_fp: 6_850_000_000_000,
            spread_fp: 200_000_000,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        }])
        .unwrap();
    engine.refresh_table_providers().await.unwrap();

    // 1. Enforce tiny 4 KiB per-query memory quota on heavy sort/group-by -> must fail with MemoryQuotaExceeded!
    let oom_res = engine
        .execute_sql(
            "SELECT fill_id, timestamp_ns, SUM(notional_fp) AS n FROM trades GROUP BY fill_id, timestamp_ns ORDER BY n DESC",
            QueryRequestOptions {
                memory_limit_bytes: Some(4 * 1024),
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(oom_res, Err(LakehouseError::MemoryQuotaExceeded { .. })),
        "Expected MemoryQuotaExceeded under 4 KiB limit, got: {oom_res:?}"
    );

    // 2. Test HTTP Router: JSON (64-bit `_fp` string safety), Arrow IPC Stream, and 403 AST Guard
    let app = build_http_router(Arc::clone(&engine));

    let json_req = Request::builder()
        .method("POST")
        .uri("/api/v1/olap/query")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "sql": "SELECT COUNT(*) AS cnt, MAX(price_fp) AS max_price_fp, SUM(qty_fp) AS sum_qty_fp FROM trades WHERE symbol = 'BTC-USDT'",
                "include_plan": true
            }))
            .unwrap(),
        ))
        .unwrap();
    let json_resp = app.clone().oneshot(json_req).await.unwrap();
    assert_eq!(json_resp.status(), StatusCode::OK);
    let body_bytes = json_resp.into_body().collect().await.unwrap().to_bytes();
    let parsed: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(parsed["rows"][0]["cnt"], 4000);
    // `_fp` column must be serialized as string to preserve > 2^53 precision in JS/MCP clients
    assert!(parsed["rows"][0]["max_price_fp"].is_string());
    assert!(parsed["physical_plan"].as_str().unwrap().contains("DataSourceExec"));

    // HTTP Zero-Copy Arrow IPC Stream
    let ipc_req = Request::builder()
        .method("POST")
        .uri("/api/v1/lakehouse/query")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/vnd.apache.arrow.stream")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "sql": "SELECT seq_id, price_fp FROM trades ORDER BY seq_id ASC LIMIT 25"
            }))
            .unwrap(),
        ))
        .unwrap();
    let ipc_resp = app.clone().oneshot(ipc_req).await.unwrap();
    assert_eq!(ipc_resp.status(), StatusCode::OK);
    let ipc_bytes = ipc_resp.into_body().collect().await.unwrap().to_bytes();
    let mut reader = StreamReader::try_new(Cursor::new(ipc_bytes), None).expect("arrow ipc reader");
    let ipc_batch = reader.next().unwrap().expect("ipc batch");
    assert_eq!(ipc_batch.num_rows(), 25);

    // HTTP 403 Forbidden on DDL injection
    let bad_http_req = Request::builder()
        .method("POST")
        .uri("/api/v1/lakehouse/query")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({ "sql": "DROP TABLE trades" })).unwrap(),
        ))
        .unwrap();
    let bad_http_resp = app.oneshot(bad_http_req).await.unwrap();
    assert_eq!(bad_http_resp.status(), StatusCode::FORBIDDEN);

    // 3. Spin up real gRPC Apache Arrow Flight server on ephemeral localhost port & stream RecordBatches
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let flight_svc = LakehouseFlightService::into_server(Arc::clone(&engine));

    let server_handle = tokio::spawn(async move {
        TonicServer::builder()
            .add_service(flight_svc)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .expect("connect flight grpc");

    let mut raw_client = FlightServiceClient::new(channel.clone());
    let listed: Vec<_> = raw_client
        .list_flights(Criteria::default())
        .await
        .unwrap()
        .into_inner()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(listed.len(), 3);

    // Reject malicious SQL over gRPC Arrow Flight with PermissionDenied
    let err_status = raw_client
        .do_get(Ticket::new("DELETE FROM trades"))
        .await
        .expect_err("must reject DELETE over Flight");
    assert_eq!(err_status.code(), Code::PermissionDenied);

    // Stream valid aggregation over FlightClient (`FlightRecordBatchStream`)
    let mut flight_client = FlightClient::new(channel);
    let info = flight_client
        .get_flight_info(FlightDescriptor::new_cmd(
            "SELECT COUNT(*) AS cnt, twap_precise(price_fp, timestamp_ns) AS twap_fp FROM trades",
        ))
        .await
        .expect("get_flight_info");
    let ticket = info.endpoint[0].ticket.clone().unwrap();
    let flight_batches: Vec<_> = flight_client
        .do_get(ticket)
        .await
        .expect("do_get")
        .try_collect()
        .await
        .expect("collect flight stream");
    assert_eq!(flight_batches.len(), 1);
    let cnt_val = flight_batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(cnt_val, 4_000);

    // Flight `explain` action
    let action_res: Vec<_> = flight_client
        .do_action(Action::new(
            "explain",
            "SELECT COUNT(*) FROM trades WHERE symbol = 'BTC-USDT'",
        ))
        .await
        .expect("do_action explain")
        .try_collect()
        .await
        .unwrap();
    assert_eq!(action_res.len(), 1);

    server_handle.abort();
}
