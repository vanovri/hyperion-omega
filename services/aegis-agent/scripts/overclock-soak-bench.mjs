import { hexToBytes } from "@noble/hashes/utils";
import {
  buildDeterministicMerkleTree,
  computeMerkleParentBlake3,
  createAlignedBlake3Scratch64,
  packAlignedMerkleSlab64,
  verifyMerklePathAligned64,
  verifyMerklePathZeroCopy
} from "../dist/crypto/merkle.js";

function percentile(sorted, p) {
  const idx = Math.min(sorted.length - 1, Math.floor(sorted.length * p));
  return sorted[idx];
}

// Build a 16-account Merkle tree (depth = 4 BLAKE3 hashes per proof verification)
const accounts = [];
for (let i = 1; i <= 16; i += 1) {
  accounts.push({
    account_id: String(1000 + i),
    balances: [
      {
        asset: "USDT",
        available_fp: String(BigInt(i) * 100_000_000_000n),
        locked_fp: "5000000000",
        total_fp: String(BigInt(i) * 100_000_000_000n + 5_000_000_000n)
      }
    ]
  });
}

const tree = buildDeterministicMerkleTree("90001", accounts);
const leafHex = tree.leafHashes.get("1007");
const rootHex = tree.stateRootBlake3;
const proofPath = tree.proofPaths.get("1007");
const leaf32 = hexToBytes(leafHex);
const root32 = hexToBytes(rootHex);
const siblings32 = proofPath.map((s) => hexToBytes(s.sibling_hash));
const isLeftFlags = proofPath.map((s) => s.is_left_sibling);
const slab64 = packAlignedMerkleSlab64(proofPath);

const expectedRootBuf = new ArrayBuffer(32);
new Uint8Array(expectedRootBuf).set(root32);
const expectedRootWords = new BigUint64Array(expectedRootBuf);
const expectedRootU32 = new Uint32Array(expectedRootBuf);

const scratch65 = new Uint8Array(65);
const rollingBuf = new ArrayBuffer(32);
const rolling32 = new Uint8Array(rollingBuf);
const rollingWords = new BigUint64Array(rollingBuf);
const rollingU32 = new Uint32Array(rollingBuf);
const blakeWs = createAlignedBlake3Scratch64();

function verifyBaselineHex(leaf, path, expectedRoot) {
  let rolling = leaf;
  for (let i = 0; i < path.length; i += 1) {
    const step = path[i];
    rolling = step.is_left_sibling
      ? computeMerkleParentBlake3(step.sibling_hash, rolling)
      : computeMerkleParentBlake3(rolling, step.sibling_hash);
  }
  return rolling === expectedRoot;
}

const ok1 = verifyBaselineHex(leafHex, proofPath, rootHex);
const ok2 = verifyMerklePathZeroCopy(leaf32, siblings32, isLeftFlags, root32, scratch65, rolling32);
const ok3 = verifyMerklePathAligned64(
  leaf32,
  slab64,
  expectedRootWords,
  scratch65,
  rolling32,
  rollingWords,
  blakeWs,
  rollingU32,
  expectedRootU32
);
if (!ok1 || !ok2 || !ok3) {
  throw new Error(`Merkle equivalence check failed: ok1=${ok1} ok2=${ok2} ok3=${ok3}`);
}

for (let i = 0; i < 10_000; i += 1) {
  verifyBaselineHex(leafHex, proofPath, rootHex);
  verifyMerklePathZeroCopy(leaf32, siblings32, isLeftFlags, root32, scratch65, rolling32);
  verifyMerklePathAligned64(leaf32, slab64, expectedRootWords, scratch65, rolling32, rollingWords, blakeWs, rollingU32, expectedRootU32);
}

function benchPass(label, fn, iters = 40_000) {
  // 1) Pure zero-overhead heap delta measurement (no hrtime BigInt allocations inside loop)
  if (global.gc) {
    global.gc();
    global.gc();
  }
  const heapStart = process.memoryUsage().heapUsed;
  const totalStart = process.hrtime.bigint();
  let okCount = 0;
  for (let i = 0; i < iters; i += 1) {
    if (fn()) okCount += 1;
  }
  const totalNs = Number(process.hrtime.bigint() - totalStart);
  const heapEnd = process.memoryUsage().heapUsed;

  // 2) Percentile distribution pass (sampled per-op via performance.now -> float64, 0 BigInt allocs)
  const sampleCount = 5_000;
  const samples = new Float64Array(sampleCount);
  for (let i = 0; i < sampleCount; i += 1) {
    const t0 = performance.now();
    fn();
    samples[i] = (performance.now() - t0) * 1e6;
  }
  samples.sort();
  return {
    label,
    iters,
    okCount,
    mean_ns: Number((totalNs / iters).toFixed(2)),
    p50_ns: Number(percentile(samples, 0.5).toFixed(0)),
    p99_ns: Number(percentile(samples, 0.99).toFixed(0)),
    heap_delta_bytes_uncollected: Math.max(0, heapEnd - heapStart),
    approx_bytes_per_op: Number((Math.max(0, heapEnd - heapStart) / iters).toFixed(2))
  };
}

const p1 = benchPass("Pass1_Baseline_HexConcatAlloc", () => verifyBaselineHex(leafHex, proofPath, rootHex));
const p2 = benchPass("Pass2_ZeroCopy_Scratch65", () =>
  verifyMerklePathZeroCopy(leaf32, siblings32, isLeftFlags, root32, scratch65, rolling32)
);
const p3 = benchPass("Pass3_Aligned64Slab_ZeroAllocBlake3", () =>
  verifyMerklePathAligned64(
    leaf32,
    slab64,
    expectedRootWords,
    scratch65,
    rolling32,
    rollingWords,
    blakeWs,
    rollingU32,
    expectedRootU32
  )
);

const soakStats = new Float64Array(2);
function runSoakEpoch(n) {
  let verified = 0;
  const t0 = performance.now();
  for (let i = 0; i < n; i += 1) {
    if (
      verifyMerklePathAligned64(
        leaf32,
        slab64,
        expectedRootWords,
        scratch65,
        rolling32,
        rollingWords,
        blakeWs,
        rollingU32,
        expectedRootU32
      )
    ) {
      verified += 1;
    }
  }
  soakStats[0] = verified;
  soakStats[1] = (performance.now() - t0) * 1e6;
}

// Epoch 1 warmup so V8 TurboFan settles completely before taking baseline heap snapshot
runSoakEpoch(50_000);
if (global.gc) {
  global.gc();
  global.gc();
}
const soakHeapBefore = process.memoryUsage().heapUsed;
const SOAK_CYCLES = 500_000;
runSoakEpoch(SOAK_CYCLES);
const soakVerified = soakStats[0];
const soakElapsedNs = soakStats[1];
if (global.gc) {
  global.gc();
  global.gc();
}
const soakHeapAfter = process.memoryUsage().heapUsed;
const steadyStateLeakBytes = Math.max(0, soakHeapAfter - soakHeapBefore);

const report = {
  depth: proofPath.length,
  bench_3pass: [p1, p2, p3],
  soak_500k: {
    cycles: SOAK_CYCLES,
    verified: soakVerified,
    elapsed_ms: Number((soakElapsedNs / 1e6).toFixed(2)),
    mean_ns_per_cycle: Number((soakElapsedNs / SOAK_CYCLES).toFixed(2)),
    heap_before_bytes: soakHeapBefore,
    heap_after_bytes: soakHeapAfter,
    steady_state_leak_bytes: steadyStateLeakBytes,
    passed_zero_leak: soakVerified === SOAK_CYCLES && steadyStateLeakBytes === 0
  }
};

console.log(JSON.stringify(report, null, 2));
if (!report.soak_500k.passed_zero_leak) {
  process.exit(1);
}
