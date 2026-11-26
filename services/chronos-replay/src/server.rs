#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Deserializer, Serialize};
use tokio_stream::Stream;
use tonic::{Request as TonicRequest, Response as TonicResponse, Status as TonicStatus};

use crate::error::ChronosError;
use crate::fixed::{encode_hex_lower, pb_to_i128, pb_to_u128, u128_to_pb};
use crate::pb::{
    chronos_replay_service_server::{ChronosReplayService, ChronosReplayServiceServer},
    time_travel_seek_request::Target,
    AccountPolicyOverride, AccountStatus, ControlReplaySessionRequest,
    ControlReplaySessionResponse, MarketRegime, OrderSide, ReplayControlAction, ReplayFrameStream,
    SelfTradePreventionMode, SimulationPolicyOverride, StateTreeDiffResponse,
    StreamReplayFramesRequest, TimeTravelSeekRequest, TimeTravelSeekResponse, TimelineForkRequest,
};
use crate::playback::PlaybackSessionManager;
use crate::ChronosEngine;

/// Shared runtime telemetry & state across gRPC (:50154) and HTTP/SSE (:8088).
pub struct ChronosServiceState {
    pub engine: Arc<ChronosEngine>,
    pub playback: Arc<PlaybackSessionManager>,
    pub seek_requests_total: AtomicU64,
    pub fork_requests_total: AtomicU64,
    pub replayed_events_total: AtomicU64,
    pub last_seek_latency_us: AtomicU64,
    pub last_fork_latency_us: AtomicU64,
}

impl ChronosServiceState {
    pub fn new(engine: Arc<ChronosEngine>) -> Arc<Self> {
        let playback = Arc::new(PlaybackSessionManager::new(Arc::clone(&engine)));
        Arc::new(Self {
            engine,
            playback,
            seek_requests_total: AtomicU64::new(0),
            fork_requests_total: AtomicU64::new(0),
            replayed_events_total: AtomicU64::new(0),
            last_seek_latency_us: AtomicU64::new(0),
            last_fork_latency_us: AtomicU64::new(0),
        })
    }
}

// =============================================================================
// TONIC gRPC SERVER (`ChronosReplayService` on Port 50154)
// =============================================================================

pub struct ChronosGrpcService {
    state: Arc<ChronosServiceState>,
}

impl ChronosGrpcService {
    pub fn new(state: Arc<ChronosServiceState>) -> Self {
        Self { state }
    }

    pub fn into_server(self) -> ChronosReplayServiceServer<Self> {
        ChronosReplayServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl ChronosReplayService for ChronosGrpcService {
    async fn time_travel_seek(
        &self,
        request: TonicRequest<TimeTravelSeekRequest>,
    ) -> Result<TonicResponse<TimeTravelSeekResponse>, TonicStatus> {
        let req = request.into_inner();
        let out = self.state.engine.seek(&req).map_err(TonicStatus::from)?;
        self.state
            .seek_requests_total
            .fetch_add(1, Ordering::Relaxed);
        self.state
            .replayed_events_total
            .fetch_add(out.response.replayed_wal_events, Ordering::Relaxed);
        self.state
            .last_seek_latency_us
            .store(out.response.reconstruction_latency_us, Ordering::Relaxed);
        Ok(TonicResponse::new(out.response))
    }

    async fn timeline_fork(
        &self,
        request: TonicRequest<TimelineForkRequest>,
    ) -> Result<TonicResponse<StateTreeDiffResponse>, TonicStatus> {
        let req = request.into_inner();
        let diff = self
            .state
            .engine
            .fork_timeline(&req)
            .map_err(TonicStatus::from)?;
        self.state
            .fork_requests_total
            .fetch_add(1, Ordering::Relaxed);
        self.state
            .replayed_events_total
            .fetch_add(diff.replayed_events_count, Ordering::Relaxed);
        self.state
            .last_fork_latency_us
            .store(diff.computation_latency_us, Ordering::Relaxed);
        Ok(TonicResponse::new(diff))
    }

    type StreamReplayFramesStream =
        Pin<Box<dyn Stream<Item = Result<ReplayFrameStream, TonicStatus>> + Send + 'static>>;

    async fn stream_replay_frames(
        &self,
        request: TonicRequest<StreamReplayFramesRequest>,
    ) -> Result<TonicResponse<Self::StreamReplayFramesStream>, TonicStatus> {
        let req = request.into_inner();
        let (_session_id, mut rx) = self
            .state
            .playback
            .start_session(req)
            .map_err(TonicStatus::from)?;

        let stream = async_stream::try_stream! {
            loop {
                match rx.recv().await {
                    Ok(frame) => yield frame,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        };

        Ok(TonicResponse::new(Box::pin(stream)))
    }

    async fn control_replay_session(
        &self,
        request: TonicRequest<ControlReplaySessionRequest>,
    ) -> Result<TonicResponse<ControlReplaySessionResponse>, TonicStatus> {
        let req = request.into_inner();
        let resp = self
            .state
            .playback
            .control_session(&req)
            .map_err(TonicStatus::from)?;
        Ok(TonicResponse::new(resp))
    }
}

// =============================================================================
// AXUM HTTP REST + SSE + PROMETHEUS METRICS SERVER (Port 8088)
// =============================================================================

pub fn build_http_router(state: Arc<ChronosServiceState>) -> Router {
    Router::new()
        .route("/healthz", get(handle_healthz))
        .route("/metrics", get(handle_metrics))
        .route("/api/v1/chronos/timeline", get(handle_timeline_info))
        .route(
            "/api/v1/chronos/seek",
            get(handle_seek_get).post(handle_seek_post),
        )
        .route("/api/v1/chronos/fork", post(handle_fork_post))
        .route("/api/v1/chronos/forks", get(handle_list_forks))
        .route("/events/replay", get(handle_sse_replay))
        .route("/api/v1/chronos/replay/stream", get(handle_sse_replay))
        .route("/events/replay/control", post(handle_replay_control))
        .route(
            "/api/v1/chronos/replay/control",
            post(handle_replay_control),
        )
        .layer(middleware::from_fn(cors_middleware))
        .with_state(state)
}

async fn cors_middleware(req: Request<axum::body::Body>, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if req.method() == Method::OPTIONS {
        let mut resp = StatusCode::NO_CONTENT.into_response();
        attach_cors_headers(resp.headers_mut(), origin.as_deref());
        return resp;
    }

    // SEC-04: Enforce X-Hyperion-Internal-Token on mutating Chronos endpoints when configured or in production.
    let path = req.uri().path();
    let is_mutation = req.method() == Method::POST
        && (path == "/api/v1/chronos/fork"
            || path == "/events/replay/control"
            || path == "/api/v1/chronos/replay/control");
    if is_mutation {
        let expected = std::env::var("HYPERION_INTERNAL_AUTH_TOKEN")
            .unwrap_or_default()
            .trim()
            .to_string();
        let prod = std::env::var("HYPERION_ENV")
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("production");
        let unauthorized = if expected.is_empty() && prod {
            true
        } else if !expected.is_empty() {
            let provided = req
                .headers()
                .get("x-hyperion-internal-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .trim();
            !constant_time_eq(provided.as_bytes(), expected.as_bytes())
        } else {
            false
        };
        if unauthorized {
            let body = ErrorEnvelopeDto {
                code: "UNAUTHORIZED_INTERNAL_ENDPOINT".to_string(),
                message: "Missing or invalid X-Hyperion-Internal-Token".to_string(),
                trace_id: "00000000000000000000000000000008".to_string(),
                ts_ns: now_ts_ns().to_string(),
            };
            let mut resp = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
            attach_cors_headers(resp.headers_mut(), origin.as_deref());
            return resp;
        }
    }

    let mut resp = next.run(req).await;
    attach_cors_headers(resp.headers_mut(), origin.as_deref());
    resp
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn attach_cors_headers(headers: &mut axum::http::HeaderMap, origin: Option<&str>) {
    let allow_origin = if let Ok(raw) = std::env::var("HYPERION_CORS_ORIGINS") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            Some("*".to_string())
        } else {
            let mut matched = None;
            for item in trimmed.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if item == "*" {
                    matched = Some("*".to_string());
                    break;
                }
                if let Some(o) = origin {
                    if item == o {
                        matched = Some(o.to_string());
                        break;
                    }
                }
            }
            matched
        }
    } else if std::env::var("HYPERION_ENV")
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("production")
    {
        origin
            .filter(|o| *o == "http://localhost:3000" || *o == "http://127.0.0.1:3000")
            .map(str::to_owned)
    } else {
        Some("*".to_string())
    };

    if let Some(val) = allow_origin.and_then(|v| HeaderValue::from_str(&v).ok()) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, val);
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type, Authorization, X-Hyperion-Internal-Token"),
    );
}

fn now_ts_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[derive(Serialize)]
struct ErrorEnvelopeDto {
    code: String,
    message: String,
    trace_id: String,
    ts_ns: String,
}

fn http_error(err: ChronosError) -> Response {
    let (status, code) = match &err {
        ChronosError::InvalidArgument(_) => (StatusCode::BAD_REQUEST, "INVALID_ARGUMENT"),
        ChronosError::NotFound(_) => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "CHRONOS_ENGINE_ERROR"),
    };
    let body = ErrorEnvelopeDto {
        code: code.to_string(),
        message: err.to_string(),
        trace_id: "00000000000000000000000000000008".to_string(),
        ts_ns: now_ts_ns().to_string(),
    };
    (status, Json(body)).into_response()
}

// Flexible string-or-integer u64/u128 deserializers for ergonomic REST + OpenAPI 3.1 interop
fn de_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrNum {
        Str(String),
        Num(u64),
    }
    match Option::<StrOrNum>::deserialize(d)? {
        None => Ok(None),
        Some(StrOrNum::Num(n)) => Ok(Some(n)),
        Some(StrOrNum::Str(s)) => {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                s.trim()
                    .parse::<u64>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

fn de_u64_default<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(de_opt_u64(d)?.unwrap_or(0))
}

fn de_opt_u128<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u128>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StrOrNum {
        Str(String),
        Num(u64),
    }
    match Option::<StrOrNum>::deserialize(d)? {
        None => Ok(None),
        Some(StrOrNum::Num(n)) => Ok(Some(n as u128)),
        Some(StrOrNum::Str(s)) => {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                s.trim()
                    .parse::<u128>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

fn de_vec_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Str(String),
        Num(u64),
    }
    let raw = Option::<Vec<Item>>::deserialize(d)?.unwrap_or_default();
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        match item {
            Item::Num(n) => out.push(n),
            Item::Str(s) => out.push(s.trim().parse::<u64>().map_err(serde::de::Error::custom)?),
        }
    }
    Ok(out)
}

#[derive(Serialize)]
struct HealthDto {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    engine_seq: String,
    ts_ns: String,
}

async fn handle_healthz(State(state): State<Arc<ChronosServiceState>>) -> impl IntoResponse {
    Json(HealthDto {
        status: "ok",
        service: "chronos-replay",
        version: "1.0.0",
        engine_seq: state.engine.store().max_engine_seq().to_string(),
        ts_ns: now_ts_ns().to_string(),
    })
}

async fn handle_metrics(State(state): State<Arc<ChronosServiceState>>) -> impl IntoResponse {
    let body = format!(
        "# HELP chronos_max_engine_seq Highest verified WAL sequence loaded in Chronos\n\
         # TYPE chronos_max_engine_seq gauge\n\
         chronos_max_engine_seq {}\n\
         # HELP chronos_wal_chain_verified Cryptographic Blake3 + CRC32C WAL chain status\n\
         # TYPE chronos_wal_chain_verified gauge\n\
         chronos_wal_chain_verified {}\n\
         # HELP chronos_seek_requests_total Total TimeTravelSeek operations served\n\
         # TYPE chronos_seek_requests_total counter\n\
         chronos_seek_requests_total {}\n\
         # HELP chronos_fork_requests_total Total Counterfactual TimelineFork simulations executed\n\
         # TYPE chronos_fork_requests_total counter\n\
         chronos_fork_requests_total {}\n\
         # HELP chronos_replayed_events_total Cumulative WAL delta events replayed\n\
         # TYPE chronos_replayed_events_total counter\n\
         chronos_replayed_events_total {}\n\
         # HELP chronos_last_seek_latency_microseconds Latency of latest TimeTravelSeek in us\n\
         # TYPE chronos_last_seek_latency_microseconds gauge\n\
         chronos_last_seek_latency_microseconds {}\n\
         # HELP chronos_last_fork_latency_microseconds Latency of latest TimelineFork in us\n\
         # TYPE chronos_last_fork_latency_microseconds gauge\n\
         chronos_last_fork_latency_microseconds {}\n\
         # HELP chronos_active_vcr_sessions Active variable-speed VCR streaming sessions\n\
         # TYPE chronos_active_vcr_sessions gauge\n\
         chronos_active_vcr_sessions {}\n",
        state.engine.store().max_engine_seq(),
        usize::from(state.engine.store().is_chain_verified()),
        state.seek_requests_total.load(Ordering::Relaxed),
        state.fork_requests_total.load(Ordering::Relaxed),
        state.replayed_events_total.load(Ordering::Relaxed),
        state.last_seek_latency_us.load(Ordering::Relaxed),
        state.last_fork_latency_us.load(Ordering::Relaxed),
        state.playback.active_sessions_count(),
    );
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

#[derive(Serialize)]
struct TimelineInfoDto {
    min_engine_seq: String,
    max_engine_seq: String,
    max_ts_ns: String,
    total_events: String,
    snapshots_count: usize,
    sparse_index_entries: usize,
    wal_chain_verified: bool,
}

async fn handle_timeline_info(State(state): State<Arc<ChronosServiceState>>) -> impl IntoResponse {
    let store = state.engine.store();
    Json(TimelineInfoDto {
        min_engine_seq: store.min_engine_seq().to_string(),
        max_engine_seq: store.max_engine_seq().to_string(),
        max_ts_ns: store.max_ts_ns().to_string(),
        total_events: store.events().len().to_string(),
        snapshots_count: store.snapshots().len(),
        sparse_index_entries: store.sparse_index().len(),
        wal_chain_verified: store.is_chain_verified(),
    })
}

#[derive(Debug, Default, Deserialize)]
struct SeekParamsDto {
    #[serde(default)]
    symbol: String,
    #[serde(default, deserialize_with = "de_opt_u64")]
    target_engine_seq: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    target_ts_ns: Option<u64>,
    #[serde(default)]
    l2_depth: Option<u32>,
    #[serde(default)]
    include_account_balances: Option<bool>,
    #[serde(default)]
    verify_wal_hash_chain: Option<bool>,
}

#[derive(Serialize)]
struct PriceLevelDto {
    price_fp: String,
    total_qty_fp: String,
    order_count: u32,
}

#[derive(Serialize)]
struct L3OrderDto {
    order_id: String,
    account_id: String,
    side: &'static str,
    price_fp: String,
    remaining_qty_fp: String,
    priority_seq: String,
}

#[derive(Serialize)]
struct AccountBalanceLeafDto {
    asset: String,
    available_fp: String,
    locked_fp: String,
    total_fp: String,
}

#[derive(Serialize)]
struct AccountMerkleStateDto {
    account_id: String,
    balances: Vec<AccountBalanceLeafDto>,
    leaf_blake3_hash: String,
    double_entry_balanced: bool,
}

#[derive(Serialize)]
struct LiquidityMetricsDto {
    best_bid_fp: String,
    best_ask_fp: String,
    mid_price_fp: String,
    spread_fp: String,
    spread_bps: u32,
    total_bid_depth_fp: String,
    total_ask_depth_fp: String,
    book_imbalance_top5_ppm: i64,
    last_clearing_price_fp: String,
    vwap_fp: String,
}

/// Satisfies OpenAPI `L2BookSnapshotDto` top-level fields while enriching with full L3, account
/// balances, liquidity metrics, and Chronos seek diagnostics.
#[derive(Serialize)]
struct SeekResponseDto {
    engine_seq: String,
    symbol: String,
    bids: Vec<PriceLevelDto>,
    asks: Vec<PriceLevelDto>,
    best_bid_fp: String,
    best_ask_fp: String,
    mid_price_fp: String,
    spread_fp: String,
    snapshot_ts_ns: String,
    state_root_blake3: String,
    resolved_engine_seq: String,
    resolved_ts_ns: String,
    base_snapshot_seq: String,
    replayed_wal_events: String,
    reconstruction_latency_us: u64,
    wal_chain_verified: bool,
    l3_orders: Vec<L3OrderDto>,
    liquidity_metrics: LiquidityMetricsDto,
    account_states: Vec<AccountMerkleStateDto>,
}

async fn handle_seek_get(
    State(state): State<Arc<ChronosServiceState>>,
    Query(params): Query<SeekParamsDto>,
) -> Response {
    execute_http_seek(state, params)
}

async fn handle_seek_post(
    State(state): State<Arc<ChronosServiceState>>,
    Json(params): Json<SeekParamsDto>,
) -> Response {
    execute_http_seek(state, params)
}

fn execute_http_seek(state: Arc<ChronosServiceState>, params: SeekParamsDto) -> Response {
    let target = if let Some(seq) = params.target_engine_seq {
        Some(Target::TargetEngineSeq(seq))
    } else {
        params.target_ts_ns.map(Target::TargetTsNs)
    };
    let req = TimeTravelSeekRequest {
        symbol: if params.symbol.is_empty() {
            "BTC-USDT".to_string()
        } else {
            params.symbol
        },
        target,
        l2_depth: params.l2_depth.unwrap_or(50),
        include_account_balances: params.include_account_balances.unwrap_or(true),
        verify_wal_hash_chain: params.verify_wal_hash_chain.unwrap_or(false),
        trace: None,
    };

    let out = match state.engine.seek(&req) {
        Ok(v) => v,
        Err(e) => return http_error(e),
    };
    state.seek_requests_total.fetch_add(1, Ordering::Relaxed);
    state
        .replayed_events_total
        .fetch_add(out.response.replayed_wal_events, Ordering::Relaxed);
    state
        .last_seek_latency_us
        .store(out.response.reconstruction_latency_us, Ordering::Relaxed);

    let l2 = out.response.l2_snapshot.unwrap_or_default();
    let root_hex = encode_hex_lower(&out.response.state_root_blake3);

    let bids = l2
        .bids
        .into_iter()
        .map(|b| PriceLevelDto {
            price_fp: b.price_fp.to_string(),
            total_qty_fp: b.total_qty_fp.to_string(),
            order_count: b.order_count,
        })
        .collect();
    let asks = l2
        .asks
        .into_iter()
        .map(|a| PriceLevelDto {
            price_fp: a.price_fp.to_string(),
            total_qty_fp: a.total_qty_fp.to_string(),
            order_count: a.order_count,
        })
        .collect();
    let l3_orders = out
        .l3_orders
        .into_iter()
        .map(|o| L3OrderDto {
            order_id: o.order_id.to_string(),
            account_id: o.account_id.to_string(),
            side: if o.side == 2 { "SELL" } else { "BUY" },
            price_fp: o.price_fp.to_string(),
            remaining_qty_fp: o.remaining_qty_fp.to_string(),
            priority_seq: o.priority_seq.to_string(),
        })
        .collect();
    let account_states = out
        .response
        .account_states
        .into_iter()
        .map(|a| AccountMerkleStateDto {
            account_id: a.account_id.to_string(),
            balances: a
                .balances
                .into_iter()
                .map(|b| AccountBalanceLeafDto {
                    asset: b.asset,
                    available_fp: pb_to_i128(b.available_fp).to_string(),
                    locked_fp: pb_to_i128(b.locked_fp).to_string(),
                    total_fp: pb_to_i128(b.total_fp).to_string(),
                })
                .collect(),
            leaf_blake3_hash: encode_hex_lower(&a.leaf_blake3_hash),
            double_entry_balanced: a.double_entry_balanced,
        })
        .collect();

    let liq = out.liquidity;
    Json(SeekResponseDto {
        engine_seq: l2.engine_seq.to_string(),
        symbol: l2.symbol,
        bids,
        asks,
        best_bid_fp: l2.best_bid_fp.to_string(),
        best_ask_fp: l2.best_ask_fp.to_string(),
        mid_price_fp: l2.mid_price_fp.to_string(),
        spread_fp: l2.spread_fp.to_string(),
        snapshot_ts_ns: l2.snapshot_ts_ns.to_string(),
        state_root_blake3: root_hex,
        resolved_engine_seq: out.response.resolved_engine_seq.to_string(),
        resolved_ts_ns: out.response.resolved_ts_ns.to_string(),
        base_snapshot_seq: out.response.base_snapshot_seq.to_string(),
        replayed_wal_events: out.response.replayed_wal_events.to_string(),
        reconstruction_latency_us: out.response.reconstruction_latency_us,
        wal_chain_verified: out.response.wal_chain_verified,
        l3_orders,
        liquidity_metrics: LiquidityMetricsDto {
            best_bid_fp: liq.best_bid_fp.to_string(),
            best_ask_fp: liq.best_ask_fp.to_string(),
            mid_price_fp: liq.mid_price_fp.to_string(),
            spread_fp: liq.spread_fp.to_string(),
            spread_bps: liq.spread_bps,
            total_bid_depth_fp: liq.total_bid_depth_fp.to_string(),
            total_ask_depth_fp: liq.total_ask_depth_fp.to_string(),
            book_imbalance_top5_ppm: liq.book_imbalance_top5_ppm,
            last_clearing_price_fp: liq.last_clearing_price_fp.to_string(),
            vwap_fp: liq.vwap_fp.to_string(),
        },
        account_states,
    })
    .into_response()
}

#[derive(Debug, Default, Deserialize)]
struct AccountOverrideDto {
    #[serde(deserialize_with = "de_u64_default")]
    account_id: u64,
    #[serde(default)]
    status_override: Option<String>,
    #[serde(default, deserialize_with = "de_u64_default")]
    effective_from_seq: u64,
    #[serde(default, deserialize_with = "de_u64_default")]
    effective_from_ts_ns: u64,
    #[serde(default)]
    rate_limit_rps: u32,
    #[serde(default = "default_true")]
    cancel_resting_orders_on_activation: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
struct NestedPolicyDto {
    #[serde(default)]
    maker_fee_ppm_override: Option<i64>,
    #[serde(default)]
    taker_fee_ppm_override: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    max_order_qty_fp_override: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_u128")]
    max_order_notional_fp_override: Option<u128>,
    #[serde(default)]
    stp_mode_override: Option<String>,
    #[serde(default)]
    regime_override: Option<String>,
    #[serde(default)]
    volatility_guard_spread_bps: Option<u32>,
    #[serde(default)]
    account_overrides: Vec<AccountOverrideDto>,
}

/// Supports both OpenAPI flat `TimelineForkPayload` AND rich nested `policy_override`.
#[derive(Debug, Default, Deserialize)]
struct ForkRequestDto {
    #[serde(default)]
    fork_name: String,
    #[serde(default)]
    symbol: String,
    #[serde(default, deserialize_with = "de_u64_default")]
    start_engine_seq: u64,
    #[serde(default, deserialize_with = "de_u64_default")]
    end_engine_seq: u64,
    #[serde(default, deserialize_with = "de_u64_default")]
    start_ts_ns: u64,
    #[serde(default, deserialize_with = "de_u64_default")]
    end_ts_ns: u64,
    #[serde(default)]
    maker_fee_ppm_override: Option<i64>,
    #[serde(default)]
    taker_fee_ppm_override: Option<i64>,
    #[serde(default, deserialize_with = "de_opt_u64")]
    max_order_qty_fp_override: Option<u64>,
    #[serde(default, deserialize_with = "de_vec_u64")]
    blocked_account_ids: Vec<u64>,
    #[serde(default)]
    policy_override: Option<NestedPolicyDto>,
}

#[derive(Serialize)]
struct AccountBalanceDiffDto {
    account_id: String,
    asset: String,
    baseline_total_fp: String,
    forked_total_fp: String,
    delta_total_fp: String,
}

#[derive(Serialize)]
struct PriceLevelDiffDto {
    side: &'static str,
    price_fp: String,
    baseline_qty_fp: String,
    forked_qty_fp: String,
    delta_qty_fp: String,
}

#[derive(Serialize)]
struct StateTreeDiffDto {
    fork_id: String,
    symbol: String,
    start_engine_seq: String,
    end_engine_seq: String,
    baseline_state_root_blake3: String,
    forked_state_root_blake3: String,
    state_identical: bool,
    baseline_clearing_price_fp: String,
    forked_clearing_price_fp: String,
    clearing_price_delta_fp: String,
    baseline_volatility_bps: u32,
    forked_volatility_bps: u32,
    max_slippage_delta_bps: i32,
    prevented_wash_volume_fp: String,
    prevented_fills_count: String,
    rejected_orders_count: String,
    treasury_fee_delta_fp: String,
    account_diffs: Vec<AccountBalanceDiffDto>,
    l2_level_diffs: Vec<PriceLevelDiffDto>,
    replayed_events_count: String,
    computation_latency_us: u64,
}

fn map_diff_to_dto(diff: StateTreeDiffResponse) -> StateTreeDiffDto {
    StateTreeDiffDto {
        fork_id: diff.fork_id,
        symbol: diff.symbol,
        start_engine_seq: diff.start_engine_seq.to_string(),
        end_engine_seq: diff.end_engine_seq.to_string(),
        baseline_state_root_blake3: encode_hex_lower(&diff.baseline_state_root_blake3),
        forked_state_root_blake3: encode_hex_lower(&diff.forked_state_root_blake3),
        state_identical: diff.state_identical,
        baseline_clearing_price_fp: diff.baseline_clearing_price_fp.to_string(),
        forked_clearing_price_fp: diff.forked_clearing_price_fp.to_string(),
        clearing_price_delta_fp: diff.clearing_price_delta_fp.to_string(),
        baseline_volatility_bps: diff.baseline_volatility_bps,
        forked_volatility_bps: diff.forked_volatility_bps,
        max_slippage_delta_bps: diff.max_slippage_delta_bps,
        prevented_wash_volume_fp: pb_to_u128(diff.prevented_wash_volume_fp).to_string(),
        prevented_fills_count: diff.prevented_fills_count.to_string(),
        rejected_orders_count: diff.rejected_orders_count.to_string(),
        treasury_fee_delta_fp: pb_to_i128(diff.treasury_fee_delta_fp).to_string(),
        account_diffs: diff
            .account_diffs
            .into_iter()
            .map(|a| AccountBalanceDiffDto {
                account_id: a.account_id.to_string(),
                asset: a.asset,
                baseline_total_fp: pb_to_i128(a.baseline_total_fp).to_string(),
                forked_total_fp: pb_to_i128(a.forked_total_fp).to_string(),
                delta_total_fp: pb_to_i128(a.delta_total_fp).to_string(),
            })
            .collect(),
        l2_level_diffs: diff
            .l2_level_diffs
            .into_iter()
            .map(|l| PriceLevelDiffDto {
                side: if l.side == OrderSide::Sell as i32 {
                    "SELL"
                } else {
                    "BUY"
                },
                price_fp: l.price_fp.to_string(),
                baseline_qty_fp: l.baseline_qty_fp.to_string(),
                forked_qty_fp: l.forked_qty_fp.to_string(),
                delta_qty_fp: l.delta_qty_fp.to_string(),
            })
            .collect(),
        replayed_events_count: diff.replayed_events_count.to_string(),
        computation_latency_us: diff.computation_latency_us,
    }
}

fn parse_account_status(s: Option<&str>) -> i32 {
    match s.unwrap_or("FROZEN").to_ascii_uppercase().as_str() {
        "ACTIVE" | "ACCOUNT_STATUS_ACTIVE" => AccountStatus::Active as i32,
        "THROTTLED" | "ACCOUNT_STATUS_THROTTLED" => AccountStatus::Throttled as i32,
        "LIQUIDATION_ONLY" | "ACCOUNT_STATUS_LIQUIDATION_ONLY" => {
            AccountStatus::LiquidationOnly as i32
        }
        _ => AccountStatus::Frozen as i32,
    }
}

fn parse_stp_mode(s: Option<&str>) -> i32 {
    match s.unwrap_or("").to_ascii_uppercase().as_str() {
        "NONE" | "SELF_TRADE_PREVENTION_MODE_NONE" => SelfTradePreventionMode::None as i32,
        "CANCEL_RESTING" | "SELF_TRADE_PREVENTION_MODE_CANCEL_RESTING" => {
            SelfTradePreventionMode::CancelResting as i32
        }
        "CANCEL_AGGRESSING" | "SELF_TRADE_PREVENTION_MODE_CANCEL_AGGRESSING" => {
            SelfTradePreventionMode::CancelAggressing as i32
        }
        "CANCEL_BOTH" | "SELF_TRADE_PREVENTION_MODE_CANCEL_BOTH" => {
            SelfTradePreventionMode::CancelBoth as i32
        }
        _ => SelfTradePreventionMode::Unspecified as i32,
    }
}

fn parse_regime(s: Option<&str>) -> i32 {
    match s.unwrap_or("").to_ascii_uppercase().as_str() {
        "NORMAL" | "MARKET_REGIME_NORMAL" => MarketRegime::Normal as i32,
        "VOLATILITY_GUARD" | "MARKET_REGIME_VOLATILITY_GUARD" => {
            MarketRegime::VolatilityGuard as i32
        }
        "POST_ONLY_AUCTION" | "MARKET_REGIME_POST_ONLY_AUCTION" => {
            MarketRegime::PostOnlyAuction as i32
        }
        "HALTED" | "MARKET_REGIME_HALTED" => MarketRegime::Halted as i32,
        _ => MarketRegime::Unspecified as i32,
    }
}

async fn handle_fork_post(
    State(state): State<Arc<ChronosServiceState>>,
    Json(payload): Json<ForkRequestDto>,
) -> Response {
    let nested = payload.policy_override.unwrap_or_default();
    let mut account_overrides: Vec<AccountPolicyOverride> = nested
        .account_overrides
        .into_iter()
        .map(|ao| AccountPolicyOverride {
            account_id: ao.account_id,
            status_override: parse_account_status(ao.status_override.as_deref()),
            effective_from_seq: ao.effective_from_seq,
            effective_from_ts_ns: ao.effective_from_ts_ns,
            rate_limit_rps: ao.rate_limit_rps,
            cancel_resting_orders_on_activation: ao.cancel_resting_orders_on_activation,
        })
        .collect();

    for blocked_id in payload.blocked_account_ids {
        if !account_overrides.iter().any(|a| a.account_id == blocked_id) {
            account_overrides.push(AccountPolicyOverride {
                account_id: blocked_id,
                status_override: AccountStatus::Frozen as i32,
                effective_from_seq: payload.start_engine_seq,
                effective_from_ts_ns: payload.start_ts_ns,
                rate_limit_rps: 0,
                cancel_resting_orders_on_activation: true,
            });
        }
    }

    let req = TimelineForkRequest {
        fork_name: payload.fork_name,
        symbol: if payload.symbol.is_empty() {
            "BTC-USDT".to_string()
        } else {
            payload.symbol
        },
        start_engine_seq: payload.start_engine_seq,
        end_engine_seq: payload.end_engine_seq,
        start_ts_ns: payload.start_ts_ns,
        end_ts_ns: payload.end_ts_ns,
        policy_override: Some(SimulationPolicyOverride {
            maker_fee_ppm_override: payload
                .maker_fee_ppm_override
                .or(nested.maker_fee_ppm_override),
            taker_fee_ppm_override: payload
                .taker_fee_ppm_override
                .or(nested.taker_fee_ppm_override),
            max_order_qty_fp_override: payload
                .max_order_qty_fp_override
                .or(nested.max_order_qty_fp_override),
            max_order_notional_fp_override: nested.max_order_notional_fp_override.map(u128_to_pb),
            stp_mode_override: parse_stp_mode(nested.stp_mode_override.as_deref()),
            regime_override: parse_regime(nested.regime_override.as_deref()),
            volatility_guard_spread_bps: nested.volatility_guard_spread_bps,
            account_overrides,
        }),
        trace: None,
    };

    let diff = match state.engine.fork_timeline(&req) {
        Ok(d) => d,
        Err(e) => return http_error(e),
    };
    state.fork_requests_total.fetch_add(1, Ordering::Relaxed);
    state
        .replayed_events_total
        .fetch_add(diff.replayed_events_count, Ordering::Relaxed);
    state
        .last_fork_latency_us
        .store(diff.computation_latency_us, Ordering::Relaxed);

    Json(map_diff_to_dto(diff)).into_response()
}

async fn handle_list_forks(State(state): State<Arc<ChronosServiceState>>) -> impl IntoResponse {
    let forks: Vec<StateTreeDiffDto> = state
        .engine
        .list_forks()
        .into_iter()
        .map(map_diff_to_dto)
        .collect();
    Json(forks)
}

#[derive(Debug, Default, Deserialize)]
struct SseReplayQuery {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    timeline_id: String,
    #[serde(default)]
    symbol: String,
    #[serde(default, deserialize_with = "de_u64_default")]
    from_engine_seq: u64,
    #[serde(default, deserialize_with = "de_u64_default")]
    to_engine_seq: u64,
    #[serde(default)]
    speed_milli: u32,
    #[serde(default)]
    l2_depth: u32,
}

#[derive(Serialize)]
struct SseFrameDto {
    session_id: String,
    timeline_id: String,
    symbol: String,
    frame_index: String,
    engine_seq: String,
    event_ts_ns: String,
    speed_milli: u32,
    is_paused: bool,
    state_root_blake3: String,
    best_bid_fp: String,
    best_ask_fp: String,
    mid_price_fp: String,
    bids: Vec<PriceLevelDto>,
    asks: Vec<PriceLevelDto>,
    fills_count: usize,
    deltas_count: usize,
}

async fn handle_sse_replay(
    State(state): State<Arc<ChronosServiceState>>,
    Query(q): Query<SseReplayQuery>,
) -> Response {
    let req = StreamReplayFramesRequest {
        session_id: q.session_id,
        timeline_id: q.timeline_id,
        symbol: q.symbol,
        from_engine_seq: q.from_engine_seq,
        to_engine_seq: q.to_engine_seq,
        initial_speed_milli: q.speed_milli,
        l2_depth: q.l2_depth,
    };
    let (_sess_id, mut rx) = match state.playback.start_session(req) {
        Ok(v) => v,
        Err(e) => return http_error(e),
    };

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let l2 = frame.l2_snapshot.unwrap_or_default();
                    let dto = SseFrameDto {
                        session_id: frame.session_id,
                        timeline_id: frame.timeline_id,
                        symbol: frame.symbol,
                        frame_index: frame.frame_index.to_string(),
                        engine_seq: frame.engine_seq.to_string(),
                        event_ts_ns: frame.event_ts_ns.to_string(),
                        speed_milli: frame.speed_milli,
                        is_paused: frame.is_paused,
                        state_root_blake3: encode_hex_lower(&frame.state_root_blake3),
                        best_bid_fp: l2.best_bid_fp.to_string(),
                        best_ask_fp: l2.best_ask_fp.to_string(),
                        mid_price_fp: l2.mid_price_fp.to_string(),
                        bids: l2.bids.into_iter().map(|b| PriceLevelDto {
                            price_fp: b.price_fp.to_string(),
                            total_qty_fp: b.total_qty_fp.to_string(),
                            order_count: b.order_count,
                        }).collect(),
                        asks: l2.asks.into_iter().map(|a| PriceLevelDto {
                            price_fp: a.price_fp.to_string(),
                            total_qty_fp: a.total_qty_fp.to_string(),
                            order_count: a.order_count,
                        }).collect(),
                        fills_count: frame.frame_fills.len(),
                        deltas_count: frame.frame_deltas.len(),
                    };
                    if let Ok(json) = serde_json::to_string(&dto) {
                        yield Ok::<SseEvent, Infallible>(SseEvent::default().event("replay_frame").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(10)))
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ControlReplayDto {
    session_id: String,
    action: String,
    #[serde(default)]
    speed_milli: u32,
    #[serde(default, deserialize_with = "de_u64_default")]
    step_count: u64,
}

#[derive(Serialize)]
struct ControlReplayRespDto {
    session_id: String,
    current_engine_seq: String,
    effective_speed_milli: u32,
    is_paused: bool,
}

async fn handle_replay_control(
    State(state): State<Arc<ChronosServiceState>>,
    Json(dto): Json<ControlReplayDto>,
) -> Response {
    let action = match dto.action.to_ascii_uppercase().as_str() {
        "PLAY" | "REPLAY_CONTROL_ACTION_PLAY" => ReplayControlAction::Play as i32,
        "PAUSE" | "REPLAY_CONTROL_ACTION_PAUSE" => ReplayControlAction::Pause as i32,
        "STEP_FORWARD" | "STEP" | "REPLAY_CONTROL_ACTION_STEP_FORWARD" => {
            ReplayControlAction::StepForward as i32
        }
        "STEP_BACKWARD" | "BACK" | "REPLAY_CONTROL_ACTION_STEP_BACKWARD" => {
            ReplayControlAction::StepBackward as i32
        }
        "REWIND" | "REPLAY_CONTROL_ACTION_REWIND" => ReplayControlAction::Rewind as i32,
        "SET_SPEED" | "SPEED" | "REPLAY_CONTROL_ACTION_SET_SPEED" => {
            ReplayControlAction::SetSpeed as i32
        }
        other => {
            return http_error(ChronosError::InvalidArgument(format!(
                "unsupported VCR action: {other}"
            )));
        }
    };

    let req = ControlReplaySessionRequest {
        session_id: dto.session_id,
        action,
        speed_milli: dto.speed_milli,
        step_count: dto.step_count,
    };

    match state.playback.control_session(&req) {
        Ok(r) => Json(ControlReplayRespDto {
            session_id: r.session_id,
            current_engine_seq: r.current_engine_seq.to_string(),
            effective_speed_milli: r.effective_speed_milli,
            is_paused: r.is_paused,
        })
        .into_response(),
        Err(e) => http_error(e),
    }
}
