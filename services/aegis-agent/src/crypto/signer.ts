/**
 * HYPERION Channel 06 — Ed25519 + HMAC-SHA256 Cryptographic Command Signer & Token Verifier.
 *
 * Produces:
 * 1. Canonical Protobuf binary `RiskMitigationCommand` (`@hyperion/contracts`) signed with Ed25519.
 * 2. Gateway HTTP authentication headers (`00-hyperion-contract.md` §6.2).
 * 3. Cryptographically signed Rollback Tokens (`rbk1.*`) & HITL Approval Tokens (`hitl1.*`).
 */

import {
  createHmac,
  createPrivateKey,
  createPublicKey,
  sign as cryptoSign,
  verify as cryptoVerify,
  type KeyObject
} from "node:crypto";
import { sha256 } from "@noble/hashes/sha256";
import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils";
import {
  MitigationActionType,
  RiskMitigationCommand
} from "@hyperion/contracts";
import type {
  InstrumentSymbol,
  MitigationActionKind,
  SignedMitigationEnvelope
} from "../domain/types.js";

// RFC 8410 ASN.1 PKCS#8 DER prefix for a 32-byte Ed25519 private seed:
// 30 2e 02 01 00 30 05 06 03 2b 65 70 04 22 04 20 <32-byte-seed>
const ED25519_PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

// RFC 8410 ASN.1 SubjectPublicKeyInfo (SPKI) DER prefix for a 32-byte Ed25519 public key:
// 30 2a 30 05 06 03 2b 65 70 03 21 00 <32-byte-pubkey>
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export interface RollbackTokenClaims {
  v: 1;
  command_id: string;
  incident_id: string;
  source_alert_id: string;
  original_action: MitigationActionKind;
  target_account_ids: string[];
  target_symbol: InstrumentSymbol;
  issued_ts_ns: string;
  expires_ts_ns: string;
}

export interface HitlApprovalTokenClaims {
  v: 1;
  approval_id: string;
  incident_id: string;
  action: MitigationActionKind;
  target_account_ids: string[];
  target_symbol: InstrumentSymbol;
  approved_by: string;
  approved_ts_ns: string;
}

export function mapActionKindToProtoEnum(kind: MitigationActionKind): MitigationActionType {
  switch (kind) {
    case "THROTTLE_RATE_LIMIT":
      return MitigationActionType.MITIGATION_ACTION_TYPE_THROTTLE_RATE_LIMIT;
    case "FREEZE_ACCOUNT":
      return MitigationActionType.MITIGATION_ACTION_TYPE_FREEZE_ACCOUNT;
    case "CANCEL_OPEN_ORDERS":
      return MitigationActionType.MITIGATION_ACTION_TYPE_CANCEL_OPEN_ORDERS;
    case "ENABLE_VOLATILITY_GUARD":
      return MitigationActionType.MITIGATION_ACTION_TYPE_ENABLE_VOLATILITY_GUARD;
    case "RESTORE_NORMAL":
      return MitigationActionType.MITIGATION_ACTION_TYPE_RESTORE_NORMAL;
  }
}

export class AegisCryptographicSigner {
  private readonly privateKey: KeyObject;
  private readonly publicKey: KeyObject;
  private readonly publicKeyRawHex: string;
  private readonly hmacSecret: Buffer;
  private readonly keyId: string;
  private readonly serviceAccountId: string;
  private nonceCounter = 1n;

  public constructor(options?: {
    seedHex?: string;
    hmacSecret?: string;
    keyId?: string;
    serviceAccountId?: string;
  }) {
    const seedBytes = options?.seedHex
      ? hexToBytes(options.seedHex)
      : blake3(utf8ToBytes("hyperion-aegis-ch06-default-ed25519-seed-v1"));
    if (seedBytes.byteLength !== 32) {
      throw new RangeError("Ed25519 seed must be exactly 32 bytes (64 hex chars)");
    }

    const pkcs8Der = Buffer.concat([ED25519_PKCS8_PREFIX, Buffer.from(seedBytes)]);
    this.privateKey = createPrivateKey({ key: pkcs8Der, format: "der", type: "pkcs8" });
    this.publicKey = createPublicKey(this.privateKey);

    const spkiDer = this.publicKey.export({ format: "der", type: "spki" });
    this.publicKeyRawHex = Buffer.from(spkiDer).subarray(ED25519_SPKI_PREFIX.byteLength).toString("hex");

    this.hmacSecret = Buffer.from(
      options?.hmacSecret ?? "hyperion-aegis-hmac-sha256-shared-secret-v1",
      "utf8"
    );
    this.keyId = options?.keyId ?? "aegis-sre-ed25519-k1";
    this.serviceAccountId = options?.serviceAccountId ?? "999006";
  }

  public getPublicKeyHex(): string {
    return this.publicKeyRawHex;
  }

  public getKeyId(): string {
    return this.keyId;
  }

  public nextNonce(): bigint {
    const current = this.nonceCounter;
    this.nonceCounter += 1n;
    return current;
  }

  public signRawBytes(payload: Uint8Array): Uint8Array {
    const sig = cryptoSign(null, Buffer.from(payload), this.privateKey);
    return new Uint8Array(sig);
  }

  public verifyRawBytes(payload: Uint8Array, signature: Uint8Array, publicKeyHex?: string): boolean {
    try {
      const pubKeyObj = publicKeyHex
        ? createPublicKey({
            key: Buffer.concat([ED25519_SPKI_PREFIX, Buffer.from(hexToBytes(publicKeyHex))]),
            format: "der",
            type: "spki"
          })
        : this.publicKey;
      return cryptoVerify(null, Buffer.from(payload), pubKeyObj, Buffer.from(signature));
    } catch {
      return false;
    }
  }

  /**
   * Builds HTTP cryptographic headers complying with `00-hyperion-contract.md` §6.2:
   * Preimage = `<X-Hyperion-Timestamp>\n<X-Hyperion-Nonce>\n<METHOD>\n<PATH>\n<SHA256_HEX_LOWERCASE(RAW_BODY)>`
   */
  public buildGatewayAuthHeaders(params: {
    method: string;
    path: string;
    rawBody: string | Uint8Array;
    timestampNs: bigint;
    idempotencyKey: string;
    scheme?: "ed25519" | "hmac-sha256";
  }): Record<string, string> {
    const scheme = params.scheme ?? "ed25519";
    const nonceStr = this.nextNonce().toString(10);
    const tsStr = params.timestampNs.toString(10);
    const bodyBytes =
      typeof params.rawBody === "string" ? utf8ToBytes(params.rawBody) : params.rawBody;
    const bodySha256Hex = bytesToHex(sha256(bodyBytes));
    const preimage = `${tsStr}\n${nonceStr}\n${params.method.toUpperCase()}\n${params.path}\n${bodySha256Hex}`;

    let signatureHex: string;
    if (scheme === "hmac-sha256") {
      signatureHex = createHmac("sha256", this.hmacSecret).update(preimage, "utf8").digest("hex");
    } else {
      signatureHex = bytesToHex(this.signRawBytes(utf8ToBytes(preimage)));
    }

    return {
      "X-Hyperion-Account-Id": this.serviceAccountId,
      "X-Hyperion-Key-Id": this.keyId,
      "X-Hyperion-Timestamp": tsStr,
      "X-Hyperion-Nonce": nonceStr,
      "X-Hyperion-Signature": signatureHex,
      "X-Hyperion-Auth-Scheme": scheme,
      "Idempotency-Key": params.idempotencyKey
    };
  }

  public issueRollbackToken(claims: Omit<RollbackTokenClaims, "v">): string {
    const fullClaims: RollbackTokenClaims = { v: 1, ...claims };
    const payloadB64 = Buffer.from(JSON.stringify(fullClaims), "utf8").toString("base64url");
    const sigBytes = this.signRawBytes(utf8ToBytes(`rbk1.${payloadB64}`));
    const sigB64 = Buffer.from(sigBytes).toString("base64url");
    return `rbk1.${payloadB64}.${sigB64}`;
  }

  public verifyRollbackToken(token: string): RollbackTokenClaims | null {
  	const parts = token.split(".");
    if (parts.length !== 3 || parts[0] !== "rbk1") {
      return null;
    }
    const payloadB64 = parts[1]!;
    const sigB64 = parts[2]!;
    try {
      const sigBytes = new Uint8Array(Buffer.from(sigB64, "base64url"));
      const valid = this.verifyRawBytes(utf8ToBytes(`rbk1.${payloadB64}`), sigBytes);
      if (!valid) {
        return null;
      }
      const parsed = JSON.parse(Buffer.from(payloadB64, "base64url").toString("utf8")) as RollbackTokenClaims;
      if (parsed.v !== 1 || !parsed.command_id || !parsed.incident_id) {
        return null;
      }
      return parsed;
    } catch {
      return null;
    }
  }

  public issueHitlApprovalToken(claims: Omit<HitlApprovalTokenClaims, "v">): string {
    const fullClaims: HitlApprovalTokenClaims = { v: 1, ...claims };
    const payloadB64 = Buffer.from(JSON.stringify(fullClaims), "utf8").toString("base64url");
    const sigBytes = this.signRawBytes(utf8ToBytes(`hitl1.${payloadB64}`));
    const sigB64 = Buffer.from(sigBytes).toString("base64url");
    return `hitl1.${payloadB64}.${sigB64}`;
  }

  public verifyHitlApprovalToken(token: string): HitlApprovalTokenClaims | null {
    const parts = token.split(".");
    if (parts.length !== 3 || parts[0] !== "hitl1") {
      return null;
    }
    const payloadB64 = parts[1]!;
    const sigB64 = parts[2]!;
    try {
      const sigBytes = new Uint8Array(Buffer.from(sigB64, "base64url"));
      if (!this.verifyRawBytes(utf8ToBytes(`hitl1.${payloadB64}`), sigBytes)) {
        return null;
      }
      const parsed = JSON.parse(Buffer.from(payloadB64, "base64url").toString("utf8")) as HitlApprovalTokenClaims;
      if (parsed.v !== 1 || !parsed.approval_id) {
        return null;
      }
      return parsed;
    } catch {
      return null;
    }
  }

  /**
   * Serializes a canonical `RiskMitigationCommand` Protobuf payload via `@hyperion/contracts`,
   * signs the deterministic unsigned wire representation with Ed25519, embeds `ed25519Signature`
   * and `auditChainBlake3`, and produces both binary wire Base64 and Gateway signed headers.
   */
  public signRiskMitigationCommand(params: {
    commandId: string;
    sourceAlertId: string;
    incidentId: string;
    action: MitigationActionKind;
    targetAccountIds: string[];
    targetSymbol: InstrumentSymbol;
    rateLimitOverrideRps: number;
    cooldownDurationMs: number;
    dryRun: boolean;
    requiresHumanApproval: boolean;
    reason: string;
    issuedBy: string;
    issuedTsNs: bigint;
    expiresTsNs: bigint;
    auditChainBlake3Hex: string;
    traceId: string;
  }): SignedMitigationEnvelope {
    const rollbackToken = this.issueRollbackToken({
      command_id: params.commandId,
      incident_id: params.incidentId,
      source_alert_id: params.sourceAlertId,
      original_action: params.action,
      target_account_ids: [...params.targetAccountIds],
      target_symbol: params.targetSymbol,
      issued_ts_ns: params.issuedTsNs.toString(10),
      expires_ts_ns: params.expiresTsNs.toString(10)
    });

    const auditDigestBytes = hexToBytes(params.auditChainBlake3Hex);
    const unsignedProto = RiskMitigationCommand.create({
      commandId: params.commandId,
      sourceAlertId: params.sourceAlertId,
      incidentId: params.incidentId,
      action: mapActionKindToProtoEnum(params.action),
      targetAccountIds: params.targetAccountIds.map((id) => BigInt(id)),
      targetSymbol: params.targetSymbol,
      rateLimitOverrideRps: params.rateLimitOverrideRps,
      cooldownDurationMs: BigInt(params.cooldownDurationMs),
      dryRun: params.dryRun,
      requiresHumanApproval: params.requiresHumanApproval,
      rollbackToken,
      reason: params.reason,
      issuedBy: params.issuedBy,
      issuedTsNs: params.issuedTsNs,
      expiresTsNs: params.expiresTsNs,
      ed25519Signature: new Uint8Array(0),
      auditChainBlake3: auditDigestBytes,
      trace: {
        traceId: params.traceId,
        spanId: params.traceId.slice(0, 16),
        correlationId: params.incidentId,
        sourceService: "aegis-agent",
        ingestTsNs: params.issuedTsNs
      }
    });

    const unsignedWireBytes = RiskMitigationCommand.encode(unsignedProto).finish();
    const signatureBytes = this.signRawBytes(unsignedWireBytes);

    const signedProto = RiskMitigationCommand.create({
      ...unsignedProto,
      ed25519Signature: signatureBytes
    });
    const signedWireBytes = RiskMitigationCommand.encode(signedProto).finish();

    const gatewayAuthHeaders = this.buildGatewayAuthHeaders({
      method: "POST",
      path: "/api/v1/risk/mitigations",
      rawBody: signedWireBytes,
      timestampNs: params.issuedTsNs,
      idempotencyKey: params.commandId
    });

    return {
      command_id: params.commandId,
      source_alert_id: params.sourceAlertId,
      incident_id: params.incidentId,
      action: params.action,
      target_account_ids: [...params.targetAccountIds],
      target_symbol: params.targetSymbol,
      rate_limit_override_rps: params.rateLimitOverrideRps,
      cooldown_duration_ms: params.cooldownDurationMs,
      dry_run: params.dryRun,
      requires_human_approval: params.requiresHumanApproval,
      rollback_token: rollbackToken,
      reason: params.reason,
      issued_by: params.issuedBy,
      issued_ts_ns: params.issuedTsNs.toString(10),
      expires_ts_ns: params.expiresTsNs.toString(10),
      ed25519_signature_hex: bytesToHex(signatureBytes),
      ed25519_public_key_hex: this.publicKeyRawHex,
      audit_chain_blake3_hex: params.auditChainBlake3Hex,
      protobuf_wire_base64: Buffer.from(signedWireBytes).toString("base64"),
      gateway_auth_headers: gatewayAuthHeaders
    };
  }

  /**
   * Decodes a binary `RiskMitigationCommand` Protobuf frame and verifies its Ed25519 signature.
   */
  public verifySignedMitigationWire(protobufWireBase64: string, publicKeyHex?: string): {
    valid: boolean;
    decoded?: RiskMitigationCommand;
  } {
    try {
      const wireBytes = new Uint8Array(Buffer.from(protobufWireBase64, "base64"));
      const decoded = RiskMitigationCommand.decode(wireBytes);
      if (decoded.ed25519Signature.byteLength !== 64) {
        return { valid: false };
      }
      const unsignedCopy = RiskMitigationCommand.create({
        ...decoded,
        ed25519Signature: new Uint8Array(0)
      });
      const unsignedWire = RiskMitigationCommand.encode(unsignedCopy).finish();
      const valid = this.verifyRawBytes(unsignedWire, decoded.ed25519Signature, publicKeyHex);
      return { valid, decoded };
    } catch {
      return { valid: false };
    }
  }
}
