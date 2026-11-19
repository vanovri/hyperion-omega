/**
 * HYPERION Channel 06 — Immutable BLAKE3 + Ed25519 Hash-Chained Audit Log.
 *
 * Every MCP tool invocation, prompt-injection quarantine, policy verdict, HITL decision,
 * risk mitigation execution, and rollback is cryptographically chained (`prev_blake3 -> entry_blake3`)
 * and signed with Aegis's Ed25519 key so any historical mutation is 100% detectable.
 */

import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils";
import type { NanosecondClock } from "../domain/clock.js";
import type { AegisCryptographicSigner } from "./signer.js";

export const GENESIS_BLAKE3_HEX = "0".repeat(64);

export interface AuditLogEntry {
  readonly seq: number;
  readonly timestamp_ns: string;
  readonly incident_id: string;
  readonly event_type: string;
  readonly actor: string;
  readonly payload: Record<string, unknown>;
  readonly prev_blake3: string;
  readonly entry_blake3: string;
  readonly ed25519_signature_hex: string;
}

export interface AuditChainVerificationReport {
  valid: boolean;
  entries_checked: number;
  head_blake3: string;
  corrupted_at_seq?: number;
  failure_reason?: string;
}

/**
 * Deterministic canonical JSON serializer (recursively sorts object keys and handles BigInt safely).
 */
export function canonicalJsonSerialize(value: unknown): string {
  if (value === null || value === undefined) {
    return "null";
  }
  if (typeof value === "bigint") {
    return JSON.stringify(value.toString(10));
  }
  if (typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map((item) => canonicalJsonSerialize(item)).join(",")}]`;
  }
  if (typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const keys = Object.keys(obj)
      .filter((k) => obj[k] !== undefined)
      .sort();
    const pairs = keys.map((k) => `${JSON.stringify(k)}:${canonicalJsonSerialize(obj[k])}`);
    return `{${pairs.join(",")}}`;
  }
  return JSON.stringify(String(value));
}

function deepFreezeClone<T>(val: T): T {
  const cloned = JSON.parse(
    JSON.stringify(val, (_k, v: unknown) => (typeof v === "bigint" ? v.toString(10) : v))
  ) as T;
  return Object.freeze(cloned);
}

export class ImmutableAuditChain {
  private readonly entries: AuditLogEntry[] = [];
  private readonly clock: NanosecondClock;
  private readonly signer: AegisCryptographicSigner;

  public constructor(clock: NanosecondClock, signer: AegisCryptographicSigner) {
    this.clock = clock;
    this.signer = signer;
  }

  public getHeadHash(): string {
    if (this.entries.length === 0) {
      return GENESIS_BLAKE3_HEX;
    }
    return this.entries[this.entries.length - 1]!.entry_blake3;
  }

  public getLength(): number {
    return this.entries.length;
  }

  public append(params: {
    incidentId: string;
    eventType: string;
    actor: string;
    payload: Record<string, unknown>;
    timestampNsOverride?: bigint;
  }): AuditLogEntry {
    const seq = this.entries.length + 1;
    const tsNs = (params.timestampNsOverride ?? this.clock.nowNs()).toString(10);
    const prevBlake3 = this.getHeadHash();
    const frozenPayload = deepFreezeClone(params.payload);

    const preimage = canonicalJsonSerialize({
      actor: params.actor,
      event_type: params.eventType,
      incident_id: params.incidentId,
      payload: frozenPayload,
      prev_blake3: prevBlake3,
      seq,
      timestamp_ns: tsNs
    });

    const digestBytes = blake3(utf8ToBytes(preimage));
    const entryBlake3 = bytesToHex(digestBytes);
    const signatureHex = bytesToHex(this.signer.signRawBytes(digestBytes));

    const entry: AuditLogEntry = Object.freeze({
      seq,
      timestamp_ns: tsNs,
      incident_id: params.incidentId,
      event_type: params.eventType,
      actor: params.actor,
      payload: frozenPayload,
      prev_blake3: prevBlake3,
      entry_blake3: entryBlake3,
      ed25519_signature_hex: signatureHex
    });

    this.entries.push(entry);
    return entry;
  }

  public getEntries(filterIncidentId?: string): ReadonlyArray<AuditLogEntry> {
    if (!filterIncidentId) {
      return [...this.entries];
    }
    return this.entries.filter((e) => e.incident_id === filterIncidentId);
  }

  /**
   * Exports raw entries for external inspection or tamper-resistance testing,
   * and verifies any array of entries against cryptographic BLAKE3 + Ed25519 invariants.
   */
  public verifyChain(entriesToVerify?: ReadonlyArray<AuditLogEntry>): AuditChainVerificationReport {
    const target = entriesToVerify ?? this.entries;
    let expectedPrev = GENESIS_BLAKE3_HEX;
    let prevTs = -1n;

    for (let i = 0; i < target.length; i++) {
      const entry = target[i]!;
      const expectedSeq = i + 1;
      if (entry.seq !== expectedSeq) {
        return {
          valid: false,
          entries_checked: i,
          head_blake3: expectedPrev,
          corrupted_at_seq: entry.seq,
          failure_reason: `Sequence gap/mismatch: expected seq=${expectedSeq}, got seq=${entry.seq}`
        };
      }
      if (entry.prev_blake3 !== expectedPrev) {
        return {
          valid: false,
          entries_checked: i,
          head_blake3: expectedPrev,
          corrupted_at_seq: entry.seq,
          failure_reason: `Broken hash link at seq=${entry.seq}: prev_blake3 mismatch`
        };
      }

      const currentTs = BigInt(entry.timestamp_ns);
      if (currentTs < prevTs) {
        return {
          valid: false,
          entries_checked: i,
          head_blake3: expectedPrev,
          corrupted_at_seq: entry.seq,
          failure_reason: `Non-monotonic timestamp at seq=${entry.seq}`
        };
      }
      prevTs = currentTs;

      const recomputedPreimage = canonicalJsonSerialize({
        actor: entry.actor,
        event_type: entry.event_type,
        incident_id: entry.incident_id,
        payload: entry.payload,
        prev_blake3: entry.prev_blake3,
        seq: entry.seq,
        timestamp_ns: entry.timestamp_ns
      });
      const recomputedDigest = blake3(utf8ToBytes(recomputedPreimage));
      const recomputedHex = bytesToHex(recomputedDigest);
      if (recomputedHex !== entry.entry_blake3) {
        return {
          valid: false,
          entries_checked: i,
          head_blake3: expectedPrev,
          corrupted_at_seq: entry.seq,
          failure_reason: `BLAKE3 payload digest tampered at seq=${entry.seq}`
        };
      }

      const sigValid = this.signer.verifyRawBytes(
        recomputedDigest,
        hexToBytes(entry.ed25519_signature_hex)
      );
      if (!sigValid) {
        return {
          valid: false,
          entries_checked: i,
          head_blake3: expectedPrev,
          corrupted_at_seq: entry.seq,
          failure_reason: `Ed25519 signature verification failed at seq=${entry.seq}`
        };
      }

      expectedPrev = entry.entry_blake3;
    }

    return {
      valid: true,
      entries_checked: target.length,
      head_blake3: expectedPrev
    };
  }
}
