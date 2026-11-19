/**
 * HYPERION Fixed-Point Arithmetic & Formatting Module (Scale = 1e8).
 *
 * ABSOLUTE INVARIANT (.agents/rules/00-hyperion-contract.md §5.1):
 * Floating-point numbers are strictly forbidden for storing or computing monetary
 * prices, asset quantities, notional amounts, fees, or account balances.
 * Screen-space pixel ratio projection is permitted ONLY at the final Canvas/WebGL
 * rasterization boundary.
 */

export const FP_SCALE = 100_000_000n;
export const PPM_SCALE = 1_000_000n;
export const BPS_SCALE = 10_000n;

/** Parse decimal string or integer into exact 1e8 fixed-point bigint. */
export function parseDecimalToFp(input: string): bigint {
  const trimmed = input.trim();
  if (!trimmed) {
    throw new RangeError("Empty fixed-point input");
  }
  const negative = trimmed.startsWith("-");
  const unsigned = negative || trimmed.startsWith("+") ? trimmed.slice(1) : trimmed;
  const parts = unsigned.split(".");
  if (parts.length > 2) {
    throw new SyntaxError(`Malformed decimal fixed-point string: ${input}`);
  }
  const wholePart = parts[0] || "0";
  const fracPartRaw = parts[1] ?? "";
  if (!/^\d+$/.test(wholePart) || (fracPartRaw.length > 0 && !/^\d+$/.test(fracPartRaw))) {
    throw new SyntaxError(`Invalid digits in fixed-point string: ${input}`);
  }
  const fracPadded = fracPartRaw.slice(0, 8).padEnd(8, "0");
  const combined = BigInt(wholePart) * FP_SCALE + BigInt(fracPadded);
  return negative ? -combined : combined;
}

/** Parse stringified 1e8 integer from OpenAPI DTO (e.g. "6842550000000"). */
export function parseRawFpString(raw: string | bigint | number): bigint {
  if (typeof raw === "bigint") return raw;
  if (typeof raw === "number") {
    if (!Number.isSafeInteger(raw)) {
      throw new RangeError(`Unsafe integer passed to parseRawFpString: ${raw}`);
    }
    return BigInt(raw);
  }
  return BigInt(raw.trim());
}

/** Exact quote notional = floor((priceFp * qtyFp) / 1e8). */
export function checkedNotionalFp(priceFp: bigint, qtyFp: bigint): bigint {
  if (priceFp < 0n || qtyFp < 0n) {
    throw new RangeError(`Negative price or qty in checkedNotionalFp: ${priceFp}, ${qtyFp}`);
  }
  return (priceFp * qtyFp) / FP_SCALE;
}

/** Exact fee in 1e8 units from notionalFp and signed feePpm (round toward zero). */
export function checkedFeeFp(notionalFp: bigint, feePpm: bigint): bigint {
  if (notionalFp < 0n) {
    throw new RangeError(`Negative notional in checkedFeeFp: ${notionalFp}`);
  }
  return (notionalFp * feePpm) / PPM_SCALE;
}

/** Compute spread in integer basis points = ((ask - bid) * 10_000) / mid. */
export function computeSpreadBps(bestBidFp: bigint, bestAskFp: bigint): number {
  if (bestBidFp <= 0n || bestAskFp <= bestBidFp) return 0;
  const midFp = (bestBidFp + bestAskFp) / 2n;
  if (midFp <= 0n) return 0;
  return Number(((bestAskFp - bestBidFp) * BPS_SCALE) / midFp);
}

/** Compute Order Book Imbalance in signed parts-per-million [-1_000_000, +1_000_000]. */
export function computeImbalancePpm(bidVolFp: bigint, askVolFp: bigint): bigint {
  const denom = bidVolFp + askVolFp;
  if (denom <= 0n) return 0n;
  return ((bidVolFp - askVolFp) * PPM_SCALE) / denom;
}

function insertThousandsSeparators(digits: string): string {
  if (digits.length <= 3) return digits;
  let out = "";
  let count = 0;
  for (let i = digits.length - 1; i >= 0; i--) {
    out = digits[i] + out;
    count++;
    if (count === 3 && i > 0) {
      out = "," + out;
      count = 0;
    }
  }
  return out;
}

/**
 * Pure bigint formatter for 1e8 fixed-point values — zero IEEE-754 float conversion.
 */
export function formatFp(
  valueFp: bigint,
  decimals = 2,
  options?: { signed?: boolean; grouping?: boolean }
): string {
  const clampedDecimals = Math.max(0, Math.min(8, decimals));
  const isNeg = valueFp < 0n;
  const absVal = isNeg ? -valueFp : valueFp;

  const divisor = 10n ** BigInt(8 - clampedDecimals);
  const rounded = (absVal + divisor / 2n) / divisor;
  const scaleOut = 10n ** BigInt(clampedDecimals);

  const whole = rounded / scaleOut;
  const frac = rounded % scaleOut;

  const wholeStr =
    options?.grouping === false ? whole.toString() : insertThousandsSeparators(whole.toString());
  const signPrefix = isNeg ? "-" : options?.signed && valueFp > 0n ? "+" : "";

  if (clampedDecimals === 0) {
    return `${signPrefix}${wholeStr}`;
  }
  return `${signPrefix}${wholeStr}.${frac.toString().padStart(clampedDecimals, "0")}`;
}

/** Compact institutional notional formatter (K / M / B) using pure integer math. */
export function formatCompactNotionalFp(valueFp: bigint): string {
  const isNeg = valueFp < 0n;
  const absUnits = (isNeg ? -valueFp : valueFp) / (FP_SCALE / 100n); // centi-units
  const sign = isNeg ? "-" : "";

  if (absUnits >= 100_000_000_000n) {
    const b = absUnits / 1_000_000_000n;
    return `${sign}${(b / 100n).toString()}.${(b % 100n).toString().padStart(2, "0")}B`;
  }
  if (absUnits >= 100_000_000n) {
    const m = absUnits / 1_000_000n;
    return `${sign}${(m / 100n).toString()}.${(m % 100n).toString().padStart(2, "0")}M`;
  }
  if (absUnits >= 100_000n) {
    const k = absUnits / 1_000n;
    return `${sign}${(k / 100n).toString()}.${(k % 100n).toString().padStart(2, "0")}K`;
  }
  return formatFp(valueFp, 2);
}

/** Format parts-per-million value as percentage string (e.g. 984_000 -> "98.40%"). */
export function formatPpmPercent(ppm: number | bigint, decimals = 2): string {
  const val = typeof ppm === "bigint" ? Number(ppm) : ppm;
  return `${(val / 10_000).toFixed(decimals)}%`;
}

/**
 * Project a 1e8 fixed-point value into normalized [0, 1] screen space at the
 * Canvas/WebGL rasterization boundary without overflowing IEEE-754 precision.
 */
export function projectFpRatio(valueFp: bigint, minFp: bigint, maxFp: bigint): number {
  const span = maxFp - minFp;
  if (span <= 0n) return 0.5;
  const scaled = ((valueFp - minFp) * 1_000_000n) / span;
  const clamped = scaled < 0n ? 0n : scaled > 1_000_000n ? 1_000_000n : scaled;
  return Number(clamped) / 1_000_000;
}
