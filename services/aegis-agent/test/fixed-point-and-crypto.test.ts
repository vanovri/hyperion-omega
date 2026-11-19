import { describe, expect, it } from "vitest";
import { hexToBytes } from "@noble/hashes/utils";
import {
  AegisCryptographicSigner,
  DeterministicNanosecondClock,
  DeterministicSimulationBackend,
  ImmutableAuditChain,
  buildDeterministicMerkleTree,
  fixedPoint,
  verifyMerkleLedgerProof
} from "../src/index.js";
import {
  computeMerkleParentBlake3,
  packAlignedMerkleSlab64,
  verifyMerklePathAligned64,
  verifyMerklePathZeroCopy
} from "../src/crypto/merkle.js";

describe("Fixed-Point 1e8 Arithmetic & Cryptographic Primitives", () => {
  it("performs exact bigint 1e8 fixed-point conversions and notional/fee math without float precision loss", () => {
    const priceFp = fixedPoint.decimalToFixedPoint1e8("68425.50");
    const qtyFp = fixedPoint.decimalToFixedPoint1e8("1.25000000");
    expect(priceFp).toBe(6_842_550_000_000n);
    expect(qtyFp).toBe(125_000_000n);

    const notionalFp = fixedPoint.checkedNotionalFp(priceFp, qtyFp);
    expect(notionalFp).toBe(8_553_187_500_000n);
    expect(fixedPoint.formatFixedPoint1e8(notionalFp)).toBe("85531.87500000");

    // Maker rebate (-200 ppm = -2 bps) and taker fee (+500 ppm = +5 bps)
    const makerRebateFp = fixedPoint.checkedFeeFp(notionalFp, -200n);
    const takerFeeFp = fixedPoint.checkedFeeFp(notionalFp, 500n);
    expect(makerRebateFp).toBe(-1_710_637_500n);
    expect(takerFeeFp).toBe(4_276_593_750n);

    // Values far beyond IEEE-754 MAX_SAFE_INTEGER (2^53 - 1 = 9,007,199,254,740,991)
    const institutionalWhaleFp = (1n << 92n) + 123_456_789n;
    const protoU128 = fixedPoint.toProtoU128(institutionalWhaleFp);
    expect(fixedPoint.fromProtoU128(protoU128)).toBe(institutionalWhaleFp);

    const negativeLargeFp = -((1n << 88n) + 987_654_321n);
    const protoI128 = fixedPoint.toProtoI128(negativeLargeFp);
    expect(fixedPoint.fromProtoI128(protoI128)).toBe(negativeLargeFp);

    // Exhaustive Mutation-Survival-Zero boundary tests (parse, format, ppm, bps, checked overflow/underflow)
    const maxU128 = (1n << 128n) - 1n;
    const minI128 = -(1n << 127n);
    const maxI128 = (1n << 127n) - 1n;

    expect(fixedPoint.parseFixedPointUint("0")).toBe(0n);
    expect(fixedPoint.parseFixedPointUint(maxU128.toString(10))).toBe(maxU128);
    expect(() => fixedPoint.parseFixedPointUint("-1")).toThrow(RangeError);
    expect(() => fixedPoint.parseFixedPointUint("12.34")).toThrow(RangeError);
    expect(() => fixedPoint.parseFixedPointUint((maxU128 + 1n).toString(10))).toThrow(RangeError);

    expect(fixedPoint.parseFixedPointInt(minI128.toString(10))).toBe(minI128);
    expect(fixedPoint.parseFixedPointInt(maxI128.toString(10))).toBe(maxI128);
    expect(() => fixedPoint.parseFixedPointInt("abc")).toThrow(RangeError);
    expect(() => fixedPoint.parseFixedPointInt((minI128 - 1n).toString(10))).toThrow(RangeError);
    expect(() => fixedPoint.parseFixedPointInt((maxI128 + 1n).toString(10))).toThrow(RangeError);

    expect(fixedPoint.decimalToFixedPoint1e8("0")).toBe(0n);
    expect(fixedPoint.decimalToFixedPoint1e8("-0.0025")).toBe(-250_000n);
    expect(fixedPoint.decimalToFixedPoint1e8("42")).toBe(4_200_000_000n);
    expect(fixedPoint.decimalToFixedPoint1e8("-1701411834604692317316873037158.84105728")).toBe(minI128);
    expect(fixedPoint.decimalToFixedPoint1e8("1701411834604692317316873037158.84105727")).toBe(maxI128);
    expect(() => fixedPoint.decimalToFixedPoint1e8("1.123456789")).toThrow(RangeError);
    expect(() => fixedPoint.decimalToFixedPoint1e8("99999999999999999999999999999999999999")).toThrow(RangeError);
    expect(() => fixedPoint.decimalToFixedPoint1e8("-99999999999999999999999999999999999999")).toThrow(RangeError);

    expect(fixedPoint.formatFixedPoint1e8(0n, true)).toBe("0.00");
    expect(fixedPoint.formatFixedPoint1e8(150_000_000n, true)).toBe("1.50");
    expect(fixedPoint.formatFixedPoint1e8(-123_450_000n, true)).toBe("-1.2345");
    expect(fixedPoint.formatFixedPoint1e8(-100_000_000n, false)).toBe("-1.00000000");

    expect(fixedPoint.formatPpmAsPercent(0)).toBe("0.00%");
    expect(fixedPoint.formatPpmAsPercent(984_500)).toBe("98.45%");
    expect(fixedPoint.formatPpmAsPercent(-25_100n)).toBe("-2.51%");
    expect(fixedPoint.formatBpsAsPercent(0)).toBe("0.00%");
    expect(fixedPoint.formatBpsAsPercent(125)).toBe("1.25%");
    expect(fixedPoint.formatBpsAsPercent(-305n)).toBe("-3.05%");

    expect(fixedPoint.checkedNotionalFp(0n, 100n)).toBe(0n);
    expect(fixedPoint.checkedNotionalFp(100n, 0n)).toBe(0n);
    expect(fixedPoint.checkedNotionalFp(3n, 33_333_333n)).toBe(0n);
    expect(fixedPoint.checkedNotionalFp(3n, 33_333_334n)).toBe(1n);
    expect(fixedPoint.checkedNotionalFp(maxU128, fixedPoint.FP_SCALE)).toBe(maxU128);
    expect(() => fixedPoint.checkedNotionalFp(-1n, 1n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedNotionalFp(1n, -1n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedNotionalFp(maxU128, 200_000_000n)).toThrow(RangeError);

    expect(fixedPoint.checkedFeeFp(0n, 100n)).toBe(0n);
    expect(fixedPoint.checkedFeeFp(999_999n, -1n)).toBe(0n);
    expect(fixedPoint.checkedFeeFp(maxI128, fixedPoint.PPM_SCALE)).toBe(maxI128);
    expect(fixedPoint.checkedFeeFp(1n << 127n, -fixedPoint.PPM_SCALE)).toBe(minI128);
    expect(() => fixedPoint.checkedFeeFp(-1n, 100n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedFeeFp(maxU128, 2_000_000n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedFeeFp(maxU128, -2_000_000n)).toThrow(RangeError);

    expect(fixedPoint.checkedAddI128(maxI128, 0n)).toBe(maxI128);
    expect(fixedPoint.checkedAddI128(minI128, 0n)).toBe(minI128);
    expect(() => fixedPoint.checkedAddI128(maxI128, 1n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedAddI128(minI128, -1n)).toThrow(RangeError);
    expect(fixedPoint.checkedSubI128(minI128, 0n)).toBe(minI128);
    expect(fixedPoint.checkedSubI128(maxI128, 0n)).toBe(maxI128);
    expect(() => fixedPoint.checkedSubI128(minI128, 1n)).toThrow(RangeError);
    expect(() => fixedPoint.checkedSubI128(maxI128, -1n)).toThrow(RangeError);

    expect(fixedPoint.fromProtoU128(undefined)).toBe(0n);
    expect(fixedPoint.fromProtoI128(undefined)).toBe(0n);
    expect(fixedPoint.fromProtoU128(fixedPoint.toProtoU128(0n))).toBe(0n);
    expect(fixedPoint.fromProtoU128(fixedPoint.toProtoU128(maxU128))).toBe(maxU128);
    expect(fixedPoint.fromProtoI128(fixedPoint.toProtoI128(0n))).toBe(0n);
    expect(fixedPoint.fromProtoI128(fixedPoint.toProtoI128(minI128))).toBe(minI128);
    expect(fixedPoint.fromProtoI128(fixedPoint.toProtoI128(maxI128))).toBe(maxI128);
    expect(() => fixedPoint.toProtoU128(-1n)).toThrow(RangeError);
    expect(() => fixedPoint.toProtoU128(maxU128 + 1n)).toThrow(RangeError);
    expect(() => fixedPoint.toProtoI128(minI128 - 1n)).toThrow(RangeError);
    expect(() => fixedPoint.toProtoI128(maxI128 + 1n)).toThrow(RangeError);
  });

  it("verifies zero-copy and 64B cache-line-aligned BLAKE3 Merkle verifiers and odd-leaf trees", () => {
    const accounts = [
      {
        account_id: "300",
        balances: [{ asset: "USDT", available_fp: "100000000", locked_fp: "0", total_fp: "100000000" }]
      },
      {
        account_id: "100",
        balances: [
          { asset: "USDT", available_fp: "200000000", locked_fp: "50000000", total_fp: "250000000" },
          { asset: "BTC", available_fp: "100000000", locked_fp: "0", total_fp: "100000000" }
        ]
      },
      {
        account_id: "200",
        balances: [{ asset: "USDT", available_fp: "300000000", locked_fp: "0", total_fp: "300000000" }]
      }
    ];
    const tree = buildDeterministicMerkleTree("77", accounts);
    expect(() => computeMerkleParentBlake3("deadbeef", tree.stateRootBlake3)).toThrow(RangeError);
    expect(() => computeMerkleParentBlake3(tree.stateRootBlake3, "deadbeef")).toThrow(RangeError);

    const expectedRoot32 = hexToBytes(tree.stateRootBlake3);
    const expectedRootBuf = new ArrayBuffer(32);
    new Uint8Array(expectedRootBuf).set(expectedRoot32);
    const expectedRootWords = new BigUint64Array(expectedRootBuf);

    const scratch65 = new Uint8Array(65);
    const rollingBuf = new ArrayBuffer(32);
    const rolling32 = new Uint8Array(rollingBuf);
    const rollingWords = new BigUint64Array(rollingBuf);

    for (const id of ["100", "200", "300"]) {
      const leaf32 = hexToBytes(tree.leafHashes.get(id)!);
      const path = tree.proofPaths.get(id)!;
      const sibs = path.map((p) => hexToBytes(p.sibling_hash));
      const flags = path.map((p) => p.is_left_sibling);

      expect(verifyMerklePathZeroCopy(leaf32, sibs, flags, expectedRoot32, scratch65, rolling32)).toBe(true);
      const slab = packAlignedMerkleSlab64(path);
      expect(slab.buffer.byteLength % 64).toBe(0);
      expect(verifyMerklePathAligned64(leaf32, slab, expectedRootWords, scratch65, rolling32, rollingWords)).toBe(true);

      // Flip 1 bit in leaf -> both verifiers reject
      const badLeaf = new Uint8Array(leaf32);
      badLeaf[31] = badLeaf[31]! ^ 0x01;
      expect(verifyMerklePathZeroCopy(badLeaf, sibs, flags, expectedRoot32, scratch65, rolling32)).toBe(false);
      expect(verifyMerklePathAligned64(badLeaf, slab, expectedRootWords, scratch65, rolling32, rollingWords)).toBe(false);
    }

    // Input length guard checks
    expect(verifyMerklePathZeroCopy(new Uint8Array(31), [], [], expectedRoot32, scratch65, rolling32)).toBe(false);
    expect(verifyMerklePathZeroCopy(new Uint8Array(32), [new Uint8Array(31)], [false], expectedRoot32, scratch65, rolling32)).toBe(false);
    expect(verifyMerklePathZeroCopy(new Uint8Array(32), [new Uint8Array(32)], [], expectedRoot32, scratch65, rolling32)).toBe(false);
    const emptySlab = packAlignedMerkleSlab64([]);
    expect(verifyMerklePathAligned64(new Uint8Array(31), emptySlab, expectedRootWords, scratch65, rolling32, rollingWords)).toBe(false);
    expect(verifyMerklePathAligned64(new Uint8Array(32), emptySlab, expectedRootWords, new Uint8Array(64), rolling32, rollingWords)).toBe(false);
    expect(() => packAlignedMerkleSlab64([{ sibling_hash: "aa", is_left_sibling: false, depth: 0 }])).toThrow(RangeError);
  });

  it("verifies BLAKE3 Merkle inclusion proofs and detects 1-tick (1e-8) conservation breaches", async () => {
    const clock = new DeterministicNanosecondClock();
    const sim = new DeterministicSimulationBackend(clock);

    const validProof = await sim.fetchMerkleLedgerProof({
      accountId: "9001",
      asset: "USDT"
    });
    const verified = verifyMerkleLedgerProof(validProof, "USDT");
    expect(verified.merkle_proof_valid).toBe(true);
    expect(verified.double_entry_balanced).toBe(true);
    expect(verified.conservation_delta_fp).toBe("0");
    expect(verified.verdict).toBe("VERIFIED_SOLVENT");

    // 1 atomic tick (0.00000001 USDT) conservation discrepancy -> CRITICAL_LEDGER_INVARIANT_BREACH
    sim.setConservationCorruptionFp(1n);
    const unbalancedProof = await sim.fetchMerkleLedgerProof({
      accountId: "9001",
      asset: "USDT"
    });
    const unbalancedVerdict = verifyMerkleLedgerProof(unbalancedProof, "USDT");
    expect(unbalancedVerdict.merkle_proof_valid).toBe(true);
    expect(unbalancedVerdict.double_entry_balanced).toBe(false);
    expect(unbalancedVerdict.conservation_delta_fp).toBe("1");
    expect(unbalancedVerdict.verdict).toBe("CRITICAL_LEDGER_INVARIANT_BREACH");
    sim.setConservationCorruptionFp(0n);

    // Merkle leaf nibble tamper -> MERKLE_PROOF_TAMPERED
    sim.setMerkleTamperForAccount("9001", true);
    const tamperedProof = await sim.fetchMerkleLedgerProof({
      accountId: "9001",
      asset: "USDT"
    });
    const tamperedVerdict = verifyMerkleLedgerProof(tamperedProof, "USDT");
    expect(tamperedVerdict.merkle_proof_valid).toBe(false);
    expect(tamperedVerdict.verdict).toBe("MERKLE_PROOF_TAMPERED");

    // Empty tree edge case
    const emptyTree = buildDeterministicMerkleTree("1", []);
    expect(emptyTree.stateRootBlake3).toMatch(/^[0-9a-f]{64}$/);
  });

  it("signs canonical Protobuf RiskMitigationCommand with Ed25519 and detects wire or token tampering", () => {
    const clock = new DeterministicNanosecondClock();
    const signer = new AegisCryptographicSigner();
    const audit = new ImmutableAuditChain(clock, signer);

    const entry = audit.append({
      incidentId: "INC-CRYPTO-01",
      eventType: "TEST_GENESIS",
      actor: "vitest",
      payload: { ok: true }
    });

    const envelope = signer.signRiskMitigationCommand({
      commandId: "mit-crypto-001",
      sourceAlertId: "alert-01",
      incidentId: "INC-CRYPTO-01",
      action: "FREEZE_ACCOUNT",
      targetAccountIds: ["9001", "9002"],
      targetSymbol: "BTC-USDT",
      rateLimitOverrideRps: 0,
      cooldownDurationMs: 300_000,
      dryRun: false,
      requiresHumanApproval: false,
      reason: "Verified circular wash-trading syndicate ring",
      issuedBy: "aegis-test",
      issuedTsNs: clock.nowNs(),
      expiresTsNs: clock.nowNs() + 300_000_000_000n,
      auditChainBlake3Hex: entry.entry_blake3,
      traceId: "0123456789abcdef0123456789abcdef"
    });

    const wireCheck = signer.verifySignedMitigationWire(envelope.protobuf_wire_base64);
    expect(wireCheck.valid).toBe(true);
    expect(wireCheck.decoded?.commandId).toBe("mit-crypto-001");
    expect(wireCheck.decoded?.targetAccountIds).toEqual([9001n, 9002n]);

    // Corrupt 1 byte in Protobuf wire -> Ed25519 verification fails
    const rawBuf = Buffer.from(envelope.protobuf_wire_base64, "base64");
    rawBuf[rawBuf.length - 5] = (rawBuf[rawBuf.length - 5] ?? 0) ^ 0xff;
    expect(signer.verifySignedMitigationWire(rawBuf.toString("base64")).valid).toBe(false);

    // Verify rollback token & reject forged rollback token
    const claims = signer.verifyRollbackToken(envelope.rollback_token);
    expect(claims?.command_id).toBe("mit-crypto-001");
    expect(signer.verifyRollbackToken(`${envelope.rollback_token}tamper`)).toBeNull();
  });

  it("maintains an immutable BLAKE3 + Ed25519 hash chain and pinpoints single-bit tampering", () => {
    const clock = new DeterministicNanosecondClock();
    const signer = new AegisCryptographicSigner();
    const chain = new ImmutableAuditChain(clock, signer);

    chain.append({
      incidentId: "INC-CHAIN-1",
      eventType: "STEP_1",
      actor: "aegis",
      payload: { notional_fp: "100000000" }
    });
    chain.append({
      incidentId: "INC-CHAIN-1",
      eventType: "STEP_2",
      actor: "aegis",
      payload: { notional_fp: "250000000" }
    });
    chain.append({
      incidentId: "INC-CHAIN-1",
      eventType: "STEP_3",
      actor: "aegis",
      payload: { notional_fp: "500000000" }
    });

    const cleanReport = chain.verifyChain();
    expect(cleanReport.valid).toBe(true);
    expect(cleanReport.entries_checked).toBe(3);

    // Clone and mutate payload of seq=2
    const snapshot = chain.getEntries().map((e) => ({
      ...e,
      payload: { ...e.payload }
    }));
    snapshot[1]!.payload.notional_fp = "999999999";

    const tamperedReport = chain.verifyChain(snapshot);
    expect(tamperedReport.valid).toBe(false);
    expect(tamperedReport.corrupted_at_seq).toBe(2);
    expect(tamperedReport.failure_reason).toContain("BLAKE3 payload digest tampered");
  });
});
