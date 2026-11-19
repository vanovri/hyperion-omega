// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";

import {HyperionVault} from "../src/HyperionVault.sol";
import {SolvencyStateAnchor} from "../src/SolvencyStateAnchor.sol";
import {CensorshipEscapeHatch} from "../src/CensorshipEscapeHatch.sol";
import {Groth16BN254SolvencyVerifier} from "../src/Groth16BN254SolvencyVerifier.sol";
import {MockERC20} from "./mocks/MockTokensAndAttackers.sol";
import {BN254Groth16TestProver} from "./helpers/SettlementTestHelpers.sol";
import {SettlementActorHandler} from "./handlers/SettlementActorHandler.sol";

/// @title HyperionSettlementInvariantTest
/// @notice Stateful Foundry Invariant Fuzzing Campaign proving Eternal Vault Solvency across
///         thousands of interleaved deposits, off-chain trades, ZK epoch anchors, EIP-712 withdrawals,
///         malleability/reentrancy attacks, and Escape Hatch forced exits.
contract HyperionSettlementInvariantTest is StdInvariant, Test {
    uint256 internal constant SEQUENCER_PK = 0x5E05E0999;

    address internal proposer = makeAddr("invProposer");
    address internal guardian = makeAddr("invGuardian");
    address internal relayer = makeAddr("invRelayer");

    HyperionVault internal vault;
    SolvencyStateAnchor internal anchor;
    CensorshipEscapeHatch internal escapeHatch;
    Groth16BN254SolvencyVerifier internal zkVerifier;

    MockERC20 internal usdt;
    MockERC20 internal wbtc;
    SettlementActorHandler internal handler;

    function setUp() public {
        Groth16BN254SolvencyVerifier.VerifyingKey memory vk = BN254Groth16TestProver.buildVerifyingKey();
        zkVerifier = new Groth16BN254SolvencyVerifier(vk);

        vault = new HyperionVault(
            proposer,
            guardian,
            vm.addr(SEQUENCER_PK),
            30 minutes,
            40, // 40-block escape hatch liveness window
            2 days
        );

        anchor = new SolvencyStateAnchor(address(vault), address(zkVerifier), relayer, 10_000);
        escapeHatch = new CensorshipEscapeHatch(address(vault), address(anchor), 40);
        anchor.configureEscapeHatch(address(escapeHatch));

        usdt = new MockERC20("Tether USD", "USDT", 6, true);
        wbtc = new MockERC20("Wrapped BTC", "WBTC", 8, false);

        address[] memory assets = new address[](3);
        assets[0] = address(0);
        assets[1] = address(usdt);
        assets[2] = address(wbtc);

        uint8[] memory decs = new uint8[](3);
        decs[0] = 18;
        decs[1] = 6;
        decs[2] = 8;

        uint192[] memory dailyLimits = new uint192[](3);
        dailyLimits[0] = 500 ether;
        dailyLimits[1] = 5_000_000e6;
        dailyLimits[2] = 500e8;

        uint256[] memory largeThresholds = new uint256[](3);
        largeThresholds[0] = 25 ether;
        largeThresholds[1] = 100_000e6;
        largeThresholds[2] = 5e8;

        vm.prank(proposer);
        vault.initializeSettlementSuite(
            address(anchor),
            address(escapeHatch),
            assets,
            decs,
            dailyLimits,
            largeThresholds
        );

        handler = new SettlementActorHandler(
            vault,
            anchor,
            escapeHatch,
            usdt,
            wbtc,
            relayer,
            SEQUENCER_PK
        );

        targetContract(address(handler));
    }

    /// @notice CORE ETERNAL INVARIANT:
    ///         At every single block and after every random state transition without exception:
    ///         Actual token balance in HyperionVault >= Sum of all valid unclaimed claims
    ///         (unclaimed anchored Merkle-Sum leaves + unanchored deposits + queued EIP-712 withdrawals).
    function invariant_EternalVaultBalanceCoversAllValidUnclaimedClaims() public view {
        for (uint256 a = 0; a < handler.NUM_ASSETS(); ++a) {
            address asset = handler.assets(a);
            uint256 actualBalanceRaw = vault.actualAssetBalanceRaw(asset);
            uint256 trueValidClaimsRaw = handler.computeTrueUnclaimedValidClaimsRaw(a);
            uint256 vaultTrackedClaimsRaw = vault.totalProtectedClaimsRaw(asset);

            assertGe(
                actualBalanceRaw,
                trueValidClaimsRaw,
                "CRITICAL: Actual Vault balance dropped below sum of true valid unclaimed claims!"
            );
            assertGe(
                actualBalanceRaw,
                vaultTrackedClaimsRaw,
                "CRITICAL: Actual Vault balance dropped below Vault tracked segregated buckets!"
            );
            assertEq(
                vaultTrackedClaimsRaw,
                trueValidClaimsRaw,
                "CRITICAL: Vault segregated custody buckets diverged from ground-truth claims!"
            );
        }
    }

    /// @notice EXACT CUSTODY CONSERVATION LAW:
    ///         ActualVaultBalance + TotalPaidOut == TotalDeposited + DirectDonations
    function invariant_ExactCustodyFlowConservation() public view {
        for (uint256 a = 0; a < handler.NUM_ASSETS(); ++a) {
            address asset = handler.assets(a);
            uint256 actualRaw = vault.actualAssetBalanceRaw(asset);
            uint256 lhs = actualRaw + handler.ghostWithdrawnRaw(a);
            uint256 rhs = handler.ghostDepositedRaw(a) + handler.ghostDonatedRaw(a);
            assertEq(lhs, rhs, "CRITICAL: Custody value created or destroyed!");
        }
    }

    /// @notice ZERO DOUBLE-SPEND ACROSS EIP-712 & ESCAPE HATCH:
    ///         Cumulative epoch withdrawals per account never exceed committed MSST leaf balance.
    function invariant_ZeroPerAccountEpochOverdraw() public view {
        uint64 ep = anchor.latestEpoch();
        if (ep == 0) return;

        for (uint256 a = 0; a < handler.NUM_ASSETS(); ++a) {
            address asset = handler.assets(a);
            for (uint256 i = 0; i < handler.NUM_ACTORS(); ++i) {
                uint128 drawnFp = vault.epochAccountWithdrawnFp(ep, handler.accountIds(i), asset);
                uint128 leafFp = handler.anchoredLeafFp(i, a);
                assertLe(drawnFp, leafFp, "CRITICAL: Account withdrew more than anchored MSST leaf!");
            }
        }
    }

    /// @notice SEQUENCE WATERMARK INTEGRITY:
    ///         Anchored deposit watermark never exceeds Vault deposit head, and Escape Hatch freeze is symmetric.
    function invariant_SequenceAndFreezeSymmetry() public view {
        assertLe(anchor.lastProcessedDepositSeq(), vault.latestDepositSeq());
        assertEq(vault.isEscapeHatchFrozen(), anchor.isEscapeHatchFrozen());
    }
}
