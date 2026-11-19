// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {ISolvencyStateAnchorView, IHyperionVaultSettlement} from "./interfaces/IHyperionSettlement.sol";
import {ReentrancyGuardTransient} from "./libraries/ReentrancyGuardTransient.sol";
import {MerkleSumLib} from "./libraries/MerkleSumLib.sol";

/// @title CensorshipEscapeHatch
/// @notice Trustless Forced-Exit & Unanchored Deposit Recovery Contract for HYPERION.
/// @dev If the off-chain Sequencer / Gateway halts or censors users for `> timeoutBlocks` blocks without
///      anchoring a new ZK-verified state epoch, any user can permissionlessly latch Escape Hatch Mode and:
///      1. Submit a Channel 13 Merkle-Sum Inclusion Proof binding `(accountId, msg.sender, asset, leafBalanceFp)`
///         to withdraw their exact unclaimed balance from the final anchored epoch (`leafBalanceFp - alreadyWithdrawnInEpochFp`).
///      2. Reclaim 100% of any unanchored deposit (`depositSeq > lastProcessedDepositSeq`).
///      Cannot be blocked by Emergency Pause or Daily Rate Limits.
contract CensorshipEscapeHatch is ReentrancyGuardTransient {
    // =========================================================================
    // CUSTOM ERRORS
    // =========================================================================
    error ZeroAddressNotAllowed();
    error SequencerStillLive(uint64 lastLivenessBlock, uint64 timeoutBlocks, uint256 currentBlock);
    error NoEpochAnchoredForLeafExit();
    error NothingToExit(uint64 accountId, address asset, uint128 leafBalanceFp, uint128 alreadyWithdrawnFp);
    error UnauthorizedDepositReclaim(uint64 depositSeq, address caller, address originalDepositor);

    // =========================================================================
    // EVENTS
    // =========================================================================
    event EscapeHatchActivated(uint64 indexed frozenEpoch, uint64 lastLivenessBlock, uint64 activatedAtBlock);
    event ForcedExitExecuted(
        uint64 indexed frozenEpoch,
        uint64 indexed accountId,
        address indexed asset,
        address accountOwner,
        address recipient,
        uint128 exitedFp,
        uint256 rawPaid
    );
    event UnanchoredDepositReclaimed(
        uint64 indexed depositSeq,
        uint64 indexed accountId,
        address indexed asset,
        address recipient,
        uint256 rawRefunded
    );

    // =========================================================================
    // IMMUTABLES & STATE
    // =========================================================================
    IHyperionVaultSettlement public immutable vault;
    ISolvencyStateAnchorView public immutable stateAnchor;

    uint64 public timeoutBlocks;
    bool public escapeHatchLatched;

    constructor(address vault_, address stateAnchor_, uint64 timeoutBlocks_) {
        if (vault_ == address(0) || stateAnchor_ == address(0)) {
            revert ZeroAddressNotAllowed();
        }
        vault = IHyperionVaultSettlement(vault_);
        stateAnchor = ISolvencyStateAnchorView(stateAnchor_);
        timeoutBlocks = timeoutBlocks_;
    }

    // =========================================================================
    // LIVENESS & FORCED EXIT EXECUTION
    // =========================================================================
    function lastLivenessBlock() public view returns (uint64) {
        return stateAnchor.latestEpoch() == 0 ? vault.deploymentBlock() : stateAnchor.lastAnchorBlock();
    }

    function isEscapeHatchEligible() public view returns (bool) {
        if (escapeHatchLatched || vault.isEscapeHatchFrozen()) return true;
        return block.number > uint256(lastLivenessBlock()) + uint256(timeoutBlocks);
    }

    /// @notice Permissionlessly latches Forced Exit Mode once the Sequencer liveness window `timeoutBlocks` expires.
    function activateEscapeHatch() public {
        if (escapeHatchLatched) return;
        uint64 lastLive = lastLivenessBlock();
        if (block.number <= uint256(lastLive) + uint256(timeoutBlocks)) {
            revert SequencerStillLive(lastLive, timeoutBlocks, block.number);
        }
        escapeHatchLatched = true;
        vault.activateEscapeHatchFreeze();
        emit EscapeHatchActivated(stateAnchor.latestEpoch(), lastLive, uint64(block.number));
    }

    /// @notice Executes a trustless Forced Exit against the final anchored Merkle-Sum Epoch Root.
    /// @dev `msg.sender` MUST match the cryptographic `accountOwner` committed inside the MSST leaf,
    ///      making front-running and account squatting mathematically impossible.
    function forceExit(
        uint64 accountId,
        address asset,
        uint128 leafBalanceFp,
        address recipient,
        MerkleSumLib.ProofStep[] calldata proof
    ) external nonReentrantTransient returns (uint128 exitedFp, uint256 rawPaid) {
        if (!escapeHatchLatched) {
            activateEscapeHatch();
        }
        if (recipient == address(0)) revert ZeroAddressNotAllowed();

        uint64 frozenEpoch = stateAnchor.latestEpoch();
        if (frozenEpoch == 0) {
            revert NoEpochAnchoredForLeafExit();
        }

        (bytes32 merkleSumRoot, , uint128 epochTotalLiabilityFp, ) = stateAnchor.getEpochAssetCommitment(
            frozenEpoch,
            asset
        );

        MerkleSumLib.verifyInclusion(
            merkleSumRoot,
            epochTotalLiabilityFp,
            accountId,
            msg.sender,
            asset,
            leafBalanceFp,
            proof
        );

        uint128 alreadyWithdrawnFp = vault.epochAccountWithdrawnFp(frozenEpoch, accountId, asset);
        if (leafBalanceFp <= alreadyWithdrawnFp) {
            revert NothingToExit(accountId, asset, leafBalanceFp, alreadyWithdrawnFp);
        }

        exitedFp = leafBalanceFp - alreadyWithdrawnFp;
        rawPaid = vault.payoutEscapeHatchExit(frozenEpoch, accountId, asset, exitedFp, recipient);

        emit ForcedExitExecuted(frozenEpoch, accountId, asset, msg.sender, recipient, exitedFp, rawPaid);
    }

    /// @notice Trustlessly refunds an unanchored deposit (`depositSeq > lastProcessedDepositSeq`) during Forced Exit Mode.
    function reclaimUnanchoredDeposit(
        uint64 depositSeq,
        address recipient
    ) external nonReentrantTransient returns (uint256 rawRefunded) {
        if (!escapeHatchLatched) {
            activateEscapeHatch();
        }
        if (recipient == address(0)) revert ZeroAddressNotAllowed();

        IHyperionVaultSettlement.DepositReceipt memory d = vault.getDepositReceipt(depositSeq);
        if (msg.sender != d.depositor) {
            revert UnauthorizedDepositReclaim(depositSeq, msg.sender, d.depositor);
        }

        uint64 lastAnchoredSeq = stateAnchor.lastProcessedDepositSeq();
        rawRefunded = vault.payoutUnanchoredDepositRefund(depositSeq, lastAnchoredSeq, recipient);

        emit UnanchoredDepositReclaimed(depositSeq, d.accountId, d.asset, recipient, rawRefunded);
    }
}
