// Ultra-fast Node 24 native TS mutation test harness (~100ms/mutant) for CH06 src/domain/fixed-point.ts
import assert from "node:assert/strict";
import * as fixedPoint from "../src/domain/fixed-point.ts";

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
assert.equal(fixedPoint.decimalToFixedPoint1e8("-1701411834604692317316873037158.84105728"), minI128);
assert.equal(fixedPoint.decimalToFixedPoint1e8("1701411834604692317316873037158.84105727"), maxI128);
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
assert.equal(fixedPoint.checkedNotionalFp(maxU128, fixedPoint.FP_SCALE), maxU128);
assert.throws(() => fixedPoint.checkedNotionalFp(-1n, 1n), RangeError);
assert.throws(() => fixedPoint.checkedNotionalFp(1n, -1n), RangeError);
assert.throws(() => fixedPoint.checkedNotionalFp(maxU128, 200_000_000n), RangeError);

assert.equal(fixedPoint.checkedFeeFp(8_553_187_500_000n, -200n), -1_710_637_500n);
assert.equal(fixedPoint.checkedFeeFp(8_553_187_500_000n, 500n), 4_276_593_750n);
assert.equal(fixedPoint.checkedFeeFp(0n, 100n), 0n);
assert.equal(fixedPoint.checkedFeeFp(999_999n, -1n), 0n);
assert.equal(fixedPoint.checkedFeeFp(maxI128, fixedPoint.PPM_SCALE), maxI128);
assert.equal(fixedPoint.checkedFeeFp(1n << 127n, -fixedPoint.PPM_SCALE), minI128);
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
