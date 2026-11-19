/**
 * Monotonic Nanosecond Clock Abstraction for Deterministic SRE Timelines & Auto-Rollbacks.
 */

export interface NanosecondClock {
  nowNs(): bigint;
}

/**
 * Production monotonic wall-clock anchored at process boot epoch + process.hrtime.bigint().
 * Guarantees strictly monotonically increasing nanosecond timestamps (`t_{k+1} > t_k`).
 */
export class SystemNanosecondClock implements NanosecondClock {
  private readonly baseEpochNs: bigint;
  private readonly baseHrtimeNs: bigint;
  private lastIssuedNs = 0n;

  public constructor() {
    this.baseEpochNs = BigInt(Date.now()) * 1_000_000n;
    this.baseHrtimeNs = process.hrtime.bigint();
  }

  public nowNs(): bigint {
    const elapsedNs = process.hrtime.bigint() - this.baseHrtimeNs;
    const candidate = this.baseEpochNs + elapsedNs;
    if (candidate <= this.lastIssuedNs) {
      this.lastIssuedNs += 1n;
      return this.lastIssuedNs;
    }
    this.lastIssuedNs = candidate;
    return candidate;
  }
}

/**
 * Deterministic controllable nanosecond clock for CI/offline state-graph runs and Vitest suites.
 */
export class DeterministicNanosecondClock implements NanosecondClock {
  private currentNs: bigint;
  private readonly autoStepNs: bigint;

  public constructor(initialNs = 1_795_013_580_000_000_000n, autoStepNs = 1_000n) {
    this.currentNs = initialNs;
    this.autoStepNs = autoStepNs;
  }

  public nowNs(): bigint {
    const out = this.currentNs;
    this.currentNs += this.autoStepNs;
    return out;
  }

  public peekNs(): bigint {
    return this.currentNs;
  }

  public advanceMs(ms: number | bigint): bigint {
    const deltaNs = (typeof ms === "bigint" ? ms : BigInt(Math.trunc(ms))) * 1_000_000n;
    if (deltaNs < 0n) {
      throw new RangeError("DeterministicNanosecondClock cannot move backwards");
    }
    this.currentNs += deltaNs;
    return this.currentNs;
  }

  public advanceNs(ns: bigint): bigint {
    if (ns < 0n) {
      throw new RangeError("DeterministicNanosecondClock cannot move backwards");
    }
    this.currentNs += ns;
    return this.currentNs;
  }
}

/**
 * Formats UTC epoch nanoseconds into ISO-8601 with full 9-digit nanosecond precision:
 * e.g. "2026-11-18T14:53:00.000001000Z"
 */
export function formatEpochNsIso(tsNs: bigint): string {
  const ms = tsNs / 1_000_000n;
  const subMsNs = tsNs % 1_000_000n;
  const isoMs = new Date(Number(ms)).toISOString(); // YYYY-MM-DDTHH:mm:ss.sssZ
  const prefix = isoMs.slice(0, -1); // strip trailing Z
  return `${prefix}${subMsNs.toString(10).padStart(6, "0")}Z`;
}
