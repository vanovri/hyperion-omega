//! End-to-End Integration & Telemetry Verification Suite for Hyperion Channel 14 (`margin-risk-rs`):
//! 1. Verifies live Tonic gRPC `MarginRiskService` (`GetPortfolioMargin`, `GetOptionGreeksSurface`)
//!    and Axum HTTP REST/Metrics (`/healthz`, `/metrics`, `/api/v1/margin/accounts/10042`).
//! 2. Emits complete verified telemetry for:
//!    - 6,600-combination 4th-order Central Finite-Difference Analytical Greeks audit (including 50ms pin risk).
//!    - 100,000-path Merton Jump-Diffusion x 5,000-account high-leverage zero-deficit stress report.

use margin_risk_rs::greeks_soa::run_exhaustive_finite_difference_verification;
use margin_risk_rs::monte_carlo::{run_merton_jump_diffusion_stress, MertonJumpDiffusionConfig};
use margin_risk_rs::omega_wire::pb::margin_risk_service_server::MarginRiskService;
use margin_risk_rs::omega_wire::pb::{
    GetOptionGreeksSurfaceRequest, GetPortfolioMarginRequest, MarginHealthStatus,
};
use margin_risk_rs::service::{build_http_router, MarginRiskEngineState, MarginRiskGrpcService};
use tonic::Request;

#[tokio::test]
async fn test_grpc_and_http_service_endpoints_e2e() {
    let state = MarginRiskEngineState::new_with_canonical_defaults();
    let grpc = MarginRiskGrpcService {
        state: state.clone(),
    };

    // 1. gRPC GetPortfolioMargin
    let margin_resp = grpc
        .get_portfolio_margin(Request::new(GetPortfolioMarginRequest { account_id: 10042 }))
        .await
        .expect("GetPortfolioMargin must succeed")
        .into_inner();
    assert_eq!(margin_resp.account_id, 10042);
    assert_eq!(margin_resp.span_grid.len(), 16);
    assert_eq!(margin_resp.health_status, MarginHealthStatus::Healthy as i32);

    // 2. gRPC GetOptionGreeksSurface
    let surf_resp = grpc
        .get_option_greeks_surface(Request::new(GetOptionGreeksSurfaceRequest {
            underlying_symbol: "BTC-USDT".to_string(),
        }))
        .await
        .expect("GetOptionGreeksSurface must succeed")
        .into_inner();
    assert!(surf_resp.surface_arbitrage_free);
    assert_eq!(surf_resp.svi_slices.len(), 4);
    assert_eq!(surf_resp.points.len(), 56); // 4 tenors x 7 strikes x 2 rights
    for pt in &surf_resp.points {
        assert!(pt.solver_iterations <= 6, "IV solver iterations {} > 6", pt.solver_iterations);
    }

    // 3. Axum HTTP Server on ephemeral loopback port (verifying /healthz, /metrics, /api/v1/margin/accounts/10042)
    let router = build_http_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let srv = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    // Minimal raw HTTP/1.1 GET probe via tokio::net::TcpStream (zero extra client crate needed!)
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut resp_buf = String::new();
    stream.read_to_string(&mut resp_buf).await.unwrap();
    assert!(resp_buf.contains("200 OK"), "unexpected /healthz: {resp_buf}");
    assert!(resp_buf.contains("\"service\":\"margin-risk-rs\""));
    assert!(resp_buf.contains("hyperion.omega.margin.adl.v1"));

    let mut stream2 = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream2
        .write_all(
            b"GET /api/v1/margin/accounts/10042 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut resp_buf2 = String::new();
    stream2.read_to_string(&mut resp_buf2).await.unwrap();
    assert!(resp_buf2.contains("200 OK"), "unexpected REST response: {resp_buf2}");
    assert!(resp_buf2.contains("\"account_id\":10042"));

    let mut stream3 = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream3
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut resp_buf3 = String::new();
    stream3.read_to_string(&mut resp_buf3).await.unwrap();
    assert!(resp_buf3.contains("hyperion_margin_evaluations_total"));

    srv.abort();
}

#[test]
fn test_emit_full_quantitative_verification_reports() {
    // 1. 6,600-point Central Finite-Difference Analytical Greeks Audit
    let fd = run_exhaustive_finite_difference_verification(1e-5);
    println!("\n[FD_VERIFICATION_JSON] {}", serde_json::to_string(&fd).unwrap());
    assert!(fd.all_within_tolerance, "FD check failed: {fd:?}");

    // 2. 100,000-path Merton Jump-Diffusion x 5,000-account Stress Report
    let mc = run_merton_jump_diffusion_stress(&MertonJumpDiffusionConfig::default());
    println!("\n[MERTON_STRESS_JSON] {}", serde_json::to_string(&mc).unwrap());
    assert_eq!(mc.num_paths, 100_000);
    assert_eq!(mc.num_accounts, 5_000);
    assert_eq!(mc.total_unbacked_deficit_leakage_fp, 0);
    assert_eq!(mc.max_single_path_unbacked_deficit_fp, 0);
}
