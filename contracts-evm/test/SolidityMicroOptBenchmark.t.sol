// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {console2} from "forge-std/console2.sol";
import {MerkleSumLib} from "../src/libraries/MerkleSumLib.sol";
import {EIP712Lib} from "../src/libraries/EIP712Lib.sol";
import {ReentrancyGuardTransient} from "../src/libraries/ReentrancyGuardTransient.sol";

/// @notice Pass 1 Baseline: Legacy SSTORE ReentrancyGuard + Solidity `abi.encodePacked` MSST & `abi.encode` EIP-712.
contract SettlementGasBaseline {
    uint256 private _status = 1;
    uint256 public counter;

    modifier nonReentrantSSTORE() {
        require(_status != 2, "REENTRANT");
        _status = 2;
        _;
        _status = 1;
    }

    function guardAndHashPass1(
        uint64 accountId,
        address owner,
        address asset,
        uint128 balanceFp,
        bytes32 siblingHash,
        uint128 siblingSum,
        EIP712Lib.WithdrawalIntent memory intent,
        bytes32 domainSep
    ) external nonReentrantSSTORE returns (bytes32 out) {
        bytes32 leaf = keccak256(abi.encodePacked(uint8(0x00), accountId, owner, asset, balanceFp));
        uint128 sum = leaf != bytes32(0) ? balanceFp + siblingSum : 0;
        bytes32 node = keccak256(abi.encodePacked(uint8(0x01), leaf, balanceFp, siblingHash, siblingSum));
        bytes32 structHash = keccak256(
            abi.encode(
                EIP712Lib.WITHDRAWAL_INTENT_TYPEHASH,
                intent.withdrawalId,
                intent.accountId,
                intent.recipient,
                intent.asset,
                intent.rawAmount,
                intent.amountFp,
                intent.stateEpoch,
                intent.nonce,
                intent.deadline,
                intent.chainId
            )
        );
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", domainSep, structHash));
        out = keccak256(abi.encodePacked(node, digest, sum));
        counter += 1;
    }
}

/// @notice Pass 2 Intermediate: Legacy SSTORE ReentrancyGuard + `calldata` struct + Yul MSST/EIP-712 assembly hashing.
contract SettlementGasPass2 {
    uint256 private _status = 1;
    uint256 public counter;

    modifier nonReentrantSSTORE() {
        require(_status != 2, "REENTRANT");
        _status = 2;
        _;
        _status = 1;
    }

    function guardAndHashPass2(
        uint64 accountId,
        address owner,
        address asset,
        uint128 balanceFp,
        bytes32 siblingHash,
        uint128 siblingSum,
        EIP712Lib.WithdrawalIntent calldata intent,
        bytes32 domainSep
    ) external nonReentrantSSTORE returns (bytes32 out) {
        bytes32 leaf = MerkleSumLib.computeLeafHash(accountId, owner, asset, balanceFp);
        (bytes32 node, uint128 sum) = MerkleSumLib.hashInternalNode(leaf, balanceFp, siblingHash, siblingSum);
        bytes32 structHash = EIP712Lib.hashWithdrawalIntent(intent);
        bytes32 digest = EIP712Lib.toTypedDataHash(domainSep, structHash);
        out = keccak256(abi.encodePacked(node, digest, sum));
        counter += 1;
    }
}

/// @notice Pass 3 Final (Production HYPERION CH18): Cancun EIP-1153 `TLOAD`/`TSTORE` + `calldata` + Yul Assembly Hashing.
contract SettlementGasPass3Final is ReentrancyGuardTransient {
    uint256 public counter;

    function guardAndHashPass3(
        uint64 accountId,
        address owner,
        address asset,
        uint128 balanceFp,
        bytes32 siblingHash,
        uint128 siblingSum,
        EIP712Lib.WithdrawalIntent calldata intent,
        bytes32 domainSep
    ) external nonReentrantTransient returns (bytes32 out) {
        bytes32 leaf = MerkleSumLib.computeLeafHash(accountId, owner, asset, balanceFp);
        (bytes32 node, uint128 sum) = MerkleSumLib.hashInternalNode(leaf, balanceFp, siblingHash, siblingSum);
        bytes32 structHash = EIP712Lib.hashWithdrawalIntent(intent);
        bytes32 digest = EIP712Lib.toTypedDataHash(domainSep, structHash);
        out = keccak256(abi.encodePacked(node, digest, sum));
        counter += 1;
    }
}

/// @title SolidityMicroOptBenchmarkTest
/// @notice Proves 3-pass EVM gas reduction & cryptographic bit-exact equivalence across Baseline -> Pass 2 -> Pass 3 Final.
contract SolidityMicroOptBenchmarkTest is Test {
    SettlementGasBaseline internal p1;
    SettlementGasPass2 internal p2;
    SettlementGasPass3Final internal p3;

    function setUp() public {
        p1 = new SettlementGasBaseline();
        p2 = new SettlementGasPass2();
        p3 = new SettlementGasPass3Final();
    }

    function test_3PassEVMGasOptimizationAndBitEquivalence() public {
        EIP712Lib.WithdrawalIntent memory intent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("bench_w_1"),
            accountId: 101,
            recipient: address(0xBEEF),
            asset: address(0xCAFE),
            rawAmount: 10 ether,
            amountFp: 10_00000000,
            stateEpoch: 1,
            nonce: 42,
            deadline: 1_900_000_000,
            chainId: 31337
        });
        bytes32 domainSep = EIP712Lib.computeDomainSeparator(
            keccak256("HyperionVault"),
            keccak256("1"),
            31337,
            address(this)
        );
        bytes32 siblingHash = keccak256("sibling");

        // Warm counter storage slots first so we measure steady-state execution cost cleanly
        p1.guardAndHashPass1(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);
        p2.guardAndHashPass2(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);
        p3.guardAndHashPass3(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);

        uint256 g1Start = gasleft();
        bytes32 r1 = p1.guardAndHashPass1(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);
        uint256 gasP1 = g1Start - gasleft();

        uint256 g2Start = gasleft();
        bytes32 r2 = p2.guardAndHashPass2(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);
        uint256 gasP2 = g2Start - gasleft();

        uint256 g3Start = gasleft();
        bytes32 r3 = p3.guardAndHashPass3(101, address(0xABCD), address(0xCAFE), 10_00000000, siblingHash, 20_00000000, intent, domainSep);
        uint256 gasP3 = g3Start - gasleft();

        assertEq(r1, r2, "Pass1 != Pass2 digest");
        assertEq(r2, r3, "Pass2 != Pass3 digest");
        assertLt(gasP2, gasP1, "Pass2 must beat Pass1");
        assertLt(gasP3, gasP2, "Pass3 must beat Pass2");

        console2.log("EVM_BENCH Pass1_Baseline_SSTORE_ABI_Gas:", gasP1);
        console2.log("EVM_BENCH Pass2_YulHash_Calldata_Gas:", gasP2);
        console2.log("EVM_BENCH Pass3_EIP1153_TLOAD_Yul_Gas:", gasP3);
        console2.log("EVM_BENCH Total_Saved_Gas:", gasP1 - gasP3);
    }
}
