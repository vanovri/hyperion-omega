//! # gRPC Server (`WasmQuantHostService` on `:50164`) & Ledger Order Forwarder (`grpc_service.rs`)

use crate::engine::{
    now_epoch_ns, OrderGatewaySink, QuantWasmHost, SandboxError, TelemetryEvent,
    ValidatedSandboxConfig,
};
use crate::proto::omega::wasm_quant_host_service_server::WasmQuantHostService;
use crate::proto::omega::{
    wasm_telemetry_envelope, DeployStrategyModule, DeployStrategyResponse,
    PauseOrTerminateStrategyRequest, PauseOrTerminateStrategyResponse, StrategyLifecycleState,
    StreamWasmTelemetryRequest, WasmTelemetryEnvelope,
};
use crate::proto::v1::orderbook_service_client::OrderbookServiceClient;
use crate::proto::v1::{
    CancelOrder, CancelOrderRequest, SubmitOrder, SubmitOrderRequest, TraceMetadata,
};
use crate::risk_gate::{InstrumentSpec, RiskPolicy, ValidatedOrderAction};
use hyperion_quant_abi::StrategyInitBlock;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::Stream;
use tonic::{Request, Response, Status};

/// Asynchronous non-blocking forwarder bridging validated sandbox order mutations
/// directly to `services/ledger-rs` (`OrderbookService` on port `50151`) or `gateway-go`.
pub struct LedgerGrpcBridgeSink {
    tx: mpsc::UnboundedSender<(String, ValidatedOrderAction)>,
}

impl LedgerGrpcBridgeSink {
    pub fn connect_background(ledger_grpc_endpoint: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<(String, ValidatedOrderAction)>();
        tokio::spawn(async move {
            let mut client_opt = OrderbookServiceClient::connect(ledger_grpc_endpoint.clone())
                .await
                .ok();
            while let Some((strategy_id, action)) = rx.recv().await {
                if client_opt.is_none() {
                    client_opt = OrderbookServiceClient::connect(ledger_grpc_endpoint.clone())
                        .await
                        .ok();
                }
                if let Some(client) = client_opt.as_mut() {
                    match action {
                        ValidatedOrderAction::SubmitLimit {
                            order_id,
                            client_order_id,
                            account_id,
                            instrument_id,
                            side,
                            order_type,
                            stp_mode,
                            price_fp,
                            qty_fp,
                            max_slippage_bps,
                            client_ts_ns,
                            ..
                        } => {
                            let req = SubmitOrderRequest {
                                order: Some(SubmitOrder {
                                    client_order_id: format!("{strategy_id}-{client_order_id}"),
                                    account_id,
                                    symbol: InstrumentSpec::symbol_str(instrument_id).to_string(),
                                    instrument_id,
                                    side: side as i32,
                                    order_type: order_type as i32,
                                    stp_mode: stp_mode as i32,
                                    price_fp,
                                    qty_fp,
                                    max_slippage_bps,
                                    client_ts_ns,
                                    nonce: order_id,
                                    signature: Vec::new(),
                                    idempotency_key: format!(
                                        "wasm-{strategy_id}-{account_id}-{client_order_id}"
                                    ),
                                    trace: Some(TraceMetadata {
                                        trace_id: format!("{order_id:032x}"),
                                        span_id: format!("{client_order_id:016x}"),
                                        correlation_id: format!("wasm-ord-{order_id}"),
                                        source_service: "quant-wasm-host".to_string(),
                                        ingest_ts_ns: client_ts_ns,
                                    }),
                                }),
                            };
                            if client.submit_order(req).await.is_err() {
                                client_opt = None;
                            }
                        }
                        ValidatedOrderAction::Cancel {
                            order_id,
                            client_order_id,
                            account_id,
                            instrument_id,
                            client_ts_ns,
                        } => {
                            let req = CancelOrderRequest {
                                cancel: Some(CancelOrder {
                                    order_id,
                                    client_order_id: format!("{strategy_id}-{client_order_id}"),
                                    account_id,
                                    symbol: InstrumentSpec::symbol_str(instrument_id).to_string(),
                                    instrument_id,
                                    client_ts_ns,
                                    nonce: client_order_id,
                                    signature: Vec::new(),
                                    idempotency_key: format!(
                                        "wasm-cxl-{strategy_id}-{order_id}-{client_order_id}"
                                    ),
                                    trace: None,
                                }),
                            };
                            if client.cancel_order(req).await.is_err() {
                                client_opt = None;
                            }
                        }
                    }
                }
            }
        });
        Self { tx }
    }
}

impl OrderGatewaySink for LedgerGrpcBridgeSink {
    #[inline]
    fn on_validated_order(&self, strategy_id: &str, action: ValidatedOrderAction) {
        let _ = self.tx.send((strategy_id.to_string(), action));
    }
}

pub struct WasmQuantGrpcService {
    host: Arc<Mutex<QuantWasmHost>>,
}

impl WasmQuantGrpcService {
    pub fn new(host: Arc<Mutex<QuantWasmHost>>) -> Self {
        Self { host }
    }
}

#[tonic::async_trait]
impl WasmQuantHostService for WasmQuantGrpcService {
    async fn deploy_strategy(
        &self,
        request: Request<DeployStrategyModule>,
    ) -> Result<Response<DeployStrategyResponse>, Status> {
        let req = request.into_inner();
        if req.strategy_id.trim().is_empty() {
            return Err(Status::invalid_argument("strategy_id must not be empty"));
        }
        if req.wasm_bytecode.is_empty() {
            return Err(Status::invalid_argument("wasm_bytecode must not be empty"));
        }

        let mut cfg = ValidatedSandboxConfig {
            risk_policy: RiskPolicy {
                account_id: req.account_id,
                ..RiskPolicy::default()
            },
            ..ValidatedSandboxConfig::default()
        };

        if let Some(lim) = &req.limits {
            if lim.max_linear_memory_bytes > 0 {
                cfg.max_linear_memory_bytes = lim.max_linear_memory_bytes as usize;
            }
            if lim.max_stack_bytes > 0 {
                cfg.max_stack_bytes = lim.max_stack_bytes as usize;
            }
            if lim.max_fuel_per_tick > 0 {
                cfg.max_fuel_per_tick = lim.max_fuel_per_tick;
            }
            if lim.max_epoch_walltime_ns > 0 {
                cfg.max_epoch_walltime_ns = lim.max_epoch_walltime_ns;
            }
            if lim.max_orders_per_tick > 0 {
                cfg.risk_policy.max_orders_per_tick = lim.max_orders_per_tick;
            }
            if let Some(n) = &lim.max_order_notional_fp {
                let val = n.to_u128();
                if val > 0 {
                    cfg.risk_policy.max_order_notional_fp = val;
                }
            }
            if lim.max_abs_position_qty_fp > 0 {
                cfg.risk_policy.max_abs_position_qty_fp = lim.max_abs_position_qty_fp;
            }
        }

        let init_block = decode_optional_init_payload(&req.initial_config_payload, req.account_id);
        let expected_b3 = if req.bytecode_blake3.is_empty() {
            None
        } else {
            Some(req.bytecode_blake3.as_slice())
        };
        let sig_slice = if req.author_ed25519_signature.is_empty() {
            None
        } else {
            Some(req.author_ed25519_signature.as_slice())
        };

        let mut guard = self
            .host
            .lock()
            .map_err(|_| Status::internal("host mutex poisoned"))?;

        match guard.deploy_or_hot_swap(
            &req.strategy_id,
            &req.strategy_name,
            req.account_id,
            req.version,
            req.subscribed_symbols,
            &req.wasm_bytecode,
            expected_b3,
            sig_slice,
            cfg,
            init_block,
            req.atomic_hot_swap,
            0,
        ) {
            Ok((compile_us, handed_off)) => Ok(Response::new(DeployStrategyResponse {
                strategy_id: req.strategy_id,
                active_version: req.version,
                state: StrategyLifecycleState::Active as i32,
                jit_compile_latency_us: compile_us,
                handed_off_open_orders: handed_off,
                reject_reason: String::new(),
            })),
            Err(err) => {
                let active_ver = guard.strategy_version(&req.strategy_id).unwrap_or(0);
                let state = match &err {
                    SandboxError::ExecutionTrapped { .. } => {
                        StrategyLifecycleState::QuarantinedOnTrap
                    }
                    _ => guard
                        .strategy_state(&req.strategy_id)
                        .unwrap_or(StrategyLifecycleState::Terminated),
                };
                Ok(Response::new(DeployStrategyResponse {
                    strategy_id: req.strategy_id,
                    active_version: active_ver,
                    state: state as i32,
                    jit_compile_latency_us: 0,
                    handed_off_open_orders: 0,
                    reject_reason: err.to_string(),
                }))
            }
        }
    }

    async fn pause_or_terminate_strategy(
        &self,
        request: Request<PauseOrTerminateStrategyRequest>,
    ) -> Result<Response<PauseOrTerminateStrategyResponse>, Status> {
        let req = request.into_inner();
        let mut guard = self
            .host
            .lock()
            .map_err(|_| Status::internal("host mutex poisoned"))?;
        match guard.pause_or_terminate(
            &req.strategy_id,
            req.terminate_permanently,
            req.cancel_open_orders,
            now_epoch_ns(),
        ) {
            Ok((state, canceled)) => Ok(Response::new(PauseOrTerminateStrategyResponse {
                strategy_id: req.strategy_id,
                state: state as i32,
                canceled_orders_count: canceled,
            })),
            Err(SandboxError::StrategyNotFound(id)) => {
                Err(Status::not_found(format!("strategy `{id}` not found")))
            }
            Err(e) => Err(Status::internal(e.to_string())),
        }
    }

    type StreamWasmTelemetryStream =
        Pin<Box<dyn Stream<Item = Result<WasmTelemetryEnvelope, Status>> + Send + 'static>>;

    async fn stream_wasm_telemetry(
        &self,
        request: Request<StreamWasmTelemetryRequest>,
    ) -> Result<Response<Self::StreamWasmTelemetryStream>, Status> {
        let req = request.into_inner();
        let mut broadcast_rx = {
            let mut guard = self
                .host
                .lock()
                .map_err(|_| Status::internal("host mutex poisoned"))?;
            if req.include_fuel_reports {
                guard.set_per_tick_telemetry_broadcast(true);
            }
            guard.subscribe_telemetry()
        };

        let (tx, rx) = mpsc::channel::<Result<WasmTelemetryEnvelope, Status>>(512);
        let filter_id = req.strategy_id;
        let want_fuel = req.include_fuel_reports;
        let want_traps = req.include_traps;

        tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(TelemetryEvent::Fuel(report)) => {
                        if !want_fuel {
                            continue;
                        }
                        if !filter_id.is_empty() && report.strategy_id != filter_id {
                            continue;
                        }
                        let env = WasmTelemetryEnvelope {
                            ts_ns: report.reported_ts_ns,
                            payload: Some(wasm_telemetry_envelope::Payload::FuelReport(report)),
                        };
                        if tx.send(Ok(env)).await.is_err() {
                            break;
                        }
                    }
                    Ok(TelemetryEvent::Trap(trap)) => {
                        if !want_traps {
                            continue;
                        }
                        if !filter_id.is_empty() && trap.strategy_id != filter_id {
                            continue;
                        }
                        let env = WasmTelemetryEnvelope {
                            ts_ns: trap.trapped_ts_ns,
                            payload: Some(wasm_telemetry_envelope::Payload::TrapAlert(trap)),
                        };
                        if tx.send(Ok(env)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

fn decode_optional_init_payload(payload: &[u8], account_id: u64) -> StrategyInitBlock {
    let mut cfg = StrategyInitBlock::default_btc();
    cfg.account_id = account_id;
    if payload.len() >= 125 {
        cfg.instrument_id = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        cfg.strategy_version = u32::from_le_bytes(payload[12..16].try_into().unwrap());
        cfg.tick_size_fp = u64::from_le_bytes(payload[16..24].try_into().unwrap());
        cfg.lot_size_fp = u64::from_le_bytes(payload[24..32].try_into().unwrap());
        cfg.max_position_fp = i64::from_le_bytes(payload[32..40].try_into().unwrap());
        cfg.max_order_qty_fp = u64::from_le_bytes(payload[40..48].try_into().unwrap());
        cfg.order_qty_fp = u64::from_le_bytes(payload[48..56].try_into().unwrap());
        cfg.zscore_entry_ppm = i64::from_le_bytes(payload[56..64].try_into().unwrap());
        cfg.zscore_exit_ppm = i64::from_le_bytes(payload[64..72].try_into().unwrap());
        cfg.ofi_weight_ppm = i64::from_le_bytes(payload[72..80].try_into().unwrap());
        cfg.obi_threshold_ppm = i64::from_le_bytes(payload[80..88].try_into().unwrap());
        cfg.hedge_band_fp = i64::from_le_bytes(payload[88..96].try_into().unwrap());
        cfg.target_delta_fp = i64::from_le_bytes(payload[96..104].try_into().unwrap());
        cfg.cooldown_ns = u64::from_le_bytes(payload[104..112].try_into().unwrap());
        cfg.max_spread_fp = u64::from_le_bytes(payload[112..120].try_into().unwrap());
        cfg.zscore_window = u32::from_le_bytes(payload[120..124].try_into().unwrap());
        cfg.stp_mode = payload[124];
    }
    cfg
}
