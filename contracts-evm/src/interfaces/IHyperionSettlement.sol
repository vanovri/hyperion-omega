// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title IHyperionSettlement
/// @notice Cross-contract interfaces binding HyperionVault, SolvencyStateAnchor, and CensorshipEscapeHatch.
interface ISolvencyStateAnchorView {
    function latestEpoch() external view returns (uint64);
    function latestEngineSeq() external view returns (uint64);
    function lastProcessedDepositSeq() external view returns (uint64);
    function lastAnchorBlock() external view returns (uint64);
    function isEscapeHatchFrozen() external view returns (bool);

    function getEpochAssetCommitment(
        uint64 epoch,
        address asset
    ) external view returns (bytes32 merkleSumRoot, bytes32 poseidonRoot, uint128 totalLiabilityFp, uint256 verifiedReserveRaw);

    function markEscapeHatchFrozen() external;
}

interface IHyperionVaultSettlement {
    struct DepositReceipt {
        address depositor;
        uint64 accountId;
        uint32 blockNumber;
        address asset;
        bool reclaimed;
        uint128 amountFp;
        uint256 rawAmount;
    }

    function timelockAddress() external view returns (address);
    function deploymentBlock() external view returns (uint64);
    function latestDepositSeq() external view returns (uint64);
    function registeredAssetCount() external view returns (uint256);
    function registeredAssetAt(uint256 index) external view returns (address);
    function isEscapeHatchFrozen() external view returns (bool);

    function availableBackingReserveRaw(address asset) external view returns (uint256);
    function fpToExactRaw(address asset, uint128 amountFp) external view returns (uint256);
    function rawToFloorFp(address asset, uint256 rawAmount) external view returns (uint128);

    function epochAccountWithdrawnFp(uint64 epoch, uint64 accountId, address asset) external view returns (uint128);
    function getDepositReceipt(uint64 depositSeq) external view returns (DepositReceipt memory);

    function settleEpochAnchor(
        uint64 epoch,
        uint64 prevProcessedDepositSeq,
        uint64 newProcessedDepositSeq,
        address[] calldata assets,
        uint256[] calldata liabilitiesRaw
    ) external;

    function activateEscapeHatchFreeze() external;

    function payoutEscapeHatchExit(
        uint64 frozenEpoch,
        uint64 accountId,
        address asset,
        uint128 claimableFp,
        address recipient
    ) external returns (uint256 rawPaid);

    function payoutUnanchoredDepositRefund(
        uint64 depositSeq,
        uint64 lastAnchoredDepositSeq,
        address recipient
    ) external returns (uint256 rawRefunded);
}
