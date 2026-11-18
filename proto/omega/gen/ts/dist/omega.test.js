"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const node_test_1 = require("node:test");
const strict_1 = __importDefault(require("node:assert/strict"));
const index_1 = require("./index");
(0, node_test_1.describe)("@hyperion/omega-contracts wire roundtrip", () => {
    (0, node_test_1.it)("encodes and decodes Raft ReadIndexBarrier & ZK Solvency Receipt with strict bigint", () => {
        const barrier = index_1.ConsensusProto.ReadIndexBarrier.create({
            readRequestId: "ri-0001",
            term: 4n,
            leaderId: 1n,
            requiredReadIndex: 128n,
            quorumAckMask: 7n,
            servedViaLeaderLease: false,
            barrierLatencyNs: 95000n,
            committedStateRootBlake3: new Uint8Array(32).fill(0xab),
        });
        const raw = index_1.ConsensusProto.ReadIndexBarrier.encode(barrier).finish();
        const dec = index_1.ConsensusProto.ReadIndexBarrier.decode(raw);
        strict_1.default.equal(dec.requiredReadIndex, 128n);
        const receipt = index_1.ZkSolvencyProto.ZKSolvencyReceipt.create({
            receiptId: "zk-ep-09",
            epochId: 9n,
            snapshotEngineSeq: 500000n,
            proofSystem: index_1.ZkSolvencyProto.ZkProofSystem.ZK_PROOF_SYSTEM_GROTH16_BN254,
            selfVerifiedOk: true,
        });
        const zkDec = index_1.ZkSolvencyProto.ZKSolvencyReceipt.decode(index_1.ZkSolvencyProto.ZKSolvencyReceipt.encode(receipt).finish());
        strict_1.default.equal(zkDec.epochId, 9n);
        strict_1.default.equal(zkDec.selfVerifiedOk, true);
    });
    (0, node_test_1.it)("encodes OptionGreeksSurface, DeployStrategyModule, and WithdrawalEIP712Intent", () => {
        const surf = index_1.MarginRiskProto.OptionGreeksSurface.create({
            underlyingSymbol: "BTC-USDT",
            engineSeq: 901n,
            spotIndexPriceFp: 6850000000000n,
            surfaceArbitrageFree: true,
        });
        strict_1.default.equal(index_1.MarginRiskProto.OptionGreeksSurface.decode(index_1.MarginRiskProto.OptionGreeksSurface.encode(surf).finish()).spotIndexPriceFp, 6850000000000n);
        const mod = index_1.WasmQuantProto.DeployStrategyModule.create({
            strategyId: "delta_hedger",
            accountId: 55n,
            version: 1,
            limits: {
                maxLinearMemoryBytes: index_1.OMEGA_GOVERNANCE.WASM_MAX_LINEAR_MEMORY_BYTES,
                maxStackBytes: index_1.OMEGA_GOVERNANCE.WASM_MAX_STACK_BYTES,
                maxFuelPerTick: index_1.OMEGA_GOVERNANCE.WASM_DEFAULT_FUEL_PER_TICK,
                maxEpochWalltimeNs: index_1.OMEGA_GOVERNANCE.WASM_MAX_EPOCH_WALLTIME_NS,
                maxOrdersPerTick: 16,
                maxAbsPositionQtyFp: 1000000000n,
            },
        });
        strict_1.default.equal(index_1.WasmQuantProto.DeployStrategyModule.decode(index_1.WasmQuantProto.DeployStrategyModule.encode(mod).finish()).limits?.maxLinearMemoryBytes, 16 * 1024 * 1024);
        const wd = index_1.SettlementL2Proto.WithdrawalEIP712Intent.create({
            withdrawalId: "wd-ts-1",
            chainId: index_1.OMEGA_GOVERNANCE.HYPERION_L2_CANONICAL_CHAIN_ID,
            accountId: 10042n,
            withdrawalNonce: 3n,
            epochId: 9n,
        });
        strict_1.default.equal(index_1.SettlementL2Proto.WithdrawalEIP712Intent.decode(index_1.SettlementL2Proto.WithdrawalEIP712Intent.encode(wd).finish()).chainId, 994201n);
    });
});
//# sourceMappingURL=omega.test.js.map