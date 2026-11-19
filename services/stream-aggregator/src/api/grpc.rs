use crate::api::QueryServiceState;
use crate::candle::Timeframe;
use hyperion_proto::stream_analytics_service_server::{
    StreamAnalyticsService, StreamAnalyticsServiceServer,
};
use hyperion_proto::{
    GetCandlesRequest, GetCandlesResponse, GetLatestMicrostructureRequest,
    GetLatestMicrostructureResponse, GetLiquidityHeatmapRequest, GetLiquidityHeatmapResponse,
    GetVolumeProfileRequest, GetVolumeProfileResponse, I128, MicrostructureMetrics,
};
use std::sync::atomic::Ordering;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct StreamAnalyticsGrpcServer {
    state: QueryServiceState,
}

impl StreamAnalyticsGrpcServer {
    pub fn new(state: QueryServiceState) -> Self {
        Self { state }
    }

    pub fn into_service(self) -> StreamAnalyticsServiceServer<Self> {
        StreamAnalyticsServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl StreamAnalyticsService for StreamAnalyticsGrpcServer {
    async fn get_candles(
        &self,
        request: Request<GetCandlesRequest>,
    ) -> Result<Response<GetCandlesResponse>, Status> {
        self.state
            .grpc_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let req = request.into_inner();
        if req.symbol.trim().is_empty() {
            return Err(Status::invalid_argument("symbol is required"));
        }
        let tf = Timeframe::from_proto_i32(req.interval)
            .ok_or_else(|| Status::invalid_argument("unsupported CandleInterval"))?;

        let candles = self
            .state
            .merged_candles(
                &req.symbol,
                tf,
                req.from_ts_ns,
                req.to_ts_ns,
                req.limit as usize,
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(GetCandlesResponse { candles }))
    }

    async fn get_volume_profile(
        &self,
        request: Request<GetVolumeProfileRequest>,
    ) -> Result<Response<GetVolumeProfileResponse>, Status> {
        self.state
            .grpc_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let req = request.into_inner();
        if req.symbol.trim().is_empty() {
            return Err(Status::invalid_argument("symbol is required"));
        }

        let resp = self
            .state
            .volume_profile(
                &req.symbol,
                req.from_ts_ns,
                req.to_ts_ns,
                req.bucket_size_fp,
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(resp))
    }

    async fn get_liquidity_heatmap(
        &self,
        request: Request<GetLiquidityHeatmapRequest>,
    ) -> Result<Response<GetLiquidityHeatmapResponse>, Status> {
        self.state
            .grpc_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let req = request.into_inner();
        if req.symbol.trim().is_empty() {
            return Err(Status::invalid_argument("symbol is required"));
        }

        let buckets = self
            .state
            .heatmap(
                &req.symbol,
                req.from_ts_ns,
                req.to_ts_ns,
                req.price_bucket_size_fp,
                req.time_bucket_ns,
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(GetLiquidityHeatmapResponse {
            symbol: req.symbol,
            buckets,
        }))
    }

    async fn get_latest_microstructure(
        &self,
        request: Request<GetLatestMicrostructureRequest>,
    ) -> Result<Response<GetLatestMicrostructureResponse>, Status> {
        self.state
            .grpc_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let req = request.into_inner();
        if req.symbol.trim().is_empty() {
            return Err(Status::invalid_argument("symbol is required"));
        }

        let metrics = self
            .state
            .latest_microstructure(&req.symbol)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .unwrap_or_else(|| MicrostructureMetrics {
                symbol: req.symbol,
                engine_seq: 0,
                ts_ns: 0,
                mid_price_fp: 0,
                spread_fp: 0,
                spread_bps: 0,
                rolling_vwap_fp: 0,
                rolling_twap_fp: 0,
                ofi_fp: Some(I128::from(0_i128)),
                book_imbalance_top5_ppm: 0,
                book_imbalance_top10_ppm: 0,
                book_imbalance_top20_ppm: 0,
                kyles_lambda_fp: 0,
            });

        Ok(Response::new(GetLatestMicrostructureResponse {
            metrics: Some(metrics),
        }))
    }
}
