//! # `quant-wasm-host` Production Daemon (`gRPC :50164` + `HTTP Health/Metrics :8094`)
//!
//! Conforms strictly to `01-hyperion-omega.md` §2 & §4:
//! - gRPC `WasmQuantHostService`: `0.0.0.0:50164` (`QUANT_WASM_GRPC_ADDR`)
//! - HTTP `/healthz`, `/metrics`, `/api/v1/wasm/strategies`: `0.0.0.0:8094` (`QUANT_WASM_HTTP_ADDR`)
//! - Upstream `ledger-rs` `OrderbookService`: `http://127.0.0.1:50151` (`HYPERION_LEDGER_GRPC_URL`)

use quant_wasm_host::engine::QuantWasmHost;
use quant_wasm_host::grpc_service::{LedgerGrpcBridgeSink, WasmQuantGrpcService};
use quant_wasm_host::proto::omega::wasm_quant_host_service_server::WasmQuantHostServiceServer;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tonic::transport::Server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let grpc_addr: SocketAddr = std::env::var("QUANT_WASM_GRPC_ADDR")
        .or_else(|_| std::env::var("GRPC_PORT").map(|p| format!("0.0.0.0:{p}")))
        .unwrap_or_else(|_| "0.0.0.0:50164".to_string())
        .parse()?;
    let http_addr: SocketAddr = std::env::var("QUANT_WASM_HTTP_ADDR")
        .or_else(|_| std::env::var("HTTP_PORT").map(|p| format!("0.0.0.0:{p}")))
        .unwrap_or_else(|_| "0.0.0.0:8094".to_string())
        .parse()?;
    let ledger_url = std::env::var("HYPERION_LEDGER_GRPC_URL")
        .or_else(|_| std::env::var("LEDGER_GRPC_ADDR").map(|a| format!("http://{a}")))
        .unwrap_or_else(|_| "http://127.0.0.1:50151".to_string());

    let sink = Arc::new(LedgerGrpcBridgeSink::connect_background(ledger_url));
    let host = Arc::new(Mutex::new(QuantWasmHost::new(sink)?));

    let http_host = Arc::clone(&host);
    tokio::spawn(async move {
        if let Ok(listener) = TcpListener::bind(http_addr).await {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                let host_ref = Arc::clone(&http_host);
                tokio::spawn(async move {
                    let mut req_buf = [0u8; 1024];
                    let n = socket.read(&mut req_buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let req_line = String::from_utf8_lossy(&req_buf[..n]);
                    let path = req_line
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");

                    let (status, content_type, body) = match path {
                        "/healthz" => (
                            "200 OK",
                            "application/json",
                            r#"{"status":"ok","service":"quant-wasm-host","channel":15}"#
                                .to_string(),
                        ),
                        "/metrics" => {
                            let m = host_ref
                                .lock()
                                .map(|h| h.render_prometheus_metrics())
                                .unwrap_or_default();
                            ("200 OK", "text/plain; version=0.0.4", m)
                        }
                        "/api/v1/wasm/strategies" => {
                            let j = host_ref
                                .lock()
                                .map(|h| h.list_strategies_json().to_string())
                                .unwrap_or_else(|_| r#"{"strategies":[]}"#.to_string());
                            ("200 OK", "application/json", j)
                        }
                        _ => ("404 Not Found", "application/json", r#"{"error":"not_found"}"#.to_string()),
                    };

                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                });
            }
        }
    });

    println!(
        "[quant-wasm-host] CH15 Zero-Syscall Wasmtime JIT Host listening on gRPC={} HTTP={}",
        grpc_addr, http_addr
    );

    Server::builder()
        .add_service(WasmQuantHostServiceServer::new(WasmQuantGrpcService::new(
            host,
        )))
        .serve_with_shutdown(grpc_addr, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    Ok(())
}
