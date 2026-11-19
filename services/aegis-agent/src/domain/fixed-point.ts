/**
 * HYPERION Channel 06 — Pure Fixed-Point Arithmetic Engine (Scale 1e8).
 *
 * ABSOLUTE INVARIANT (00-hyperion-contract.md §5.1):
 * Floating-point math (Number, parseFloat, Math.round, /) is STRICTLY FORBIDDEN
 * for prices, asset quantities, notional amounts, fees, PnL, and ledger balances.
 * Every monetary and quantity value is represented as an exact `bigint` scaled by 10^8.
 */

export const FP_SCALE = 100_000_000n;
export const PPM_SCALE = 1_000_000n;
export const BPS_SCALE = 10_000n;

const UINT_FP_REGEX = /^[0-9]{1,39}$/;
const INT_FP_REGEX = /^-?[0-9]{1,39}$/;
const DECIMAL_INPUT_REGEX = /^(-?)([0-9]+)(?:\.([0-9]{1,8}))?$/;

const TWO_POW_64 = 1n << 64n;
const MASK_64 = TWO_POW_64 - 1n;
const MIN_I128 = -(1n << 127n);
const MAX_I128 = (1n << 127n) - 1n;
const MAX_U128 = (1n << 128n) - 1n;

export interface ProtoU128Like {
  hi: bigint;
  lo: bigint;
}

export interface ProtoI128Like {
  hi: bigint;
  lo: bigint;
}

export function parseFixedPointUint(raw: string, fieldName = "value"): bigint {
  if (!UINT_FP_REGEX.test(raw)) {
    throw new RangeError(`Invalid unsigned fixed-point integer string for ${fieldName}: "${raw}"`);
  }
  const value = BigInt(raw);
  if (value > MAX_U128) {
    throw new RangeError(`Unsigned fixed-point value exceeds u128 bounds for ${fieldName}: "${raw}"`);
  }
  return value;
}

export function parseFixedPointInt(raw: string, fieldName = "value"): bigint {
  if (!INT_FP_REGEX.test(raw)) {
    throw new RangeError(`Invalid signed fixed-point integer string for ${fieldName}: "${raw}"`);
  }
  const value = BigInt(raw);
  if (value < MIN_I128 || value > MAX_I128) {
    throw new RangeError(`Signed fixed-point value exceeds i128 bounds for ${fieldName}: "${raw}"`);
  }
  return value;
}

/**
 * Parses a human decimal string (e.g. "68425.50" or "-0.0025") into exact 1e8 atomic units
 * using pure string padding and BigInt arithmetic (zero IEEE-754 floats).
 */
export function decimalToFixedPoint1e8(decimalStr: string): bigint {
  const trimmed = decimalStr.trim();
  const match = DECIMAL_INPUT_REGEX.exec(trimmed);
  if (!match) {
    throw new RangeError(`Invalid fixed-point decimal literal (max 8 fractional digits): "${decimalStr}"`);
  }
  const sign = match[1] === "-" ? -1n : 1n;
  const wholePart = BigInt(match[2] ?? "0");
  const fracRaw = (match[3] ?? "").padEnd(8, "0");
  const fracPart = BigInt(fracRaw);
  const combined = sign * (wholePart * FP_SCALE + fracPart);
  if (combined < MIN_I128 || combined > MAX_I128) {
    throw new RangeError(`Decimal literal overflows i128 fixed-point range: "${decimalStr}"`);
  }
  return combined;
}

/**
 * Formats an exact 1e8 `bigint` into a human-readable decimal string (e.g. "68425.50000000")
 * using pure BigInt division and remainder (zero floating-point operations).
 */
export function formatFixedPoint1e8(valueFp: bigint, trimTrailingZeros = false): string {
  const negative = valueFp < 0n;
  const absVal = negative ? -valueFp : valueFp;
  const whole = absVal / FP_SCALE;
  const frac = (absVal % FP_SCALE).toString(10).padStart(8, "0");
  const cleanFrac = trimTrailingZeros ? frac.replace(/0+$/, "") : frac;
  const signPrefix = negative ? "-" : "";
  if (cleanFrac.length === 0) {
    return `${signPrefix}${whole.toString(10)}.00`;
  }
  if (trimTrailingZeros && cleanFrac.length < 2) {
    return `${signPrefix}${whole.toString(10)}.${cleanFrac.padEnd(2, "0")}`;
  }
  return `${signPrefix}${whole.toString(10)}.${cleanFrac}`;
}

/**
 * Formats integer parts-per-million (`1_000_000` = `100.0000%`) as exact percentage text
 * using pure integer division/modulo.
 */
export function formatPpmAsPercent(ppm: number | bigint): string {
  const val = typeof ppm === "bigint" ? ppm : BigInt(Math.trunc(ppm));
  const negative = val < 0n;
  const abs = negative ? -val : val;
  const pctWhole = abs / 10_000n;
  const pctFrac = (abs % 10_000n).toString(10).padStart(4, "0").slice(0, 2);
  return `${negative ? "-" : ""}${pctWhole.toString(10)}.${pctFrac}%`;
}

/**
 * Formats integer basis points (`10_000` = `100.00%`) using pure integer math.
 */
export function formatBpsAsPercent(bps: number | bigint): string {
  const val = typeof bps === "bigint" ? bps : BigInt(Math.trunc(bps));
  const negative = val < 0n;
  const abs = negative ? -val : val;
  const whole = abs / 100n;
  const frac = (abs % 100n).toString(10).padStart(2, "0");
  return `${negative ? "-" : ""}${whole.toString(10)}.${frac}%`;
}

/**
 * Canonical notional formula (00-hyperion-contract.md §5.1):
 * notional_fp = floor((price_fp * qty_fp) / 100_000_000)
 */
export function checkedNotionalFp(priceFp: bigint, qtyFp: bigint): bigint {
  if (priceFp < 0n || qtyFp < 0n) {
    throw new RangeError("priceFp and qtyFp must be non-negative fixed-point integers");
  }
  const notional = (priceFp * qtyFp) / FP_SCALE;
  if (notional > MAX_U128) {
    throw new RangeError("Computed notional_fp overflows u128");
  }
  return notional;
}

/**
 * Canonical signed fee formula (00-hyperion-contract.md §5.1):
 * fee_fp = trunc_toward_zero((notional_fp * fee_ppm) / 1_000_000)
 */
export function checkedFeeFp(notionalFp: bigint, feePpm: bigint): bigint {
  if (notionalFp < 0n) {
    throw new RangeError("notionalFp must be non-negative");
  }
  const fee = (notionalFp * feePpm) / PPM_SCALE;
  if (fee < MIN_I128 || fee > MAX_I128) {
    throw new RangeError("Computed fee_fp overflows i128");
  }
  return fee;
}

export function checkedAddI128(a: bigint, b: bigint): bigint {
  const sum = a + b;
  if (sum < MIN_I128 || sum > MAX_I128) {
    throw new RangeError("i128 addition overflow/underflow");
  }
  return sum;
}

export function checkedSubI128(a: bigint, b: bigint): bigint {
  const diff = a - b;
  if (diff < MIN_I128 || diff > MAX_I128) {
    throw new RangeError("i128 subtraction overflow/underflow");
  }
  return diff;
}

export function toProtoU128(value: bigint): ProtoU128Like {
  if (value < 0n || value > MAX_U128) {
    throw new RangeError(`u128 value out of range: ${value.toString(10)}`);
  }
  return {
    hi: (value >> 64n) & MASK_64,
    lo: value & MASK_64
  };
}

export function fromProtoU128(msg: ProtoU128Like | undefined): bigint {
  if (!msg) {
    return 0n;
  }
  return ((msg.hi & MASK_64) << 64n) | (msg.lo & MASK_64);
}

export function toProtoI128(value: bigint): ProtoI128Like {
  if (value < MIN_I128 || value > MAX_I128) {
    throw new RangeError(`i128 value out of range: ${value.toString(10)}`);
  }
  return {
    hi: BigInt.asIntN(64, value >> 64n),
    lo: value & MASK_64
  };
}

export function fromProtoI128(msg: ProtoI128Like | undefined): bigint {
  if (!msg) {
    return 0n;
  }
  const hiSigned = BigInt.asIntN(64, msg.hi);
  return (hiSigned << 64n) + (msg.lo & MASK_64);
}
