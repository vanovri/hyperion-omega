// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title SafeTransferLib
/// @notice Institutional inline-assembly token & native ETH transfer primitives.
/// @dev Protects against:
///      - Non-standard ERC-20 tokens returning empty returndata (e.g., mainnet USDT).
///      - Calls to EOAs / empty bytecode addresses (checks `extcodesize > 0` when returndata is empty).
///      - Returndata memory expansion bombs (never copies unbounded returndata to EVM memory).
library SafeTransferLib {
    error ETHTransferFailed();
    error TransferFailed();
    error TransferFromFailed();
    error BalanceQueryFailed();

    function safeTransferETH(address to, uint256 amount) internal {
        bool success;
        assembly ("memory-safe") {
            success := call(gas(), to, amount, 0, 0, 0, 0)
        }
        if (!success) revert ETHTransferFailed();
    }

    function safeTransfer(address token, address to, uint256 amount) internal {
        bool ok;
        assembly ("memory-safe") {
            let m := mload(0x40)
            // transfer(address,uint256) selector = 0xa9059cbb
            mstore(m, 0xa9059cbb00000000000000000000000000000000000000000000000000000000)
            mstore(add(m, 0x04), and(to, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(add(m, 0x24), amount)

            let callOk := call(gas(), token, 0, m, 0x44, 0x00, 0x20)
            switch returndatasize()
            case 0 {
                ok := and(callOk, gt(extcodesize(token), 0))
            }
            default {
                ok := and(callOk, and(gt(returndatasize(), 0x1f), eq(mload(0x00), 1)))
            }
        }
        if (!ok) revert TransferFailed();
    }

    function safeTransferFrom(address token, address from, address to, uint256 amount) internal {
        bool ok;
        assembly ("memory-safe") {
            let m := mload(0x40)
            // transferFrom(address,address,uint256) selector = 0x23b872dd
            mstore(m, 0x23b872dd00000000000000000000000000000000000000000000000000000000)
            mstore(add(m, 0x04), and(from, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(add(m, 0x24), and(to, 0xffffffffffffffffffffffffffffffffffffffff))
            mstore(add(m, 0x44), amount)

            let callOk := call(gas(), token, 0, m, 0x64, 0x00, 0x20)
            switch returndatasize()
            case 0 {
                ok := and(callOk, gt(extcodesize(token), 0))
            }
            default {
                ok := and(callOk, and(gt(returndatasize(), 0x1f), eq(mload(0x00), 1)))
            }
        }
        if (!ok) revert TransferFromFailed();
    }

    function balanceOf(address token, address account) internal view returns (uint256 bal) {
        bool ok;
        assembly ("memory-safe") {
            mstore(0x00, 0x70a0823100000000000000000000000000000000000000000000000000000000)
            mstore(0x04, and(account, 0xffffffffffffffffffffffffffffffffffffffff))
            let callOk := staticcall(gas(), token, 0x00, 0x24, 0x00, 0x20)
            ok := and(callOk, gt(returndatasize(), 0x1f))
            bal := mload(0x00)
        }
        if (!ok) revert BalanceQueryFailed();
    }
}
