// Ultra-fast Node 24 native TS mutation test harness (~150ms/mutant) for CH06 fixed-point.ts + merkle.ts
import assert from "node:assert/strict";
import { hexToBytes } from "@noble/hashes/utils";
import * as fixedPoint from "../src/domain/fixed-point.ts";
import {
  buildDeterministicMerkleTree,
  computeAccountLeafBlake3,
  computeMerkleParentBlake3,
  createAlignedBlake3Scratch64,
  packAlignedMerkleSlab64,
  verifyMerkleLedgerProof,
  verifyMerklePathAligned64,
  verifyMerklePathZeroCopy
} from "../src/crypto/merkle.ts";

// 1. Fixed-point 1e8 exhaustive boundary checks
const maxU128 = (1n << 128n) - 1n;
const minI128 = -(1n << 127n);
const maxI128 = (1n << 127n) - 1n;

assert.equal(fixedPoint.parseFixedPointUint("0"), 0n);
assert.equal(fixedPoint.parseFixedPointUint(maxU128.toString(10)), maxU128);
assert.throws(() => fixedPoint.parseFixedPointUint("-1"), RangeError);
assert.throws(() => fixedPoint.parseFixedPointUint("12.34"), RangeError);
assert.throws(() => fixedPoint.parseFixedPointUint((maxU128 + 1n).toString(10)), RangeError);

assert.equal(fixedPoint.parseFixedPointInt(minI128.toString(10)), minI128);
assert.equal(fixedPoint.parseFixedPointInt(maxI128.toString(10)), maxI128);
assert.throws(() => fixedPoint.parseFixedPointInt("abc"), RangeError);
assert.throws(() => fixedPoint.parseFixedPointInt((minI128 - 1n).toString(10)), RangeError);
assert.throws(() => fixedPoint.parseFixedPointInt((maxI128 + 1n).toString(10)), RangeError);

assert.equal(fixedPoint.decimalToFixedPoint1e8("0"), 0n);
assert.equal(fixedPoint.decimalToFixedPoint1e8("68425.50"), 6_842_550_000_000n);
assert.equal(fixedPoint.decimalToFixedPoint1e8("-0.0025"), -250_000n);
assert.equal(fixedPoint.decimalToFixedPoint1e8("42"), 4_200_000_000n);
assert.throws(() => fixedPoint.decimalToFixedPoint1e8("1.123456789"), RangeError);
assert.throws(() => fixedPoint.decimalToFixedPoint1e8("99999999999999999999999999999999999999"), RangeError);
assert.throws(() => fixedPoint.decimalToFixedPoint1e8("-99999999999999999999999999999999999999"), RangeError);

assert.equal(fixedPoint.formatFixedPoint1e8(0n, true), "0.00");
assert.equal(fixedPoint.formatFixedPoint1e8(150_000_000n, true), "1.50");
assert.equal(fixedPoint.formatFixedPoint1e8(-123_450_000n, true), "-1.2345");
assert.equal(fixedPoint.formatFixedPoint1e8(-100_000_000n, false), "-1.00000000");
assert.equal(fixedPoint.formatFixedPoint1e8(8_553_187_500_000n), "85531.87500000");

assert.equal(fixedPoint.formatPpmAsPercent(0), "0.00%");
assert.equal(fixedPoint.formatPpmAsPercent(984_500), "98.45%");
assert.equal(fixedPoint.formatPpmAsPercent(-25_100n), "-2.51%");
assert.equal(fixedPoint.formatBpsAsPercent(0), "0.00%");
assert.equal(fixedPoint.formatBpsAsPercent(125), "1.25%");
assert.equal(fixedPoint.formatBpsAsPercent(-305n), "-3.05%");

assert.equal(fixedPoint.checkedNotionalFp(6_842_550_000_000n, 125_000_000n), 8_553_187_500_000n);
assert.equal(fixedPoint.checkedNotionalFp(0n, 100n), 0n);
assert.equal(fixedPoint.checkedNotionalFp(100n, 0n), 0n);
assert.equal(fixedPoint.checkedNotionalFp(3n, 33_333_333n), 0n);
assert.equal(fixedPoint.checkedNotionalFp(3n, 33_333_334n), 1n);
assert.throws(() => fixedPoint.checkedNotionalFp(-1n, 1n), RangeError);
assert.throws(() => fixedPoint.checkedNotionalFp(1n, -1n), RangeError);
assert.throws(() => fixedPoint.checkedNotionalFp(maxU128, 200_000_000n), RangeError);

assert.equal(fixedPoint.checkedFeeFp(8_553_187_500_000n, -200n), -1_710_637_500n);
assert.equal(fixedPoint.checkedFeeFp(8_553_187_500_000n, 500n), 4_276_593_750n);
assert.equal(fixedPoint.checkedFeeFp(0n, 100n), 0n);
assert.equal(fixedPoint.checkedFeeFp(999_999n, -1n), 0n);
assert.throws(() => fixedPoint.checkedFeeFp(-1n, 100n), RangeError);
assert.throws(() => fixedPoint.checkedFeeFp(maxU128, 2_000_000n), RangeError);
assert.throws(() => fixedPoint.checkedFeeFp(maxU128, -2_000_000n), RangeError);

assert.equal(fixedPoint.checkedAddI128(maxI128, 0n), maxI128);
assert.equal(fixedPoint.checkedAddI128(minI128, 0n), minI128);
assert.equal(fixedPoint.checkedAddI128(10n, 5n), 15n);
assert.throws(() => fixedPoint.checkedAddI128(maxI128, 1n), RangeError);
assert.throws(() => fixedPoint.checkedAddI128(minI128, -1n), RangeError);

assert.equal(fixedPoint.checkedSubI128(minI128, 0n), minI128);
assert.equal(fixedPoint.checkedSubI128(maxI128, 0n), maxI128);
assert.equal(fixedPoint.checkedSubI128(10n, 3n), 7n);
assert.throws(() => fixedPoint.checkedSubI128(minI128, 1n), RangeError);
assert.throws(() => fixedPoint.checkedSubI128(maxI128, -1n), RangeError);

assert.equal(fixedPoint.fromProtoU128(undefined), 0n);
assert.equal(fixedPoint.fromProtoI128(undefined), 0n);
for (const v of [0n, 1n, (1n << 92n) + 123_456_789n, maxU128]) {
  assert.equal(fixedPoint.fromProtoU128(fixedPoint.toProtoU128(v)), v);
}
for (const v of [0n, 1n, -1n, -((1n << 88n) + 987_654_321n), minI128, maxI128]) {
  assert.equal(fixedPoint.fromProtoI128(fixedPoint.toProtoI128(v)), v);
}
assert.throws(() => fixedPoint.toProtoU128(-1n), RangeError);
assert.throws(() => fixedPoint.toProtoU128(maxU128 + 1n), RangeError);
assert.throws(() => fixedPoint.toProtoI128(minI128 - 1n), RangeError);
assert.throws(() => fixedPoint.toProtoI128(maxI128 + 1n), RangeError);

// 2. Merkle + zero-alloc 65B BLAKE3 + conservation checks
const accounts = [
  { account_id: "300", balances: [{ asset: "USDT", available_fp: "100000000", locked_fp: "0", total_fp: "100000000" }] },
  {
    account_id: "100",
    balances: [
      { asset: "USDT", available_fp: "200000000", locked_fp: "50000000", total_fp: "250000000" },
      { asset: "BTC", available_fp: "100000000", locked_fp: "0", total_fp: "100000000" }
    ]
  },
  { account_id: "200", balances: [{ asset: "USDT", available_fp: "300000000", locked_fp: "0", total_fp: "300000000" }] }
];
const tree = buildDeterministicMerkleTree("77", accounts);
assert.equal(buildDeterministicMerkleTree("77", []).leafHashes.size, 0);
assert.throws(() => computeMerkleParentBlake3("deadbeef", tree.stateRootBlake3), RangeError);
assert.throws(() => computeMerkleParentBlake3(tree.stateRootBlake3, "deadbeef"), RangeError);

const expectedRoot32 = hexToBytes(tree.stateRootBlake3);
const expectedRootBuf = new ArrayBuffer(32);
new Uint8Array(expectedRootBuf).set(expectedRoot32);
const expectedRootWords = new BigUint64Array(expectedRootBuf);
const scratch65 = new Uint8Array(65);
const rollingBuf = new ArrayBuffer(32);
const rolling32 = new Uint8Array(rollingBuf);
const rollingWords = new BigUint64Array(rollingBuf);
const rollingU32 = new Uint32Array(rollingBuf);
const ws = createAlignedBlake3Scratch64();

for (const id of ["100", "200", "300"]) {
  const leafHex = tree.leafHashes.get(id);
  const path = tree.proofPaths.get(id);
  const leaf32 = hexToBytes(leafHex);
  const sibs = path.map((p) => hexToBytes(p.sibling_hash));
  const flags = path.map((p) => p.is_left_sibling);
  const slab = packAlignedMerkleSlab64(path);
  assert.equal(slab.buffer.byteLength % 64, 0);
  assert.equal(verifyMerklePathZeroCopy(leaf32, sibs, flags, expectedRoot32, scratch65, rolling32), true);
  assert.equal(verifyMerklePathAligned64(leaf32, slab, expectedRootWords, scratch65, rolling32, rollingWords, ws, rollingU32), true);

  const badLeaf = new Uint8Array(leaf32);
  badLeaf[31] ^= 1;
  assert.equal(verifyMerklePathZeroCopy(badLeaf, sibs, flags, expectedRoot32, scratch65, rolling32), false);
  assert.equal(verifyMerklePathAligned64(badLeaf, slab, expectedRootWords, scratch65, rolling32, rollingWords, ws, rollingU32), false);
}

// Verify full DTO audit & 1-tick conservation breach + tampered leaf + mismatched account balance
const acct100 = accounts[1];
const validDto = {
  account_id: "100",
  engine_seq: "77",
  leaf_blake3_hash: tree.leafHashes.get("100"),
  state_root_blake3: tree.stateRootBlake3,
  proof_path: tree.proofPaths.get("100"),
  balances: acct100.balances,
  system_total_deposits_fp: "650000000",
  system_total_withdrawals_fp: "0",
  system_total_user_equity_fp: "640000000",
  treasury_fee_pool_fp: "10000000",
  double_entry_balanced: true,
  proof_ts_ns: "1000"
};
assert.equal(verifyMerkleLedgerProof(validDto, "USDT").verdict, "VERIFIED_SOLVENT");
assert.equal(
  verifyMerkleLedgerProof({ ...validDto, treasury_fee_pool_fp: "10000001" }, "USDT").verdict,
  "CRITICAL_LEDGER_INVARIANT_BREACH"
);
assert.equal(
  verifyMerkleLedgerProof(
    { ...validDto, balances: [{ asset: "USDT", available_fp: "200000000", locked_fp: "50000000", total_fp: "250000001" }] },
    "USDT"
  ).verdict,
  "MERKLE_PROOF_TAMPERED"
);
assert.equal(
  verifyMerkleLedgerProof(validDto, "USDT", "0".repeat(64)).verdict,
  "MERKLE_PROOF_TAMPERED"
);
assert.equal(computeAccountLeafBlake3("77", "100", acct100.balances), tree.leafHashes.get("100"));
