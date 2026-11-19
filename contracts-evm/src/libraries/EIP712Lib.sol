// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title EIP712Lib
/// @notice Institutional EIP-712 typed data hashing & EIP-2 non-malleable ECDSA recovery.
/// @dev Explicitly masks all sub-256-bit struct fields in inline assembly to eliminate calldata dirty-bit divergence.
library EIP712Lib {
    error InvalidSignatureLength(uint256 length);
    error InvalidSignatureSValue(bytes32 s);
    error InvalidSignatureVValue(uint8 v);
    error InvalidRecoveredSigner();

    bytes32 internal constant EIP712_DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    bytes32 internal constant WITHDRAWAL_INTENT_TYPEHASH =
        keccak256(
            "WithdrawalIntent(bytes32 withdrawalId,uint64 accountId,address recipient,address asset,uint256 rawAmount,uint128 amountFp,uint64 stateEpoch,uint64 nonce,uint64 deadline,uint256 chainId)"
        );

    /// @dev secp256k1 curve half-order n/2 per EIP-2 to reject upper-half malleable s-values.
    uint256 internal constant SECP256K1_HALF_N =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    struct WithdrawalIntent {
        bytes32 withdrawalId;
        uint64 accountId;
        address recipient;
        address asset;
        uint256 rawAmount;
        uint128 amountFp;
        uint64 stateEpoch;
        uint64 nonce;
        uint64 deadline;
        uint256 chainId;
    }

    function computeDomainSeparator(
        bytes32 nameHash,
        bytes32 versionHash,
        uint256 chainId,
        address verifyingContract
    ) internal pure returns (bytes32 sep) {
        bytes32 domainTypeHash = EIP712_DOMAIN_TYPEHASH;
        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore(m, domainTypeHash)
            mstore(add(m, 0x20), nameHash)
            mstore(add(m, 0x40), versionHash)
            mstore(add(m, 0x60), chainId)
            mstore(add(m, 0x80), and(verifyingContract, 0xffffffffffffffffffffffffffffffffffffffff))
            sep := keccak256(m, 0xa0)
        }
    }

    function hashWithdrawalIntent(WithdrawalIntent calldata intent) internal pure returns (bytes32 structHash) {
        bytes32 typeHash = WITHDRAWAL_INTENT_TYPEHASH;
        bytes32 withdrawalId = intent.withdrawalId;
        uint64 accountId = intent.accountId;
        address recipient = intent.recipient;
        address asset = intent.asset;
        uint256 rawAmount = intent.rawAmount;
        uint128 amountFp = intent.amountFp;
        uint64 stateEpoch = intent.stateEpoch;
        uint64 nonce = intent.nonce;
        uint64 deadline = intent.deadline;
        uint256 chainId = intent.chainId;

        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore(m, typeHash)
            mstore(add(m, 0x20), withdrawalId)
            mstore(add(m, 0x40), and(accountId, 0xffffffffffffffff))
            mstore(add(m, 0x60), and(recipient, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(add(m, 0x80), and(asset, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(add(m, 0xa0), rawAmount)
            mstore(add(m, 0xc0), and(amountFp, 0xffffffffffffffffffffffffffffffff))
            mstore(add(m, 0xe0), and(stateEpoch, 0xffffffffffffffff))
            mstore(add(m, 0x100), and(nonce, 0xffffffffffffffff))
            mstore(add(m, 0x120), and(deadline, 0xffffffffffffffff))
            mstore(add(m, 0x140), chainId)
            structHash := keccak256(m, 0x160)
        }
    }

    function toTypedDataHash(bytes32 domainSeparator, bytes32 structHash) internal pure returns (bytes32 digest) {
        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore(m, 0x1901000000000000000000000000000000000000000000000000000000000000)
            mstore(add(m, 0x02), domainSeparator)
            mstore(add(m, 0x22), structHash)
            digest := keccak256(m, 0x42)
        }
    }

    /// @notice Recovers signer from 65-byte `(r, s, v)` signature with strict EIP-2 malleability enforcement.
    function recoverStrict(bytes32 digest, bytes calldata signature) internal pure returns (address signer) {
        if (signature.length != 65) {
            revert InvalidSignatureLength(signature.length);
        }
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly ("memory-safe") {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 0x20))
            v := byte(0, calldataload(add(signature.offset, 0x40)))
        }

        if (uint256(s) > SECP256K1_HALF_N || uint256(s) == 0 || uint256(r) == 0) {
            revert InvalidSignatureSValue(s);
        }
        if (v != 27 && v != 28) {
            revert InvalidSignatureVValue(v);
        }

        signer = ecrecover(digest, v, r, s);
        if (signer == address(0)) {
            revert InvalidRecoveredSigner();
        }
    }
}
