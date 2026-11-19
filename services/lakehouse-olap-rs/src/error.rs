use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LakehouseError {
    #[error("SQL syntax parse error: {0}")]
    SqlParse(String),

    #[error("AST security violation — empty or multi-statement SQL rejected (count={0})")]
    MultiStatementRejected(usize),

    #[error("AST security violation — DDL/DML/admin statement forbidden: {0}")]
    ReadOnlyViolation(String),

    #[error("AST security violation — explosive cross-join or unpredicated join blocked: {0}")]
    ExplosiveCrossJoinRejected(String),

    #[error("AST security violation — tautological or non-selective join condition blocked: {0}")]
    TautologicalJoinRejected(String),

    #[error("AST security violation — forbidden function, table, or recursive CTE: {0}")]
    ForbiddenConstruct(String),

    #[error("AST security violation — query AST depth or complexity exceeded limit ({0})")]
    AstComplexityExceeded(usize),

    #[error("Query execution exceeded hard timeout of {0} ms")]
    QueryTimeout(u64),

    #[error("Query execution exceeded memory quota ({limit_bytes} bytes): {detail}")]
    MemoryQuotaExceeded { limit_bytes: usize, detail: String },

    #[error("WAL binary integrity error: {0}")]
    WalIntegrity(String),

    #[error("Protobuf wire decode error: {0}")]
    ProtobufDecode(String),

    #[error("Invalid schema or ingestion input: {0}")]
    InvalidInput(String),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("DataFusion execution error: {0}")]
    DataFusion(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<datafusion::error::DataFusionError> for LakehouseError {
    fn from(err: datafusion::error::DataFusionError) -> Self {
        let msg = err.to_string();
        if msg.contains("Resources exhausted")
            || msg.contains("Failed to allocate")
            || msg.contains("Not enough memory")
        {
            Self::MemoryQuotaExceeded {
                limit_bytes: 0,
                detail: msg,
            }
        } else {
            Self::DataFusion(msg)
        }
    }
}

impl From<prost::DecodeError> for LakehouseError {
    fn from(err: prost::DecodeError) -> Self {
        Self::ProtobufDecode(err.to_string())
    }
}

impl From<LakehouseError> for tonic::Status {
    fn from(err: LakehouseError) -> Self {
        match &err {
            LakehouseError::SqlParse(_) | LakehouseError::InvalidInput(_) => {
                tonic::Status::invalid_argument(err.to_string())
            }
            LakehouseError::MultiStatementRejected(_)
            | LakehouseError::ReadOnlyViolation(_)
            | LakehouseError::ExplosiveCrossJoinRejected(_)
            | LakehouseError::TautologicalJoinRejected(_)
            | LakehouseError::ForbiddenConstruct(_)
            | LakehouseError::AstComplexityExceeded(_) => {
                tonic::Status::permission_denied(err.to_string())
            }
            LakehouseError::QueryTimeout(_) => tonic::Status::deadline_exceeded(err.to_string()),
            LakehouseError::MemoryQuotaExceeded { .. } => {
                tonic::Status::resource_exhausted(err.to_string())
            }
            LakehouseError::WalIntegrity(_) | LakehouseError::ProtobufDecode(_) => {
                tonic::Status::data_loss(err.to_string())
            }
            LakehouseError::Arrow(_)
            | LakehouseError::Parquet(_)
            | LakehouseError::DataFusion(_)
            | LakehouseError::Io(_) => tonic::Status::internal(err.to_string()),
        }
    }
}

#[derive(Serialize)]
struct HttpErrorEnvelope {
    error_code: &'static str,
    message: String,
}

impl LakehouseError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::SqlParse(_) => "SQL_PARSE_ERROR",
            Self::MultiStatementRejected(_) => "AST_MULTI_STATEMENT_REJECTED",
            Self::ReadOnlyViolation(_) => "AST_DDL_DML_FORBIDDEN",
            Self::ExplosiveCrossJoinRejected(_) => "AST_CROSS_JOIN_BLOCKED",
            Self::TautologicalJoinRejected(_) => "AST_TAUTOLOGICAL_JOIN_BLOCKED",
            Self::ForbiddenConstruct(_) => "AST_FORBIDDEN_CONSTRUCT",
            Self::AstComplexityExceeded(_) => "AST_COMPLEXITY_EXCEEDED",
            Self::QueryTimeout(_) => "QUERY_TIMEOUT_EXCEEDED",
            Self::MemoryQuotaExceeded { .. } => "QUERY_MEMORY_QUOTA_EXCEEDED",
            Self::WalIntegrity(_) => "WAL_INTEGRITY_FAILURE",
            Self::ProtobufDecode(_) => "PROTOBUF_DECODE_FAILURE",
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::Arrow(_) => "ARROW_INTERNAL_ERROR",
            Self::Parquet(_) => "PARQUET_INTERNAL_ERROR",
            Self::DataFusion(_) => "DATAFUSION_EXECUTION_ERROR",
            Self::Io(_) => "IO_ERROR",
        }
    }

    pub fn http_status(&self) -> StatusCode {
        match self {
            Self::SqlParse(_) | Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::MultiStatementRejected(_)
            | Self::ReadOnlyViolation(_)
            | Self::ExplosiveCrossJoinRejected(_)
            | Self::TautologicalJoinRejected(_)
            | Self::ForbiddenConstruct(_)
            | Self::AstComplexityExceeded(_) => StatusCode::FORBIDDEN,
            Self::QueryTimeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::MemoryQuotaExceeded { .. } => StatusCode::INSUFFICIENT_STORAGE,
            Self::WalIntegrity(_) | Self::ProtobufDecode(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Arrow(_) | Self::Parquet(_) | Self::DataFusion(_) | Self::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl IntoResponse for LakehouseError {
    fn into_response(self) -> Response {
        let status = self.http_status();
        let body = HttpErrorEnvelope {
            error_code: self.error_code(),
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, LakehouseError>;
