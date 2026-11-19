import { describe, expect, it } from "vitest";
import {
  computeFeeFp,
  computeNotionalFp,
  formatFixedPoint,
  FP_SCALE,
  parseFixedPointDecimal,
  verifyDoubleEntryConservation,
  verifyUncrossedBook,
} from "../../src/lib/fixed-point";
import {
  crc32c,
  decodeBinaryHeader,
  decodeExecutionFillFrame,
  decodeL2SnapshotFrame,
  encodeBinaryFrame,
  evaluateSequenceContinuity,
  MSG_EXECUTION_FILL,
  MSG_L2_SNAPSHOT,
  WS_FLAG_SLOW_CONSUMER_DROPPED,
} from "../../src/lib/ws-binary-codec";
import {
  computeAnalyticalGreeks,
  evaluateSpanPortfolioMargin,
  evaluateSviImpliedVol,
  executeLiquidationWaterfall,
  REGIME_SVI_PRESETS,
  solveImpliedVolHalleyBrent,
  sviDurrlemanDensityG,
} from "../../src/lib/svi-greeks";
import {
  buildCanonicalSampleSolvencyBundle,
  injectAdversarialTamper,
  verifyMerkleSumSolvencyProof,
} from "../../src/lib/zk-merkle-sum-verifier";
import {
  executeLocalForensicSql,
  validateForensicSqlAst,
} from "../../src/lib/sql-ast-guard";
import { computeDeflatedSharpeRatio } from "../../src/lib/telemetry-types";

describe("1. Fixed-Point 1e8 Monetary Arithmetic & Ledger Invariants", () => {
  it("computes exact floor quote notional without IEEE-754 float precision loss", () => {
    const priceFp = 68_425_50000000n; // 68,425.50 USDT
    const qtyFp = 25000000n; // 0.25 BTC
    expect(computeNotionalFp(priceFp, qtyFp)).toBe(17_106_37500000n);
    // Odd sub-tick floor truncation check: (3 * 33_333_333) / 1e8 == 0, (3 * 33_333_334) / 1e8 == 1
    expect(computeNotionalFp(3n, 33_333_333n)).toBe(0n);
    expect(computeNotionalFp(3n, 33_333_334n)).toBe(1n);
  });

  it("computes taker fees and negative maker rebates rounded toward zero", () => {
    const notionalFp = 10_000n * FP_SCALE;
    expect(computeFeeFp(notionalFp, 500n)).toBe(5n * FP_SCALE); // +5 bps taker fee = +5.00 USDT
    expect(computeFeeFp(notionalFp, -200n)).toBe(-2n * FP_SCALE); // -2 bps maker rebate = -2.00 USDT
    // Sub-unit truncation toward zero for both signs
    expect(computeFeeFp(999_999n, 1n)).toBe(0n);
    expect(computeFeeFp(999_999n, -1n)).toBe(0n);
    expect(computeFeeFp(1_000_000n, -1n)).toBe(-1n);
  });

  it("enforces Global Double-Entry Conservation to 1e-8 atomic unit", () => {
    const accounts = [
      { accountId: 1n, availableFp: 500n * FP_SCALE, lockedFp: 120n * FP_SCALE },
      { accountId: 2n, availableFp: 375n * FP_SCALE, lockedFp: 0n },
    ];
    const treasuryFp = 5n * FP_SCALE;
    const ok = verifyDoubleEntryConservation(accounts, treasuryFp, 1_100n * FP_SCALE, 100n * FP_SCALE);
    expect(ok.holds).toBe(true);
    expect(ok.deltaFp).toBe(0n);

    // 1 atomic unit (10^-8) discrepancy MUST trigger breach
    const breached = verifyDoubleEntryConservation(
      accounts,
      treasuryFp + 1n,
      1_100n * FP_SCALE,
      100n * FP_SCALE
    );
    expect(breached.holds).toBe(false);
    expect(breached.deltaFp).toBe(1n);
  });

  it("enforces Uncrossed Book Law (BestBid < BestAsk)", () => {
    expect(verifyUncrossedBook(68_420_00000000n, 68_420_01000000n)).toBe(true);
    expect(verifyUncrossedBook(68_420_00000000n, 68_420_00000000n)).toBe(false);
    expect(verifyUncrossedBook(68_421_00000000n, 68_420_00000000n)).toBe(false);
    expect(verifyUncrossedBook(null, 68_420_00000000n)).toBe(true);
  });

  it("round-trips decimal strings to/from BigInt 1e8 accurately", () => {
    const parsed = parseFixedPointDecimal("68,425.50000001");
    expect(parsed).toBe(68_425_50000001n);
    expect(formatFixedPoint(parsed, 8)).toBe("68,425.50000001");
    expect(formatFixedPoint(-12_50000000n, 2, true)).toBe("-12.50");
    expect(formatFixedPoint(12_50000000n, 2, true)).toBe("+12.50");
  });
});

describe("2. Little-Endian 0x4859 Binary WebSocket Wire Codec & CRC-32C", () => {
  it("computes standard Castagnoli CRC-32C check vector (123456789 -> 0xe3069283)", () => {
    const ascii = new TextEncoder().encode("123456789");
    expect(crc32c(ascii)).toBe(0xe3069283);
  });

  it("encodes and decodes MSG_L2_SNAPSHOT & MSG_EXECUTION_FILL and rejects bit-rot", () => {
    const l2Payload = new Uint8Array(56 + 2 * 20);
    const lv = new DataView(l2Payload.buffer);
    lv.setBigUint64(0, 68_420_00000000n, true);
    lv.setBigUint64(8, 68_421_00000000n, true);
    lv.setUint16(16, 1, true);
    lv.setUint16(18, 1, true);
    l2Payload.fill(0xab, 24, 56);
    lv.setBigUint64(56, 68_420_00000000n, true);
    lv.setBigUint64(64, 1_50000000n, true);
    lv.setUint32(72, 4, true);
    lv.setBigUint64(76, 68_421_00000000n, true);
    lv.setBigUint64(84, 2_25000000n, true);
    lv.setUint32(92, 7, true);

    const frame = encodeBinaryFrame(MSG_L2_SNAPSHOT, 0, 1, 9912n, 1700000000n, l2Payload);
    const decoded = decodeL2SnapshotFrame(frame);
    expect(decoded.header.engineSeq).toBe(9912n);
    expect(decoded.bestBidFp).toBe(68_420_00000000n);
    expect(decoded.bestAskFp).toBe(68_421_00000000n);
    expect(decoded.bids[0]?.orderCount).toBe(4);
    expect(decoded.asks[0]?.totalQtyFp).toBe(2_25000000n);

    // Flip 1 bit in payload -> CRC32C must trap Protocol 1002 error
    const corrupted = new Uint8Array(frame);
    corrupted[40] = (corrupted[40] ?? 0) ^ 0x01;
    expect(() => decodeBinaryHeader(corrupted)).toThrow(/CRC32C mismatch/);

    // ExecutionFill 64B payload roundtrip
    const fillPayload = new Uint8Array(64);
    const fv = new DataView(fillPayload.buffer);
    fv.setBigUint64(0, 501n, true);
    fv.setBigUint64(40, 68_420_50000000n, true);
    fv.setBigUint64(48, 50000000n, true);
    fv.setUint8(56, 2);
    const fillFrame = encodeBinaryFrame(MSG_EXECUTION_FILL, 0, 1, 9913n, 1700000010n, fillPayload);
    const fill = decodeExecutionFillFrame(fillFrame);
    expect(fill.fillId).toBe(501n);
    expect(fill.aggressorSide).toBe(2);
  });

  it("enforces L3 sequence continuity and detects gaps or slow-consumer drops", () => {
    expect(evaluateSequenceContinuity(100n, 100n, 105n, 0)).toEqual({
      status: "APPLY",
      nextLocalSeq: 105n,
    });
    expect(evaluateSequenceContinuity(100n, 101n, 105n, 0).status).toBe("GAP_DETECTED");
    expect(
      evaluateSequenceContinuity(100n, 100n, 105n, WS_FLAG_SLOW_CONSUMER_DROPPED).status
    ).toBe("GAP_DETECTED");
    expect(evaluateSequenceContinuity(105n, 100n, 104n, 0).status).toBe("DUPLICATE_OVERLAP");
  });
});

describe("3. SVI Volatility Surface, Halley IV Solver, Analytical Greeks & Waterfall", () => {
  it("preserves non-negative Durrleman butterfly density g(k) >= 0 across regimes", () => {
    for (const regime of Object.values(REGIME_SVI_PRESETS)) {
      for (const k of [-0.4, -0.2, 0.0, 0.2, 0.4]) {
        expect(sviDurrlemanDensityG(k, regime)).toBeGreaterThanOrEqual(0);
        expect(evaluateSviImpliedVol(k, 0.5, regime)).toBeGreaterThan(0.05);
      }
    }
  });

  it("converges Halley/Brent IV solver to < 1e-8 within <= 6 iterations", () => {
    const spot = 68_000;
    const strike = 70_000;
    const T = 0.25;
    const r = 0.04;
    const targetVol = 0.62;
    const mktPrice = computeAnalyticalGreeks(spot, strike, T, r, targetVol, true).price;
    const solved = solveImpliedVolHalleyBrent(mktPrice, spot, strike, T, r, true);
    expect(solved.converged).toBe(true);
    expect(solved.iterations).toBeLessThanOrEqual(6);
    expect(Math.abs(solved.impliedVol - targetVol)).toBeLessThan(1e-8);
  });

  it("matches Central Finite-Difference numerical derivatives for Delta, Gamma, Vega, Vanna & Volga", () => {
    const S = 100.0;
    const K = 105.0;
    const T = 0.5;
    const r = 0.03;
    const sig = 0.35;
    const g = computeAnalyticalGreeks(S, K, T, r, sig, true);

    const dS = 1e-3;
    const dSig = 1e-4;
    const pUp = computeAnalyticalGreeks(S + dS, K, T, r, sig, true).price;
    const pDn = computeAnalyticalGreeks(S - dS, K, T, r, sig, true).price;
    const numDelta = (pUp - pDn) / (2 * dS);
    const numGamma = (pUp - 2 * g.price + pDn) / (dS * dS);

    const vUp = computeAnalyticalGreeks(S, K, T, r, sig + dSig, true);
    const vDn = computeAnalyticalGreeks(S, K, T, r, sig - dSig, true);
    const numVega = (vUp.price - vDn.price) / (2 * dSig);
    const numVanna = (vUp.delta - vDn.delta) / (2 * dSig);
    const numVolga = (vUp.vega - vDn.vega) / (2 * dSig);

    expect(Math.abs(g.delta - numDelta)).toBeLessThan(1e-6);
    expect(Math.abs(g.gamma - numGamma)).toBeLessThan(1e-5);
    expect(Math.abs(g.vega - numVega)).toBeLessThan(1e-5);
    expect(Math.abs(g.vanna - numVanna)).toBeLessThan(1e-4);
    expect(Math.abs(g.volga - numVolga)).toBeLessThan(1e-3);
  });

  it("guarantees zero unbacked deficit across SPAN 16-grid and 4-stage Liquidation Waterfall", () => {
    const span = evaluateSpanPortfolioMargin({
      collateralFp: 100_000n * FP_SCALE,
      spotPriceFp: 68_000n * FP_SCALE,
      netDeltaBaseFp: -2n * FP_SCALE,
      netGammaFp: -50_000n,
      netVegaFp: 10_000n * FP_SCALE,
    });
    expect(span.scenarios).toHaveLength(16);
    expect(span.worstCaseLossFp).toBeGreaterThan(0n);

    // Stage 3 Insurance absorption
    const s3 = executeLiquidationWaterfall({
      accountEquityFp: -25_000n * FP_SCALE,
      maintenanceMarginFp: 15_000n * FP_SCALE,
      lockedOrderMarginFp: 0n,
      insuranceFundBalanceFp: 100_000n * FP_SCALE,
      profitableCounterpartyPoolFp: 500_000n * FP_SCALE,
    });
    expect(s3.stage).toBe("STAGE_3_INSURANCE_FUND");
    expect(s3.insuranceAbsorbedFp).toBe(25_000n * FP_SCALE);
    expect(s3.residualUnbackedDeficitFp).toBe(0n);

    // Stage 4 Quantile ADL when deficit exceeds Insurance Fund
    const s4 = executeLiquidationWaterfall({
      accountEquityFp: -150_000n * FP_SCALE,
      maintenanceMarginFp: 15_000n * FP_SCALE,
      lockedOrderMarginFp: 0n,
      insuranceFundBalanceFp: 100_000n * FP_SCALE,
      profitableCounterpartyPoolFp: 500_000n * FP_SCALE,
    });
    expect(s4.stage).toBe("STAGE_4_QUANTILE_ADL");
    expect(s4.insuranceAbsorbedFp).toBe(100_000n * FP_SCALE);
    expect(s4.adlHaircutFp).toBe(50_000n * FP_SCALE);
    expect(s4.residualUnbackedDeficitFp).toBe(0n);
  });
});

describe("4. Client-Side ZK Merkle-Sum Solvency Verifier & Adversarial Soundness", () => {
  it("accepts a valid Merkle-Sum inclusion proof anchored to L2 SolvencyStateAnchor", () => {
    const clean = buildCanonicalSampleSolvencyBundle();
    const res = verifyMerkleSumSolvencyProof(clean);
    expect(res.valid).toBe(true);
    expect(res.failureReasons).toHaveLength(0);
    expect(res.solvencySurplusFp).toBeGreaterThan(0n);
  });

  it("rejects all 4 cryptographic soundness attacks deterministically", () => {
    const clean = buildCanonicalSampleSolvencyBundle();
    for (const attack of [
      "INFLATE_LEAF_1_SATOSHI",
      "NEGATIVE_GHOST_SIBLING",
      "FINITE_FIELD_WRAP_AROUND",
      "UNDERCOLLATERALIZED_VAULT",
    ] as const) {
      const tampered = injectAdversarialTamper(clean, attack);
      const res = verifyMerkleSumSolvencyProof(tampered);
      expect(res.valid).toBe(false);
      expect(res.failureReasons.length).toBeGreaterThan(0);
    }
  });
});

describe("5. DataFusion SQL AST Security Validator & Deflated Sharpe Ratio", () => {
  it("allows read-only SELECT + ASOF JOIN and blocks DDL/DML/Stacked/Cross-Join attacks", () => {
    const ok = executeLocalForensicSql(
      "SELECT e.symbol, markout_pnl(e.price_fp, b.mid_price_fp, 1, 1000) FROM lakehouse.executions e ASOF JOIN lakehouse.l2_snapshots b ON e.symbol = b.symbol LIMIT 10;"
    );
    expect(ok.validation.allowed).toBe(true);
    expect(ok.validation.hasAsofJoin).toBe(true);
    expect(ok.rows.length).toBeGreaterThan(0);

    expect(
      validateForensicSqlAst("SELECT 1; DROP TABLE lakehouse.executions;").violationCode
    ).toBe("MULTI_STATEMENT_INJECTION");
    expect(
      validateForensicSqlAst("DELETE FROM lakehouse.executions WHERE 1=1").violationCode
    ).toBe("NON_SELECT_ROOT");
    expect(
      validateForensicSqlAst("SELECT * FROM a CROSS JOIN b LIMIT 10").violationCode
    ).toBe("EXPLOSIVE_CROSS_JOIN");
    expect(
      validateForensicSqlAst("SELECT * FROM lakehouse.executions LIMIT 999999").violationCode
    ).toBe("UNBOUNDED_RESULT_LIMIT");
  });

  it("computes Bailey & López de Prado Deflated Sharpe Ratio strictly in (0, 1)", () => {
    const highAlpha = computeDeflatedSharpeRatio({
      observedSharpe: 3.5,
      trialVariance: 0.4,
      numTrials: 200,
      sampleLength: 1000,
      skewness: 0.1,
      kurtosis: 3.2,
    });
    const overfitNoise = computeDeflatedSharpeRatio({
      observedSharpe: 0.8,
      trialVariance: 0.9,
      numTrials: 5000,
      sampleLength: 120,
      skewness: -0.8,
      kurtosis: 6.5,
    });
    expect(highAlpha.dsrProbability).toBeGreaterThan(0.95);
    expect(overfitNoise.dsrProbability).toBeLessThan(0.05);
  });
});
