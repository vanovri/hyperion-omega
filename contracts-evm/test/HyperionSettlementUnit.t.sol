// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {HyperionVault} from "../src/HyperionVault.sol";
import {SolvencyStateAnchor} from "../src/SolvencyStateAnchor.sol";
import {CensorshipEscapeHatch} from "../src/CensorshipEscapeHatch.sol";
import {Groth16BN254SolvencyVerifier} from "../src/Groth16BN254SolvencyVerifier.sol";
import {IZKSolvencyVerifier} from "../src/interfaces/IZKSolvencyVerifier.sol";
import {MerkleSumLib} from "../src/libraries/MerkleSumLib.sol";
import {EIP712Lib} from "../src/libraries/EIP712Lib.sol";
import {SafeTransferLib} from "../src/libraries/SafeTransferLib.sol";
import {ReentrancyGuardTransient} from "../src/libraries/ReentrancyGuardTransient.sol";
import {
    MockERC20,
    FeeOnTransferERC20,
    MaliciousReentrantERC20,
    MaliciousETHReentrancyAttacker
} from "./mocks/MockTokensAndAttackers.sol";
import {BN254Groth16TestProver, MerkleSumTreeHelper} from "./helpers/SettlementTestHelpers.sol";

/// @title HyperionSettlementUnitTest
/// @notice Institutional unit, boundary, cryptographic, and adversarial exploit test suite for Channel 18.
contract HyperionSettlementUnitTest is Test {
    uint256 internal constant SECP256K1_N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    uint256 internal constant SEQUENCER_PK = 0xA11CE5E0001;
    uint256 internal constant ROGUE_PK = 0xBADBAD00002;

    address internal proposer = makeAddr("governanceProposer");
    address internal guardian = makeAddr("guardian");
    address internal relayer = makeAddr("chainRelayer");
    address internal sequencerSigner;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");
    address internal charlie = makeAddr("charlie");
    address internal coldWallet = makeAddr("coldWallet");

    HyperionVault internal vault;
    SolvencyStateAnchor internal anchor;
    CensorshipEscapeHatch internal escapeHatch;
    Groth16BN254SolvencyVerifier internal zkVerifier;

    MockERC20 internal usdt; // 6 decimals, non-standard empty return (mainnet USDT behavior)
    MockERC20 internal wbtc; // 8 decimals, standard return

    uint64 internal constant COOLDOWN_SEC = 1 hours;
    uint64 internal constant ESCAPE_TIMEOUT_BLOCKS = 50;
    uint256 internal constant TIMELOCK_DELAY = 2 days;
    uint32 internal constant MIN_RESERVE_BPS = 10_000; // 100.00%

    function setUp() public {
        sequencerSigner = vm.addr(SEQUENCER_PK);

        Groth16BN254SolvencyVerifier.VerifyingKey memory vk = BN254Groth16TestProver.buildVerifyingKey();
        zkVerifier = new Groth16BN254SolvencyVerifier(vk);

        vault = new HyperionVault(
            proposer,
            guardian,
            sequencerSigner,
            COOLDOWN_SEC,
            ESCAPE_TIMEOUT_BLOCKS,
            TIMELOCK_DELAY
        );

        anchor = new SolvencyStateAnchor(address(vault), address(zkVerifier), relayer, MIN_RESERVE_BPS);
        escapeHatch = new CensorshipEscapeHatch(address(vault), address(anchor), ESCAPE_TIMEOUT_BLOCKS);

        anchor.configureEscapeHatch(address(escapeHatch));

        usdt = new MockERC20("Tether USD", "USDT", 6, true);
        wbtc = new MockERC20("Wrapped BTC", "WBTC", 8, false);

        address[] memory assets = new address[](3);
        assets[0] = address(0); // Native ETH (18 decimals)
        assets[1] = address(usdt);
        assets[2] = address(wbtc);

        uint8[] memory decs = new uint8[](3);
        decs[0] = 18;
        decs[1] = 6;
        decs[2] = 8;

        uint192[] memory dailyLimits = new uint192[](3);
        dailyLimits[0] = 100 ether;
        dailyLimits[1] = 500_000e6;
        dailyLimits[2] = 20e8;

        uint256[] memory largeThresholds = new uint256[](3);
        largeThresholds[0] = 10 ether;
        largeThresholds[1] = 50_000e6;
        largeThresholds[2] = 2e8;

        vm.prank(proposer);
        vault.initializeSettlementSuite(
            address(anchor),
            address(escapeHatch),
            assets,
            decs,
            dailyLimits,
            largeThresholds
        );

        vm.deal(alice, 1_000 ether);
        vm.deal(bob, 1_000 ether);
        vm.deal(charlie, 1_000 ether);

        usdt.mint(alice, 1_000_000e6);
        usdt.mint(bob, 1_000_000e6);
        wbtc.mint(alice, 100e8);
        wbtc.mint(bob, 100e8);
    }

    // =========================================================================
    // 1. DEPOSIT PRECISION, NON-STANDARD USDT & FEE-ON-TRANSFER DEFENSE
    // =========================================================================
    function test_DepositNativeAndMultiDecimalERC20_ExactFixedPointScaling() public {
        // Alice deposits 2.5 ETH -> 250_000_000 fp
        vm.prank(alice);
        (uint64 seq1, uint128 fp1) = vault.depositNative{value: 2.5 ether}(101);
        assertEq(seq1, 1);
        assertEq(fp1, 250_000_000);
        assertEq(vault.unanchoredDepositsRaw(address(0)), 2.5 ether);

        // Alice deposits 1,250.50 USDT (6 decimals = 1_250_500_000 raw) -> 125_050_000_000 fp
        vm.startPrank(alice);
        usdt.approve(address(vault), 1_250_500_000);
        (uint64 seq2, uint128 fp2) = vault.depositERC20(address(usdt), 1_250_500_000, 101);
        vm.stopPrank();
        assertEq(seq2, 2);
        assertEq(fp2, 125_050_000_000);
        assertEq(vault.unanchoredDepositsRaw(address(usdt)), 1_250_500_000);

        // Bob deposits 1.75 WBTC (8 decimals = 175_000_000 raw) -> 175_000_000 fp
        vm.startPrank(bob);
        wbtc.approve(address(vault), 175_000_000);
        (uint64 seq3, uint128 fp3) = vault.depositERC20(address(wbtc), 175_000_000, 102);
        vm.stopPrank();
        assertEq(seq3, 3);
        assertEq(fp3, 175_000_000);
    }

    function test_RevertWhen_DepositNativeHasSubFixedPointWeiDust() public {
        // 1e8 scale on 18-decimal ETH requires multiples of 1e10 wei
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.UnrepresentableRawDust.selector, 1 ether + 1, 1e10)
        );
        vault.depositNative{value: 1 ether + 1}(101);
    }

    function test_RevertWhen_FeeOnTransferTokenDeposited() public {
        FeeOnTransferERC20 fot = new FeeOnTransferERC20();
        _timelockCall(
            address(vault),
            abi.encodeCall(HyperionVault.registerAsset, (address(fot), 18, 1000 ether, 100 ether)),
            bytes32("reg_fot")
        );

        fot.mint(alice, 10 ether);
        vm.startPrank(alice);
        fot.approve(address(vault), 10 ether);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.FeeOnTransferTokenUnsupported.selector, 10 ether, 9.9 ether)
        );
        vault.depositERC20(address(fot), 10 ether, 101);
        vm.stopPrank();
    }

    // =========================================================================
    // 2. SOLVENCY STATE ANCHOR & BN254 GROTH16 ZK PROOF GATES
    // =========================================================================
    function test_CommitStateRoot_VerifiesBN254Groth16AndGraduatesDeposits() public {
        vm.prank(alice);
        vault.depositNative{value: 20 ether}(101);
        vm.prank(bob);
        vault.depositNative{value: 30 ether}(102);

        (
            SolvencyStateAnchor.AssetLiabilityInput[] memory decls,
            ,

        ) = _buildThreeAssetEpochDeclarations(
                1,
                100,
                20_00000000, // Alice 20 ETH
                30_00000000, // Bob 30 ETH
                0,
                0
            );

        vm.prank(relayer);
        bytes32 stateRoot = anchor.commitStateRoot(1, 100, 2, keccak256("wal_100"), decls);
        assertTrue(stateRoot != bytes32(0));
        assertEq(anchor.latestEpoch(), 1);
        assertEq(anchor.latestEngineSeq(), 100);
        assertEq(anchor.lastProcessedDepositSeq(), 2);
        assertEq(vault.unanchoredDepositsRaw(address(0)), 0);
        assertEq(vault.remainingAnchoredLiabilitiesRaw(address(0)), 50 ether);
    }

    function test_RevertWhen_AnchorSequenceGapOrNonMonotonicEngineSeq() public {
        vm.prank(alice);
        vault.depositNative{value: 10 ether}(101);

        (SolvencyStateAnchor.AssetLiabilityInput[] memory declsEpoch2, , ) = _buildThreeAssetEpochDeclarations(
            2,
            100,
            10_00000000,
            0,
            0,
            0
        );

        // Skipping epoch 1 -> epoch 2 must revert
        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(SolvencyStateAnchor.EpochSequenceMismatch.selector, 1, 2));
        anchor.commitStateRoot(2, 100, 1, keccak256("wal"), declsEpoch2);

        // Commit valid epoch 1
        (SolvencyStateAnchor.AssetLiabilityInput[] memory decls1, , ) = _buildThreeAssetEpochDeclarations(
            1,
            100,
            10_00000000,
            0,
            0,
            0
        );
        vm.prank(relayer);
        anchor.commitStateRoot(1, 100, 1, keccak256("wal_1"), decls1);

        // Non-monotonic engineSeq == 100 on epoch 2 must revert
        (SolvencyStateAnchor.AssetLiabilityInput[] memory decls2SameSeq, , ) = _buildThreeAssetEpochDeclarations(
            2,
            100,
            10_00000000,
            0,
            0,
            0
        );
        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(SolvencyStateAnchor.EngineSeqNotMonotonic.selector, 100, 100));
        anchor.commitStateRoot(2, 100, 1, keccak256("wal_2"), decls2SameSeq);
    }

    function test_RevertWhen_DeclaredLiabilitiesExceedVaultReserveByOneTick() public {
        vm.prank(alice);
        vault.depositNative{value: 10 ether}(101);

        // Declare 10.00000001 ETH (10_00000001 fp = 10 ether + 1e10 wei) when Vault only holds 10 ether!
        (SolvencyStateAnchor.AssetLiabilityInput[] memory decls, , ) = _buildThreeAssetEpochDeclarations(
            1,
            50,
            10_00000001,
            0,
            0,
            0
        );

        vm.prank(relayer);
        vm.expectRevert(
            abi.encodeWithSelector(
                SolvencyStateAnchor.ReserveRatioBelowMinimum.selector,
                address(0),
                10 ether,
                10 ether + 1e10,
                MIN_RESERVE_BPS
            )
        );
        anchor.commitStateRoot(1, 50, 1, keccak256("wal_insolvent"), decls);
    }

    function test_RevertWhen_MinReserveRatio120PercentViolated() public {
        // Governance upgrades minReserveRatioBps to 12,000 (120%)
        _timelockCall(
            address(anchor),
            abi.encodeCall(SolvencyStateAnchor.setMinReserveRatioBps, (12_000)),
            bytes32("ratio_120")
        );
        assertEq(anchor.minReserveRatioBps(), 12_000);

        vm.prank(alice);
        vault.depositNative{value: 10 ether}(101);
        // Donate 1.99 ether treasury buffer (total 11.99 ether < 12.00 ether required for 10 ETH liability)
        vm.deal(address(vault), 11.99 ether);

        (SolvencyStateAnchor.AssetLiabilityInput[] memory decls, , ) = _buildThreeAssetEpochDeclarations(
            1,
            10,
            10_00000000,
            0,
            0,
            0
        );

        vm.prank(relayer);
        vm.expectRevert(
            abi.encodeWithSelector(
                SolvencyStateAnchor.ReserveRatioBelowMinimum.selector,
                address(0),
                11.99 ether,
                12 ether,
                12_000
            )
        );
        anchor.commitStateRoot(1, 10, 1, keccak256("wal_under_120"), decls);

        // Top up buffer to exact 12.00 ether -> succeeds!
        vm.deal(address(vault), 12 ether);
        (SolvencyStateAnchor.AssetLiabilityInput[] memory declsValid, , ) = _buildThreeAssetEpochDeclarations(
            1,
            10,
            10_00000000,
            0,
            0,
            0
        );
        vm.prank(relayer);
        anchor.commitStateRoot(1, 10, 1, keccak256("wal_ok_120"), declsValid);
        assertEq(anchor.latestEpoch(), 1);
    }

    function test_RevertWhen_ForgedBN254Groth16ProofSubmitted() public {
        vm.prank(alice);
        vault.depositNative{value: 10 ether}(101);

        (SolvencyStateAnchor.AssetLiabilityInput[] memory decls, , ) = _buildThreeAssetEpochDeclarations(
            1,
            25,
            10_00000000,
            0,
            0,
            0
        );

        // Corrupt proof C point by substituting G1 generator (1, 2) (valid on-curve point, wrong discrete log!)
        (
            uint256 ax,
            uint256 ay,
            uint256 bx1,
            uint256 bx0,
            uint256 by1,
            uint256 by0,
            ,

        ) = abi.decode(decls[0].zkProof, (uint256, uint256, uint256, uint256, uint256, uint256, uint256, uint256));
        decls[0].zkProof = abi.encode(ax, ay, bx1, bx0, by1, by0, uint256(1), uint256(2));

        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(SolvencyStateAnchor.InvalidZKSolvencyProof.selector, 1, address(0)));
        anchor.commitStateRoot(1, 25, 1, keccak256("wal_forged"), decls);
    }

    // =========================================================================
    // 3. EIP-712 WITHDRAWALS, MALLEABILITY, REPLAY & CIRCUIT BREAKER
    // =========================================================================
    function test_EIP712Withdrawal_InstantSmallAndDelayedWhaleQueue() public {
        (
            bytes32[][][] memory ethTreesH,
            uint128[][][] memory ethTreesS
        ) = _seedAndAnchorAliceBobEth(40_00000000, 30_00000000);

        MerkleSumLib.ProofStep[] memory aliceProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 0);

        // 1. Small withdrawal (< 10 ETH threshold): 4 ETH -> instant unlock (cooldown = 0)
        EIP712Lib.WithdrawalIntent memory smallIntent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_small_1"),
            accountId: 101,
            recipient: coldWallet,
            asset: address(0),
            rawAmount: 4 ether,
            amountFp: 4_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 300),
            chainId: block.chainid
        });
        bytes memory sigSmall = _signIntent(SEQUENCER_PK, smallIntent);

        vm.prank(alice);
        uint64 unlockSmall = vault.queueWithdrawal(smallIntent, sigSmall, alice, 40_00000000, aliceProof);
        assertEq(unlockSmall, block.timestamp);

        uint256 coldBefore = coldWallet.balance;
        vault.executeWithdrawal(smallIntent.withdrawalId);
        assertEq(coldWallet.balance - coldBefore, 4 ether);

        // 2. Large whale withdrawal (>= 10 ETH threshold): 15 ETH -> delayed by COOLDOWN_SEC (1 hour)
        EIP712Lib.WithdrawalIntent memory whaleIntent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_whale_2"),
            accountId: 101,
            recipient: coldWallet,
            asset: address(0),
            rawAmount: 15 ether,
            amountFp: 15_00000000,
            stateEpoch: 1,
            nonce: 2,
            deadline: uint64(block.timestamp + 300),
            chainId: block.chainid
        });
        bytes memory sigWhale = _signIntent(SEQUENCER_PK, whaleIntent);

        vm.prank(alice);
        uint64 unlockWhale = vault.queueWithdrawal(whaleIntent, sigWhale, alice, 40_00000000, aliceProof);
        assertEq(unlockWhale, block.timestamp + COOLDOWN_SEC);

        // Premature execution reverts
        vm.expectRevert(
            abi.encodeWithSelector(
                HyperionVault.WithdrawalCooldownActive.selector,
                whaleIntent.withdrawalId,
                unlockWhale,
                block.timestamp
            )
        );
        vault.executeWithdrawal(whaleIntent.withdrawalId);

        // Warp exact cooldown -> succeeds
        vm.warp(block.timestamp + COOLDOWN_SEC);
        vault.executeWithdrawal(whaleIntent.withdrawalId);
        assertEq(coldWallet.balance - coldBefore, 19 ether);
        // 70 ETH anchored - 19 ETH withdrawn = 51 ETH remaining anchored liabilities
        assertEq(vault.remainingAnchoredLiabilitiesRaw(address(0)), 51 ether);
    }

    function test_RevertWhen_EIP712SignatureMalleabilityCrossChainReplayOrExpired() public {
        (
            bytes32[][][] memory ethTreesH,
            uint128[][][] memory ethTreesS
        ) = _seedAndAnchorAliceBobEth(20_00000000, 10_00000000);

        MerkleSumLib.ProofStep[] memory aliceProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 0);

        EIP712Lib.WithdrawalIntent memory intent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_sec_1"),
            accountId: 101,
            recipient: alice,
            asset: address(0),
            rawAmount: 5 ether,
            amountFp: 5_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 60),
            chainId: block.chainid
        });

        // A. EIP-2 Signature Malleability Attack: flip (v, s) -> (v ^ 1, SECP256K1_N - s)
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
        bytes32 digest = EIP712Lib.toTypedDataHash(vault.domainSeparator(), structHash);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(SEQUENCER_PK, digest);
        bytes32 malleableS = bytes32(SECP256K1_N - uint256(s));
        uint8 malleableV = v == 27 ? 28 : 27;
        bytes memory malleableSig = abi.encodePacked(r, malleableS, malleableV);

        vm.expectRevert(abi.encodeWithSelector(EIP712Lib.InvalidSignatureSValue.selector, malleableS));
        vault.queueWithdrawal(intent, malleableSig, alice, 20_00000000, aliceProof);

        // B. Forged unauthorized signer
        bytes memory rogueSig = _signIntent(ROGUE_PK, intent);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.UnauthorizedSequencerSigner.selector, vm.addr(ROGUE_PK))
        );
        vault.queueWithdrawal(intent, rogueSig, alice, 20_00000000, aliceProof);

        // C. Cross-chain replay (wrong chainId) - construct fresh struct to avoid Solidity memory pointer aliasing!
        EIP712Lib.WithdrawalIntent memory wrongChain = EIP712Lib.WithdrawalIntent({
            withdrawalId: intent.withdrawalId,
            accountId: intent.accountId,
            recipient: intent.recipient,
            asset: intent.asset,
            rawAmount: intent.rawAmount,
            amountFp: intent.amountFp,
            stateEpoch: intent.stateEpoch,
            nonce: intent.nonce,
            deadline: intent.deadline,
            chainId: 42161
        });
        bytes memory wrongChainSig = _signIntent(SEQUENCER_PK, wrongChain);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.CrossChainReplayRejected.selector, 42161, block.chainid)
        );
        vault.queueWithdrawal(wrongChain, wrongChainSig, alice, 20_00000000, aliceProof);

        // D. Deadline expiration
        vm.warp(block.timestamp + 61);
        bytes memory validSig = abi.encodePacked(r, s, v);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.WithdrawalDeadlineExpired.selector, intent.deadline, block.timestamp)
        );
        vault.queueWithdrawal(intent, validSig, alice, 20_00000000, aliceProof);

        // E. Replay of already-used nonce
        intent.deadline = uint64(block.timestamp + 600);
        bytes memory freshSig = _signIntent(SEQUENCER_PK, intent);
        vault.queueWithdrawal(intent, freshSig, alice, 20_00000000, aliceProof);

        EIP712Lib.WithdrawalIntent memory replayNonce = intent;
        replayNonce.withdrawalId = keccak256("w_sec_2_different_id");
        bytes memory replayNonceSig = _signIntent(SEQUENCER_PK, replayNonce);
        vm.expectRevert(abi.encodeWithSelector(HyperionVault.AccountNonceAlreadyUsed.selector, 101, 1));
        vault.queueWithdrawal(replayNonce, replayNonceSig, alice, 20_00000000, aliceProof);
    }

    function test_RevertWhen_CompromisedSequencerAttemptsOverLeafWithdrawal() public {
        (
            bytes32[][][] memory ethTreesH,
            uint128[][][] memory ethTreesS
        ) = _seedAndAnchorAliceBobEth(10_00000000, 50_00000000);

        MerkleSumLib.ProofStep[] memory aliceProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 0);

        // Compromised sequencer signs 25 ETH withdrawal for Alice whose committed leaf is only 10 ETH!
        EIP712Lib.WithdrawalIntent memory overIntent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_over"),
            accountId: 101,
            recipient: alice,
            asset: address(0),
            rawAmount: 25 ether,
            amountFp: 25_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 600),
            chainId: block.chainid
        });
        bytes memory sig = _signIntent(SEQUENCER_PK, overIntent);

        vm.expectRevert(
            abi.encodeWithSelector(
                HyperionVault.ExceedsAnchoredAccountBalance.selector,
                101,
                25_00000000,
                10_00000000
            )
        );
        vault.queueWithdrawal(overIntent, sig, alice, 10_00000000, aliceProof);
    }

    function test_DailyCircuitBreakerRateLimit_BlocksExcessAndResetsNextDay() public {
        (
            bytes32[][][] memory ethTreesH,
            uint128[][][] memory ethTreesS
        ) = _seedAndAnchorAliceBobEth(120_00000000, 30_00000000);

        MerkleSumLib.ProofStep[] memory aliceProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 0);

        // Daily limit on ETH is 100 ether. Withdraw 80 ether first -> succeeds.
        EIP712Lib.WithdrawalIntent memory w1 = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_cb_1"),
            accountId: 101,
            recipient: alice,
            asset: address(0),
            rawAmount: 80 ether,
            amountFp: 80_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 2 days),
            chainId: block.chainid
        });
        vault.queueWithdrawal(w1, _signIntent(SEQUENCER_PK, w1), alice, 120_00000000, aliceProof);

        // Attempt 25 ether more in same 24h window -> exceeds 100 ether limit (only 20 ether remaining)
        EIP712Lib.WithdrawalIntent memory w2 = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_cb_2"),
            accountId: 101,
            recipient: alice,
            asset: address(0),
            rawAmount: 25 ether,
            amountFp: 25_00000000,
            stateEpoch: 1,
            nonce: 2,
            deadline: uint64(block.timestamp + 2 days),
            chainId: block.chainid
        });
        bytes memory sig2 = _signIntent(SEQUENCER_PK, w2);
        vm.expectRevert(
            abi.encodeWithSelector(
                HyperionVault.DailyWithdrawalLimitExceeded.selector,
                address(0),
                25 ether,
                20 ether
            )
        );
        vault.queueWithdrawal(w2, sig2, alice, 120_00000000, aliceProof);

        // Advance to next UTC day -> 25 ether withdrawal succeeds!
        vm.warp(block.timestamp + 1 days);
        vault.queueWithdrawal(w2, sig2, alice, 120_00000000, aliceProof);
    }

    // =========================================================================
    // 4. REENTRANCY TORTURE (EIP-1153 TRANSIENT GUARD)
    // =========================================================================
    function test_RevertWhen_MaliciousETHReceiverReentersVaultAndEscapeHatch() public {
        MaliciousETHReentrancyAttacker attacker = new MaliciousETHReentrancyAttacker(vault, escapeHatch);
        vm.deal(address(attacker), 50 ether);

        // Attacker deposits 20 ETH for account 666
        attacker.depositFromAttacker{value: 20 ether}(666);

        // Build epoch 1 tree containing attacker (account 666, 20 ETH) and bob (account 102, 10 ETH)
        vm.prank(bob);
        vault.depositNative{value: 10 ether}(102);

        MerkleSumTreeHelper.LeafInput[] memory ethLeaves = new MerkleSumTreeHelper.LeafInput[](4);
        ethLeaves[0] = MerkleSumTreeHelper.LeafInput(666, address(attacker), address(0), 20_00000000);
        ethLeaves[1] = MerkleSumTreeHelper.LeafInput(102, bob, address(0), 10_00000000);
        ethLeaves[2] = MerkleSumTreeHelper.LeafInput(103, charlie, address(0), 0);
        ethLeaves[3] = MerkleSumTreeHelper.LeafInput(104, alice, address(0), 0);

        (bytes32 ethRoot, uint128 ethSum, bytes32[][] memory h, uint128[][] memory s) = MerkleSumTreeHelper.buildTree(
            ethLeaves
        );
        SolvencyStateAnchor.AssetLiabilityInput[] memory decls = _wrapSingleEthWithZeroTokens(1, 10, ethRoot, ethSum);
        vm.prank(relayer);
        anchor.commitStateRoot(1, 10, 2, keccak256("wal_reent"), decls);

        MerkleSumLib.ProofStep[] memory attackerProof = MerkleSumTreeHelper.extractProof(h, s, 0);

        // Queue small withdrawal (5 ETH) to attacker contract
        EIP712Lib.WithdrawalIntent memory w = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_reent_eth"),
            accountId: 666,
            recipient: address(attacker),
            asset: address(0),
            rawAmount: 5 ether,
            amountFp: 5_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 600),
            chainId: block.chainid
        });
        vault.queueWithdrawal(w, _signIntent(SEQUENCER_PK, w), address(attacker), 20_00000000, attackerProof);

        // 1. Same-function reentrancy on executeWithdrawal fails (transfer reverts when inner call hits TLOAD=1)
        attacker.setAttackWithdrawal(w.withdrawalId);
        vm.expectRevert();
        vault.executeWithdrawal(w.withdrawalId);

        // 2. Cross-function reentrancy (executeWithdrawal -> depositNative) also blocked by shared TLOAD lock
        attacker.setAttackDepositNative();
        vm.expectRevert();
        vault.executeWithdrawal(w.withdrawalId);

        // Clean execution when attacker stops reentering (revert above rolled back attacker's `mode = NONE`)
        attacker.resetMode();
        vault.executeWithdrawal(w.withdrawalId);

        // 3. Reentrancy during Escape Hatch forceExit also blocked
        vm.roll(block.number + ESCAPE_TIMEOUT_BLOCKS + 1);
        attacker.setAttackForceExit(666, 20_00000000, attackerProof);
        vm.expectRevert();
        attacker.triggerForceExit(666, 20_00000000, attackerProof);

        // Clean Escape Hatch exit pays exact remaining 15 ETH (20 - 5)
        attacker.resetMode();
        uint256 balBefore = address(attacker).balance;
        attacker.triggerForceExit(666, 20_00000000, attackerProof);
        assertEq(address(attacker).balance - balBefore, 15 ether);
    }

    function test_RevertWhen_MaliciousERC20HookReentersVault() public {
        MaliciousReentrantERC20 evilToken = new MaliciousReentrantERC20(vault, escapeHatch);
        _timelockCall(
            address(vault),
            abi.encodeCall(HyperionVault.registerAsset, (address(evilToken), 18, 1000 ether, 100 ether)),
            bytes32("reg_evil")
        );

        evilToken.mint(alice, 30 ether);
        vm.startPrank(alice);
        evilToken.approve(address(vault), 30 ether);

        // 1. Strict mode: inner reentrancy fails -> token hook reverts -> SafeTransferLib aborts deposit
        evilToken.configureAttack(false, true, bytes32(0));
        vm.expectRevert(SafeTransferLib.TransferFromFailed.selector);
        vault.depositERC20(address(evilToken), 10 ether, 101);

        // 2. Probe mode: inner reentrancy call hits EIP-1153 TLOAD lock and returns exact ReentrancyGuardReentrantCall selector
        evilToken.configureProbeAttack(false, true, bytes32(0));
        vault.depositERC20(address(evilToken), 10 ether, 101);
        assertTrue(evilToken.lastReentrancyBlocked());
        assertEq(
            evilToken.lastInnerRevertSelector(),
            ReentrancyGuardTransient.ReentrancyGuardReentrantCall.selector
        );
        vm.stopPrank();
    }

    // =========================================================================
    // 5. CENSORSHIP ESCAPE HATCH & UNANCHORED DEPOSIT RECOVERY
    // =========================================================================
    function test_CensorshipEscapeHatch_PartialEIP712ThenForcedExitAndUnanchoredRefund() public {
        (
            bytes32[][][] memory ethTreesH,
            uint128[][][] memory ethTreesS
        ) = _seedAndAnchorAliceBobEth(150_00000000, 40_00000000); // Alice 150 ETH (> 100 ETH daily limit!), Bob 40 ETH

        MerkleSumLib.ProofStep[] memory aliceProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 0);
        MerkleSumLib.ProofStep[] memory bobProof = MerkleSumTreeHelper.extractProof(ethTreesH[0], ethTreesS[0], 1);

        // Alice withdraws 30 ETH via normal EIP-712 before Sequencer halts
        EIP712Lib.WithdrawalIntent memory wAlice = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_alice_pre_halt"),
            accountId: 101,
            recipient: alice,
            asset: address(0),
            rawAmount: 30 ether,
            amountFp: 30_00000000,
            stateEpoch: 1,
            nonce: 1,
            deadline: uint64(block.timestamp + 600),
            chainId: block.chainid
        });
        vault.queueWithdrawal(wAlice, _signIntent(SEQUENCER_PK, wAlice), alice, 150_00000000, aliceProof);
        vm.warp(block.timestamp + COOLDOWN_SEC);
        vault.executeWithdrawal(wAlice.withdrawalId);

        // Charlie makes a 25 ETH deposit AFTER Epoch 1, then Sequencer dies and Guardian pauses Vault!
        vm.prank(charlie);
        (uint64 charlieDepositSeq, ) = vault.depositNative{value: 25 ether}(103);

        vm.prank(guardian);
        vault.setEmergencyPause(true);

        // At exact boundary `lastAnchorBlock + ESCAPE_TIMEOUT_BLOCKS`, Sequencer is STILL within window -> revert!
        uint64 anchorBlock = anchor.lastAnchorBlock();
        vm.roll(anchorBlock + ESCAPE_TIMEOUT_BLOCKS);
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(
                CensorshipEscapeHatch.SequencerStillLive.selector,
                anchorBlock,
                ESCAPE_TIMEOUT_BLOCKS,
                anchorBlock + ESCAPE_TIMEOUT_BLOCKS
            )
        );
        escapeHatch.forceExit(101, address(0), 150_00000000, alice, aliceProof);

        // Roll +1 block past timeout -> Escape Hatch activates even though emergencyPaused == true!
        vm.roll(anchorBlock + ESCAPE_TIMEOUT_BLOCKS + 1);

        // Front-running attempt: Charlie tries to submit Alice's Merkle proof -> fails leaf owner commitment!
        vm.prank(charlie);
        vm.expectRevert();
        escapeHatch.forceExit(101, address(0), 150_00000000, charlie, aliceProof);

        // Alice exits remaining 120 ETH (150 - 30 = 120 ETH, bypassing 100 ETH daily limit & emergency pause!)
        uint256 aliceBefore = alice.balance;
        vm.prank(alice);
        (uint128 exitedFp, uint256 rawPaid) = escapeHatch.forceExit(
            101,
            address(0),
            150_00000000,
            alice,
            aliceProof
        );
        assertEq(exitedFp, 120_00000000);
        assertEq(rawPaid, 120 ether);
        assertEq(alice.balance - aliceBefore, 120 ether);

        // Double exit attempt by Alice reverts with NothingToExit
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(
                CensorshipEscapeHatch.NothingToExit.selector,
                101,
                address(0),
                150_00000000,
                150_00000000
            )
        );
        escapeHatch.forceExit(101, address(0), 150_00000000, alice, aliceProof);

        // Bob exits his full 40 ETH
        uint256 bobBefore = bob.balance;
        vm.prank(bob);
        escapeHatch.forceExit(102, address(0), 40_00000000, bob, bobProof);
        assertEq(bob.balance - bobBefore, 40 ether);

        // Charlie reclaims his unanchored 25 ETH deposit (and anchored deposits <= seq 2 cannot be reclaimed!)
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(HyperionVault.DepositAlreadyAnchored.selector, 1, 2));
        escapeHatch.reclaimUnanchoredDeposit(1, alice);

        // Non-depositor cannot steal Charlie's unanchored deposit
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(
                CensorshipEscapeHatch.UnauthorizedDepositReclaim.selector,
                charlieDepositSeq,
                alice,
                charlie
            )
        );
        escapeHatch.reclaimUnanchoredDeposit(charlieDepositSeq, alice);

        uint256 charlieBefore = charlie.balance;
        vm.prank(charlie);
        uint256 refunded = escapeHatch.reclaimUnanchoredDeposit(charlieDepositSeq, charlie);
        assertEq(refunded, 25 ether);
        assertEq(charlie.balance - charlieBefore, 25 ether);

        // Vault balance is now cleanly 0 wei with 0 remaining claims!
        assertEq(address(vault).balance, 0);
        assertEq(vault.totalProtectedClaimsRaw(address(0)), 0);
    }

    // =========================================================================
    // 6. GOVERNANCE TIMELOCK & ANTI-RUG CUSTODY PROTECTION
    // =========================================================================
    function test_GovernanceTimelock_EnforcesDelayExpiryAndBlocksCustodyTokenBypass() public {
        vm.startPrank(alice);
        usdt.approve(address(vault), 10_000e6);
        vault.depositERC20(address(usdt), 10_000e6, 101);
        vm.stopPrank();

        // Malicious governance attempt to call `usdt.transfer(proposer, 10_000e6)` via Timelock -> FORBIDDEN!
        vm.prank(proposer);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.TimelockCustodyBypassForbidden.selector, address(usdt), 0)
        );
        vault.scheduleTimelockOperation(
            address(usdt),
            0,
            abi.encodeCall(MockERC20.transfer, (proposer, 10_000e6)),
            bytes32(0),
            bytes32("rug_usdt"),
            TIMELOCK_DELAY
        );

        // Malicious governance attempt to drain ETH via `value > 0` -> FORBIDDEN!
        vm.prank(proposer);
        vm.expectRevert(abi.encodeWithSelector(HyperionVault.TimelockCustodyBypassForbidden.selector, bob, 1 ether));
        vault.scheduleTimelockOperation(bob, 1 ether, "", bytes32(0), bytes32("rug_eth"), TIMELOCK_DELAY);

        // Valid governance update: change withdrawalCooldownSec to 2 hours
        bytes memory data = abi.encodeCall(HyperionVault.setWithdrawalCooldown, (2 hours));
        vm.prank(proposer);
        bytes32 opId = vault.scheduleTimelockOperation(
            address(vault),
            0,
            data,
            bytes32(0),
            bytes32("cooldown_2h"),
            TIMELOCK_DELAY
        );

        // Executing 1 second early reverts
        vm.warp(block.timestamp + TIMELOCK_DELAY - 1);
        vm.prank(proposer);
        vm.expectRevert();
        vault.executeTimelockOperation(address(vault), 0, data, bytes32(0), bytes32("cooldown_2h"));

        // Executing at exact readyTimestamp succeeds
        vm.warp(block.timestamp + 1);
        assertTrue(opId != bytes32(0));
        vm.prank(proposer);
        vault.executeTimelockOperation(address(vault), 0, data, bytes32(0), bytes32("cooldown_2h"));
        assertEq(vault.withdrawalCooldownSec(), 2 hours);
    }

    // =========================================================================
    // 7. PROTOCOL SURPLUS, MERKLE-SUM & GROTH16 VERIFIER BOUNDARY TESTS
    // =========================================================================
    function test_QueueSurplusWithdrawal_EnforcesExactSurplusBound() public {
        _seedAndAnchorAliceBobEth(20_00000000, 10_00000000);

        // Vault holds 30 ETH protected liabilities; donate 5 ETH protocol fee surplus
        vm.deal(address(vault), 35 ether);
        assertEq(vault.protocolSurplusRaw(address(0)), 5 ether);

        // Withdrawing 6 ETH (> 5 ETH surplus) must revert with ExceedsProtocolSurplus
        EIP712Lib.WithdrawalIntent memory overSurplus = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_surplus_over"),
            accountId: 101,
            recipient: coldWallet,
            asset: address(0),
            rawAmount: 6 ether,
            amountFp: 6_00000000,
            stateEpoch: 1,
            nonce: 10,
            deadline: uint64(block.timestamp + 300),
            chainId: block.chainid
        });
        bytes memory sigOver = _signIntent(SEQUENCER_PK, overSurplus);
        vm.expectRevert(
            abi.encodeWithSelector(HyperionVault.ExceedsProtocolSurplus.selector, address(0), 6 ether, 5 ether)
        );
        vault.queueSurplusWithdrawal(overSurplus, sigOver);

        // Withdrawing exact 5 ETH surplus succeeds and cannot touch the 30 ETH user liabilities
        EIP712Lib.WithdrawalIntent memory exactSurplus = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256("w_surplus_ok"),
            accountId: 101,
            recipient: coldWallet,
            asset: address(0),
            rawAmount: 5 ether,
            amountFp: 5_00000000,
            stateEpoch: 1,
            nonce: 11,
            deadline: uint64(block.timestamp + 300),
            chainId: block.chainid
        });
        vault.queueSurplusWithdrawal(exactSurplus, _signIntent(SEQUENCER_PK, exactSurplus));
        vault.executeWithdrawal(exactSurplus.withdrawalId);

        assertEq(vault.protocolSurplusRaw(address(0)), 0);
        assertEq(vault.remainingAnchoredLiabilitiesRaw(address(0)), 30 ether);
        assertEq(address(vault).balance, 30 ether);
    }

    function externalVerifyInclusion(
        bytes32 expectedRootHash,
        uint128 expectedRootSumFp,
        uint64 accountId,
        address accountOwner,
        address asset,
        uint128 leafBalanceFp,
        MerkleSumLib.ProofStep[] calldata proof
    ) external pure {
        MerkleSumLib.verifyInclusion(
            expectedRootHash,
            expectedRootSumFp,
            accountId,
            accountOwner,
            asset,
            leafBalanceFp,
            proof
        );
    }

    function test_MerkleSumLib_RejectsMismatchedRootSumEvenWithMatchingHash() public {
        MerkleSumTreeHelper.LeafInput[] memory leaves = new MerkleSumTreeHelper.LeafInput[](2);
        leaves[0] = MerkleSumTreeHelper.LeafInput(101, alice, address(0), 10_00000000);
        leaves[1] = MerkleSumTreeHelper.LeafInput(102, bob, address(0), 15_00000000);

        (bytes32 rootHash, uint128 rootSumFp, bytes32[][] memory h, uint128[][] memory s) = MerkleSumTreeHelper
            .buildTree(leaves);
        MerkleSumLib.ProofStep[] memory proof = MerkleSumTreeHelper.extractProof(h, s, 0);

        // Valid sum (25_00000000) succeeds
        this.externalVerifyInclusion(rootHash, rootSumFp, 101, alice, address(0), 10_00000000, proof);

        // Same rootHash but inflated expectedRootSumFp (25_00000001) must revert InvalidMerkleSumRoot
        vm.expectRevert(
            abi.encodeWithSelector(
                MerkleSumLib.InvalidMerkleSumRoot.selector,
                rootHash,
                rootHash,
                rootSumFp,
                rootSumFp + 1
            )
        );
        this.externalVerifyInclusion(rootHash, rootSumFp + 1, 101, alice, address(0), 10_00000000, proof);
    }

    function test_Groth16Verifier_RejectsInsolventPublicInputsEvenWithValidPairing() public {
        IZKSolvencyVerifier.SolvencyPublicInputs memory insolventIn = IZKSolvencyVerifier.SolvencyPublicInputs({
            epoch: 1,
            engineSeq: 77,
            asset: address(0),
            merkleSumRoot: keccak256("msst_root"),
            poseidonRoot: keccak256("poseidon_root"),
            totalLiabilityFp: 100_00000001,
            verifiedVaultReserveFp: 100_00000000
        });
        // Even if a malicious prover generates a mathematically valid Groth16 proof for insolvent public inputs,
        // the verifier's hard solvency inequality guard MUST revert fail-closed with InsolventPublicInputs!
        bytes memory mathematicallyValidProof = BN254Groth16TestProver.proveSolvency(insolventIn, 777);
        vm.expectRevert(
            abi.encodeWithSelector(
                Groth16BN254SolvencyVerifier.InsolventPublicInputs.selector,
                100_00000001,
                100_00000000
            )
        );
        zkVerifier.verifySolvencyProof(insolventIn, mathematicallyValidProof);
    }

    // =========================================================================
    // INTERNAL TEST FIXTURE BUILDERS
    // =========================================================================
    function _seedAndAnchorAliceBobEth(
        uint128 aliceEthFp,
        uint128 bobEthFp
    ) internal returns (bytes32[][][] memory treesH, uint128[][][] memory treesS) {
        uint256 aliceRaw = uint256(aliceEthFp) * 1e10;
        uint256 bobRaw = uint256(bobEthFp) * 1e10;

        vm.prank(alice);
        vault.depositNative{value: aliceRaw}(101);
        vm.prank(bob);
        vault.depositNative{value: bobRaw}(102);

        SolvencyStateAnchor.AssetLiabilityInput[] memory decls;
        (decls, treesH, treesS) = _buildThreeAssetEpochDeclarations(1, 100, aliceEthFp, bobEthFp, 0, 0);

        vm.prank(relayer);
        anchor.commitStateRoot(1, 100, 2, keccak256("wal_seed"), decls);
    }

    function _buildThreeAssetEpochDeclarations(
        uint64 epoch,
        uint64 engineSeq,
        uint128 aliceEthFp,
        uint128 bobEthFp,
        uint128 aliceUsdtFp,
        uint128 bobWbtcFp
    )
        internal
        view
        returns (
            SolvencyStateAnchor.AssetLiabilityInput[] memory decls,
            bytes32[][][] memory treesH,
            uint128[][][] memory treesS
        )
    {
        decls = new SolvencyStateAnchor.AssetLiabilityInput[](3);
        treesH = new bytes32[][][](3);
        treesS = new uint128[][][](3);

        address[3] memory assetAddrs = [address(0), address(usdt), address(wbtc)];
        uint128[3] memory aBals = [aliceEthFp, aliceUsdtFp, uint128(0)];
        uint128[3] memory bBals = [bobEthFp, uint128(0), bobWbtcFp];

        for (uint256 i = 0; i < 3; ++i) {
            MerkleSumTreeHelper.LeafInput[] memory leaves = new MerkleSumTreeHelper.LeafInput[](4);
            leaves[0] = MerkleSumTreeHelper.LeafInput(101, alice, assetAddrs[i], aBals[i]);
            leaves[1] = MerkleSumTreeHelper.LeafInput(102, bob, assetAddrs[i], bBals[i]);
            leaves[2] = MerkleSumTreeHelper.LeafInput(103, charlie, assetAddrs[i], 0);
            leaves[3] = MerkleSumTreeHelper.LeafInput(104, coldWallet, assetAddrs[i], 0);

            (bytes32 rootHash, uint128 rootSumFp, bytes32[][] memory lh, uint128[][] memory ls) = MerkleSumTreeHelper
                .buildTree(leaves);
            treesH[i] = lh;
            treesS[i] = ls;

            bytes32 poseidonCommit = keccak256(abi.encodePacked("POSEIDON_MSST_V1", epoch, assetAddrs[i], rootHash, rootSumFp));

            // Post-graduation backing reserve == totalVaultActual - queuedWithdrawals
            uint256 postGradBackingRaw = vault.actualAssetBalanceRaw(assetAddrs[i]) -
                vault.queuedWithdrawalsRaw(assetAddrs[i]);
            uint128 backingFp = vault.rawToFloorFp(assetAddrs[i], postGradBackingRaw);

            // Clamp proof input reserve to >= rootSumFp so proveSolvency generates matching scalar when testing anchor's own ratio check
            uint128 proofReserveFp = backingFp;
            IZKSolvencyVerifier.SolvencyPublicInputs memory pubIn = IZKSolvencyVerifier.SolvencyPublicInputs({
                epoch: epoch,
                engineSeq: engineSeq,
                asset: assetAddrs[i],
                merkleSumRoot: rootHash,
                poseidonRoot: poseidonCommit,
                totalLiabilityFp: rootSumFp,
                verifiedVaultReserveFp: proofReserveFp
            });

            decls[i] = SolvencyStateAnchor.AssetLiabilityInput({
                asset: assetAddrs[i],
                merkleSumRoot: rootHash,
                poseidonRoot: poseidonCommit,
                totalLiabilityFp: rootSumFp,
                zkProof: BN254Groth16TestProver.proveSolvency(pubIn, epoch * 10 + i + 1)
            });
        }
    }

    function _wrapSingleEthWithZeroTokens(
        uint64 epoch,
        uint64 engineSeq,
        bytes32 ethRoot,
        uint128 ethSumFp
    ) internal view returns (SolvencyStateAnchor.AssetLiabilityInput[] memory decls) {
        decls = new SolvencyStateAnchor.AssetLiabilityInput[](3);
        address[3] memory assetAddrs = [address(0), address(usdt), address(wbtc)];

        for (uint256 i = 0; i < 3; ++i) {
            bytes32 rHash;
            uint128 rSum;
            if (i == 0) {
                rHash = ethRoot;
                rSum = ethSumFp;
            } else {
                MerkleSumTreeHelper.LeafInput[] memory emptyLeaves = new MerkleSumTreeHelper.LeafInput[](4);
                emptyLeaves[0] = MerkleSumTreeHelper.LeafInput(101, alice, assetAddrs[i], 0);
                emptyLeaves[1] = MerkleSumTreeHelper.LeafInput(102, bob, assetAddrs[i], 0);
                emptyLeaves[2] = MerkleSumTreeHelper.LeafInput(103, charlie, assetAddrs[i], 0);
                emptyLeaves[3] = MerkleSumTreeHelper.LeafInput(104, coldWallet, assetAddrs[i], 0);
                (rHash, rSum, , ) = MerkleSumTreeHelper.buildTree(emptyLeaves);
            }
            bytes32 poseidonCommit = keccak256(abi.encodePacked("POSEIDON", epoch, assetAddrs[i], rHash));
            uint256 backingRaw = vault.actualAssetBalanceRaw(assetAddrs[i]) -
                vault.queuedWithdrawalsRaw(assetAddrs[i]);
            uint128 backingFp = vault.rawToFloorFp(assetAddrs[i], backingRaw);

            IZKSolvencyVerifier.SolvencyPublicInputs memory pubIn = IZKSolvencyVerifier.SolvencyPublicInputs({
                epoch: epoch,
                engineSeq: engineSeq,
                asset: assetAddrs[i],
                merkleSumRoot: rHash,
                poseidonRoot: poseidonCommit,
                totalLiabilityFp: rSum,
                verifiedVaultReserveFp: backingFp
            });
            decls[i] = SolvencyStateAnchor.AssetLiabilityInput({
                asset: assetAddrs[i],
                merkleSumRoot: rHash,
                poseidonRoot: poseidonCommit,
                totalLiabilityFp: rSum,
                zkProof: BN254Groth16TestProver.proveSolvency(pubIn, 99 + i)
            });
        }
    }

    function _signIntent(
        uint256 pk,
        EIP712Lib.WithdrawalIntent memory intent
    ) internal view returns (bytes memory sig) {
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
        bytes32 digest = EIP712Lib.toTypedDataHash(vault.domainSeparator(), structHash);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, digest);
        return abi.encodePacked(r, s, v);
    }

    function _timelockCall(address target, bytes memory data, bytes32 salt) internal {
        vm.prank(proposer);
        vault.scheduleTimelockOperation(target, 0, data, bytes32(0), salt, TIMELOCK_DELAY);
        vm.warp(block.timestamp + TIMELOCK_DELAY);
        vm.prank(proposer);
        vault.executeTimelockOperation(target, 0, data, bytes32(0), salt);
    }
}
