use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use arrow_flight::encode::FlightDataEncoderBuilder;
use arrow_flight::error::FlightError;
use arrow_flight::flight_descriptor::DescriptorType;
use arrow_flight::flight_service_server::{FlightService, FlightServiceServer};
use arrow_flight::{
    Action, ActionType, Criteria, Empty, FlightData, FlightDescriptor, FlightEndpoint, FlightInfo,
    HandshakeRequest, HandshakeResponse, PollInfo, PutResult, Result as FlightProtoResult,
    SchemaResult, Ticket,
};
use bytes::Bytes;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tonic::{Request, Response, Status, Streaming};

use crate::engine::{LakehouseEngine, QueryRequestOptions};
use crate::schema::{
    fraud_alerts_physical_schema, quotes_physical_schema, trades_physical_schema,
    TABLE_FRAUD_ALERTS, TABLE_QUOTES, TABLE_TRADES,
};

#[derive(Debug, Deserialize)]
struct FlightTicketPayload {
    sql: String,
    #[serde(default)]
    memory_limit_bytes: Option<usize>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_rows: Option<usize>,
}

/// Apache Arrow Flight gRPC Streaming Server backed by `LakehouseEngine` & `SqlAstGuard`.
pub struct LakehouseFlightService {
    engine: Arc<LakehouseEngine>,
}

impl LakehouseFlightService {
    pub fn new(engine: Arc<LakehouseEngine>) -> Self {
        Self { engine }
    }

    pub fn into_server(engine: Arc<LakehouseEngine>) -> FlightServiceServer<Self> {
        FlightServiceServer::new(Self::new(engine))
            .max_decoding_message_size(64 * 1024 * 1024)
            .max_encoding_message_size(64 * 1024 * 1024)
    }

    #[allow(clippy::result_large_err)]
    fn decode_ticket(raw: &[u8]) -> std::result::Result<FlightTicketPayload, Status> {
        let text = std::str::from_utf8(raw)
            .map_err(|e| Status::invalid_argument(format!("Ticket must be valid UTF-8: {e}")))?;
        if text.trim_start().starts_with('{') {
            serde_json::from_str::<FlightTicketPayload>(text)
                .map_err(|e| Status::invalid_argument(format!("Invalid JSON Flight ticket: {e}")))
        } else {
            Ok(FlightTicketPayload {
                sql: text.to_string(),
                memory_limit_bytes: None,
                timeout_ms: None,
                max_rows: None,
            })
        }
    }
}

#[tonic::async_trait]
impl FlightService for LakehouseFlightService {
    type HandshakeStream = BoxStream<'static, Result<HandshakeResponse, Status>>;
    type ListFlightsStream = BoxStream<'static, Result<FlightInfo, Status>>;
    type DoGetStream = BoxStream<'static, Result<FlightData, Status>>;
    type DoPutStream = BoxStream<'static, Result<PutResult, Status>>;
    type DoExchangeStream = BoxStream<'static, Result<FlightData, Status>>;
    type DoActionStream = BoxStream<'static, Result<FlightProtoResult, Status>>;
    type ListActionsStream = BoxStream<'static, Result<ActionType, Status>>;

    async fn handshake(
        &self,
        request: Request<Streaming<HandshakeRequest>>,
    ) -> Result<Response<Self::HandshakeStream>, Status> {
        let mut inbound = request.into_inner();
        let resp = if let Some(req) = inbound.message().await? {
            HandshakeResponse {
                protocol_version: req.protocol_version,
                payload: Bytes::from_static(b"hyperion-lakehouse-flight-ok"),
            }
        } else {
            HandshakeResponse {
                protocol_version: 1,
                payload: Bytes::from_static(b"hyperion-lakehouse-flight-ok"),
            }
        };
        Ok(Response::new(Box::pin(stream::once(async { Ok(resp) }))))
    }

    async fn list_flights(
        &self,
        _request: Request<Criteria>,
    ) -> Result<Response<Self::ListFlightsStream>, Status> {
        let tables = [
            (TABLE_TRADES, trades_physical_schema()),
            (TABLE_QUOTES, quotes_physical_schema()),
            (TABLE_FRAUD_ALERTS, fraud_alerts_physical_schema()),
        ];

        let mut infos = Vec::with_capacity(tables.len());
        for (name, schema) in tables {
            let desc = FlightDescriptor::new_path(vec![name.to_string()]);
            let endpoint = FlightEndpoint::new().with_ticket(Ticket::new(format!(
                "SELECT * FROM {name} LIMIT 1000"
            )));
            let info = FlightInfo::new()
                .try_with_schema(schema.as_ref())
                .map_err(|e| Status::internal(e.to_string()))?
                .with_descriptor(desc)
                .with_endpoint(endpoint);
            infos.push(Ok(info));
        }

        Ok(Response::new(Box::pin(stream::iter(infos))))
    }

    async fn get_flight_info(
        &self,
        request: Request<FlightDescriptor>,
    ) -> Result<Response<FlightInfo>, Status> {
        let desc = request.into_inner();
        let sql = if desc.r#type == DescriptorType::Cmd as i32 {
            String::from_utf8(desc.cmd.to_vec())
                .map_err(|e| Status::invalid_argument(format!("Invalid UTF-8 SQL cmd: {e}")))?
        } else if let Some(tbl) = desc.path.first() {
            format!("SELECT * FROM {tbl} LIMIT 10000")
        } else {
            return Err(Status::invalid_argument(
                "FlightDescriptor must specify SQL cmd or table path",
            ));
        };

        // Run AST Security Guard + physical plan validation before issuing Flight Ticket
        let _plan = self
            .engine
            .explain_physical_plan(&sql)
            .await
            .map_err(Status::from)?;

        let outcome = self
            .engine
            .execute_sql(
                &format!("SELECT * FROM ({sql}) AS __schema_probe LIMIT 0"),
                QueryRequestOptions {
                    max_rows: Some(0),
                    ..Default::default()
                },
            )
            .await
            .map_err(Status::from)?;

        let endpoint = FlightEndpoint::new().with_ticket(Ticket::new(sql));
        let info = FlightInfo::new()
            .try_with_schema(outcome.schema.as_ref())
            .map_err(|e| Status::internal(e.to_string()))?
            .with_descriptor(desc)
            .with_endpoint(endpoint);

        Ok(Response::new(info))
    }

    async fn poll_flight_info(
        &self,
        _request: Request<FlightDescriptor>,
    ) -> Result<Response<PollInfo>, Status> {
        Err(Status::unimplemented("poll_flight_info is not required"))
    }

    async fn get_schema(
        &self,
        request: Request<FlightDescriptor>,
    ) -> Result<Response<SchemaResult>, Status> {
        let info = self.get_flight_info(request).await?.into_inner();
        Ok(Response::new(SchemaResult {
            schema: info.schema,
        }))
    }

    async fn do_get(
        &self,
        request: Request<Ticket>,
    ) -> Result<Response<Self::DoGetStream>, Status> {
        let ticket = request.into_inner();
        let payload = Self::decode_ticket(&ticket.ticket)?;

        let opts = QueryRequestOptions {
            memory_limit_bytes: payload.memory_limit_bytes,
            timeout: payload.timeout_ms.map(Duration::from_millis),
            max_rows: payload.max_rows,
            capture_plan: false,
        };

        let outcome = self
            .engine
            .execute_sql(&payload.sql, opts)
            .await
            .map_err(Status::from)?;

        let batch_results: Vec<std::result::Result<_, FlightError>> =
            outcome.batches.into_iter().map(Ok).collect();

        #[allow(clippy::result_large_err)]
        let flight_stream = FlightDataEncoderBuilder::new()
            .with_schema(outcome.schema)
            .build(stream::iter(batch_results))
            .map(|res| res.map_err(|e| Status::internal(format!("Arrow Flight encode error: {e}"))));

        Ok(Response::new(
            Box::pin(flight_stream) as Pin<Box<dyn futures::Stream<Item = _> + Send>>
        ))
    }

    async fn do_put(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoPutStream>, Status> {
        Err(Status::permission_denied(
            "Forensic Arrow Flight endpoint is strictly read-only (DoPut disabled)",
        ))
    }

    async fn do_exchange(
        &self,
        _request: Request<Streaming<FlightData>>,
    ) -> Result<Response<Self::DoExchangeStream>, Status> {
        Err(Status::permission_denied(
            "Forensic Arrow Flight endpoint is strictly read-only (DoExchange disabled)",
        ))
    }

    async fn do_action(
        &self,
        request: Request<Action>,
    ) -> Result<Response<Self::DoActionStream>, Status> {
        let action = request.into_inner();
        match action.r#type.as_str() {
            "health" => {
                let payload = serde_json::to_vec(&json!({
                    "status": "ok",
                    "service": "lakehouse-olap-rs",
                }))
                .unwrap_or_default();
                let res = FlightProtoResult {
                    body: Bytes::from(payload),
                };
                Ok(Response::new(Box::pin(stream::once(async { Ok(res) }))))
            }
            "explain" => {
                let sql = std::str::from_utf8(&action.body)
                    .map_err(|e| Status::invalid_argument(format!("Invalid UTF-8 SQL: {e}")))?;
                let summary = self
                    .engine
                    .explain_physical_plan(sql)
                    .await
                    .map_err(Status::from)?;
                let payload = serde_json::to_vec(&json!({
                    "physical_plan": summary.physical_plan_tree,
                    "has_parquet_scan": summary.has_parquet_scan,
                    "has_pushed_predicate": summary.has_pushed_predicate,
                    "has_asof_join_exec": summary.has_asof_join_exec,
                    "scheduled_parquet_files": summary.total_parquet_files_scheduled,
                }))
                .unwrap_or_default();
                let res = FlightProtoResult {
                    body: Bytes::from(payload),
                };
                Ok(Response::new(Box::pin(stream::once(async { Ok(res) }))))
            }
            "compact" => {
                let eng = Arc::clone(&self.engine);
                let report = tokio::task::spawn_blocking(move || eng.compact_now(true))
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?
                    .map_err(Status::from)?;
                self.engine
                    .refresh_table_providers()
                    .await
                    .map_err(Status::from)?;
                let payload = serde_json::to_vec(&json!({
                    "partitions_compacted": report.partitions_compacted,
                    "output_segments_written": report.output_segments_written,
                    "rows_after_dedup": report.rows_after_dedup,
                }))
                .unwrap_or_default();
                let res = FlightProtoResult {
                    body: Bytes::from(payload),
                };
                Ok(Response::new(Box::pin(stream::once(async { Ok(res) }))))
            }
            other => Err(Status::invalid_argument(format!(
                "Unsupported Flight action '{other}'"
            ))),
        }
    }

    async fn list_actions(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::ListActionsStream>, Status> {
        let actions = vec![
            Ok(ActionType {
                r#type: "health".into(),
                description: "Returns JSON health telemetry".into(),
            }),
            Ok(ActionType {
                r#type: "explain".into(),
                description: "Validates SQL via AST Guard and returns PhysicalExecutionPlan".into(),
            }),
            Ok(ActionType {
                r#type: "compact".into(),
                description: "Triggers Zstd Parquet L0->L1 segment compaction".into(),
            }),
        ];
        Ok(Response::new(Box::pin(stream::iter(actions))))
    }
}
