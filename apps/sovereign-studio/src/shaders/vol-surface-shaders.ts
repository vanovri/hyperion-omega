/**
 * HYPERION OMEGA Channel 20 — Custom WebGL2 / GLSL ES 3.00 Shaders
 * 3D Arbitrage-Free SVI Implied Volatility & Portfolio Greeks Surface
 *
 * Follows three.js r172+/r186 ShaderMaterial conventions:
 * - glslVersion left unset so three.js injects #version 300 es + gl_FragColor alias
 * - View-space lighting & Fresnel rim (normalMatrix * analyticNormal paired with -mvPosition.xyz)
 * - Screen-space derivative anti-aliased iso-contours via fwidth() (dFdx/dFdy)
 * - Ends fragment shader with #include <tonemapping_fragment> and #include <colorspace_fragment>
 */

export const VOL_SURFACE_VERTEX_SHADER = /* glsl */ `
  uniform float uTime;
  uniform float uSviA;
  uniform float uSviB;
  uniform float uSviRho;
  uniform float uSviM;
  uniform float uSviSigma;
  uniform float uPulseIntensity;
  uniform int uGreekMode; // 0 = Implied Vol, 1 = Gamma, 2 = Vega, 3 = Vanna, 4 = Volga

  varying vec2 vGridUv;
  varying float vMoneyness;
  varying float vMaturity;
  varying float vImpliedVol;
  varying float vMetricValue;
  varying vec3 vViewNormal;
  varying vec3 vViewDir;

  float evalImpliedVol(float k, float tenor) {
    float T = max(tenor, 0.02);
    float dk = k - uSviM;
    float root = sqrt(dk * dk + uSviSigma * uSviSigma);
    float wBase = max(uSviA + uSviB * (uSviRho * dk + root), 0.0001);
    float totalVar = wBase * pow(T, 0.92) + 0.004 * T;
    float regimeWave = uPulseIntensity * 0.038 * sin(k * 9.5 - uTime * 2.8) * exp(-1.6 * T);
    return clamp(sqrt(totalVar / T) + regimeWave, 0.05, 1.85);
  }

  float evalGreekMetric(float k, float tenor, float iv) {
    float T = max(tenor, 0.025);
    float sqrtT = sqrt(T);
    float sigSqrtT = max(iv * sqrtT, 0.005);
    float d1 = (-k + 0.5 * iv * iv * T) / sigSqrtT;
    float d2 = d1 - sigSqrtT;
    float pdf = 0.39894228 * exp(-0.5 * d1 * d1);

    if (uGreekMode == 1) {
      // Normalized Portfolio Gamma density (peaks sharply at-the-money & short expiry pin-risk)
      return clamp((pdf / sigSqrtT) * 0.18, 0.0, 1.6);
    } else if (uGreekMode == 2) {
      // Normalized Vega surface
      return clamp(pdf * sqrtT * 2.8, 0.0, 1.6);
    } else if (uGreekMode == 3) {
      // Normalized Vanna |dDelta/dVol| = |-pdf * d2 / iv|
      return clamp(abs((-pdf * d2) / max(iv, 0.05)) * 0.35, 0.0, 1.6);
    } else if (uGreekMode == 4) {
      // Normalized Volga |vega * d1 * d2 / iv| (wing convexity tail-shock exposure)
      return clamp(abs((pdf * sqrtT * d1 * d2) / max(iv, 0.05)) * 0.55, 0.0, 1.6);
    }
    return iv;
  }

  void main() {
    vGridUv = uv;
    // Map plane x in [-1.4, 1.4] -> log-moneyness k in [-0.42, +0.42]
    // Map plane z in [-1.4, 1.4] -> maturity T in [0.03, 1.00] years
    float k = (uv.x - 0.5) * 0.84;
    float tenor = mix(0.03, 1.00, uv.y);

    float iv = evalImpliedVol(k, tenor);
    float metric = evalGreekMetric(k, tenor, iv);

    // Finite-difference GPU tangent vectors for crisp analytical surface lighting
    float eps = 0.008;
    float ivKx = evalImpliedVol(k + eps, tenor);
    float ivTz = evalImpliedVol(k, tenor + eps);
    float hCenter = metric * 1.15;
    float hDx = evalGreekMetric(k + eps, tenor, ivKx) * 1.15;
    float hDz = evalGreekMetric(k, tenor + eps, ivTz) * 1.15;

    vec3 displaced = vec3(position.x, hCenter - 0.38, position.z);
    vec3 tangentX = normalize(vec3(eps * 3.33, hDx - hCenter, 0.0));
    vec3 tangentZ = normalize(vec3(0.0, hDz - hCenter, eps * 2.88));
    vec3 localNormal = normalize(cross(tangentZ, tangentX));

    vec4 mvPosition = modelViewMatrix * vec4(displaced, 1.0);
    vViewNormal = normalize(normalMatrix * localNormal);
    vViewDir = -mvPosition.xyz;
    vMoneyness = k;
    vMaturity = tenor;
    vImpliedVol = iv;
    vMetricValue = metric;

    gl_Position = projectionMatrix * mvPosition;
  }
`;

export const VOL_SURFACE_FRAGMENT_SHADER = /* glsl */ `
  uniform float uTime;
  uniform float uRiskThreshold;
  uniform float uPulseIntensity;
  uniform vec3 uColdColor;
  uniform vec3 uMidColor;
  uniform vec3 uHotColor;
  uniform vec3 uContourColor;

  varying vec2 vGridUv;
  varying float vMoneyness;
  varying float vMaturity;
  varying float vImpliedVol;
  varying float vMetricValue;
  varying vec3 vViewNormal;
  varying vec3 vViewDir;

  void main() {
    float t = clamp((vMetricValue - 0.14) / 0.86, 0.0, 1.0);
    vec3 basePalette = t < 0.52
      ? mix(uColdColor, uMidColor, t / 0.52)
      : mix(uMidColor, uHotColor, (t - 0.52) / 0.48);

    // Anti-aliased iso-volatility / iso-risk contours via screen-space derivatives
    float isoScaled = vMetricValue * 14.0;
    float isoDist = abs(fract(isoScaled - 0.5) - 0.5) / max(fwidth(isoScaled), 0.0001);
    float isoLine = 1.0 - smoothstep(0.0, 1.35, isoDist);

    // Strike x Tenor subtle coordinate wireframe grid
    vec2 gridCoord = vGridUv * vec2(16.0, 12.0);
    vec2 gridDist = abs(fract(gridCoord - 0.5) - 0.5) / max(fwidth(gridCoord), vec2(0.0001));
    float wireLine = 1.0 - smoothstep(0.0, 1.15, min(gridDist.x, gridDist.y));

    // Portfolio Critical Risk Threshold Contour Highlight (pulsing crimson-amber band)
    float critDist = abs(vImpliedVol - uRiskThreshold) / max(fwidth(vImpliedVol), 0.002);
    float critContour = (1.0 - smoothstep(0.0, 2.2, critDist)) * (0.65 + 0.35 * sin(uTime * 5.0));

    // View-space directional + Fresnel rim lighting
    vec3 n = normalize(vViewNormal);
    vec3 v = normalize(vViewDir);
    vec3 lightDir = normalize(vec3(0.45, 0.85, 0.55));
    float diff = max(dot(n, lightDir), 0.0) * 0.55 + 0.45;
    float rim = pow(1.0 - max(dot(v, n), 0.0), 3.2);

    vec3 litColor = basePalette * diff;
    litColor = mix(litColor, uContourColor, isoLine * 0.42);
    litColor += vec3(0.09, 0.22, 0.32) * wireLine * 0.28;
    litColor = mix(litColor, vec3(1.0, 0.28, 0.38), critContour * (0.55 + 0.45 * uPulseIntensity));
    litColor += vec3(0.14, 0.72, 0.92) * rim * 0.45;

    gl_FragColor = vec4(litColor, 0.96);
    #include <tonemapping_fragment>
    #include <colorspace_fragment>
  }
`;
