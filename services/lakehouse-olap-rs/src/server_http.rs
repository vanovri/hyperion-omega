use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{
    Array, BooleanArray, DictionaryArray, Int32Array, Int64Array, RecordBatch, StringArray,
    UInt32Array, UInt64Array,
};
use arrow::datatypes::Int8Type;
use arrow::ipc::writer::StreamWriter;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::engine::{LakehouseEngine, QueryRequestOptions};
use crate::error::Result;

#[derive(Debug, Deserialize)]
pub struct HttpQueryRequest {
    pub sql: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub max_rows: Option<usize>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub memory_limit_bytes: Option<usize>,
    #[serde(default)]
    pub include_plan: bool,
}

#[derive(Debug, Serialize)]
pub struct HttpQueryJsonResponse {
    pub row_count: usize,
    pub elapsed_us: u64,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_plan: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ColumnMeta {
    pub name: String,
    pub arrow_type: String,
}

#[derive(Debug, Deserialize)]
pub struct HttpExplainRequest {
    pub sql: String,
}

#[derive(Debug, Serialize)]
pub struct HttpExplainResponse {
    pub physical_plan: String,
    pub has_parquet_scan: bool,
    pub has_pushed_predicate: bool,
    pub has_asof_join_exec: bool,
    pub scheduled_parquet_files: usize,
}

#[derive(Debug, Deserialize)]
pub struct HttpCompactRequest {
    #[serde(default = "default_force_true")]
    pub force: bool,
}

fn default_force_true() -> bool {
    true
}

/// Builds the Axum HTTP Router serving JSON, Zero-Copy Arrow IPC Stream, Explain, Compaction, Health, and Prometheus Metrics.
/// Exposes canonical Omega routes (`/api/v1/olap/*` per `01-hyperion-omega.md` §2) plus `/api/v1/lakehouse/*` aliases.
pub fn build_http_router(engine: Arc<LakehouseEngine>) -> Router {
    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/metrics", get(handle_metrics))
        .route("/api/v1/olap/query", post(handle_query))
        .route("/api/v1/olap/explain", post(handle_explain))
        .route("/api/v1/olap/compact", post(handle_compact))
        .route("/api/v1/lakehouse/query", post(handle_query))
        .route("/api/v1/lakehouse/explain", post(handle_explain))
        .route("/api/v1/lakehouse/compact", post(handle_compact))
        .with_state(engine)
}

async fn handle_healthz(State(engine): State<Arc<LakehouseEngine>>) -> impl IntoResponse {
    let tel = &engine.telemetry;
    Json(json!({
        "status": "ok",
        "service": "lakehouse-olap-rs",
        "channel": 17,
        "ingested_trade_rows": tel.ingested_trade_rows.load(Ordering::Relaxed),
        "ingested_quote_rows": tel.ingested_quote_rows.load(Ordering::Relaxed),
        "ingested_alert_rows": tel.ingested_alert_rows.load(Ordering::Relaxed),
        "queries_total": tel.queries_total.load(Ordering::Relaxed),
        "queries_rejected_ast": tel.queries_rejected_ast.load(Ordering::Relaxed),
        "compactions_completed": tel.compactions_completed.load(Ordering::Relaxed),
    }))
}

async fn handle_metrics(State(engine): State<Arc<LakehouseEngine>>) -> Response {
    let tel = &engine.telemetry;
    let body = format!(
        "# HELP hyperion_lakehouse_queries_total Total forensic SQL queries received\n\
         # TYPE hyperion_lakehouse_queries_total counter\n\
         hyperion_lakehouse_queries_total {}\n\
         # HELP hyperion_lakehouse_queries_rejected_ast_total Queries blocked by SqlAstGuard\n\
         # TYPE hyperion_lakehouse_queries_rejected_ast_total counter\n\
         hyperion_lakehouse_queries_rejected_ast_total {}\n\
         # HELP hyperion_lakehouse_queries_timed_out_total Queries aborted by hard wall-clock timeout\n\
         # TYPE hyperion_lakehouse_queries_timed_out_total counter\n\
         hyperion_lakehouse_queries_timed_out_total {}\n\
         # HELP hyperion_lakehouse_queries_oom_blocked_total Queries aborted by per-query GreedyMemoryPool\n\
         # TYPE hyperion_lakehouse_queries_oom_blocked_total counter\n\
         hyperion_lakehouse_queries_oom_blocked_total {}\n\
         # HELP hyperion_lakehouse_ingested_rows_total Rows ingested per table\n\
         # TYPE hyperion_lakehouse_ingested_rows_total counter\n\
         hyperion_lakehouse_ingested_rows_total{{table=\"trades\"}} {}\n\
         hyperion_lakehouse_ingested_rows_total{{table=\"quotes\"}} {}\n\
         hyperion_lakehouse_ingested_rows_total{{table=\"fraud_alerts\"}} {}\n\
         # HELP hyperion_lakehouse_compactions_completed_total Partitions compacted\n\
         # TYPE hyperion_lakehouse_compactions_completed_total counter\n\
         hyperion_lakehouse_compactions_completed_total {}\n",
        tel.queries_total.load(Ordering::Relaxed),
        tel.queries_rejected_ast.load(Ordering::Relaxed),
        tel.queries_timed_out.load(Ordering::Relaxed),
        tel.queries_oom_blocked.load(Ordering::Relaxed),
        tel.ingested_trade_rows.load(Ordering::Relaxed),
        tel.ingested_quote_rows.load(Ordering::Relaxed),
        tel.ingested_alert_rows.load(Ordering::Relaxed),
        tel.compactions_completed.load(Ordering::Relaxed),
    );
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

async fn handle_query(
    State(engine): State<Arc<LakehouseEngine>>,
    headers: HeaderMap,
    Json(req): Json<HttpQueryRequest>,
) -> Result<Response> {
    let wants_arrow_ipc = req
        .format
        .as_deref()
        .is_some_and(|f| f.eq_ignore_ascii_case("arrow_ipc") || f.eq_ignore_ascii_case("ipc"))
        || headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("application/vnd.apache.arrow.stream"));

    let default_cap = if wants_arrow_ipc {
        None
    } else {
        Some(engine.config().max_json_rows)
    };

    let opts = QueryRequestOptions {
        memory_limit_bytes: req.memory_limit_bytes,
        timeout: req.timeout_ms.map(Duration::from_millis),
        max_rows: req.max_rows.or(default_cap),
        capture_plan: req.include_plan,
    };

    let outcome = engine.execute_sql(&req.sql, opts).await?;

    if wants_arrow_ipc {
        let mut ipc_buf = Vec::with_capacity(4096 + outcome.row_count * 32);
        {
            let mut writer = StreamWriter::try_new(&mut ipc_buf, &outcome.schema)?;
            for batch in &outcome.batches {
                writer.write(batch)?;
            }
            writer.finish()?;
        }
        let mut resp_headers = HeaderMap::new();
        resp_headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/vnd.apache.arrow.stream"),
        );
        if let Ok(hv) = header::HeaderValue::from_str(&outcome.elapsed_us.to_string()) {
            resp_headers.insert(header::HeaderName::from_static("x-hyperion-elapsed-us"), hv);
        }
        return Ok((StatusCode::OK, resp_headers, ipc_buf).into_response());
    }

    let columns: Vec<ColumnMeta> = outcome
        .schema
        .fields()
        .iter()
        .map(|f| ColumnMeta {
            name: f.name().clone(),
            arrow_type: f.data_type().to_string(),
        })
        .collect();

    let rows = record_batches_to_safe_json(&outcome.batches);
    let resp = HttpQueryJsonResponse {
        row_count: outcome.row_count,
        elapsed_us: outcome.elapsed_us,
        columns,
        rows,
        physical_plan: outcome.plan_summary.map(|p| p.physical_plan_tree),
    };
    Ok((StatusCode::OK, Json(resp)).into_response())
}

async fn handle_explain(
    State(engine): State<Arc<LakehouseEngine>>,
    Json(req): Json<HttpExplainRequest>,
) -> Result<Json<HttpExplainResponse>> {
    let summary = engine.explain_physical_plan(&req.sql).await?;
    Ok(Json(HttpExplainResponse {
        physical_plan: summary.physical_plan_tree,
        has_parquet_scan: summary.has_parquet_scan,
        has_pushed_predicate: summary.has_pushed_predicate,
        has_asof_join_exec: summary.has_asof_join_exec,
        scheduled_parquet_files: summary.total_parquet_files_scheduled,
    }))
}

async fn handle_compact(
    State(engine): State<Arc<LakehouseEngine>>,
    Json(req): Json<HttpCompactRequest>,
) -> Result<Json<Value>> {
    let eng_clone = Arc::clone(&engine);
    let force = req.force;
    let report = tokio::task::spawn_blocking(move || eng_clone.compact_now(force))
        .await
        .map_err(|e| crate::error::LakehouseError::DataFusion(format!("Compaction task join error: {e}")))??;
    engine.refresh_table_providers().await?;

    Ok(Json(json!({
        "partitions_compacted": report.partitions_compacted,
        "input_files_merged": report.input_files_merged,
        "output_segments_written": report.output_segments_written,
        "rows_before_dedup": report.rows_before_dedup,
        "rows_after_dedup": report.rows_after_dedup,
        "output_bytes_total": report.output_bytes_total,
    })))
}

/// Serializes Arrow `RecordBatch`es to JSON while emitting 64-bit `_fp` fixed-point values
/// as exact decimal strings per `00-hyperion-contract.md` §5.1.3 (preventing IEEE-754 > 2^53 truncation in JS/TS).
fn record_batches_to_safe_json(batches: &[RecordBatch]) -> Vec<Map<String, Value>> {
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    let mut rows = Vec::with_capacity(total);

    for batch in batches {
        let schema = batch.schema();
        let num_cols = batch.num_columns();
        for r in 0..batch.num_rows() {
            let mut obj = Map::with_capacity(num_cols);
            for c in 0..num_cols {
                let col_name = schema.field(c).name();
                let is_fp_col = col_name.ends_with("_fp");
                let arr = batch.column(c);
                let val = if arr.is_null(r) {
                    Value::Null
                } else if let Some(a) = arr.as_any().downcast_ref::<Int64Array>() {
                    let v = a.value(r);
                    if is_fp_col || v.unsigned_abs() > 9_007_199_254_740_991 {
                        Value::String(v.to_string())
                    } else {
                        json!(v)
                    }
                } else if let Some(a) = arr.as_any().downcast_ref::<UInt64Array>() {
                    let v = a.value(r);
                    if is_fp_col || v > 9_007_199_254_740_991 {
                        Value::String(v.to_string())
                    } else {
                        json!(v)
                    }
                } else if let Some(a) = arr.as_any().downcast_ref::<UInt32Array>() {
                    json!(a.value(r))
                } else if let Some(a) = arr.as_any().downcast_ref::<Int32Array>() {
                    json!(a.value(r))
                } else if let Some(a) = arr.as_any().downcast_ref::<BooleanArray>() {
                    json!(a.value(r))
                } else if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
                    Value::String(a.value(r).to_string())
                } else if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<Int8Type>>() {
                    if let Some(vals) = d.values().as_any().downcast_ref::<StringArray>() {
                        let k = d.keys().value(r) as usize;
                        Value::String(vals.value(k).to_string())
                    } else {
                        Value::String(arrow::util::display::array_value_to_string(arr, r).unwrap_or_default())
                    }
                } else {
                    Value::String(arrow::util::display::array_value_to_string(arr, r).unwrap_or_default())
                };
                obj.insert(col_name.clone(), val);
            }
            rows.push(obj);
        }
    }
    rows
}
