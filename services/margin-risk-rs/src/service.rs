//! Hyperion Channel 14 Runtime Servers:
//! - **gRPC Server (`:50163`)**: `hyperion.omega.v1.MarginRiskService`
//!   (`GetPortfolioMargin`, `GetOptionGreeksSurface`, `StreamMarginEvents`).
//! - **HTTP / REST / Prometheus Observability Server (`:8093`)**:
//!   `GET /healthz`, `GET /metrics`, `GET /api/v1/margin/accounts/:account_id`,
//!   `GET /api/v1/margin/surface/:symbol`.

use crate::fixed_point::FP_SCALE;
use crate::greeks_soa::{ExerciseStyle, OptionRight};
use crate::omega_wire::{
    build_option_greeks_surface_proto, pb, to_portfolio_margin_snapshot, TOPIC_MARGIN_ADL,
    TOPIC_MARGIN_GREEKS, TOPIC_MARGIN_LIQUIDATIONS, TOPIC_MARGIN_SNAPSHOTS,
};
use crate::perp_funding::{DynamicFundingCalculator, PerpetualMarkPriceEngine};
use crate::span_margin::{
    evaluate_account_cross_margin, CrossMarginAccountInput, OptionLegInput, SpanRiskConfig,
};
use crate::svi_surface::ArbitrageFreeSviSurface;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use parking_lot::RwLock;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

pub const DEFAULT_GRPC_PORT: u16 = 50163;
pub const DEFAULT_HTTP_PORT: u16 = 8093;

/// Thread-safe shared state powering both `:50163` gRPC and `:8093` HTTP/Metrics endpoints.
pub struct MarginRiskEngineState {
    pub engine_seq: AtomicU64,
    pub total_evaluations: AtomicU64,
    pub total_liquidations: AtomicU64,
    pub total_adl_triggers: AtomicU64,
    pub spot_index_fp: RwLock<HashMap<String, i128>>,
    pub perp_mark_fp: RwLock<HashMap<String, i128>>,
    pub mark_engines: RwLock<HashMap<String, PerpetualMarkPriceEngine>>,
    pub svi_surfaces: RwLock<HashMap<String, ArbitrageFreeSviSurface>>,
    pub accounts: RwLock<HashMap<u64, CrossMarginAccountInput>>,
    pub risk_cfg: SpanRiskConfig,
    pub funding_calc: DynamicFundingCalculator,
    pub event_tx: broadcast::Sender<pb::MarginEventEnvelope>,
}

impl MarginRiskEngineState {
    pub fn new_with_canonical_defaults() -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(4096);
        let btc_spot_fp = 68_500 * FP_SCALE;
        let eth_spot_fp = 3_450 * FP_SCALE;

        let mut spot_map = HashMap::new();
        spot_map.insert("BTC-USDT".to_string(), btc_spot_fp);
        spot_map.insert("ETH-USDT".to_string(), eth_spot_fp);

        let mut perp_map = HashMap::new();
        perp_map.insert("BTC-USDT".to_string(), btc_spot_fp + 25 * FP_SCALE);
        perp_map.insert("ETH-USDT".to_string(), eth_spot_fp + 2 * FP_SCALE);

        let mut mark_engines = HashMap::new();
        mark_engines.insert(
            "BTC-USDT".to_string(),
            PerpetualMarkPriceEngine::new("BTC-USDT-PERP", 100_000, 5_000_000_000, 50_000, 100_000),
        );
        mark_engines.insert(
            "ETH-USDT".to_string(),
            PerpetualMarkPriceEngine::new("ETH-USDT-PERP", 100_000, 5_000_000_000, 50_000, 100_000),
        );

        let mut surfaces = HashMap::new();
        let btc_pillars = [
            (7.0 / 365.0, 68_500.0, 0.58),
            (30.0 / 365.0, 68_850.0, 0.60),
            (90.0 / 365.0, 69_600.0, 0.63),
            (180.0 / 365.0, 70_750.0, 0.65),
        ];
        if let Ok(surf) =
            ArbitrageFreeSviSurface::from_ssvi_term_structure(&btc_pillars, -0.35, 0.85, 0.40)
        {
            surfaces.insert("BTC-USDT".to_string(), surf);
        }
        let eth_pillars = [
            (7.0 / 365.0, 3_450.0, 0.66),
            (30.0 / 365.0, 3_470.0, 0.68),
            (90.0 / 365.0, 3_510.0, 0.70),
        ];
        if let Ok(surf) =
            ArbitrageFreeSviSurface::from_ssvi_term_structure(&eth_pillars, -0.30, 0.80, 0.40)
        {
            surfaces.insert("ETH-USDT".to_string(), surf);
        }

        // Pre-seed demo institutional account `10042` (matching contract test fixtures)
        let mut accounts = HashMap::new();
        accounts.insert(
            10042,
            CrossMarginAccountInput {
                account_id: 10042,
                quote_cash_fp: 250_000 * FP_SCALE,
                spot_qty_fp: 2 * FP_SCALE,
                perp_qty_fp: -3 * FP_SCALE,
                perp_entry_price_fp: btc_spot_fp,
                open_order_margin_fp: 5_000 * FP_SCALE,
                options: vec![
                    OptionLegInput {
                        strike_fp: 68_000 * FP_SCALE,
                        time_to_expiry_yrs: 30.0 / 365.0,
                        implied_vol: 0.60,
                        rate_r: 0.04,
                        carry_q: 0.01,
                        right: OptionRight::Call,
                        style: ExerciseStyle::European,
                        qty_fp: 2 * FP_SCALE,
                    },
                    OptionLegInput {
                        strike_fp: 64_000 * FP_SCALE,
                        time_to_expiry_yrs: 30.0 / 365.0,
                        implied_vol: 0.63,
                        rate_r: 0.04,
                        carry_q: 0.01,
                        right: OptionRight::Put,
                        style: ExerciseStyle::European,
                        qty_fp: FP_SCALE,
                    },
                ],
            },
        );

        Arc::new(Self {
            engine_seq: AtomicU64::new(1),
            total_evaluations: AtomicU64::new(0),
            total_liquidations: AtomicU64::new(0),
            total_adl_triggers: AtomicU64::new(0),
            spot_index_fp: RwLock::new(spot_map),
            perp_mark_fp: RwLock::new(perp_map),
            mark_engines: RwLock::new(mark_engines),
            svi_surfaces: RwLock::new(surfaces),
            accounts: RwLock::new(accounts),
            risk_cfg: SpanRiskConfig::default(),
            funding_calc: DynamicFundingCalculator::default(),
            event_tx,
        })
    }

    pub fn compute_portfolio_snapshot_proto(
        &self,
        account_id: u64,
    ) -> Option<pb::PortfolioMarginSnapshot> {
        let acct = self.accounts.read().get(&account_id)?.clone();
        let spot_fp = *self
            .spot_index_fp
            .read()
            .get("BTC-USDT")
            .unwrap_or(&(68_500 * FP_SCALE));
        let perp_fp = *self.perp_mark_fp.read().get("BTC-USDT").unwrap_or(&spot_fp);
        let res = evaluate_account_cross_margin(&acct, spot_fp, perp_fp, &self.risk_cfg);
        let seq = self.engine_seq.fetch_add(1, Ordering::Relaxed);
        self.total_evaluations.fetch_add(1, Ordering::Relaxed);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let snap = to_portfolio_margin_snapshot(
            &acct, &res, "BTC-USDT", 1, spot_fp, perp_fp, 100, 200_000, seq, now_ns,
        );
        let _ = self.event_tx.send(pb::MarginEventEnvelope {
            engine_seq: seq,
            ts_ns: now_ns,
            event: Some(pb::margin_event_envelope::Event::MarginSnapshot(
                snap.clone(),
            )),
        });
        Some(snap)
    }

    pub fn compute_greeks_surface_proto(
        &self,
        underlying_symbol: &str,
    ) -> Option<pb::OptionGreeksSurface> {
        let surf = self.svi_surfaces.read().get(underlying_symbol)?.clone();
        let spot_fp = *self
            .spot_index_fp
            .read()
            .get(underlying_symbol)
            .unwrap_or(&(68_500 * FP_SCALE));
        let spot_usd = (spot_fp as f64) * 1e-8;
        let strikes: Vec<f64> = [0.85, 0.90, 0.95, 1.00, 1.05, 1.10, 1.15]
            .iter()
            .map(|&m| (spot_usd * m).round())
            .collect();
        let seq = self.engine_seq.fetch_add(1, Ordering::Relaxed);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        Some(build_option_greeks_surface_proto(
            underlying_symbol,
            &surf,
            spot_fp,
            0.04,
            0.01,
            &strikes,
            seq,
            now_ns,
        ))
    }
}

/// Tonic gRPC Server implementing `hyperion.omega.v1.MarginRiskService` on port `50163`.
#[derive(Clone)]
pub struct MarginRiskGrpcService {
    pub state: Arc<MarginRiskEngineState>,
}

type MarginEventStream =
    std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<pb::MarginEventEnvelope, Status>> + Send>>;

#[tonic::async_trait]
impl pb::margin_risk_service_server::MarginRiskService for MarginRiskGrpcService {
    type StreamMarginEventsStream = MarginEventStream;

    async fn get_portfolio_margin(
        &self,
        request: Request<pb::GetPortfolioMarginRequest>,
    ) -> Result<Response<pb::PortfolioMarginSnapshot>, Status> {
        let acct_id = request.into_inner().account_id;
        match self.state.compute_portfolio_snapshot_proto(acct_id) {
            Some(snap) => Ok(Response::new(snap)),
            None => Err(Status::not_found(format!(
                "account_id {acct_id} not registered in margin-risk-rs"
            ))),
        }
    }

    async fn get_option_greeks_surface(
        &self,
        request: Request<pb::GetOptionGreeksSurfaceRequest>,
    ) -> Result<Response<pb::OptionGreeksSurface>, Status> {
        let sym = request.into_inner().underlying_symbol;
        match self.state.compute_greeks_surface_proto(&sym) {
            Some(surf) => Ok(Response::new(surf)),
            None => Err(Status::not_found(format!(
                "no SVI volatility surface calibrated for {sym}"
            ))),
        }
    }

    async fn stream_margin_events(
        &self,
        _request: Request<pb::StreamMarginEventsRequest>,
    ) -> Result<Response<Self::StreamMarginEventsStream>, Status> {
        let rx = self.state.event_tx.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(|item| match item {
            Ok(env) => Some(Ok(env)),
            Err(_) => None,
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    channel: &'static str,
    grpc_port: u16,
    http_port: u16,
    topics: [&'static str; 4],
    engine_seq: u64,
}

#[derive(Serialize)]
struct AccountMarginJson {
    account_id: u64,
    health_status: i32,
    net_equity_fp: String,
    initial_margin_req_fp: String,
    maintenance_margin_req_fp: String,
    worst_span_loss_fp: String,
    portfolio_delta_fp: String,
    portfolio_gamma_fp: String,
    portfolio_vega_fp: String,
    portfolio_theta_fp: String,
    portfolio_vanna_fp: String,
    portfolio_volga_fp: String,
    effective_leverage_ppm: u64,
}

async fn healthz(State(st): State<Arc<MarginRiskEngineState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "margin-risk-rs",
        channel: "ch14",
        grpc_port: DEFAULT_GRPC_PORT,
        http_port: DEFAULT_HTTP_PORT,
        topics: [
            TOPIC_MARGIN_SNAPSHOTS,
            TOPIC_MARGIN_GREEKS,
            TOPIC_MARGIN_LIQUIDATIONS,
            TOPIC_MARGIN_ADL,
        ],
        engine_seq: st.engine_seq.load(Ordering::Relaxed),
    })
}

async fn metrics(State(st): State<Arc<MarginRiskEngineState>>) -> String {
    format!(
        "# HELP hyperion_margin_evaluations_total Total cross-margin portfolio evaluations\n\
         # TYPE hyperion_margin_evaluations_total counter\n\
         hyperion_margin_evaluations_total {}\n\
         # HELP hyperion_margin_liquidations_total Total liquidation waterfall triggers\n\
         # TYPE hyperion_margin_liquidations_total counter\n\
         hyperion_margin_liquidations_total {}\n\
         # HELP hyperion_margin_adl_triggers_total Total Stage-4 ADL events\n\
         # TYPE hyperion_margin_adl_triggers_total counter\n\
         hyperion_margin_adl_triggers_total {}\n",
        st.total_evaluations.load(Ordering::Relaxed),
        st.total_liquidations.load(Ordering::Relaxed),
        st.total_adl_triggers.load(Ordering::Relaxed),
    )
}

async fn get_account_margin_rest(
    State(st): State<Arc<MarginRiskEngineState>>,
    Path(account_id): Path<u64>,
) -> Result<Json<AccountMarginJson>, StatusCode> {
    let snap = st
        .compute_portfolio_snapshot_proto(account_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    // Strict `00-hyperion-contract.md` §5.1.3: fixed-point fields in JSON REST sent as decimal strings!
    Ok(Json(AccountMarginJson {
        account_id: snap.account_id,
        health_status: snap.health_status,
        net_equity_fp: i128::from(snap.net_equity_fp.unwrap_or_default()).to_string(),
        initial_margin_req_fp: u128::from(snap.initial_margin_req_fp.unwrap_or_default())
            .to_string(),
        maintenance_margin_req_fp: u128::from(snap.maintenance_margin_req_fp.unwrap_or_default())
            .to_string(),
        worst_span_loss_fp: u128::from(snap.worst_span_loss_fp.unwrap_or_default()).to_string(),
        portfolio_delta_fp: snap.portfolio_delta_fp.to_string(),
        portfolio_gamma_fp: snap.portfolio_gamma_fp.to_string(),
        portfolio_vega_fp: snap.portfolio_vega_fp.to_string(),
        portfolio_theta_fp: snap.portfolio_theta_fp.to_string(),
        portfolio_vanna_fp: snap.portfolio_vanna_fp.to_string(),
        portfolio_volga_fp: snap.portfolio_volga_fp.to_string(),
        effective_leverage_ppm: snap.effective_leverage_ppm,
    }))
}

/// Build Axum HTTP Router for `:8093` (`/healthz`, `/metrics`, `/api/v1/margin/*`).
pub fn build_http_router(state: Arc<MarginRiskEngineState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route(
            "/api/v1/margin/accounts/:account_id",
            get(get_account_margin_rest),
        )
        .with_state(state)
}
