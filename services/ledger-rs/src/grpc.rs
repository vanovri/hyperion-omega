use crate::engine::{AckStatus, DeterministicExchangeEngine};
use crate::publisher::PublisherMetrics;
use crate::ring::MpscDisruptorRing;
use hyperion_proto::orderbook_service_server::{OrderbookService, OrderbookServiceServer};
use hyperion_proto::{
    market_event_envelope, ApplyAccountRiskActionRequest, ApplyAccountRiskActionResponse,
    CancelOrderRequest, CancelOrderResponse, CreditDepositRequest, CreditDepositResponse,
    GetL2BookSnapshotRequest, GetL2BookSnapshotResponse, GetMerkleLedgerProofRequest,
    GetMerkleLedgerProofResponse, MarketEventEnvelope, ModifyOrderRequest, ModifyOrderResponse,
    OrderAck, StreamMarketEventsRequest, SubmitOrderRequest, SubmitOrderResponse, TraceMetadata,
    VerifySystemLedgerInvariantRequest, VerifySystemLedgerInvariantResponse,
};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tonic::{Request, Response, Status};
use tracing::Instrument;

pub enum DisruptorCommand {
    Submit {
        req: SubmitOrderRequest,
        reply: oneshot::Sender<Result<OrderAck, String>>,
    },
    Cancel {
        req: CancelOrderRequest,
        reply: oneshot::Sender<Result<OrderAck, String>>,
    },
    Modify {
        req: ModifyOrderRequest,
        reply: oneshot::Sender<Result<OrderAck, String>>,
    },
    Deposit {
        req: CreditDepositRequest,
        reply: oneshot::Sender<Result<CreditDepositResponse, String>>,
    },
    L2Snapshot {
        req: GetL2BookSnapshotRequest,
        reply: oneshot::Sender<Option<GetL2BookSnapshotResponse>>,
    },
    MerkleProof {
        req: GetMerkleLedgerProofRequest,
        reply: oneshot::Sender<GetMerkleLedgerProofResponse>,
    },
    VerifyInvariant {
        req: VerifySystemLedgerInvariantRequest,
        reply: oneshot::Sender<VerifySystemLedgerInvariantResponse>,
    },
    RiskAction {
        req: ApplyAccountRiskActionRequest,
        reply: oneshot::Sender<Result<ApplyAccountRiskActionResponse, String>>,
    },
    FlushWal {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

#[derive(Default)]
pub struct EngineTelemetryMetrics {
    pub submit_total: AtomicU64,
    pub filled_total: AtomicU64,
    pub rejected_total: AtomicU64,
    pub cancel_total: AtomicU64,
    pub modify_total: AtomicU64,
    pub deposit_total: AtomicU64,
    pub risk_actions_total: AtomicU64,
    pub last_engine_seq: AtomicU64,
    pub last_exec_ns: AtomicU64,
    pub hist_lt_1us: AtomicU64,
    pub hist_lt_5us: AtomicU64,
    pub hist_lt_10us: AtomicU64,
    pub hist_ge_10us: AtomicU64,
}

impl EngineTelemetryMetrics {
    #[inline(always)]
    pub fn record_latency_ns(&self, ns: u64) {
        self.last_exec_ns.store(ns, Ordering::Relaxed);
        if ns < 1_000 {
            self.hist_lt_1us.fetch_add(1, Ordering::Relaxed);
        } else if ns < 5_000 {
            self.hist_lt_5us.fetch_add(1, Ordering::Relaxed);
        } else if ns < 10_000 {
            self.hist_lt_10us.fetch_add(1, Ordering::Relaxed);
        } else {
            self.hist_ge_10us.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub struct LmaxDisruptorHandle {
    ring: Arc<MpscDisruptorRing<DisruptorCommand>>,
    writer_thread: Arc<std::sync::OnceLock<std::thread::Thread>>,
    pub metrics: Arc<EngineTelemetryMetrics>,
    pub pub_metrics: Arc<PublisherMetrics>,
    pub stream_tx: broadcast::Sender<MarketEventEnvelope>,
    shutdown: Arc<AtomicBool>,
}

impl LmaxDisruptorHandle {
    pub fn spawn(
        mut engine: DeterministicExchangeEngine,
        ring_capacity: usize,
        pub_metrics: Arc<PublisherMetrics>,
        stream_tx: broadcast::Sender<MarketEventEnvelope>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let ring = MpscDisruptorRing::new(ring_capacity);
        let metrics = Arc::new(EngineTelemetryMetrics::default());
        metrics
            .last_engine_seq
            .store(engine.engine_seq(), Ordering::Release);

        let writer_slot = Arc::new(std::sync::OnceLock::new());
        let ring_clone = Arc::clone(&ring);
        let metrics_clone = Arc::clone(&metrics);
        let shutdown_clone = Arc::clone(&shutdown);
        let slot_clone = Arc::clone(&writer_slot);

        std::thread::Builder::new()
            .name("hyperion-lmax-writer".to_string())
            .spawn(move || {
                let _ = slot_clone.set(std::thread::current());
                let mut idle_spins: u32 = 0;

                loop {
                    if let Some(cmd) = ring_clone.try_recv_single_consumer() {
                        idle_spins = 0;
                        let t0 = Instant::now();
                        match cmd {
                            DisruptorCommand::Submit { req, reply } => {
                                metrics_clone.submit_total.fetch_add(1, Ordering::Relaxed);
                                let res = match req.order {
                                    Some(ord) => engine
                                        .execute_submit_proto(ord)
                                        .map_err(|e| e.to_string()),
                                    None => Err("missing SubmitOrder payload".to_string()),
                                };
                                if let Ok(ack) = &res {
                                    if ack.status == AckStatus::Filled as i32
                                        || ack.status == AckStatus::PartiallyFilled as i32
                                    {
                                        metrics_clone.filled_total.fetch_add(1, Ordering::Relaxed);
                                    } else if ack.status == AckStatus::Rejected as i32 {
                                        metrics_clone
                                            .rejected_total
                                            .fetch_add(1, Ordering::Relaxed);
                                    }
                                    metrics_clone
                                        .last_engine_seq
                                        .store(ack.engine_seq, Ordering::Release);
                                }
                                metrics_clone.record_latency_ns(t0.elapsed().as_nanos() as u64);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::Cancel { req, reply } => {
                                metrics_clone.cancel_total.fetch_add(1, Ordering::Relaxed);
                                let res = match req.cancel {
                                    Some(c) => engine
                                        .execute_cancel_proto(c)
                                        .map_err(|e| e.to_string()),
                                    None => Err("missing CancelOrder payload".to_string()),
                                };
                                if let Ok(ack) = &res {
                                    metrics_clone
                                        .last_engine_seq
                                        .store(ack.engine_seq, Ordering::Release);
                                }
                                metrics_clone.record_latency_ns(t0.elapsed().as_nanos() as u64);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::Modify { req, reply } => {
                                metrics_clone.modify_total.fetch_add(1, Ordering::Relaxed);
                                let res = match req.modify {
                                    Some(m) => engine
                                        .execute_modify_proto(m)
                                        .map_err(|e| e.to_string()),
                                    None => Err("missing ModifyOrder payload".to_string()),
                                };
                                if let Ok(ack) = &res {
                                    metrics_clone
                                        .last_engine_seq
                                        .store(ack.engine_seq, Ordering::Release);
                                }
                                metrics_clone.record_latency_ns(t0.elapsed().as_nanos() as u64);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::Deposit { req, reply } => {
                                metrics_clone.deposit_total.fetch_add(1, Ordering::Relaxed);
                                let res = engine.execute_deposit_proto(req);
                                if let Ok(r) = &res {
                                    metrics_clone
                                        .last_engine_seq
                                        .store(r.engine_seq, Ordering::Release);
                                }
                                metrics_clone.record_latency_ns(t0.elapsed().as_nanos() as u64);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::L2Snapshot { req, reply } => {
                                let res = engine.query_l2_snapshot_proto(req);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::MerkleProof { req, reply } => {
                                let res = engine.query_merkle_proof_proto(req);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::VerifyInvariant { req, reply } => {
                                let res = engine.query_system_invariant_proto(req);
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::RiskAction { req, reply } => {
                                metrics_clone
                                    .risk_actions_total
                                    .fetch_add(1, Ordering::Relaxed);
                                let res = engine.execute_risk_action_proto(req);
                                if let Ok(r) = &res {
                                    metrics_clone
                                        .last_engine_seq
                                        .store(r.engine_seq, Ordering::Release);
                                }
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::FlushWal { reply } => {
                                let res = engine
                                    .wal_mut()
                                    .flush_and_sync()
                                    .map_err(|e| e.to_string());
                                let _ = reply.send(res);
                            }
                            DisruptorCommand::Shutdown => {
                                let _ = engine.wal_mut().flush_and_sync();
                                break;
                            }
                        }
                    } else {
                        if shutdown_clone.load(Ordering::Acquire) {
                            let _ = engine.wal_mut().flush_and_sync();
                            break;
                        }
                        idle_spins = idle_spins.saturating_add(1);
                        if idle_spins < 512 {
                            std::hint::spin_loop();
                        } else if idle_spins < 1024 {
                            std::thread::yield_now();
                        } else {
                            std::thread::park_timeout(Duration::from_micros(200));
                        }
                    }
                }
            })
            .expect("spawn hyperion-lmax-writer thread");

        Self {
            ring,
            writer_thread: writer_slot,
            metrics,
            pub_metrics,
            stream_tx,
            shutdown,
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn dispatch_command(&self, mut cmd: DisruptorCommand) -> Result<(), Status> {
        let mut retries = 0u32;
        loop {
            match self.ring.try_send(cmd) {
                Ok(()) => {
                    if let Some(th) = self.writer_thread.get() {
                        th.unpark();
                    }
                    return Ok(());
                }
                Err(returned) => {
                    cmd = returned;
                    retries += 1;
                    if retries > 2_000 {
                        return Err(Status::resource_exhausted(
                            "LMAX Disruptor ingress ring buffer saturated",
                        ));
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.ring.try_send(DisruptorCommand::Shutdown);
        if let Some(th) = self.writer_thread.get() {
            th.unpark();
        }
    }
}

fn extract_trace_fields(trace: Option<&TraceMetadata>) -> (&str, &str) {
    match trace {
        Some(t) => (t.trace_id.as_str(), t.correlation_id.as_str()),
        None => ("", ""),
    }
}

#[derive(Clone)]
pub struct HyperionOrderbookGrpcService {
    handle: LmaxDisruptorHandle,
}

impl HyperionOrderbookGrpcService {
    pub fn new(handle: LmaxDisruptorHandle) -> Self {
        Self { handle }
    }

    pub fn into_server(self) -> OrderbookServiceServer<Self> {
        OrderbookServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl OrderbookService for HyperionOrderbookGrpcService {
    #[allow(clippy::result_large_err)]
    async fn submit_order(
        &self,
        request: Request<SubmitOrderRequest>,
    ) -> Result<Response<SubmitOrderResponse>, Status> {
        let inner = request.into_inner();
        let (trace_id, corr_id, sym, acc) = match &inner.order {
            Some(o) => {
                let (tid, cid) = extract_trace_fields(o.trace.as_ref());
                (
                    tid.to_string(),
                    cid.to_string(),
                    o.symbol.clone(),
                    o.account_id,
                )
            }
            None => return Err(Status::invalid_argument("missing order field")),
        };

        let span = tracing::info_span!(
            "grpc.orderbook.submit_order",
            otel.kind = "server",
            trace_id = %trace_id,
            correlation_id = %corr_id,
            symbol = %sym,
            account_id = acc,
            engine_seq = tracing::field::Empty,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::Submit {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let ack = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?
                .map_err(Status::internal)?;
            tracing::Span::current().record("engine_seq", ack.engine_seq);
            Ok(Response::new(SubmitOrderResponse { ack: Some(ack) }))
        }
        .instrument(span)
        .await
    }

    async fn cancel_order(
        &self,
        request: Request<CancelOrderRequest>,
    ) -> Result<Response<CancelOrderResponse>, Status> {
        let inner = request.into_inner();
        let (trace_id, corr_id, ord_id) = match &inner.cancel {
            Some(c) => {
                let (tid, cid) = extract_trace_fields(c.trace.as_ref());
                (tid.to_string(), cid.to_string(), c.order_id)
            }
            None => return Err(Status::invalid_argument("missing cancel field")),
        };

        let span = tracing::info_span!(
            "grpc.orderbook.cancel_order",
            otel.kind = "server",
            trace_id = %trace_id,
            correlation_id = %corr_id,
            order_id = ord_id,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::Cancel {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let ack = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?
                .map_err(Status::internal)?;
            Ok(Response::new(CancelOrderResponse { ack: Some(ack) }))
        }
        .instrument(span)
        .await
    }

    async fn modify_order(
        &self,
        request: Request<ModifyOrderRequest>,
    ) -> Result<Response<ModifyOrderResponse>, Status> {
        let inner = request.into_inner();
        let (trace_id, corr_id, ord_id) = match &inner.modify {
            Some(m) => {
                let (tid, cid) = extract_trace_fields(m.trace.as_ref());
                (tid.to_string(), cid.to_string(), m.order_id)
            }
            None => return Err(Status::invalid_argument("missing modify field")),
        };

        let span = tracing::info_span!(
            "grpc.orderbook.modify_order",
            otel.kind = "server",
            trace_id = %trace_id,
            correlation_id = %corr_id,
            order_id = ord_id,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::Modify {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let ack = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?
                .map_err(Status::internal)?;
            Ok(Response::new(ModifyOrderResponse { ack: Some(ack) }))
        }
        .instrument(span)
        .await
    }

    async fn credit_deposit(
        &self,
        request: Request<CreditDepositRequest>,
    ) -> Result<Response<CreditDepositResponse>, Status> {
        let inner = request.into_inner();
        let span = tracing::info_span!(
            "grpc.orderbook.credit_deposit",
            otel.kind = "server",
            account_id = inner.account_id,
            asset = %inner.asset,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::Deposit {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let resp = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?
                .map_err(Status::invalid_argument)?;
            Ok(Response::new(resp))
        }
        .instrument(span)
        .await
    }

    async fn get_l2_book_snapshot(
        &self,
        request: Request<GetL2BookSnapshotRequest>,
    ) -> Result<Response<GetL2BookSnapshotResponse>, Status> {
        let inner = request.into_inner();
        let span = tracing::info_span!(
            "grpc.orderbook.get_l2_book_snapshot",
            otel.kind = "server",
            symbol = %inner.symbol,
            depth = inner.depth,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::L2Snapshot {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let opt = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?;
            match opt {
                Some(resp) => Ok(Response::new(resp)),
                None => Err(Status::not_found("instrument symbol not found")),
            }
        }
        .instrument(span)
        .await
    }

    async fn get_merkle_ledger_proof(
        &self,
        request: Request<GetMerkleLedgerProofRequest>,
    ) -> Result<Response<GetMerkleLedgerProofResponse>, Status> {
        let inner = request.into_inner();
        let span = tracing::info_span!(
            "grpc.orderbook.get_merkle_ledger_proof",
            otel.kind = "server",
            account_id = inner.account_id,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::MerkleProof {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let resp = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?;
            Ok(Response::new(resp))
        }
        .instrument(span)
        .await
    }

    async fn verify_system_ledger_invariant(
        &self,
        request: Request<VerifySystemLedgerInvariantRequest>,
    ) -> Result<Response<VerifySystemLedgerInvariantResponse>, Status> {
        let inner = request.into_inner();
        let span = tracing::info_span!(
            "grpc.orderbook.verify_system_ledger_invariant",
            otel.kind = "server",
            asset = %inner.asset,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::VerifyInvariant {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let resp = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?;
            Ok(Response::new(resp))
        }
        .instrument(span)
        .await
    }

    async fn apply_account_risk_action(
        &self,
        request: Request<ApplyAccountRiskActionRequest>,
    ) -> Result<Response<ApplyAccountRiskActionResponse>, Status> {
        let inner = request.into_inner();
        let span = tracing::info_span!(
            "grpc.orderbook.apply_account_risk_action",
            otel.kind = "server",
            account_id = inner.account_id,
            command_id = %inner.command_id,
        );

        async move {
            let (tx, rx) = oneshot::channel();
            self.handle
                .dispatch_command(DisruptorCommand::RiskAction {
                    req: inner,
                    reply: tx,
                })
                .await?;
            let resp = rx
                .await
                .map_err(|_| Status::internal("LMAX writer channel closed"))?
                .map_err(Status::internal)?;
            Ok(Response::new(resp))
        }
        .instrument(span)
        .await
    }

    type StreamMarketEventsStream =
        Pin<Box<dyn tokio_stream::Stream<Item = Result<MarketEventEnvelope, Status>> + Send>>;

    async fn stream_market_events(
        &self,
        request: Request<StreamMarketEventsRequest>,
    ) -> Result<Response<Self::StreamMarketEventsStream>, Status> {
        let req = request.into_inner();
        let mut rx = self.handle.stream_tx.subscribe();
        let (out_tx, out_rx) = mpsc::channel::<Result<MarketEventEnvelope, Status>>(1024);

        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(env) => {
                        if env.engine_seq < req.from_engine_seq {
                            continue;
                        }
                        if !req.symbols.is_empty() && !req.symbols.iter().any(|s| s == &env.symbol) {
                            continue;
                        }
                        let keep = match &env.payload {
                            Some(market_event_envelope::Payload::L3Delta(_)) => {
                                req.include_l3_deltas || (!req.include_l3_deltas && !req.include_fills)
                            }
                            Some(market_event_envelope::Payload::Fill(_)) => {
                                req.include_fills || (!req.include_l3_deltas && !req.include_fills)
                            }
                            _ => !req.include_l3_deltas && !req.include_fills,
                        };
                        if keep && out_tx.send(Ok(env)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(out_rx);
        Ok(Response::new(Box::pin(stream)))
    }
}

/// Spawns the lightweight HTTP `:8082` Health (`/healthz`) & Prometheus Metrics (`/metrics`) endpoint
/// required by `00-hyperion-contract.md` Section 3.1.
pub fn spawn_health_and_metrics_http_server(
    bind_addr: String,
    handle: LmaxDisruptorHandle,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(&bind_addr).await?;
        loop {
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            let accept_res =
                tokio::time::timeout(Duration::from_millis(250), listener.accept()).await;
            let Ok(Ok((mut socket, _))) = accept_res else {
                continue;
            };
            let h = handle.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let req_str = std::str::from_utf8(&buf[..n]).unwrap_or("");
                let first_line = req_str.lines().next().unwrap_or("");

                if first_line.contains("GET /metrics") {
                    let m = &h.metrics;
                    let pm = &h.pub_metrics;
                    let body = format!(
                        "# HELP hyperion_ledger_engine_seq Latest committed LMAX engine sequence\n\
                         # TYPE hyperion_ledger_engine_seq gauge\n\
                         hyperion_ledger_engine_seq {}\n\
                         # HELP hyperion_ledger_orders_submitted_total Total SubmitOrder commands processed\n\
                         # TYPE hyperion_ledger_orders_submitted_total counter\n\
                         hyperion_ledger_orders_submitted_total {}\n\
                         # HELP hyperion_ledger_orders_filled_total Total filled/partially-filled orders\n\
                         # TYPE hyperion_ledger_orders_filled_total counter\n\
                         hyperion_ledger_orders_filled_total {}\n\
                         # HELP hyperion_ledger_orders_rejected_total Total rejected orders\n\
                         # TYPE hyperion_ledger_orders_rejected_total counter\n\
                         hyperion_ledger_orders_rejected_total {}\n\
                         # HELP hyperion_ledger_orders_canceled_total Total CancelOrder commands processed\n\
                         # TYPE hyperion_ledger_orders_canceled_total counter\n\
                         hyperion_ledger_orders_canceled_total {}\n\
                         # HELP hyperion_ledger_last_exec_ns Last single-writer execution latency in nanoseconds\n\
                         # TYPE hyperion_ledger_last_exec_ns gauge\n\
                         hyperion_ledger_last_exec_ns {}\n\
                         # HELP hyperion_ledger_latency_bucket_total Matching latency histogram buckets\n\
                         # TYPE hyperion_ledger_latency_bucket_total counter\n\
                         hyperion_ledger_latency_bucket_total{{le=\"1us\"}} {}\n\
                         hyperion_ledger_latency_bucket_total{{le=\"5us\"}} {}\n\
                         hyperion_ledger_latency_bucket_total{{le=\"10us\"}} {}\n\
                         hyperion_ledger_latency_bucket_total{{le=\"+Inf\"}} {}\n\
                         # HELP hyperion_ledger_redpanda_published_total Events published from SPSC ring\n\
                         # TYPE hyperion_ledger_redpanda_published_total counter\n\
                         hyperion_ledger_redpanda_published_total{{stream=\"l3\"}} {}\n\
                         hyperion_ledger_redpanda_published_total{{stream=\"fills\"}} {}\n\
                         hyperion_ledger_redpanda_published_total{{stream=\"journals\"}} {}\n",
                        m.last_engine_seq.load(Ordering::Relaxed),
                        m.submit_total.load(Ordering::Relaxed),
                        m.filled_total.load(Ordering::Relaxed),
                        m.rejected_total.load(Ordering::Relaxed),
                        m.cancel_total.load(Ordering::Relaxed),
                        m.last_exec_ns.load(Ordering::Relaxed),
                        m.hist_lt_1us.load(Ordering::Relaxed),
                        m.hist_lt_5us.load(Ordering::Relaxed),
                        m.hist_lt_10us.load(Ordering::Relaxed),
                        m.hist_ge_10us.load(Ordering::Relaxed),
                        pm.published_l3_total.load(Ordering::Relaxed),
                        pm.published_fills_total.load(Ordering::Relaxed),
                        pm.published_journals_total.load(Ordering::Relaxed),
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                } else {
                    let seq = h.metrics.last_engine_seq.load(Ordering::Acquire);
                    let body = format!(
                        "{{\"status\":\"ok\",\"service\":\"ledger-rs\",\"engine_seq\":{}}}",
                        seq
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            });
        }
        Ok(())
    })
}
