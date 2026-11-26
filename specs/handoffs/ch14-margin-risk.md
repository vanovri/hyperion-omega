# CHANNEL 14 HANDOFF — INSTITUTIONAL DERIVATIVES CROSS-MARGIN & RISK ENGINE (`services/margin-risk-rs`)

- **Status**: `VERIFIED 100% — SIMD SoA GREEKS + SVI/HALLEY-BRENT IV + 16-PT SPAN + 4-STAGE WATERFALL + MERTON 100K×5K + OVERCLOCK GATES 1–3`
- **Exclusive Owned Paths**: `/services/margin-risk-rs/**`, `/specs/handoffs/ch14-margin-risk.md`
- **Upstream Contracts Honored**:
  - [`00-hyperion-contract.md`](../../.agents/rules/00-hyperion-contract.md) (Zero-float `10^8` fixed-point accounting boundary, stringified JSON `_fp` decimal encoding §5.1.3, strict directory isolation)
  - [`01-hyperion-omega.md`](../../.agents/rules/01-hyperion-omega.md) (`MarginRiskService` gRPC `:50163`, HTTP/REST/Metrics `:8093`, Redpanda `hyperion.omega.margin.{snapshots,greeks,liquidations,adl}.v1`, §5.1 wire scale contract)
  - [`02-extreme-overclock-protocol.md`](../../.agents/rules/02-extreme-overclock-protocol.md) (Gate 1: `30/30 = 100.00%` Mutation Kill Score; Gate 2: `#![forbid(unsafe_code)]` + `500,000`-cycle `0 B` zero-leak soak; Gate 3: 3-Pass Micro-Optimization `2.38×` speedup & `-100%` hot-loop heap allocations)
  - [`ch11-formal-omega.md`](ch11-formal-omega.md) & [`margin_risk.proto`](../../proto/omega/margin_risk.proto) (Z3 Theorems `T2.1` conservative `ceil_div` margin floor & `T2.3` ADL principal protection; pre-compiled `hyperion-omega-proto`)
- **Owner**: Channel 14 (Principal Quantitative Financial Engineer & Low-Latency Rust Developer)

---

## 1. Quantitative & Systems Architecture

`services/margin-risk-rs` is a pure-safe Rust (`#![forbid(unsafe_code)]`) institutional derivatives risk engine supporting **Spot, Perpetual Futures, and European/American Options** under a unified cross-margin engine designed for sub-millisecond portfolio revaluation and zero-leakage liquidation cascades.

### 1.1 Zero-Float Accounting Boundary (`src/fixed_point.rs`)
- All stateful ledger balances, equity snapshots, Initial/Maintenance Margin (`IM`/`MM`), funding settlements, liquidation waterfall bookkeeping, Insurance Fund balances, ADL deficits, and Protobuf wire payloads use exact integer fixed-point arithmetic (`i128` / `u128`):
  - Prices, Quantities, Equity, PnL, IV, Funding Rate, Greeks: `FP_SCALE = 100_000_000` (`1e8`)
  - Ratios (`IMR`, `MMR`, Premium Index, ADL Quantile): `PPM_SCALE = 1_000_000` (`1e6`)
  - Risk Add-on Parameters: `BPS_SCALE = 10_000` (`1e4`)
- **Conservative Asymmetric Rounding**: `f64` is confined strictly to stateless analytical pricing kernels (`BSM`, `Bjerksund-Stensland 2002`, `SVI`, `Halley/Brent`). When crossing back into ledger accounting:
  - Margin requirements (`MM`, `IM`, scenario losses, short-option minimums) ALWAYS round **up** (`f64_to_fp_ceil`, `apply_bps_ceil_nonneg`, `div_ceil`).
  - Account equity, collateral valuations, and adverse PnL ALWAYS round **down** (`f64_to_fp_floor`, `mul_fp_floor`).

### 1.2 Arbitrage-Free Implied Volatility Surface & Analytical Greeks
- **Hybrid 3rd-Order Halley / Newton-Raphson + Brent's Method IV Solver (`src/iv_solver.rs`)**:
  - Exploits Put-Call parity to map every European option onto its **strictly Out-of-The-Money (OTM) Black-76 twin**, eliminating deep-ITM floating-point cancellation before solving.
  - Initializes via an analytical Brenner-Subrahmanyam / Manaster-Koehler / Li-Lee rational wing seed (`smart_initial_vol_guess`), executes 3rd-order Halley updates using exact Vega & Volga ($\sigma_{n+1} = \sigma_n - \frac{f / \mathcal{V}}{1 - \frac{1}{2}(f/\mathcal{V})(\text{Volga}/\mathcal{V})}$) inside a monotonically tightened bracket $[lo, hi]$, and transparently falls back to bracketed Brent's Method on extreme low-Vega wings.
  - Supports both **European (BSM)** and **American (Bjerksund-Stensland 2002)** options, verified converging to **`< 1e-8` volatility precision in `<= 6` iterations** across the moneyness/tenor/vol grid while strictly rejecting arbitrage-bound violations and expired contracts.
- **Arbitrage-Free SVI / SSVI Volatility Surface (`src/svi_surface.rs`)**:
  - Implements Gatheral-Jacquier Raw SVI total implied variance $w(k) = a + b\left(\rho(k - m) + \sqrt{(k - m)^2 + \sigma^2}\right)$ and SSVI surface parameterization plus a Quasi-Explicit Zeliade nonlinear calibrator (`calibrate_raw_svi_quasi_explicit`).
  - Explicitly verifies all static no-arbitrage constraints before admitting any surface slice:
    1. **Roger Lee Moment Wing Slope Bound**: $b(1 + |\rho|) < 2$
    2. **Durrleman Butterfly Arbitrage-Free Density Condition**: $g(k) = \left(1 - \frac{k w'(k)}{2 w(k)}\right)^2 - \frac{(w'(k))^2}{4}\left(\frac{1}{w(k)} + \frac{1}{4}\right) + \frac{w''(k)}{2} \ge 0$ across a dense `[-4.0, +4.0]` log-moneyness mesh.
    3. **Calendar-Spread Arbitrage-Free Monotonicity**: strict tenor ordering $T_i > T_{i-1}$ and total implied variance monotonicity $w(k, T_i) \ge w(k, T_{i-1})$ across log-moneyness, enabling arbitrage-free total-variance interpolation across arbitrary maturities.
- **Vectorized 8-Lane SoA Analytical Greeks & Sub-Second Pin-Risk Engine (`src/math_simd.rs`, `src/greeks_soa.rs`)**:
  - `#[repr(C, align(64))] F64x8` 64-byte cache-line tiles with Cody (1969) full-precision `erfc` (`norm_cdf`), Hart/West `< 1e-14` rational Chebyshev `norm_cdf_fast`, and exact half-`exp()` BSM evaluation (`norm_cdf_from_exp`) exploiting the identity $S e^{-qT} \phi(d_1) = K e^{-rT} \phi(d_2)$ to cut transcendental `exp()` calls by **50%** with zero mathematical approximation.
  - Evaluates all 7 analytical Greeks (**`Delta`, `Gamma`, `Vega`, `Theta`, `Rho`, `Vanna`, `Volga`**) in SoA memory layout (`OptionPositionsSoA`) for European (BSM) and American (Bjerksund-Stensland 2002 two-boundary approximation with exact Put-Call duality).
  - **Sub-Second Pin-Risk Stabilization**: Eliminates 12-digit IEEE-754 catastrophic cancellation near $T \to 0$ (tested down to `T = 50 ms` and `T = 0.0 s`) using `exp_m1()` carry decomposition and OTM Put-Call parity reconstruction, guaranteeing zero `NaN`/`Inf` at expiry.

### 1.3 16-Point SPAN Portfolio Cross-Margin & Perpetual Mark/Funding
- **16-Point SPAN Cross-Margin Grid (`src/span_margin.rs`)**:
  - Evaluates every account jointly across **Spot + Perpetual Futures + European/American Options** over `CANONICAL_SPAN_16_SCENARIOS`:
    - Scenarios `0..13`: `7` underlying price shocks (`-15%, -10%, -5%, 0%, +5%, +10%, +15%`) $\times$ `2` implied volatility shocks (`-30%`, `+30%` relative shift).
    - Scenarios `14..15`: Extreme tail-shock scenarios (`-30%` price / `+60%` IV and `+30%` price / `+60%` IV) weighted at `35%` tail scan weight per CME SPAN convention.
  - Adds conservative institutional risk floors:
    - **Adverse Vanna/Volga Tail Convexity Add-On** (charges only negative/short tail convexity without penalizing long protective hedges).
    - **Short-Option Minimum Floor** (`short_option_min_bps` per short contract notional, closing the naked far-OTM zero-margin loophole).
    - **Expiry Pin-Risk Add-On** (scaled smoothly for options inside the 24-hour pin window).
    - **Gross Basis Contingency Floor** (`gross_contingency_bps` preventing infinite leverage on zero-delta spot/perp boxes).
  - Maintains `IM >= MM` hierarchy (`im_to_mm_multiplier_bps`) and produces `SoACrossMarginEngineBatch` with 3 progressively optimized evaluation passes (benchmarked in §6.3).
- **Perpetual Mark-Price & Dynamic Clamped Funding (`src/perp_funding.rs`)**:
  - **Mark-Price Engine**: Rejects stale quotes (`quote_ts_ns`) and spoofed outlier venues (relative deviation from raw median) before computing the constituent **Median Spot Index**, applies an exponential moving average (**EMA Basis**) over `(mid_perp - index)`, and collars the fair mark price across the 3-price median of `[index + ema_basis, mid_perp, last_trade]`.
  - **Dynamic Funding Calculator**: Walks L2 orderbook depth up to Impact Margin Notional (`IMN`) for `impact_bid_vwap` and `impact_ask_vwap`, computes the interest-rate damped premium index, clamps output to `±min(outer_cap, mm_ratio_cap)` (default `±0.75%`), and settles funding via exact zero-sum integer `i128` accounting (deterministic dust tracking guarantees `sum(account_deltas) + dust == 0`).

### 1.4 4-Stage Liquidation Waterfall, Measured Insurance Fund & Quantile ADL (`src/liquidation_waterfall.rs`)
Deterministic integer fixed-point state machine with zero unbacked bankruptcy leakage:
1. **Stage 1 (`CancelOpenOrders`)**: Immediately cancels margin-consuming resting orders and releases locked `open_order_margin_fp`. Re-checks solvency before touching live positions — if healthy, waterfall exits without position liquidation.
2. **Stage 2 (`PartialBookLiquidation`)**: Computes exact **Bankruptcy Price** $P_{\text{bankrupt}}$ and executes stepwise partial fills (`slice_fraction_bps`) into the L2 orderbook strictly bounded by $P_{\text{bankrupt}}$ (`bid >= P_bankrupt` for longs; `ask <= P_bankrupt` for shorts). Stops immediately once post-slice equity covers maintenance margin, preserving remaining user position.
3. **Stage 3 (`InsuranceFundAbsorption`)**: If unfilled underwater position remains after book exhaustion at $P_{\text{bankrupt}}$, absorbs the exact residual deficit from `InsuranceFundState` up to its configured single-event drawdown cap (`max_single_event_drawdown_bps`) and available liquid reserve.
4. **Stage 4 (`AutoDeleveraging`, Quantiles `Q1..Q5`)**: Ranks opposite-side profitable counterparties by descending `profit_leverage_score = PnL_pct * effective_leverage`, buckets them into `Q1..Q5` (`Q5` = top 20% highest profit-leverage), and executes deleveraging against bankruptcy price with an $O(1)$-amortized cursor (`execute_liquidation_waterfall_with_ranked`).
- **CH11 Z3 Formal Theorem Alignment (`theorem2_margin_liquidation_adl.py`)**:
  - **Theorem `T2.1` (Zero Sub-Tick Margin Blindspot)**: `required_margin_for_check_fp` uses exact integer `div_ceil`, ensuring `MM_fp >= 1` for every non-zero position even at `|q|=1, mark=1, mmr=1`.
  - **Theorem `T2.3` (ADL Principal Protection)**: For profitable counterparties, `covered_deficit` is strictly capped by `cp_slice_pnl_mark`, guaranteeing `cash_after >= initial_cash` (ADL never seizes a profitable trader's deposited principal).

---

## 2. Physical Artifact Inventory (`services/margin-risk-rs`)

| File Path | Purpose |
|---|---|
| [`services/margin-risk-rs/Cargo.toml`](../../services/margin-risk-rs/Cargo.toml) | Standalone crate importing `hyperion-proto` & `hyperion-omega-proto`, `tonic 0.12`, `axum 0.7`, `rayon 1.10`, `blake3`, `criterion 0.5` |
| [`services/margin-risk-rs/Dockerfile`](../../services/margin-risk-rs/Dockerfile) | Multi-stage release build exposing gRPC `:50163` and HTTP/Metrics `:8093` |
| [`services/margin-risk-rs/src/lib.rs`](../../services/margin-risk-rs/src/lib.rs) | Crate root enforcing `#![forbid(unsafe_code)]` and exporting all 11 modules |
| [`services/margin-risk-rs/src/fixed_point.rs`](../../services/margin-risk-rs/src/fixed_point.rs) | `1e8`/`1e6`/`1e4` fixed-point math, conservative `ceil`/`floor` rounding, `WireI128`/`WireU128` codecs |
| [`services/margin-risk-rs/src/math_simd.rs`](../../services/margin-risk-rs/src/math_simd.rs) | `#[repr(C, align(64))] F64x8` SIMD tiles, Cody `erfc`, Hart rational `norm_cdf_fast`, exact half-`exp()` `norm_cdf_from_exp`, Acklam+Halley `norm_inv_cdf` |
| [`services/margin-risk-rs/src/greeks_soa.rs`](../../services/margin-risk-rs/src/greeks_soa.rs) | Cancellation-free European BSM + American Bjerksund-Stensland (2002), 8-lane `OptionPositionsSoA`, sub-second pin-risk handler, 6,600-point Richardson Central FD verifier |
| [`services/margin-risk-rs/src/iv_solver.rs`](../../services/margin-risk-rs/src/iv_solver.rs) | Hybrid 3rd-order Halley / Newton-Raphson + Brent's Method IV solver (`<= 6` iters to `1e-8`) for European & American options |
| [`services/margin-risk-rs/src/svi_surface.rs`](../../services/margin-risk-rs/src/svi_surface.rs) | Gatheral-Jacquier `RawSviSlice`, `ArbitrageFreeSviSurface`, Quasi-Explicit Zeliade calibrator, Durrleman butterfly & calendar spread arbitrage verifiers |
| [`services/margin-risk-rs/src/perp_funding.rs`](../../services/margin-risk-rs/src/perp_funding.rs) | Outlier-filtered constituent Median Index + EMA Basis + 3-price collar Mark-Price engine & Dynamic Clamped Funding Rate with exact zero-sum settlement |
| [`services/margin-risk-rs/src/span_margin.rs`](../../services/margin-risk-rs/src/span_margin.rs) | 16-Point SPAN scenario matrix (`CANONICAL_SPAN_16_SCENARIOS`), single-account evaluator, and `SoACrossMarginEngineBatch` (Passes 1, 2, and 3) |
| [`services/margin-risk-rs/src/liquidation_waterfall.rs`](../../services/margin-risk-rs/src/liquidation_waterfall.rs) | 4-stage liquidation state machine (`CancelOpenOrders` → `PartialBookLiquidation` → `InsuranceFundAbsorption` → `AutoDeleveraging Q1..Q5`), Z3 `T2.1`/`T2.3` enforcement |
| [`services/margin-risk-rs/src/monte_carlo.rs`](../../services/margin-risk-rs/src/monte_carlo.rs) | Deterministic `Xoshiro256PlusPlus` Merton (1976) Jump-Diffusion stress engine (`100,000` paths × `5,000` high-leverage accounts, Rayon parallelized) |
| [`services/margin-risk-rs/src/omega_wire.rs`](../../services/margin-risk-rs/src/omega_wire.rs) | Canonical `hyperion_omega_proto::v1` wire encoders/decoders honoring `01-hyperion-omega.md` §5.1 scales (`theta_fp` per calendar day `1/365.25` yr) |
| [`services/margin-risk-rs/src/service.rs`](../../services/margin-risk-rs/src/service.rs) | Tonic gRPC `MarginRiskService` + Axum HTTP/REST/Prometheus router & lock-free telemetry counters |
| [`services/margin-risk-rs/src/main.rs`](../../services/margin-risk-rs/src/main.rs) | Production binary daemon serving gRPC `:50163` & HTTP `:8093` |
| [`services/margin-risk-rs/tests/mutation_gate.rs`](../../services/margin-risk-rs/tests/mutation_gate.rs) | Overclock Gate 1: 30-mutant cross-domain semantic mutation oracle (`30 / 30 = 100.00%`) |
| [`services/margin-risk-rs/tests/overclock_soak_and_passes.rs`](../../services/margin-risk-rs/tests/overclock_soak_and_passes.rs) | Overclock Gate 2 (`500,000`-cycle `TrackingAllocator` `0 B` zero-leak soak) & Gate 3 (Pass 1/2/3 equivalence + latency/allocation telemetry) |
| [`services/margin-risk-rs/tests/service_e2e.rs`](../../services/margin-risk-rs/tests/service_e2e.rs) | Live Tonic gRPC + Axum HTTP TCP integration suite & JSON quantitative report emitter |
| [`services/margin-risk-rs/benches/portfolio_eval.rs`](../../services/margin-risk-rs/benches/portfolio_eval.rs) | Criterion single-core benchmark harness (`10,000`-position SPAN Passes 1–3 & `50,000`-option pure 7-Greeks SoA) |

---

## 3. Service Ports, Wire Contracts & Redpanda Topics

| Interface | Endpoint / Topic | Specification |
|---|---|---|
| **gRPC Service** | `0.0.0.0:50163` (`hyperion.omega.v1.MarginRiskService`) | `GetPortfolioMargin`, `GetOptionGreeksSurface`, `StreamMarginEvents` (`hyperion-omega-proto`) |
| **HTTP Health** | `GET http://localhost:8093/healthz` | `200 OK` JSON status (`{"status":"ok","service":"margin-risk-rs"}`) |
| **Prometheus Metrics** | `GET http://localhost:8093/metrics` | Live atomic evaluation counters, liquidation/ADL totals, latency telemetry |
| **HTTP Margin Query** | `GET http://localhost:8093/api/v1/margin/accounts/:account_id` | All `10^8` `_fp` values serialized as JSON decimal strings (`00-hyperion-contract.md` §5.1.3) |
| **Redpanda Topics** | `hyperion.omega.margin.snapshots.v1`<br>`hyperion.omega.margin.greeks.v1`<br>`hyperion.omega.margin.liquidations.v1`<br>`hyperion.omega.margin.adl.v1` | Canonical Omega Protobuf envelopes (`PortfolioMarginSnapshot`, `OptionGreeksSurface`, `LiquidationWaterfallEvent`, `AdlTrigger`) |

### Wire Scale Alignment (`01-hyperion-omega.md` §5.1)
- `equity_fp`, `initial_margin_fp`, `maintenance_margin_fp`, `mark_price_fp`, `implied_vol_fp`, `funding_rate_fp`, `delta_fp`, `gamma_fp`, `vega_fp`, `rho_fp`, `vanna_fp`, `volga_fp`: **`1e8` (`FP_SCALE`)**
- `theta_fp`: **`1e8` per calendar day (`annual_theta / 365.25`)**, verified in `omega_wire::tests::test_omega_protobuf_wire_roundtrip_margin_and_greeks_surface`.
- `im_ratio_ppm`, `mm_ratio_ppm`, `adl_quantile_ppm`: **`1e6` (`PPM_SCALE`)**
- *Note on upstream handoffs*: `ch02-ledger-rs.md` and `ch04-stream-aggregator.md` were not yet committed in their parallel worktrees during Channel 14 build; wire compatibility was verified directly against `contracts/gen/rust` (`hyperion-proto`), `proto/omega/gen/rust` (`hyperion-omega-proto`), `services/ledger-rs`, and `services/stream-aggregator` crate schemas.

---

## 4. Numerical Precision Verification — Central Finite-Difference & IV/SVI Proofs

### 4.1 Exhaustive 6,600-Combination Central Finite-Difference Verification (Incl. 3,000 Pin-Risk Cases)
Executed via `greeks_soa::run_exhaustive_finite_difference_verification()` (`cargo test --release --offline -- --nocapture`) using a 4th-order Richardson Central Finite-Difference stencil across `6,600` `(strike, maturity, rate, vol, right)` tuples including `3,000` near-expiry pin-risk maturities down to **`T = 50 ms`** (`1.58e-9 yr`), plus `T = 0.0 s` exact expiry boundary validation:

| Greek / Invariant | Max Relative Error (6,600 Cases) | Tolerance Gate | Status |
|---|---:|---:|---|
| **Delta** ($\partial V / \partial S$) | **`2.1045e-7`** | `< 1.0e-5` | `PASS` |
| **Gamma** ($\partial^2 V / \partial S^2$) | **`1.0654e-7`** | `< 1.0e-5` | `PASS` |
| **Vega** ($\partial V / \partial \sigma$) | **`4.6969e-8`** | `< 1.0e-5` | `PASS` |
| **Theta** ($\partial V / \partial t$) | **`4.7418e-7`** | `< 1.0e-5` | `PASS` |
| **Rho** ($\partial V / \partial r$) | **`1.0713e-6`** | `< 1.0e-5` | `PASS` |
| **Vanna** ($\partial^2 V / \partial S \partial \sigma$) | **`1.4127e-6`** | `< 1.0e-5` | `PASS` |
| **Volga** ($\partial^2 V / \partial \sigma^2$) | **`1.4424e-7`** | `< 1.0e-5` | `PASS` |
| **BSM PDE Residual** ($\Theta + (r-q)S\Delta + \frac{1}{2}\sigma^2 S^2 \Gamma - rV$) | **`6.6356e-16`** | `< 1.0e-12` | `PASS` |
| **Near-Zero Pin-Risk (`50 ms` & `0 s`)** | `3,000` cases, `0` NaN/Inf | `0` NaN/Inf | `PASS` |

### 4.2 Implied Volatility & SVI Surface Verification
- **Halley / Newton-Raphson + Brent IV Solver**: `test_halley_iv_convergence_le_6_iters_across_grid` & `test_american_put_iv_roundtrip` passed across moneyness/maturity/volatility grids with **`|sigma_hat - sigma_true| < 1e-8`** in **`<= 6` iterations** on all European points and exact roundtrip recovery on American Puts (`BS2002`).
- **SVI / SSVI Arbitrage-Free Checks**: `test_ssvi_surface_butterfly_and_calendar_arbitrage_free`, `test_quasi_explicit_svi_calibration_recovers_smile`, and `test_detects_butterfly_and_calendar_arbitrage_violations` verified both calibration recovery and deterministic rejection of Durrleman butterfly ($g(k) < 0$), Lee wing slope ($b(1+|\rho|) \ge 2$), and calendar-spread crossing ($\partial_T w < 0$) violations.

---

## 5. Merton (1976) Jump-Diffusion Stress Proof (`100,000` Paths × `5,000` Accounts)

Executed via `monte_carlo::run_merton_jump_diffusion_stress()` in `--release` mode (`Xoshiro256PlusPlus` deterministic PRNG, Rayon parallel path execution, balanced high-leverage portfolio book of `5,000` accounts across `100,000` extreme jump-diffusion trajectories = **`500,000,000`** account-path evaluations):

| Stress Metric | Verified Measurement (`--release`) |
|---|---:|
| **Simulated Price Paths** | **`100,000`** |
| **High-Leverage Portfolio Accounts per Path** | **`5,000`** |
| **Total Account-Path Evaluations** | **`500,000,000`** |
| **Minimum Simulated Mark Price (`1e8` fp)** | `1,216,419,180,062` (`$12,164.19`) |
| **Maximum Simulated Mark Price (`1e8` fp)** | `18,310,872,124,776` (`$183,108.72`) |
| **Worst Flash-Crash Drawdown** | **`-79.73%`** (`-79.7263%`) |
| **Max Violent Melt-Up Surge** | **`+205.18%`** (`+205.1812%`) |
| **Paths Triggering Liquidations** | `100,000` (`100.00%`) |
| **Paths Triggering Insurance Fund (Stage 3)** | `99,171` (`99.17%`) |
| **Paths Triggering Quantile ADL (Stage 4)** | `99,993` (`99.99%`) |
| **Total Stage 1 Open-Order Cancellations** | `2,410,407` |
| **Total Stage 2 Partial Book Liquidations** | `327,704` |
| **Total Stage 3 Insurance Fund Absorptions** | `1,707,628` |
| **Total Stage 4 ADL Counterparty Events** | `214,461,559` |
| **Total Unbacked Systemic Deficit Leakage (`1e8` fp)** | **`0`** (`$0.00000000`) |
| **Max Single-Path Unbacked Deficit (`1e8` fp)** | **`0`** (`$0.00000000`) |

---

## 6. Mandatory Overclock Protocol Verification (`02-extreme-overclock-protocol.md`)

### 6.1 Gate 1 — Mutation Survival Zero (`>= 95.0%` Kill Score)
Executed via `tests/mutation_gate.rs` across 7 quantitative & risk domains (`M01..M30`):

| Domain | Mutant IDs | Fault Classes Injected | Killed / Total | Kill Rate |
|---|---|---|---:|---:|
| `fixed_point` | `M01..M05` | Scale `1e8→1e7` / `1e6→1e5`, conservative `ceil→trunc`, equity `floor→ceil`, negative PnL truncation | `5 / 5` | `100.00%` |
| `greeks_soa` | `M06..M10` | Put Delta sign flip, BSM PDE carry sign, Vanna/Volga sign inversions, American BS2002 early-exercise bypass | `5 / 5` | `100.00%` |
| `iv_solver` | `M11..M14` | Halley tolerance loosening (`1e-8→1e-3`), lower/upper arbitrage bound bypass, `T<=0` expiry guard flip | `4 / 4` | `100.00%` |
| `svi_surface` | `M15..M18` | Lee wing slope bound bypass, Durrleman butterfly `g(k)<0`, calendar crossing `dw/dT<0`, non-monotonic tenors | `4 / 4` | `100.00%` |
| `span_margin` | `M19..M22` | Cross-margin netting removed, gross contingency floor dropped, `IM<MM` inversion, short-option minimum removed | `4 / 4` | `100.00%` |
| `perp_funding` | `M23..M25` | Unfiltered mean vs outlier-filtered median index, funding clamp bypass, zero-sum funding sign flip | `3 / 3` | `100.00%` |
| `liquidation_waterfall` | `M26..M30` | Z3 `T2.1` sub-tick MM blindspot, Stage-1 skip, Stage-2 bankruptcy limit violation, ADL sort reversal, Z3 `T2.3` principal seizure | `5 / 5` | `100.00%` |
| **TOTAL** | **`M01..M30`** | **All Critical Risk, Pricing, Margin & Liquidation Invariants** | **`30 / 30`** | **`100.00%`** |

### 6.2 Gate 2 — UB Safety & 500,000-Cycle Zero-Leak Soak
- **Undefined Behavior Immunity**: Crate enforces `#![forbid(unsafe_code)]` at root (`src/lib.rs:1`), compiled cleanly under `cargo clippy --offline --all-targets -- -D warnings` with **`0` warnings and `0` errors**.
- **500,000-Cycle Consecutive Steady-State Soak (`tests/overclock_soak_and_passes.rs`, `TrackingAllocator` global allocator)**:
  - Consecutive ticks executed: **`500,000`**
  - Elapsed wall time (`--release`): **`10.082 s`**
  - Heap live bytes before soak: **`21,880 B`**
  - Heap live bytes after soak: **`21,880 B`**
  - **Steady-State Memory Leak**: **`0 B`** (**`0.00 B/cycle`**)

### 6.3 Gate 3 — 3-Pass Micro-Optimization & Single-Core Criterion Throughput (`> 100,000 pos/s`)

#### A. Hardware `TrackingAllocator` 3-Pass Telemetry (`5,000 positions / tick`, single-core, `--release`)
| Optimization Pass | Architectural Transformation | `p50` (`ns/pos`) | `p99` (`ns/pos`) | Single-Core Throughput | Heap Alloc (`B/batch`) | `allocs/batch` | Speedup vs Pass 1 |
|---|---|---:|---:|---:|---:|---:|---:|
| **Pass 1 — Baseline AoS** | Per-account struct traversal, dynamic `Vec` result allocation, Cody `erfc` per scenario | `2,036.9 ns` | `17,084.2 ns` | `490,937 pos/s` | `192,000 B` | `1` | `1.00×` |
| **Pass 2 — Vectorized Zero-Copy SoA** | Flat SoA arrays, pre-allocated reusable buffers, affine log-spot shifts + Hart rational CDF | `1,078.6 ns` | `5,243.2 ns` | `927,128 pos/s` | **`0 B`** | **`0`** (`-100%`) | **`1.89×`** (`-47.0%` p50) |
| **Pass 3 — 64B Cache-Aligned Half-Exp SIMD** | `#[repr(C, align(64))] AlignedHotOptionRecord`, branchless Put-Call parity, exact half-`exp()` BSM identity (`bs_price_half_exp`) cutting `exp()` calls by 50% | **`854.3 ns`** | **`4,137.6 ns`** | **`1,170,494 pos/s`** | **`0 B`** | **`0`** (`-100%`) | **`2.38×`** (`-58.1%` p50, `-75.8%` p99) |

#### B. Criterion Single-Core Benchmarks (`benches/portfolio_eval.rs`, 30 samples × 3s measurement)
| Benchmark Target | Workload Size | Criterion `[low, median, high]` | Single-Core Throughput (Median) | vs `100k pos/s` SLA |
|---|---|---:|---:|---:|
| `pass1_baseline_aos/10000` | `10,000` positions (`2,000` accts × 5 legs) | `[22.806 ms, 24.318 ms, 25.951 ms]` | `411,210 pos/s` (`411.21 Kelem/s`) | `4.11×` |
| `pass2_vectorized_zerocopy_soa/10000` | `10,000` positions (`2,000` accts × 5 legs) | `[13.050 ms, 14.154 ms, 15.364 ms]` | `706,530 pos/s` (`706.53 Kelem/s`) | `7.07×` |
| **`pass3_cacheline_aligned_simd/10000`** | **`10,000` positions (`2,000` accts × 5 legs)** | **`[10.733 ms, 11.493 ms, 12.355 ms]`** | **`870,070 pos/s` (`870.07 Kelem/s`)** | **`8.70×`** (`2.12×` vs Pass 1) |
| **`pure_7greeks_soa_50k_options`** | **`50,000` European options (all 7 Greeks + price)** | **`[9.9616 ms, 10.470 ms, 11.117 ms]`** | **`4,775,500 opts/s` (`4.7755 Melem/s`)** | **`47.76×`** (`209.4 ns/opt`) |

---

## 7. Reproducible Verification Commands

All commands executed directly in `services/margin-risk-rs` with exit code `0` (`25/25` unit + integration + overclock tests passing, `0` clippy warnings):

```powershell
Set-Location services/margin-risk-rs
cargo clippy --offline --all-targets -- -D warnings
cargo build --release --offline --bin margin-risk-rs
cargo test --release --offline -- --nocapture
cargo bench --offline --bench portfolio_eval -- --sample-size 30 --measurement-time 3
```
