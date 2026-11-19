use crate::api::QueryServiceState;
use crate::candle::Timeframe;
use crate::fixed_point::calc_taker_buy_ratio_ppm;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tower_http::cors::CorsLayer;

pub fn build_http_router(state: QueryServiceState) -> Router {
    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/metrics", get(handle_metrics))
        .route("/api/v1/analytics/candles", get(handle_get_candles))
        .route(
            "/api/v1/analytics/microstructure",
            get(handle_get_microstructure),
        )
        .route(
            "/api/v1/analytics/volume-profile",
            get(handle_get_volume_profile),
        )
        .route("/api/v1/analytics/heatmap", get(handle_get_heatmap))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[derive(Debug, Serialize)]
pub struct HealthStatusDto {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
    pub engine_seq: String,
    pub ts_ns: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorEnvelopeDto {
    pub code: &'static str,
    pub message: String,
    pub trace_id: String,
    pub ts_ns: String,
}

/// Exact OpenAPI 3.1 `OhlcvCandleDto` (`contracts/gateway-openapi.yaml` lines 520-573)
/// with stringified fixed-point integers (`FixedPointUint` / `Uint64String`).
#[derive(Debug, Serialize, Deserialize)]
pub struct OhlcvCandleDto {
    pub symbol: String,
    pub interval: String,
    pub bucket_open_ts_ns: String,
    pub bucket_close_ts_ns: String,
    pub open_fp: String,
    pub high_fp: String,
    pub low_fp: String,
    pub close_fp: String,
    pub base_volume_fp: String,
    pub quote_volume_fp: String,
    pub taker_buy_base_volume_fp: String,
    pub taker_sell_base_volume_fp: String,
    pub taker_buy_ratio_ppm: u32,
    pub vwap_fp: String,
    pub twap_fp: String,
    pub trade_count: String,
    pub last_engine_seq: String,
    pub is_closed: bool,
}

/// Exact OpenAPI 3.1 `MicrostructureDto` (`contracts/gateway-openapi.yaml` lines 574-617).
#[derive(Debug, Serialize, Deserialize)]
pub struct MicrostructureDto {
    pub symbol: String,
    pub engine_seq: String,
    pub ts_ns: String,
    pub mid_price_fp: String,
    pub spread_fp: String,
    pub spread_bps: u32,
    pub rolling_vwap_fp: String,
    pub rolling_twap_fp: String,
    pub ofi_fp: String,
    pub book_imbalance_top5_ppm: i64,
    pub book_imbalance_top10_ppm: i64,
    pub book_imbalance_top20_ppm: i64,
    pub kyles_lambda_fp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VolumeProfileBinDto {
    pub price_bucket_fp: String,
    pub buy_volume_fp: String,
    pub sell_volume_fp: String,
    pub total_volume_fp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VolumeProfileDto {
    pub symbol: String,
    pub poc_price_fp: String,
    pub value_area_high_fp: String,
    pub value_area_low_fp: String,
    pub bins: Vec<VolumeProfileBinDto>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LiquidityHeatmapBucketDto {
    pub bucket_ts_ns: String,
    pub price_bucket_fp: String,
    pub resting_bid_qty_fp: String,
    pub resting_ask_qty_fp: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LiquidityHeatmapDto {
    pub symbol: String,
    pub buckets: Vec<LiquidityHeatmapBucketDto>,
}

#[derive(Debug, Deserialize)]
pub struct CandlesQueryParams {
    pub symbol: String,
    pub interval: String,
    #[serde(default)]
    pub from_ts_ns: Option<u64>,
    #[serde(default)]
    pub to_ts_ns: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SymbolOnlyParams {
    pub symbol: String,
}

#[derive(Debug, Deserialize)]
pub struct VolumeProfileQueryParams {
    pub symbol: String,
    #[serde(default)]
    pub from_ts_ns: Option<u64>,
    #[serde(default)]
    pub to_ts_ns: Option<u64>,
    #[serde(default)]
    pub bucket_size_fp: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct HeatmapQueryParams {
    pub symbol: String,
    #[serde(default)]
    pub from_ts_ns: Option<u64>,
    #[serde(default)]
    pub to_ts_ns: Option<u64>,
    #[serde(default)]
    pub price_bucket_size_fp: Option<u64>,
    #[serde(default)]
    pub time_bucket_ns: Option<u64>,
}

fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ErrorEnvelopeDto>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorEnvelopeDto {
            code: "INVALID_QUERY_PARAMETER",
            message: msg.into(),
            trace_id: "00000000000000000000000000000000".to_string(),
            ts_ns: QueryServiceState::now_ns().to_string(),
        }),
    )
}

fn internal_err(msg: impl Into<String>) -> (StatusCode, Json<ErrorEnvelopeDto>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorEnvelopeDto {
            code: "STORAGE_QUERY_ERROR",
            message: msg.into(),
            trace_id: "00000000000000000000000000000000".to_string(),
            ts_ns: QueryServiceState::now_ns().to_string(),
        }),
    )
}

async fn handle_healthz(State(state): State<QueryServiceState>) -> Json<HealthStatusDto> {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    let max_seq = {
        let g = state.engine.read();
        ["BTC-USDT", "ETH-USDT", "SOL-USDT", "HYPE-USDT"]
            .iter()
            .filter_map(|s| g.query_latest_microstructure(s).map(|m| m.engine_seq))
            .max()
            .unwrap_or(0)
    };

    Json(HealthStatusDto {
        status: "ok",
        service: "stream-aggregator",
        version: env!("CARGO_PKG_VERSION"),
        engine_seq: max_seq.to_string(),
        ts_ns: QueryServiceState::now_ns().to_string(),
    })
}

async fn handle_metrics(State(state): State<QueryServiceState>) -> impl IntoResponse {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.render_prometheus_metrics(),
    )
}

async fn handle_get_candles(
    State(state): State<QueryServiceState>,
    Query(params): Query<CandlesQueryParams>,
) -> Result<Json<Vec<OhlcvCandleDto>>, (StatusCode, Json<ErrorEnvelopeDto>)> {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    if params.symbol.trim().is_empty() {
        return Err(bad_request("symbol query parameter is required"));
    }
    let Some(tf) = Timeframe::from_code(&params.interval) else {
        return Err(bad_request(format!(
            "unsupported interval '{}'; expected 1s, 5s, 1m, 5m, 15m, or 1h",
            params.interval
        )));
    };
    let limit = params.limit.unwrap_or(300).clamp(1, 5_000);
    let candles = state
        .merged_candles(
            &params.symbol,
            tf,
            params.from_ts_ns.unwrap_or(0),
            params.to_ts_ns.unwrap_or(0),
            limit,
        )
        .await
        .map_err(|e| internal_err(e.to_string()))?;

    let dtos: Vec<OhlcvCandleDto> = candles
        .into_iter()
        .map(|c| {
            let base_v = c.base_volume_fp.map(u128::from).unwrap_or(0);
            let quote_v = c.quote_volume_fp.map(u128::from).unwrap_or(0);
            let buy_v = c.taker_buy_base_volume_fp.map(u128::from).unwrap_or(0);
            let sell_v = c.taker_sell_base_volume_fp.map(u128::from).unwrap_or(0);
            OhlcvCandleDto {
                symbol: c.symbol,
                interval: tf.code().to_string(),
                bucket_open_ts_ns: c.bucket_open_ts_ns.to_string(),
                bucket_close_ts_ns: c.bucket_close_ts_ns.to_string(),
                open_fp: c.open_fp.to_string(),
                high_fp: c.high_fp.to_string(),
                low_fp: c.low_fp.to_string(),
                close_fp: c.close_fp.to_string(),
                base_volume_fp: base_v.to_string(),
                quote_volume_fp: quote_v.to_string(),
                taker_buy_base_volume_fp: buy_v.to_string(),
                taker_sell_base_volume_fp: sell_v.to_string(),
                taker_buy_ratio_ppm: calc_taker_buy_ratio_ppm(buy_v, base_v),
                vwap_fp: c.vwap_fp.to_string(),
                twap_fp: c.twap_fp.to_string(),
                trade_count: c.trade_count.to_string(),
                last_engine_seq: c.last_engine_seq.to_string(),
                is_closed: c.is_closed,
            }
        })
        .collect();

    Ok(Json(dtos))
}

async fn handle_get_microstructure(
    State(state): State<QueryServiceState>,
    Query(params): Query<SymbolOnlyParams>,
) -> Result<Json<MicrostructureDto>, (StatusCode, Json<ErrorEnvelopeDto>)> {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    if params.symbol.trim().is_empty() {
        return Err(bad_request("symbol query parameter is required"));
    }

    let m = state
        .latest_microstructure(&params.symbol)
        .await
        .map_err(|e| internal_err(e.to_string()))?;

    let dto = match m {
        Some(m) => {
            let ofi = m.ofi_fp.map(i128::from).unwrap_or(0);
            MicrostructureDto {
                symbol: m.symbol,
                engine_seq: m.engine_seq.to_string(),
                ts_ns: m.ts_ns.to_string(),
                mid_price_fp: m.mid_price_fp.to_string(),
                spread_fp: m.spread_fp.to_string(),
                spread_bps: m.spread_bps,
                rolling_vwap_fp: m.rolling_vwap_fp.to_string(),
                rolling_twap_fp: m.rolling_twap_fp.to_string(),
                ofi_fp: ofi.to_string(),
                book_imbalance_top5_ppm: m.book_imbalance_top5_ppm,
                book_imbalance_top10_ppm: m.book_imbalance_top10_ppm,
                book_imbalance_top20_ppm: m.book_imbalance_top20_ppm,
                kyles_lambda_fp: m.kyles_lambda_fp.to_string(),
            }
        }
        None => MicrostructureDto {
            symbol: params.symbol,
            engine_seq: "0".to_string(),
            ts_ns: "0".to_string(),
            mid_price_fp: "0".to_string(),
            spread_fp: "0".to_string(),
            spread_bps: 0,
            rolling_vwap_fp: "0".to_string(),
            rolling_twap_fp: "0".to_string(),
            ofi_fp: "0".to_string(),
            book_imbalance_top5_ppm: 0,
            book_imbalance_top10_ppm: 0,
            book_imbalance_top20_ppm: 0,
            kyles_lambda_fp: "0".to_string(),
        },
    };

    Ok(Json(dto))
}

async fn handle_get_volume_profile(
    State(state): State<QueryServiceState>,
    Query(params): Query<VolumeProfileQueryParams>,
) -> Result<Json<VolumeProfileDto>, (StatusCode, Json<ErrorEnvelopeDto>)> {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    if params.symbol.trim().is_empty() {
        return Err(bad_request("symbol query parameter is required"));
    }

    let vp = state
        .volume_profile(
            &params.symbol,
            params.from_ts_ns.unwrap_or(0),
            params.to_ts_ns.unwrap_or(0),
            params.bucket_size_fp.unwrap_or(0),
        )
        .await
        .map_err(|e| internal_err(e.to_string()))?;

    let bins = vp
        .bins
        .into_iter()
        .map(|b| VolumeProfileBinDto {
            price_bucket_fp: b.price_bucket_fp.to_string(),
            buy_volume_fp: b.buy_volume_fp.map(u128::from).unwrap_or(0).to_string(),
            sell_volume_fp: b.sell_volume_fp.map(u128::from).unwrap_or(0).to_string(),
            total_volume_fp: b.total_volume_fp.map(u128::from).unwrap_or(0).to_string(),
        })
        .collect();

    Ok(Json(VolumeProfileDto {
        symbol: vp.symbol,
        poc_price_fp: vp.poc_price_fp.to_string(),
        value_area_high_fp: vp.value_area_high_fp.to_string(),
        value_area_low_fp: vp.value_area_low_fp.to_string(),
        bins,
    }))
}

async fn handle_get_heatmap(
    State(state): State<QueryServiceState>,
    Query(params): Query<HeatmapQueryParams>,
) -> Result<Json<LiquidityHeatmapDto>, (StatusCode, Json<ErrorEnvelopeDto>)> {
    state.http_requests_total.fetch_add(1, Ordering::Relaxed);
    if params.symbol.trim().is_empty() {
        return Err(bad_request("symbol query parameter is required"));
    }

    let buckets = state
        .heatmap(
            &params.symbol,
            params.from_ts_ns.unwrap_or(0),
            params.to_ts_ns.unwrap_or(0),
            params.price_bucket_size_fp.unwrap_or(0),
            params.time_bucket_ns.unwrap_or(0),
        )
        .await
        .map_err(|e| internal_err(e.to_string()))?;

    let dto_buckets = buckets
        .into_iter()
        .map(|b| LiquidityHeatmapBucketDto {
            bucket_ts_ns: b.bucket_ts_ns.to_string(),
            price_bucket_fp: b.price_bucket_fp.to_string(),
            resting_bid_qty_fp: b.resting_bid_qty_fp.to_string(),
            resting_ask_qty_fp: b.resting_ask_qty_fp.to_string(),
        })
        .collect();

    Ok(Json(LiquidityHeatmapDto {
        symbol: params.symbol,
        buckets: dto_buckets,
    }))
}
