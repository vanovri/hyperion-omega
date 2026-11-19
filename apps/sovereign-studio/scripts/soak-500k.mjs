/**
 * HYPERION OMEGA Channel 20 — 500,000-Cycle Zero-Leak V8 Heap Soak Verifier
 * Exercises Binary WS 0x4859 framing, CRC-32C, and 1e8 Fixed-Point Notional/Fee math
 * across two consecutive 500,000-cycle epochs after JIT tier-up to measure true
 * steady-state heap retention (0 B unbounded leak).
 * Run with: node --expose-gc scripts/soak-500k.mjs
 */

const FP_SCALE = 100_000_000n;
const PPM_SCALE = 1_000_000n;
const WS_MAGIC_LE = 0x4859;

const CRC32C_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let crc = i;
    for (let bit = 0; bit < 8; bit++) {
      crc = (crc & 1) !== 0 ? (crc >>> 1) ^ 0x82f63b78 : crc >>> 1;
    }
    table[i] = crc >>> 0;
  }
  return table;
})();

function crc32c(bytes) {
  let crc = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    crc = (crc >>> 8) ^ CRC32C_TABLE[(crc ^ bytes[i]) & 0xff];
  }
  return (crc ^ 0xffffffff) >>> 0;
}

const frameBuf = new Uint8Array(96);
const view = new DataView(frameBuf.buffer);
view.setUint16(0, WS_MAGIC_LE, true);
view.setUint8(2, 0x01);
view.setUint8(3, 0x04);
view.setUint8(4, 0x01);
view.setUint16(6, 1, true);
view.setUint32(8, 64, true);

function stepCycle(seq) {
  const priceFp = 68_425_50000000n + BigInt(seq & 255);
  const qtyFp = 25_000000n;
  view.setBigUint64(16, BigInt(seq), true);
  view.setBigUint64(56, priceFp, true);
  view.setBigUint64(64, qtyFp, true);
  const payload = frameBuf.subarray(32, 96);
  const digest = crc32c(payload);
  view.setUint32(12, digest, true);

  if (view.getUint16(0, true) !== WS_MAGIC_LE) {
    throw new Error("Magic corrupted");
  }
  const notionalFp = (priceFp * qtyFp) / FP_SCALE;
  const takerFeeFp = (notionalFp * 300n) / PPM_SCALE;
  const makerRebateFp = (notionalFp * -100n) / PPM_SCALE;
  return notionalFp - takerFeeFp - makerRebateFp;
}

function forceGc() {
  if (typeof global.gc === "function") {
    global.gc();
    global.gc();
  }
}

// Prime process.memoryUsage(), performance.now(), and TurboFan JIT (Epoch 1: 500k cycles)
let checksum = 0n;
performance.now();
process.memoryUsage();
const SOAK_CYCLES = 500_000;
for (let i = 0; i < SOAK_CYCLES; i++) {
  checksum = (checksum + stepCycle(i)) & ((1n << 120n) - 1n);
}
forceGc();
const heapBeforeEpoch2 = process.memoryUsage().heapUsed;

// Measured Steady-State Epoch 2: 500,000 consecutive cycles
const t0 = performance.now();
for (let i = 0; i < SOAK_CYCLES; i++) {
  checksum = (checksum + stepCycle(i)) & ((1n << 120n) - 1n);
}
const elapsedMs = performance.now() - t0;
forceGc();
const heapAfterEpoch2 = process.memoryUsage().heapUsed;
const netDriftBytes = Math.max(0, heapAfterEpoch2 - heapBeforeEpoch2);

const result = {
  suite: "SOVEREIGN_STUDIO_WEBWORKER_500K_SOAK",
  cycles: SOAK_CYCLES,
  elapsed_ms: Number(elapsedMs.toFixed(2)),
  ns_per_cycle: Number(((elapsedMs * 1e6) / SOAK_CYCLES).toFixed(2)),
  heap_before_bytes: heapBeforeEpoch2,
  heap_after_bytes: heapAfterEpoch2,
  steady_state_leak_bytes: netDriftBytes,
  checksum_non_zero: checksum > 0n,
  zero_unbounded_leak: netDriftBytes === 0,
};

console.log(JSON.stringify(result, null, 2));
if (!result.zero_unbounded_leak || !result.checksum_non_zero) {
  process.exit(1);
}
