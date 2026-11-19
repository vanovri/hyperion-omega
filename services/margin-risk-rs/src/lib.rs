#![forbid(unsafe_code)]

//! Hyperion Channel 14 (`margin-risk-rs`): Institutional Low-Latency Cross-Margin Risk Engine.
//!
//! Modules:
//! - [`fixed_point`]: `1e8` atomic integer fixed-point accounting (`Fp`, `Ppm`, `Bps`, `WireI128`, `WireU128`).
//! - [`math_simd`]: 64-byte aligned `F64x8` SIMD tiles & Cody/Hart rational Chebyshev normal CDF/PDF/InvCDF.
//! - [`greeks_soa`]: Vectorized SoA Black-Scholes-Merton (European) & Bjerksund-Stensland 2002 (American)
//!   pricing + 1st/2nd order analytical Greeks (`Delta`, `Gamma`, `Vega`, `Theta`, `Rho`, `Vanna`, `Volga`)
//!   with exact sub-second Pin-Risk handling & 6,600-point Central Finite-Difference verifier.
//! - [`iv_solver`]: Hybrid 3rd-order Halley / Newton-Raphson + monotonic bracketed Brent's Method
//!   Implied Volatility solver (`<= 6` iterations to `1e-8`).
//! - [`svi_surface`]: Arbitrage-free Gatheral-Jacquier SVI & SSVI volatility surface (Durrleman butterfly
//!   $g(k) \ge 0$ + calendar-spread total-variance monotonicity $\partial_T w \ge 0$).
//! - [`perp_funding`]: Perpetual Mark-Price (outlier-filtered constituent Median Index + EMA Basis) &
//!   Dynamic Clamped Funding Rate engine with exact zero-sum settlement.
//! - [`span_margin`]: 16-Point SPAN Scenario-Grid (`+/-15%` price, `+/-30%` IV + tail shocks) Cross-Margin
//!   calculator across Spot, Perpetual Futures, and Options (`SoACrossMarginEngineBatch`).
//! - [`liquidation_waterfall`]: 4-Stage deterministic Liquidation Waterfall (`CancelOpenOrders` ->
//!   `PartialBookLiquidation` -> `InsuranceFundAbsorption` -> `AutoDeleveraging` Quantiles `Q1..Q5`).
//! - [`monte_carlo`]: `100,000`-path Merton Jump-Diffusion stress suite over `5,000` high-leverage accounts.

pub mod fixed_point;
pub mod greeks_soa;
pub mod iv_solver;
pub mod liquidation_waterfall;
pub mod math_simd;
pub mod monte_carlo;
pub mod omega_wire;
pub mod perp_funding;
pub mod service;
pub mod span_margin;
pub mod svi_surface;
