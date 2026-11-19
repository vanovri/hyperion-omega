// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title IZKSolvencyVerifier
/// @notice Verifies Channel 13 Zero-Knowledge Proof of Reserves & Liabilities receipts.
/// @dev Enforces three circuit-level claims simultaneously:
///      1. 64-bit Non-Negativity Range Proof over every account leaf in the MSST.
///      2. Binary Path Sum Integrity (no finite-field wrap-around / overflow).
///      3. Global Solvency Inequality: totalLiabilityFp <= verifiedVaultReserveFp.
interface IZKSolvencyVerifier {
    struct SolvencyPublicInputs {
        uint64 epoch;
        uint64 engineSeq;
        address asset;
        bytes32 merkleSumRoot;
        bytes32 poseidonRoot;
        uint128 totalLiabilityFp;
        uint128 verifiedVaultReserveFp;
    }

    function verifySolvencyProof(
        SolvencyPublicInputs calldata inputs,
        bytes calldata zkProof
    ) external view returns (bool valid);
}
