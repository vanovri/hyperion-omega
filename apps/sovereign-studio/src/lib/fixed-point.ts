/**
 * HYPERION OMEGA — Exact Fixed-Point Monetary Arithmetic (Scale = 1e8)
 *
 * ABSOLUTE INVARIANT (00-hyperion-contract.md §5.1):
 * Floating-point types (Number/float/double) are STRICTLY FORBIDDEN for prices,
 * quantities, notional values, fees, PnL, and ledger balances.
 */

export const FP_SCALE = 100_000_000n;
export const PPM_SCALE = 1_000_000n;
export const BPS_SCALE = 10_000n;
export const MAX_U64 = (1n << 64n) - 1n;
export const MAX_U128 = (1n << 128n) - 1n;
export const MAX_I128 = (1n << 127n) - 1n;
export const MIN_I128 = -(1n << 127n);

export interface AccountBalanceBucket {
  readonly accountId: bigint;
  readonly availableFp: bigint;
  readonly lockedFp: bigint;
}

export interface ConservationAuditResult {
  readonly holds: boolean;
  readonly lhsFp: bigint;
  readonly rhsFp: bigint;
  readonly deltaFp: bigint;
}

export function assertNonNegativeFp(valueFp: bigint, label: string): void {
  if (valueFp < 0n) {
    throw new RangeError(`${label} must be non-negative fixed-point integer, got ${valueFp.toString()}`);
  }
}

export function assertU64Bounds(value: bigint, label: string): void {
  if (value < 0n || value > MAX_U64) {
    throw new RangeError(`${label} out of uint64 bounds: ${value.toString()}`);
  }
}

/**
 * Canonical Quote Notional (00-hyperion-contract.md §5.1.2):
 * notional_fp = floor((u128(price_fp) * u128(qty_fp)) / 100_000_000)
 */
export function computeNotionalFp(priceFp: bigint, qtyFp: bigint): bigint {
  assertU64Bounds(priceFp, "priceFp");
  assertU64Bounds(qtyFp, "qtyFp");
  return (priceFp * qtyFp) / FP_SCALE;
}

/**
 * Canonical Fee / Maker Rebate (00-hyperion-contract.md §5.1.2):
 * fee_fp = round_toward_zero((i128(notional_fp) * i128(fee_ppm)) / 1_000_000)
 * Note: BigInt `/` in ECMAScript truncates toward zero for both positive and negative dividends!
 */
export function computeFeeFp(notionalFp: bigint, feePpm: bigint): bigint {
  assertNonNegativeFp(notionalFp, "notionalFp");
  if (notionalFp > MAX_U128) {
    throw new RangeError("notionalFp exceeds u128 bounds");
  }
  return (notionalFp * feePpm) / PPM_SCALE;
}

/**
 * Global Double-Entry Conservation Law (00-hyperion-contract.md §5.2):
 * Sum(Available_u + Locked_u) + TreasuryFeePool === TotalDeposits - TotalWithdrawals
 */
export function verifyDoubleEntryConservation(
  accounts: ReadonlyArray<AccountBalanceBucket>,
  treasuryFeePoolFp: bigint,
  totalDepositsFp: bigint,
  totalWithdrawalsFp: bigint
): ConservationAuditResult {
  assertNonNegativeFp(totalDepositsFp, "totalDepositsFp");
  assertNonNegativeFp(totalWithdrawalsFp, "totalWithdrawalsFp");

  let userSumFp = 0n;
  for (const acc of accounts) {
    assertNonNegativeFp(acc.availableFp, `account[${acc.accountId.toString()}].availableFp`);
    assertNonNegativeFp(acc.lockedFp, `account[${acc.accountId.toString()}].lockedFp`);
    userSumFp += acc.availableFp + acc.lockedFp;
  }

  const lhsFp = userSumFp + treasuryFeePoolFp;
  const rhsFp = totalDepositsFp - totalWithdrawalsFp;
  const deltaFp = lhsFp - rhsFp;

  return {
    holds: deltaFp === 0n,
    lhsFp,
    rhsFp,
    deltaFp,
  };
}

/**
 * Uncrossed Book Law (00-hyperion-contract.md §5.3):
 * BestBid < BestAsk whenever both sides are non-empty.
 */
export function verifyUncrossedBook(bestBidFp: bigint | null, bestAskFp: bigint | null): boolean {
  if (bestBidFp === null || bestAskFp === null || bestBidFp === 0n || bestAskFp === 0n) {
    return true;
  }
  return bestBidFp < bestAskFp;
}

/**
 * Formats a BigInt fixed-point value (scale 1e8) as an exact decimal string
 * with thousands separators and configurable fractional digits (0..8), never using IEEE-754 floats.
 */
export function formatFixedPoint(valueFp: bigint, decimals = 2, includeSign = false): string {
  if (decimals < 0 || decimals > 8) {
    throw new RangeError("decimals must be in [0, 8]");
  }
  const isNeg = valueFp < 0n;
  const absVal = isNeg ? -valueFp : valueFp;
  const whole = absVal / FP_SCALE;
  const fracRaw = absVal % FP_SCALE;

  const wholeStr = whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const signPrefix = isNeg ? "-" : includeSign && valueFp > 0n ? "+" : "";

  if (decimals === 0) {
    return `${signPrefix}${wholeStr}`;
  }
  const fracPadded = fracRaw.toString().padStart(8, "0").slice(0, decimals);
  return `${signPrefix}${wholeStr}.${fracPadded}`;
}

/**
 * Parses a human decimal string (e.g. "68425.50" or "-12.00000001") into exact 1e8 BigInt.
 */
export function parseFixedPointDecimal(input: string): bigint {
  const trimmed = input.trim().replace(/,/g, "");
  if (!/^[+-]?\d+(\.\d{1,8})?$/.test(trimmed)) {
    throw new SyntaxError(`Invalid fixed-point decimal string: "${input}"`);
  }
  const isNeg = trimmed.startsWith("-");
  const unsigned = trimmed.replace(/^[+-]/, "");
  const [wholePart = "0", fracPart = ""] = unsigned.split(".");
  const fracScaled = fracPart.padEnd(8, "0");
  const magnitude = BigInt(wholePart) * FP_SCALE + BigInt(fracScaled);
  return isNeg ? -magnitude : magnitude;
}
