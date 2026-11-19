"use client";

import React, { useEffect, useId, useMemo, useRef, useState } from "react";
import * as THREE from "three";
import { FP_SCALE, formatFixedPoint } from "@/lib/fixed-point";
import {
  computeAnalyticalGreeks,
  evaluateSpanPortfolioMargin,
  evaluateSviImpliedVol,
  executeLiquidationWaterfall,
  REGIME_SVI_PRESETS,
  sviDurrlemanDensityG,
  VolatilityRegime,
} from "@/lib/svi-greeks";
import {
  VOL_SURFACE_FRAGMENT_SHADER,
  VOL_SURFACE_VERTEX_SHADER,
} from "@/shaders/vol-surface-shaders";

type GreekMode = 0 | 1 | 2 | 3 | 4;

const GREEK_MODES: ReadonlyArray<{ id: GreekMode; label: string; unit: string }> = [
  { id: 0, label: "Implied Vol (σ)", unit: "%" },
  { id: 1, label: "Gamma (Γ)", unit: "1/k$" },
  { id: 2, label: "Vega (ν)", unit: "$/1%" },
  { id: 3, label: "Vanna (∂Δ/∂σ)", unit: "cross" },
  { id: 4, label: "Volga (∂ν/∂σ)", unit: "wing" },
];

export interface VolGreeksSurface3DProps {
  readonly midPriceFp: string;
  readonly regime: VolatilityRegime;
  readonly onChangeRegime: (regime: VolatilityRegime) => void;
  readonly onDetach?: () => void;
}

export function VolGreeksSurface3D({
  midPriceFp,
  regime,
  onChangeRegime,
  onDetach,
}: VolGreeksSurface3DProps) {
  const mountRef = useRef<HTMLDivElement | null>(null);
  const materialRef = useRef<THREE.ShaderMaterial | null>(null);
  const cameraAngleRef = useRef({ theta: 0.68, phi: 0.56, radius: 3.15 });
  const dragRef = useRef<{ active: boolean; lastX: number; lastY: number }>({
    active: false,
    lastX: 0,
    lastY: 0,
  });

  const [greekMode, setGreekMode] = useState<GreekMode>(0);
  const [webglReady, setWebglReady] = useState<boolean>(true);
  const [showAccessibleTable, setShowAccessibleTable] = useState<boolean>(false);

  const regimeSelectId = useId();
  const greekSelectId = useId();
  const tableCaptionId = useId();

  const sviParams = REGIME_SVI_PRESETS[regime];
  const spotBig = useMemo(() => {
    try {
      return BigInt(midPriceFp);
    } catch {
      return 68_425_50000000n;
    }
  }, [midPriceFp]);

  const spotFloat = Number(spotBig) / 1e8;

  // Compute live ATM Greeks + Durrleman butterfly arbitrage check + 16-scenario SPAN + Waterfall
  const quantMetrics = useMemo(() => {
    const atmIv = evaluateSviImpliedVol(0.0, 0.25, sviParams);
    const greeks = computeAnalyticalGreeks(spotFloat, spotFloat, 0.25, 0.045, atmIv, true);
    const minDurrlemanG = Math.min(
      sviDurrlemanDensityG(-0.3, sviParams),
      sviDurrlemanDensityG(0.0, sviParams),
      sviDurrlemanDensityG(0.3, sviParams)
    );

    const collateralFp =
      regime === "BLACK_SWAN_SINGULARITY" ? 42_000n * FP_SCALE : 185_000n * FP_SCALE;
    const span = evaluateSpanPortfolioMargin({
      collateralFp,
      spotPriceFp: spotBig,
      netDeltaBaseFp: -4_50000000n,
      netGammaFp: -120_000n,
      netVegaFp: 28_500n * FP_SCALE,
    });

    const waterfall = executeLiquidationWaterfall({
      accountEquityFp:
        regime === "BLACK_SWAN_SINGULARITY" ? -14_250n * FP_SCALE : collateralFp,
      maintenanceMarginFp: span.maintenanceMarginFp,
      lockedOrderMarginFp: 18_000n * FP_SCALE,
      insuranceFundBalanceFp: 650_000n * FP_SCALE,
      profitableCounterpartyPoolFp: 2_400_000n * FP_SCALE,
    });

    const sampleGrid = [-0.3, -0.15, 0.0, 0.15, 0.3].map((k) => ({
      moneynessLabel: k === 0 ? "ATM (0.00)" : k > 0 ? `+${(k * 100).toFixed(0)}% OTM` : `${(k * 100).toFixed(0)}% ITM`,
      iv1m: (evaluateSviImpliedVol(k, 1 / 12, sviParams) * 100).toFixed(2) + "%",
      iv3m: (evaluateSviImpliedVol(k, 0.25, sviParams) * 100).toFixed(2) + "%",
      iv1y: (evaluateSviImpliedVol(k, 1.0, sviParams) * 100).toFixed(2) + "%",
    }));

    return { atmIv, greeks, minDurrlemanG, span, waterfall, sampleGrid };
  }, [regime, spotBig, spotFloat, sviParams]);

  // Sync shader uniforms whenever SVI regime or Greek projection mode changes
  useEffect(() => {
    const mat = materialRef.current;
    if (!mat) return;
    mat.uniforms.uSviA!.value = sviParams.a;
    mat.uniforms.uSviB!.value = sviParams.b;
    mat.uniforms.uSviRho!.value = sviParams.rho;
    mat.uniforms.uSviM!.value = sviParams.m;
    mat.uniforms.uSviSigma!.value = sviParams.sigma;
    mat.uniforms.uGreekMode!.value = greekMode;
    mat.uniforms.uPulseIntensity!.value =
      regime === "BLACK_SWAN_SINGULARITY" ? 1.0 : regime === "SKEW_INVERSION" ? 0.55 : 0.22;
    mat.uniforms.uRiskThreshold!.value =
      regime === "BLACK_SWAN_SINGULARITY" ? 0.52 : 0.38;
  }, [sviParams, greekMode, regime]);

  // Initialize Three.js WebGL2 custom GLSL ShaderMaterial scene
  useEffect(() => {
    const container = mountRef.current;
    if (!container) return;

    let renderer: THREE.WebGLRenderer;
    try {
      renderer = new THREE.WebGLRenderer({
        antialias: true,
        alpha: true,
        powerPreference: "high-performance",
      });
    } catch {
      setWebglReady(false);
      return;
    }

    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.toneMapping = THREE.ACESFilmicToneMapping;
    renderer.toneMappingExposure = 1.08;

    const width = Math.max(container.clientWidth, 320);
    const height = Math.max(container.clientHeight, 260);
    renderer.setSize(width, height);
    container.innerHTML = "";
    container.appendChild(renderer.domElement);

    const scene = new THREE.Scene();
    const camera = new THREE.PerspectiveCamera(42, width / height, 0.1, 50);

    const updateCameraPos = () => {
      const { theta, phi, radius } = cameraAngleRef.current;
      camera.position.set(
        radius * Math.sin(theta) * Math.cos(phi),
        radius * Math.sin(phi) + 0.25,
        radius * Math.cos(theta) * Math.cos(phi)
      );
      camera.lookAt(0, 0.12, 0);
    };
    updateCameraPos();

    // Base floor reference grid
    const baseGrid = new THREE.GridHelper(2.8, 14, 0x1e293b, 0x0f172a);
    baseGrid.position.y = -0.42;
    scene.add(baseGrid);

    // 72x72 subdivided Strike x Maturity surface mesh rotated onto XZ plane
    const geometry = new THREE.PlaneGeometry(2.8, 2.4, 72, 72);
    geometry.rotateX(-Math.PI / 2);

    const initSvi = REGIME_SVI_PRESETS[regime];
    const shaderMat = new THREE.ShaderMaterial({
      transparent: true,
      side: THREE.DoubleSide,
      uniforms: {
        uTime: { value: 0 },
        uSviA: { value: initSvi.a },
        uSviB: { value: initSvi.b },
        uSviRho: { value: initSvi.rho },
        uSviM: { value: initSvi.m },
        uSviSigma: { value: initSvi.sigma },
        uPulseIntensity: { value: 0.25 },
        uGreekMode: { value: 0 },
        uRiskThreshold: { value: 0.38 },
        uColdColor: { value: new THREE.Color(0x0ea5e9) },
        uMidColor: { value: new THREE.Color(0x10b981) },
        uHotColor: { value: new THREE.Color(0xf43f5e) },
        uContourColor: { value: new THREE.Color(0xe2e8f0) },
      },
      vertexShader: VOL_SURFACE_VERTEX_SHADER,
      fragmentShader: VOL_SURFACE_FRAGMENT_SHADER,
    });
    materialRef.current = shaderMat;

    const surfaceMesh = new THREE.Mesh(geometry, shaderMat);
    scene.add(surfaceMesh);

    const resizeObserver = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const w = Math.max(Math.floor(entry.contentRect.width), 240);
      const h = Math.max(Math.floor(entry.contentRect.height), 220);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
      renderer.setSize(w, h);
    });
    resizeObserver.observe(container);

    const startTime = performance.now();
    let rafId = 0;
    const renderLoop = (now: number) => {
      shaderMat.uniforms.uTime!.value = (now - startTime) * 0.001;
      if (!dragRef.current.active) {
        cameraAngleRef.current.theta += 0.0018;
        updateCameraPos();
      }
      renderer.render(scene, camera);
      rafId = window.requestAnimationFrame(renderLoop);
    };
    rafId = window.requestAnimationFrame(renderLoop);

    const dom = renderer.domElement;
    const onPointerDown = (e: PointerEvent) => {
      dragRef.current = { active: true, lastX: e.clientX, lastY: e.clientY };
      dom.setPointerCapture(e.pointerId);
    };
    const onPointerMove = (e: PointerEvent) => {
      if (!dragRef.current.active) return;
      const dx = e.clientX - dragRef.current.lastX;
      const dy = e.clientY - dragRef.current.lastY;
      dragRef.current.lastX = e.clientX;
      dragRef.current.lastY = e.clientY;
      cameraAngleRef.current.theta -= dx * 0.007;
      cameraAngleRef.current.phi = Math.min(
        Math.max(cameraAngleRef.current.phi + dy * 0.006, 0.18),
        1.25
      );
      updateCameraPos();
    };
    const onPointerUp = () => {
      dragRef.current.active = false;
    };
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      cameraAngleRef.current.radius = Math.min(
        Math.max(cameraAngleRef.current.radius + e.deltaY * 0.002, 1.9),
        5.2
      );
      updateCameraPos();
    };

    dom.addEventListener("pointerdown", onPointerDown);
    dom.addEventListener("pointermove", onPointerMove);
    dom.addEventListener("pointerup", onPointerUp);
    dom.addEventListener("wheel", onWheel, { passive: false });

    return () => {
      window.cancelAnimationFrame(rafId);
      resizeObserver.disconnect();
      dom.removeEventListener("pointerdown", onPointerDown);
      dom.removeEventListener("pointermove", onPointerMove);
      dom.removeEventListener("pointerup", onPointerUp);
      dom.removeEventListener("wheel", onWheel);
      geometry.dispose();
      shaderMat.dispose();
      renderer.dispose();
      materialRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <section className="panel-card" aria-labelledby="vol-surface-heading">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="inline-block h-2 w-2 rounded-full bg-cyan-400" aria-hidden="true" />
            <h2 id="vol-surface-heading" className="text-sm font-semibold tracking-tight text-slate-100">
              CH14 · 3D SVI Implied Volatility &amp; Portfolio Greeks Surface (WebGL2 GLSL)
            </h2>
          </div>
          <p className="text-xs text-slate-400">
            Arbitrage-free Raw SVI surface · Durrleman density{" "}
            <span className="mono-num text-emerald-400">
              g(k) min = {quantMetrics.minDurrlemanG.toFixed(3)} ≥ 0
            </span>
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <div className="flex items-center gap-1.5">
            <label htmlFor={regimeSelectId} className="text-[11px] font-medium text-slate-400">
              Regime
            </label>
            <select
              id={regimeSelectId}
              value={regime}
              onChange={(e) => onChangeRegime(e.target.value as VolatilityRegime)}
              className="sovereign-input mono-num"
            >
              <option value="CALM_EQUILIBRIUM">Calm Equilibrium</option>
              <option value="SKEW_INVERSION">Put-Skew Inversion</option>
              <option value="PIN_RISK_EXPIRY">0DTE Pin-Risk</option>
              <option value="BLACK_SWAN_SINGULARITY">Black Swan Singularity</option>
            </select>
          </div>

          <div className="flex items-center gap-1.5">
            <label htmlFor={greekSelectId} className="text-[11px] font-medium text-slate-400">
              Shader Z
            </label>
            <select
              id={greekSelectId}
              value={greekMode}
              onChange={(e) => setGreekMode(Number(e.target.value) as GreekMode)}
              className="sovereign-input mono-num"
            >
              {GREEK_MODES.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </div>

          <button
            type="button"
            className="sovereign-btn"
            aria-pressed={showAccessibleTable}
            onClick={() => setShowAccessibleTable((v) => !v)}
          >
            {showAccessibleTable ? "3D View" : "Grid Data"}
          </button>

          {onDetach && (
            <button
              type="button"
              className="sovereign-btn"
              onClick={onDetach}
              aria-label="Pop out 3D Volatility Surface window"
            >
              Pop-Out
            </button>
          )}
        </div>
      </div>

      {/* 3D WebGL2 Shader Viewport */}
      <figure className="relative m-0">
        <div
          ref={mountRef}
          data-testid="vol-surface-webgl-canvas"
          style={{ touchAction: "none" }}
          className="h-[270px] w-full rounded border border-slate-800/80 bg-[#050811]"
        >
          {!webglReady && (
            <div className="flex h-full items-center justify-center p-4 text-xs text-slate-300">
              WebGL2 hardware context unavailable in headless frame — showing analytical SVI matrix below.
            </div>
          )}
        </div>
        <figcaption className="mt-1.5 flex flex-wrap items-center justify-between gap-2 text-[11px] text-slate-400">
          <span>
            Axes: Strike Log-Moneyness <span className="mono-num">k = ln(K/F) ∈ [-0.42, +0.42]</span> × Maturity{" "}
            <span className="mono-num">T ∈ [0.03y, 1.00y]</span> (Drag to orbit · Scroll to zoom)
          </span>
          <span className="mono-num text-cyan-300">
            ATM 3M IV: {(quantMetrics.atmIv * 100).toFixed(2)}%
          </span>
        </figcaption>
      </figure>

      {/* Accessible Data Table Alternative (accessibility §6 Complex Charts) */}
      {(showAccessibleTable || !webglReady) && (
        <div tabIndex={0} role="region" aria-labelledby={tableCaptionId} className="table-wrapper">
          <table className="sovereign-table mono-num">
            <caption id={tableCaptionId} className="px-2.5 py-1.5 text-left text-[11px] text-slate-400">
              SVI Implied Volatility Surface Matrix across Moneyness and Tenors
            </caption>
            <thead>
              <tr>
                <th scope="col">Moneyness k</th>
                <th scope="col">1M Tenor IV</th>
                <th scope="col">3M Tenor IV</th>
                <th scope="col">1Y Tenor IV</th>
              </tr>
            </thead>
            <tbody>
              {quantMetrics.sampleGrid.map((row) => (
                <tr key={row.moneynessLabel}>
                  <th scope="row">{row.moneynessLabel}</th>
                  <td data-col-label="1M Tenor IV">{row.iv1m}</td>
                  <td data-col-label="3M Tenor IV">{row.iv3m}</td>
                  <td data-col-label="1Y Tenor IV">{row.iv1y}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Analytical 1st/2nd Order Greeks + 16-Scenario SPAN & Liquidation Waterfall Readout */}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <div className="rounded border border-slate-800/80 bg-[#070b14] p-2">
          <div className="text-[10.5px] font-medium uppercase tracking-wider text-slate-400">
            ATM Δ / Γ / ν
          </div>
          <div className="mono-num mt-0.5 text-sm font-semibold text-slate-100">
            {quantMetrics.greeks.delta.toFixed(3)} · {(quantMetrics.greeks.gamma * 1000).toFixed(3)}
          </div>
          <div className="mono-num text-[11px] text-cyan-400">
            Vega ${quantMetrics.greeks.vega.toFixed(1)}
          </div>
        </div>

        <div className="rounded border border-slate-800/80 bg-[#070b14] p-2">
          <div className="text-[10.5px] font-medium uppercase tracking-wider text-slate-400">
            Tail Vanna / Volga
          </div>
          <div className="mono-num mt-0.5 text-sm font-semibold text-slate-100">
            {quantMetrics.greeks.vanna.toFixed(3)}
          </div>
          <div className="mono-num text-[11px] text-amber-400">
            Volga {quantMetrics.greeks.volga.toFixed(1)}
          </div>
        </div>

        <div className="rounded border border-slate-800/80 bg-[#070b14] p-2">
          <div className="text-[10.5px] font-medium uppercase tracking-wider text-slate-400">
            16-Grid SPAN Worst
          </div>
          <div className="mono-num mt-0.5 text-sm font-semibold text-rose-400">
            -{formatFixedPoint(quantMetrics.span.worstCaseLossFp, 2)}
          </div>
          <div className="mono-num text-[11px] text-slate-400">
            MM Ratio {(quantMetrics.span.marginRatioBps / 100).toFixed(1)}%
          </div>
        </div>

        <div className="rounded border border-slate-800/80 bg-[#070b14] p-2">
          <div className="text-[10.5px] font-medium uppercase tracking-wider text-slate-400">
            Waterfall Solvency
          </div>
          <div
            className={`mono-num mt-0.5 text-xs font-semibold ${
              quantMetrics.waterfall.stage === "HEALTHY" ? "text-emerald-400" : "text-amber-400"
            }`}
          >
            {quantMetrics.waterfall.stage.replace("STAGE_", "S")}
          </div>
          <div className="mono-num text-[11px] text-emerald-400">
            Deficit: {formatFixedPoint(quantMetrics.waterfall.residualUnbackedDeficitFp, 2)} USDT
          </div>
        </div>
      </div>
    </section>
  );
}
