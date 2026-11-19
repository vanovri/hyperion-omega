// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

/// @title IERC20Minimal
/// @notice Minimal gas-clean ERC-20 interface for institutional vault custody.
interface IERC20Minimal {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 value) external returns (bool);
    function transferFrom(address from, address to, uint256 value) external returns (bool);
    function decimals() external view returns (uint8);
}
