import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  ConsensusProto,
  ZkSolvencyProto,
  MarginRiskProto,
  WasmQuantProto,
  SettlementL2Proto,
  OMEGA_GOVERNANCE,
} from "./index";

describe("@hyperion/omega-contracts wire roundtrip", () => {
  it("encodes and decodes Raft ReadIndexBarrier & ZK Solvency Receipt with strict bigint", () => {
    const barrier = ConsensusProto.ReadIndexBarrier.create({
      readRequestId: "ri-0001",
      term: 4n,
      leaderId: 1n,
      requiredReadIndex: 128n,
      quorumAckMask: 7n,
      servedViaLeaderLease: false,
      barrierLatencyNs: 95_000n,
      committedStateRootBlake3: new Uint8Array(32).fill(0xab),
    });
    const raw = ConsensusProto.ReadIndexBarrier.encode(barrier).finish();
    const dec = ConsensusProto.ReadIndexBarrier.decode(raw);
    assert.equal(dec.requiredReadIndex, 128n);

    const receipt = ZkSolvencyProto.ZKSolvencyReceipt.create({
      receiptId: "zk-ep-09",
      epochId: 9n,
      snapshotEngineSeq: 500_000n,
      proofSystem: ZkSolvencyProto.ZkProofSystem.ZK_PROOF_SYSTEM_GROTH16_BN254,
      selfVerifiedOk: true,
    });
    const zkDec = ZkSolvencyProto.ZKSolvencyReceipt.decode(
      ZkSolvencyProto.ZKSolvencyReceipt.encode(receipt).finish(),
    );
    assert.equal(zkDec.epochId, 9n);
    assert.equal(zkDec.selfVerifiedOk, true);
  });

  it("encodes OptionGreeksSurface, DeployStrategyModule, and WithdrawalEIP712Intent", () => {
    const surf = MarginRiskProto.OptionGreeksSurface.create({
      underlyingSymbol: "BTC-USDT",
      engineSeq: 901n,
      spotIndexPriceFp: 6_850_000_000_000n,
      surfaceArbitrageFree: true,
    });
    assert.equal(
      MarginRiskProto.OptionGreeksSurface.decode(
        MarginRiskProto.OptionGreeksSurface.encode(surf).finish(),
      ).spotIndexPriceFp,
      6_850_000_000_000n,
    );

    const mod = WasmQuantProto.DeployStrategyModule.create({
      strategyId: "delta_hedger",
      accountId: 55n,
      version: 1,
      limits: {
        maxLinearMemoryBytes: OMEGA_GOVERNANCE.WASM_MAX_LINEAR_MEMORY_BYTES,
        maxStackBytes: OMEGA_GOVERNANCE.WASM_MAX_STACK_BYTES,
        maxFuelPerTick: OMEGA_GOVERNANCE.WASM_DEFAULT_FUEL_PER_TICK,
        maxEpochWalltimeNs: OMEGA_GOVERNANCE.WASM_MAX_EPOCH_WALLTIME_NS,
        maxOrdersPerTick: 16,
        maxAbsPositionQtyFp: 1_000_000_000n,
      },
    });
    assert.equal(
      WasmQuantProto.DeployStrategyModule.decode(
        WasmQuantProto.DeployStrategyModule.encode(mod).finish(),
      ).limits?.maxLinearMemoryBytes,
      16 * 1024 * 1024,
    );

    const wd = SettlementL2Proto.WithdrawalEIP712Intent.create({
      withdrawalId: "wd-ts-1",
      chainId: OMEGA_GOVERNANCE.HYPERION_L2_CANONICAL_CHAIN_ID,
      accountId: 10042n,
      withdrawalNonce: 3n,
      epochId: 9n,
    });
    assert.equal(
      SettlementL2Proto.WithdrawalEIP712Intent.decode(
        SettlementL2Proto.WithdrawalEIP712Intent.encode(wd).finish(),
      ).chainId,
      994_201n,
    );
  });
});
