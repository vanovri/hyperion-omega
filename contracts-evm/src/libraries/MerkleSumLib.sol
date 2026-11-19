// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title MerkleSumLib
/// @notice Gas-optimized Yul-assembly verifier for Channel 13 Cryptographic Merkle-Sum Sparse Trees (MSST).
/// @dev Enforces RFC-6962 domain separation between leaves (`0x00`) and internal nodes (`0x01`)
///      to prevent second-preimage tree-depth confusion, cryptographically binds `(accountId, accountOwner, asset, balanceFp)`
///      at the leaf to eliminate front-running/squatting, and checks non-overflowing binary path sum integrity.
library MerkleSumLib {
    error MerkleSumOverflow(uint256 leftSum, uint256 rightSum);
    error MerkleSumExceedsRootTotal(uint256 accumulatedSum, uint256 expectedRootSum);
    error InvalidMerkleSumRoot(bytes32 computedHash, bytes32 expectedHash, uint256 computedSum, uint256 expectedSum);

    /// @notice Single sibling step along a binary Merkle-Sum path from leaf to root.
    struct ProofStep {
        bytes32 siblingHash;
        uint128 siblingSumFp;
        bool isLeftSibling;
    }

    /// @notice Computes domain-separated MSST leaf hash (65 bytes):
    ///         keccak256(0x00 || uint64(accountId) || address(accountOwner) || address(asset) || uint128(balanceFp))
    function computeLeafHash(
        uint64 accountId,
        address accountOwner,
        address asset,
        uint128 balanceFp
    ) internal pure returns (bytes32 leafHash) {
        assembly ("memory-safe") {
            let m := mload(0x40)
            // Bytes 0..31 (32 bytes):
            //   [0]     : 0x00 (8 bits)
            //   [1..8]  : accountId (64 bits, shl 184)
            //   [9..28] : accountOwner (160 bits, shl 24)
            //   [29..31]: top 3 bytes (24 bits) of asset (shr 136)
            let cleanId := and(accountId, 0xffffffffffffffff)
            let cleanOwner := and(accountOwner, 0xffffffffffffffffffffffffffffffffffffffff)
            let cleanAsset := and(asset, 0xffffffffffffffffffffffffffffffffffffffff)
            let cleanBal := and(balanceFp, 0xffffffffffffffffffffffffffffffff)

            let word0 := or(or(shl(184, cleanId), shl(24, cleanOwner)), shr(136, cleanAsset))
            // Bytes 32..63 (32 bytes):
            //   [32..48]: lower 17 bytes (136 bits) of asset (shl 120)
            //   [49..63]: top 15 bytes (120 bits) of balanceFp (shr 8)
            let word1 := or(shl(120, cleanAsset), shr(8, cleanBal))
            // Byte 64 (1 byte):
            //   [64]    : lowest 1 byte (8 bits) of balanceFp (shl 248)
            let word2 := shl(248, cleanBal)

            mstore(m, word0)
            mstore(add(m, 0x20), word1)
            mstore(add(m, 0x40), word2)
            leafHash := keccak256(m, 65)
        }
    }

    /// @notice Computes domain-separated MSST internal node hash and checked sum (97 bytes):
    ///         keccak256(0x01 || bytes32(leftHash) || uint128(leftSumFp) || bytes32(rightHash) || uint128(rightSumFp))
    function hashInternalNode(
        bytes32 leftHash,
        uint128 leftSumFp,
        bytes32 rightHash,
        uint128 rightSumFp
    ) internal pure returns (bytes32 parentHash, uint128 parentSumFp) {
        uint256 sum256 = uint256(leftSumFp) + uint256(rightSumFp);
        if (sum256 > type(uint128).max) {
            revert MerkleSumOverflow(leftSumFp, rightSumFp);
        }
        parentSumFp = uint128(sum256);

        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore8(m, 0x01)
            mstore(add(m, 0x01), leftHash)
            mstore(add(m, 0x21), shl(128, and(leftSumFp, 0xffffffffffffffffffffffffffffffff)))
            mstore(add(m, 0x31), rightHash)
            mstore(add(m, 0x51), shl(128, and(rightSumFp, 0xffffffffffffffffffffffffffffffff)))
            parentHash := keccak256(m, 97)
        }
    }

    /// @notice Verifies an account leaf inclusion proof and non-overflowing binary sum integrity against an anchored MSST root.
    function verifyInclusion(
        bytes32 expectedRootHash,
        uint128 expectedRootSumFp,
        uint64 accountId,
        address accountOwner,
        address asset,
        uint128 leafBalanceFp,
        ProofStep[] calldata proof
    ) internal pure {
        if (leafBalanceFp > expectedRootSumFp) {
            revert MerkleSumExceedsRootTotal(leafBalanceFp, expectedRootSumFp);
        }

        bytes32 currentHash = computeLeafHash(accountId, accountOwner, asset, leafBalanceFp);
        uint128 currentSum = leafBalanceFp;

        uint256 len = proof.length;
        for (uint256 i = 0; i < len; ) {
            ProofStep calldata step = proof[i];
            if (step.isLeftSibling) {
                (currentHash, currentSum) = hashInternalNode(
                    step.siblingHash,
                    step.siblingSumFp,
                    currentHash,
                    currentSum
                );
            } else {
                (currentHash, currentSum) = hashInternalNode(
                    currentHash,
                    currentSum,
                    step.siblingHash,
                    step.siblingSumFp
                );
            }
            if (currentSum > expectedRootSumFp) {
                revert MerkleSumExceedsRootTotal(currentSum, expectedRootSumFp);
            }
            unchecked {
                ++i;
            }
        }

        if (currentHash != expectedRootHash || currentSum != expectedRootSumFp) {
            revert InvalidMerkleSumRoot(currentHash, expectedRootHash, currentSum, expectedRootSumFp);
        }
    }
}
