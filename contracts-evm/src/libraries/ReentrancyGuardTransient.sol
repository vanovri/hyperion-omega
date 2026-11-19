// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title ReentrancyGuardTransient
/// @notice EIP-1153 Transient Storage (`TLOAD` / `TSTORE`, Cancun EVM) reentrancy lock.
/// @dev Saves ~4,900–19,900 gas per external entry compared to `SSTORE`-based guards and
///      leaves zero dirty storage slots at transaction end.
abstract contract ReentrancyGuardTransient {
    /// @dev Thrown when a reentrant call is detected within the same transaction execution frame.
    error ReentrancyGuardReentrantCall();

    /// @dev ERC-7201 namespace slot:
    ///      keccak256(abi.encode(uint256(keccak256("hyperion.storage.ReentrancyGuardTransient")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _REENTRANCY_GUARD_TRANSIENT_SLOT =
        0x9b779b17422d0df92223018b32b4d1fa46e071723d6817e2486d003becc55f00;

    modifier nonReentrantTransient() {
        _nonReentrantBefore();
        _;
        _nonReentrantAfter();
    }

    function _nonReentrantBefore() private {
        bytes32 slot = _REENTRANCY_GUARD_TRANSIENT_SLOT;
        bool locked;
        assembly ("memory-safe") {
            locked := tload(slot)
        }
        if (locked) {
            revert ReentrancyGuardReentrantCall();
        }
        assembly ("memory-safe") {
            tstore(slot, 1)
        }
    }

    function _nonReentrantAfter() private {
        bytes32 slot = _REENTRANCY_GUARD_TRANSIENT_SLOT;
        assembly ("memory-safe") {
            tstore(slot, 0)
        }
    }

    function _reentrancyGuardEntered() internal view returns (bool locked) {
        bytes32 slot = _REENTRANCY_GUARD_TRANSIENT_SLOT;
        assembly ("memory-safe") {
            locked := tload(slot)
        }
    }
}
