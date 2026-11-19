// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {CommonBase} from "forge-std/Base.sol";
import {StdCheats} from "forge-std/StdCheats.sol";
import {StdUtils} from "forge-std/StdUtils.sol";

import {HyperionVault} from "../../src/HyperionVault.sol";
import {SolvencyStateAnchor} from "../../src/SolvencyStateAnchor.sol";
import {CensorshipEscapeHatch} from "../../src/CensorshipEscapeHatch.sol";
import {IZKSolvencyVerifier} from "../../src/interfaces/IZKSolvencyVerifier.sol";
import {IHyperionVaultSettlement} from "../../src/interfaces/IHyperionSettlement.sol";
import {MerkleSumLib} from "../../src/libraries/MerkleSumLib.sol";
import {EIP712Lib} from "../../src/libraries/EIP712Lib.sol";
import {MockERC20, MaliciousETHReentrancyAttacker} from "../mocks/MockTokensAndAttackers.sol";
import {BN254Groth16TestProver, MerkleSumTreeHelper} from "../helpers/SettlementTestHelpers.sol";

/// @title SettlementActorHandler
/// @notice Stateful multi-actor fuzzer handler exercising deposits, off-chain matching conservation,
///         ZK-anchored Merkle-Sum epochs, EIP-712 withdrawals, signature malleability attacks,
///         malicious reentrancy hooks, direct donations, and Censorship Escape Hatch forced exits.
contract SettlementActorHandler is CommonBase, StdCheats, StdUtils {
    uint256 internal constant SECP256K1_N =
        0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;

    uint256 public constant NUM_ACTORS = 8; // Power of two for clean depth-3 MSST
    uint256 public constant NUM_ASSETS = 3; // 0 = Native ETH (18d), 1 = USDT (6d), 2 = WBTC (8d)

    HyperionVault public immutable vault;
    SolvencyStateAnchor public immutable anchor;
    CensorshipEscapeHatch public immutable escapeHatch;
    MockERC20 public immutable usdt;
    MockERC20 public immutable wbtc;
    MaliciousETHReentrancyAttacker public immutable reentrantAttacker;

    address public immutable relayer;
    uint256 public immutable sequencerPk;

    address[NUM_ACTORS] public actors;
    uint64[NUM_ACTORS] public accountIds;
    address[NUM_ASSETS] public assets;

    // Shadow exchange ledger balances (1e8 fixed-point) reflecting deposits, trades, and withdrawals
    uint128[NUM_ASSETS][NUM_ACTORS] public shadowLedgerFp;

    // Latest anchored epoch MSST snapshot per actor & asset
    uint128[NUM_ASSETS][NUM_ACTORS] public anchoredLeafFp;
    bytes32[][][NUM_ASSETS] internal _anchoredTreeHashes;
    uint128[][][NUM_ASSETS] internal _anchoredTreeSums;

    uint64 public currentEngineSeq = 1;
    uint64 public nextNonce = 1;
    uint256 public withdrawalCounter = 0;

    bytes32[] public queuedWithdrawalIds;
    uint64[] public allDepositSeqs;

    // Ghost conservation accumulators
    uint256[NUM_ASSETS] public ghostDepositedRaw;
    uint256[NUM_ASSETS] public ghostWithdrawnRaw;
    uint256[NUM_ASSETS] public ghostDonatedRaw;

    // Telemetry counters for fuzzing audit report
    uint256 public statDeposits;
    uint256 public statOffChainTrades;
    uint256 public statEpochAnchors;
    uint256 public statWithdrawalsQueued;
    uint256 public statWithdrawalsExecuted;
    uint256 public statAdversarialBlocked;
    uint256 public statForcedExits;
    uint256 public statUnanchoredRefunds;

    constructor(
        HyperionVault vault_,
        SolvencyStateAnchor anchor_,
        CensorshipEscapeHatch escapeHatch_,
        MockERC20 usdt_,
        MockERC20 wbtc_,
        address relayer_,
        uint256 sequencerPk_
    ) {
        vault = vault_;
        anchor = anchor_;
        escapeHatch = escapeHatch_;
        usdt = usdt_;
        wbtc = wbtc_;
        relayer = relayer_;
        sequencerPk = sequencerPk_;

        assets[0] = address(0);
        assets[1] = address(usdt_);
        assets[2] = address(wbtc_);

        reentrantAttacker = new MaliciousETHReentrancyAttacker(vault_, escapeHatch_);
        vm.deal(address(reentrantAttacker), 500 ether);

        // Actor 0 is the malicious reentrancy contract so reentrancy hooks are also fuzz-tested in-tree;
        // Actors 1..7 are institutional trader EOAs.
        actors[0] = address(reentrantAttacker);
        accountIds[0] = 1001;
        for (uint256 i = 1; i < NUM_ACTORS; ++i) {
            address a = vm.addr(0xBEEF0000 + i);
            actors[i] = a;
            accountIds[i] = uint64(1001 + i);
            vm.deal(a, 100_000 ether);
            usdt_.mint(a, 100_000_000e6);
            wbtc_.mint(a, 100_000e8);
        }
        usdt_.mint(actors[0], 10_000_000e6);
        wbtc_.mint(actors[0], 10_000e8);
    }

    // =========================================================================
    // 1. RANDOMIZED MULTI-ASSET DEPOSITS
    // =========================================================================
    function actDeposit(uint256 actorSeed, uint256 assetSeed, uint256 unitsSeed) external {
        if (escapeHatch.isEscapeHatchEligible() || vault.emergencyPaused()) return;

        uint256 actorIdx = actorSeed % NUM_ACTORS;
        uint256 assetIdx = assetSeed % NUM_ASSETS;
        address actor = actors[actorIdx];
        uint64 accId = accountIds[actorIdx];
        address asset = assets[assetIdx];

        // Choose clean representable fixed-point units (100 .. 500_000_000 fp; for 6-dec USDT step by 100 fp)
        uint128 stepFp = assetIdx == 1 ? uint128(100) : uint128(1);
        uint128 count = uint128(bound(unitsSeed, 1, 5_000_000));
        uint128 depositFp = count * stepFp;
        uint256 rawAmount = vault.fpToExactRaw(asset, depositFp);

        uint64 seq;
        uint128 creditedFp;
        if (assetIdx == 0) {
            vm.deal(actor, actor.balance + rawAmount);
            vm.prank(actor);
            (seq, creditedFp) = vault.depositNative{value: rawAmount}(accId);
        } else {
            MockERC20 token = assetIdx == 1 ? usdt : wbtc;
            token.mint(actor, rawAmount);
            vm.startPrank(actor);
            token.approve(address(vault), rawAmount);
            (seq, creditedFp) = vault.depositERC20(asset, rawAmount, accId);
            vm.stopPrank();
        }

        shadowLedgerFp[actorIdx][assetIdx] += creditedFp;
        ghostDepositedRaw[assetIdx] += rawAmount;
        allDepositSeqs.push(seq);
        ++statDeposits;
    }

    // =========================================================================
    // 2. DOUBLE-ENTRY CONSERVING OFF-CHAIN MATCHING SIMULATION
    // =========================================================================
    function actOffChainTrade(
        uint256 fromSeed,
        uint256 toSeed,
        uint256 assetSeed,
        uint256 shareBpsSeed
    ) external {
        if (escapeHatch.isEscapeHatchEligible()) return;

        uint256 fromIdx = fromSeed % NUM_ACTORS;
        uint256 toIdx = toSeed % NUM_ACTORS;
        if (fromIdx == toIdx) toIdx = (toIdx + 1) % NUM_ACTORS;

        uint256 assetIdx = assetSeed % NUM_ASSETS;
        uint128 availFp = shadowLedgerFp[fromIdx][assetIdx];

        // Keep at least `alreadyWithdrawnInCurrentEpoch` untouched if not yet re-anchored
        uint64 curEpoch = anchor.latestEpoch();
        if (curEpoch > 0) {
            uint128 unanchoredSpent = 0;
            // Ensure shadowLedger stays non-negative and respects exact 6-decimal granularity for USDT
            if (availFp == 0) return;
            unanchoredSpent;
        }
        if (availFp < 200) return;

        uint256 bps = bound(shareBpsSeed, 100, 5_000); // transfer 1% .. 50%
        uint128 deltaFp = uint128((uint256(availFp) * bps) / 10_000);
        if (assetIdx == 1) {
            deltaFp = (deltaFp / 100) * 100; // keep exact 6-decimal USDT representability
        }
        if (deltaFp == 0) return;

        shadowLedgerFp[fromIdx][assetIdx] -= deltaFp;
        shadowLedgerFp[toIdx][assetIdx] += deltaFp;
        currentEngineSeq += 1;
        ++statOffChainTrades;
    }

    // =========================================================================
    // 3. BATCH STATE ROOT & REAL BN254 GROTH16 ANCHORING
    // =========================================================================
    function actCommitEpochStateRoot(uint256 advanceBlocksSeed) external {
        if (escapeHatch.isEscapeHatchEligible()) return;

        // Advance 1..5 blocks (staying well inside escapeHatchTimeoutBlocks during live operation)
        uint256 maxSafeAdvance = 5;
        uint64 lastLive = escapeHatch.lastLivenessBlock();
        uint64 timeout = escapeHatch.timeoutBlocks();
        if (block.number + maxSafeAdvance > uint256(lastLive) + uint256(timeout)) {
            maxSafeAdvance = (uint256(lastLive) + uint256(timeout)) - block.number;
        }
        uint256 deltaBlocks = bound(advanceBlocksSeed, 0, maxSafeAdvance);
        if (deltaBlocks > 0) {
            vm.roll(block.number + deltaBlocks);
            vm.warp(block.timestamp + deltaBlocks * 12);
        }

        uint64 nextEpoch = anchor.latestEpoch() + 1;
        currentEngineSeq += 10;
        uint64 targetDepositSeq = vault.latestDepositSeq();

        SolvencyStateAnchor.AssetLiabilityInput[] memory decls = new SolvencyStateAnchor.AssetLiabilityInput[](
            NUM_ASSETS
        );

        for (uint256 a = 0; a < NUM_ASSETS; ++a) {
            MerkleSumTreeHelper.LeafInput[] memory leaves = new MerkleSumTreeHelper.LeafInput[](NUM_ACTORS);
            for (uint256 i = 0; i < NUM_ACTORS; ++i) {
                leaves[i] = MerkleSumTreeHelper.LeafInput({
                    accountId: accountIds[i],
                    owner: actors[i],
                    asset: assets[a],
                    balanceFp: shadowLedgerFp[i][a]
                });
                anchoredLeafFp[i][a] = shadowLedgerFp[i][a];
            }

            (
                bytes32 rootHash,
                uint128 rootSumFp,
                bytes32[][] memory lh,
                uint128[][] memory ls
            ) = MerkleSumTreeHelper.buildTree(leaves);

            _anchoredTreeHashes[a] = lh;
            _anchoredTreeSums[a] = ls;

            bytes32 poseidonRoot = keccak256(
                abi.encodePacked("POSEIDON_MSST", nextEpoch, currentEngineSeq, assets[a], rootHash, rootSumFp)
            );
            uint256 postGradBackingRaw = vault.actualAssetBalanceRaw(assets[a]) -
                vault.queuedWithdrawalsRaw(assets[a]);
            uint128 backingFp = vault.rawToFloorFp(assets[a], postGradBackingRaw);

            IZKSolvencyVerifier.SolvencyPublicInputs memory pubIn = IZKSolvencyVerifier.SolvencyPublicInputs({
                epoch: nextEpoch,
                engineSeq: currentEngineSeq,
                asset: assets[a],
                merkleSumRoot: rootHash,
                poseidonRoot: poseidonRoot,
                totalLiabilityFp: rootSumFp,
                verifiedVaultReserveFp: backingFp
            });

            decls[a] = SolvencyStateAnchor.AssetLiabilityInput({
                asset: assets[a],
                merkleSumRoot: rootHash,
                poseidonRoot: poseidonRoot,
                totalLiabilityFp: rootSumFp,
                zkProof: BN254Groth16TestProver.proveSolvency(pubIn, uint256(nextEpoch) * 31 + a)
            });
        }

        vm.prank(relayer);
        anchor.commitStateRoot(
            nextEpoch,
            currentEngineSeq,
            targetDepositSeq,
            keccak256(abi.encodePacked("wal_blake3_", nextEpoch, currentEngineSeq)),
            decls
        );
        ++statEpochAnchors;
    }

    // =========================================================================
    // 4. EIP-712 STRUCTURED WITHDRAWAL QUEUE & EXECUTION
    // =========================================================================
    function actQueueEIP712Withdrawal(
        uint256 actorSeed,
        uint256 assetSeed,
        uint256 shareBpsSeed
    ) external {
        if (escapeHatch.isEscapeHatchEligible() || vault.emergencyPaused()) return;
        uint64 curEpoch = anchor.latestEpoch();
        if (curEpoch == 0) return;

        uint256 actorIdx = actorSeed % NUM_ACTORS;
        uint256 assetIdx = assetSeed % NUM_ASSETS;
        address asset = assets[assetIdx];
        uint64 accId = accountIds[actorIdx];

        uint128 leafBalFp = anchoredLeafFp[actorIdx][assetIdx];
        uint128 alreadyWithdrawnFp = vault.epochAccountWithdrawnFp(curEpoch, accId, asset);
        if (leafBalFp <= alreadyWithdrawnFp) return;

        uint128 availInEpochFp = leafBalFp - alreadyWithdrawnFp;
        uint128 shadowAvailFp = shadowLedgerFp[actorIdx][assetIdx];
        uint128 maxSafeFp = availInEpochFp < shadowAvailFp ? availInEpochFp : shadowAvailFp;
        if (maxSafeFp < 100) return;

        uint256 bps = bound(shareBpsSeed, 500, 5_000);
        uint128 reqFp = uint128((uint256(maxSafeFp) * bps) / 10_000);
        if (assetIdx == 1) {
            reqFp = (reqFp / 100) * 100;
        }
        if (reqFp == 0) return;

        uint256 rawAmount = vault.fpToExactRaw(asset, reqFp);
        uint256 remDaily = vault.remainingDailyWithdrawalCapacityRaw(asset);
        if (rawAmount > remDaily) {
            // Advance 1 day to refresh circuit-breaker window or cap to remaining daily capacity
            vm.warp(block.timestamp + 1 days);
            remDaily = vault.remainingDailyWithdrawalCapacityRaw(asset);
            if (rawAmount > remDaily) return;
        }

        uint64 nonce = nextNonce++;
        bytes32 wId = keccak256(abi.encodePacked("fuzz_w_", ++withdrawalCounter, accId, nonce));

        EIP712Lib.WithdrawalIntent memory intent = EIP712Lib.WithdrawalIntent({
            withdrawalId: wId,
            accountId: accId,
            recipient: actors[actorIdx],
            asset: asset,
            rawAmount: rawAmount,
            amountFp: reqFp,
            stateEpoch: curEpoch,
            nonce: nonce,
            deadline: uint64(block.timestamp + 1 hours),
            chainId: block.chainid
        });

        bytes memory sig = _signIntent(intent);
        MerkleSumLib.ProofStep[] memory proof = MerkleSumTreeHelper.extractProof(
            _anchoredTreeHashes[assetIdx],
            _anchoredTreeSums[assetIdx],
            actorIdx
        );

        vm.prank(actors[actorIdx]);
        vault.queueWithdrawal(intent, sig, actors[actorIdx], leafBalFp, proof);

        shadowLedgerFp[actorIdx][assetIdx] -= reqFp;
        queuedWithdrawalIds.push(wId);
        ++statWithdrawalsQueued;
    }

    function actExecuteQueuedWithdrawal(uint256 idxSeed, bool warpPastCooldown) external {
        uint256 qLen = queuedWithdrawalIds.length;
        if (qLen == 0) return;

        bytes32 wId = queuedWithdrawalIds[idxSeed % qLen];
        HyperionVault.QueuedWithdrawal memory w = vault.getQueuedWithdrawal(wId);
        if (w.status != HyperionVault.WithdrawalStatus.QUEUED) return;

        if (block.timestamp < w.unlockTimestamp) {
            if (!warpPastCooldown) return;
            vm.warp(w.unlockTimestamp);
        }

        uint256 assetIdx = _assetIndex(w.asset);
        vault.executeWithdrawal(wId);

        ghostWithdrawnRaw[assetIdx] += w.rawAmount;
        ++statWithdrawalsExecuted;
    }

    // =========================================================================
    // 5. ADVERSARIAL ATTACK PROBES (MALLEABILITY, REPLAY, REENTRANCY, DONATION)
    // =========================================================================
    function actAdversarialMalleabilityAndReplayProbe(uint256 actorSeed, uint256 assetSeed) external {
        uint64 curEpoch = anchor.latestEpoch();
        if (curEpoch == 0 || escapeHatch.isEscapeHatchEligible()) return;

        uint256 actorIdx = actorSeed % NUM_ACTORS;
        uint256 assetIdx = assetSeed % NUM_ASSETS;
        uint128 leafBalFp = anchoredLeafFp[actorIdx][assetIdx];
        if (leafBalFp < 100) return;

        uint128 amountFp = assetIdx == 1 ? uint128(100) : uint128(10);
        uint256 rawAmount = vault.fpToExactRaw(assets[assetIdx], amountFp);

        EIP712Lib.WithdrawalIntent memory intent = EIP712Lib.WithdrawalIntent({
            withdrawalId: keccak256(abi.encodePacked("adv_mal_", block.timestamp, actorIdx)),
            accountId: accountIds[actorIdx],
            recipient: actors[actorIdx],
            asset: assets[assetIdx],
            rawAmount: rawAmount,
            amountFp: amountFp,
            stateEpoch: curEpoch,
            nonce: 999_999_000 + uint64(actorIdx),
            deadline: uint64(block.timestamp + 600),
            chainId: block.chainid
        });

        bytes memory malSig = _signMalleableIntent(intent);
        MerkleSumLib.ProofStep[] memory proof = MerkleSumTreeHelper.extractProof(
            _anchoredTreeHashes[assetIdx],
            _anchoredTreeSums[assetIdx],
            actorIdx
        );

        (bool ok, ) = address(vault).call(
            abi.encodeCall(HyperionVault.queueWithdrawal, (intent, malSig, actors[actorIdx], leafBalFp, proof))
        );
        require(!ok, "MALLEABLE_SIGNATURE_MUST_NEVER_SUCCEED");
        ++statAdversarialBlocked;
    }

    function _signMalleableIntent(EIP712Lib.WithdrawalIntent memory intent) internal view returns (bytes memory) {
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
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(sequencerPk, digest);
        bytes32 malS = bytes32(SECP256K1_N - uint256(s));
        uint8 malV = v == 27 ? 28 : 27;
        return abi.encodePacked(r, malS, malV);
    }

    function actMaliciousReentrancyProbe() external {
        uint256 qLen = queuedWithdrawalIds.length;
        if (qLen == 0) return;

        for (uint256 i = 0; i < qLen; ++i) {
            bytes32 wId = queuedWithdrawalIds[i];
            HyperionVault.QueuedWithdrawal memory w = vault.getQueuedWithdrawal(wId);
            if (
                w.status == HyperionVault.WithdrawalStatus.QUEUED &&
                w.recipient == address(reentrantAttacker) &&
                w.asset == address(0) &&
                block.timestamp >= w.unlockTimestamp
            ) {
                reentrantAttacker.setAttackDepositNative();
                (bool ok, ) = address(vault).call(abi.encodeCall(HyperionVault.executeWithdrawal, (wId)));
                reentrantAttacker.setAttackWithdrawal(bytes32(0)); // reset mode
                // Reset to NONE by calling setAttackDepositNative? No, let's ensure clean state:
                require(!ok, "REENTRANT_CALL_MUST_REVERT");
                ++statAdversarialBlocked;
                return;
            }
        }
    }

    function actDirectUnsolicitedDonation(uint256 assetSeed, uint256 rawSeed) external {
        uint256 assetIdx = assetSeed % NUM_ASSETS;
        uint256 donateRaw = bound(rawSeed, 1, 10_000_000);
        if (assetIdx == 0) {
            vm.deal(address(vault), address(vault).balance + donateRaw);
        } else if (assetIdx == 1) {
            usdt.mint(address(vault), donateRaw);
        } else {
            wbtc.mint(address(vault), donateRaw);
        }
        ghostDonatedRaw[assetIdx] += donateRaw;
    }

    // =========================================================================
    // 6. CENSORSHIP ESCAPE HATCH FORCED EXIT & UNANCHORED DEPOSIT RECOVERY
    // =========================================================================
    function actTriggerEscapeHatchAndExit(
        uint256 rollPastTimeoutSeed,
        uint256 actorSeed,
        uint256 assetSeed
    ) external {
        // Trigger escape hatch in ~15% of calls when rollPastTimeoutSeed % 7 == 0, or if already active
        if (!escapeHatch.isEscapeHatchEligible()) {
            if (rollPastTimeoutSeed % 7 != 0) return;
            uint64 lastLive = escapeHatch.lastLivenessBlock();
            uint64 timeout = escapeHatch.timeoutBlocks();
            vm.roll(uint256(lastLive) + uint256(timeout) + 1);
        }

        uint64 frozenEpoch = anchor.latestEpoch();
        uint256 actorIdx = actorSeed % NUM_ACTORS;
        uint256 assetIdx = assetSeed % NUM_ASSETS;
        address asset = assets[assetIdx];

        // 1. Try forced leaf exit if an epoch was anchored
        if (frozenEpoch > 0) {
            uint128 leafFp = anchoredLeafFp[actorIdx][assetIdx];
            uint128 withdrawnFp = vault.epochAccountWithdrawnFp(frozenEpoch, accountIds[actorIdx], asset);
            if (leafFp > withdrawnFp) {
                MerkleSumLib.ProofStep[] memory proof = MerkleSumTreeHelper.extractProof(
                    _anchoredTreeHashes[assetIdx],
                    _anchoredTreeSums[assetIdx],
                    actorIdx
                );
                address recipient = actorIdx == 0 ? actors[1] : actors[actorIdx];
                vm.prank(actors[actorIdx]);
                (, uint256 rawPaid) = escapeHatch.forceExit(
                    accountIds[actorIdx],
                    asset,
                    leafFp,
                    recipient,
                    proof
                );
                ghostWithdrawnRaw[assetIdx] += rawPaid;
                ++statForcedExits;
            }
        }

        // 2. Also reclaim one unanchored deposit if any exist
        uint64 lastAnchoredSeq = anchor.lastProcessedDepositSeq();
        uint64 headSeq = vault.latestDepositSeq();
        if (headSeq > lastAnchoredSeq) {
            uint64 targetSeq = lastAnchoredSeq + 1 + uint64(actorSeed % (headSeq - lastAnchoredSeq));
            IHyperionVaultSettlement.DepositReceipt memory d = vault.getDepositReceipt(targetSeq);
            if (!d.reclaimed) {
                address refundRecipient = d.depositor == address(reentrantAttacker) ? actors[1] : d.depositor;
                vm.prank(d.depositor);
                uint256 rawRefunded = escapeHatch.reclaimUnanchoredDeposit(targetSeq, refundRecipient);
                ghostWithdrawnRaw[_assetIndex(d.asset)] += rawRefunded;
                ++statUnanchoredRefunds;
            }
        }
    }

    // =========================================================================
    // INVARIANT INSPECTION HELPERS
    // =========================================================================
    function computeTrueUnclaimedValidClaimsRaw(uint256 assetIdx) external view returns (uint256 totalClaimsRaw) {
        address asset = assets[assetIdx];

        // 1. Exact sum of all remaining unclaimed anchored Merkle-Sum leaves across all actors
        uint64 ep = anchor.latestEpoch();
        if (ep > 0) {
            for (uint256 i = 0; i < NUM_ACTORS; ++i) {
                uint128 leafFp = anchoredLeafFp[i][assetIdx];
                uint128 drawnFp = vault.epochAccountWithdrawnFp(ep, accountIds[i], asset);
                if (leafFp > drawnFp) {
                    totalClaimsRaw += vault.fpToExactRaw(asset, leafFp - drawnFp);
                }
            }
        }

        // 2. Exact sum of all unanchored & unreclaimed deposits
        uint64 lastAnchored = anchor.lastProcessedDepositSeq();
        uint64 headSeq = vault.latestDepositSeq();
        for (uint64 s = lastAnchored + 1; s <= headSeq; ++s) {
            IHyperionVaultSettlement.DepositReceipt memory d = vault.getDepositReceipt(s);
            if (d.asset == asset && !d.reclaimed) {
                totalClaimsRaw += d.rawAmount;
            }
        }

        // 3. Exact sum of all QUEUED (unexecuted) withdrawals
        uint256 qLen = queuedWithdrawalIds.length;
        for (uint256 j = 0; j < qLen; ++j) {
            HyperionVault.QueuedWithdrawal memory w = vault.getQueuedWithdrawal(queuedWithdrawalIds[j]);
            if (w.asset == asset && w.status == HyperionVault.WithdrawalStatus.QUEUED) {
                totalClaimsRaw += w.rawAmount;
            }
        }
    }

    function queuedWithdrawalsLength() external view returns (uint256) {
        return queuedWithdrawalIds.length;
    }

    function _assetIndex(address asset) internal view returns (uint256) {
        if (asset == address(0)) return 0;
        if (asset == address(usdt)) return 1;
        return 2;
    }

    function _signIntent(EIP712Lib.WithdrawalIntent memory intent) internal view returns (bytes memory) {
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
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(sequencerPk, digest);
        return abi.encodePacked(r, s, v);
    }
}
