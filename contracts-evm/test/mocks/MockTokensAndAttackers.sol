// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {HyperionVault} from "../../src/HyperionVault.sol";
import {CensorshipEscapeHatch} from "../../src/CensorshipEscapeHatch.sol";
import {MerkleSumLib} from "../../src/libraries/MerkleSumLib.sol";

/// @title MockERC20
/// @notice Standard or non-standard (USDT empty-return) ERC-20 token for institutional settlement testing.
contract MockERC20 {
    string public name;
    string public symbol;
    uint8 public immutable decimals;
    bool public immutable omitReturnBoolean;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory name_, string memory symbol_, uint8 decimals_, bool omitReturnBoolean_) {
        name = name_;
        symbol = symbol_;
        decimals = decimals_;
        omitReturnBoolean = omitReturnBoolean_;
    }

    function mint(address to, uint256 amount) external {
        totalSupply += amount;
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "INSUFFICIENT_BALANCE");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        if (omitReturnBoolean) {
            assembly {
                return(0, 0)
            }
        }
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= amount, "INSUFFICIENT_ALLOWANCE");
            allowance[from][msg.sender] = allowed - amount;
        }
        require(balanceOf[from] >= amount, "INSUFFICIENT_BALANCE");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        if (omitReturnBoolean) {
            assembly {
                return(0, 0)
            }
        }
        return true;
    }
}

/// @title FeeOnTransferERC20
/// @notice Adversarial deflationary token taking a 1% cut on transferFrom to test Vault balance-delta defense.
contract FeeOnTransferERC20 {
    uint8 public constant decimals = 18;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(allowance[from][msg.sender] >= amount, "ALLOWANCE");
        require(balanceOf[from] >= amount, "BALANCE");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        uint256 net = (amount * 99) / 100;
        balanceOf[to] += net;
        return true;
    }
}

/// @title MaliciousReentrantERC20
/// @notice Adversarial ERC-777-style hook token attempting reentrancy into HyperionVault & CensorshipEscapeHatch.
contract MaliciousReentrantERC20 {
    uint8 public constant decimals = 18;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    HyperionVault public targetVault;
    CensorshipEscapeHatch public targetEscapeHatch;
    bool public attackOnTransfer;
    bool public attackOnTransferFrom;
    bool public revertOnBlocked;
    bytes32 public targetWithdrawalId;
    bool public lastReentrancyBlocked;
    bytes4 public lastInnerRevertSelector;

    constructor(HyperionVault vault_, CensorshipEscapeHatch escapeHatch_) {
        targetVault = vault_;
        targetEscapeHatch = escapeHatch_;
        revertOnBlocked = true;
    }

    function configureAttack(
        bool onTransfer_,
        bool onTransferFrom_,
        bytes32 withdrawalId_
    ) external {
        attackOnTransfer = onTransfer_;
        attackOnTransferFrom = onTransferFrom_;
        targetWithdrawalId = withdrawalId_;
        revertOnBlocked = true;
    }

    function configureProbeAttack(
        bool onTransfer_,
        bool onTransferFrom_,
        bytes32 withdrawalId_
    ) external {
        attackOnTransfer = onTransfer_;
        attackOnTransferFrom = onTransferFrom_;
        targetWithdrawalId = withdrawalId_;
        revertOnBlocked = false;
        lastReentrancyBlocked = false;
        lastInnerRevertSelector = bytes4(0);
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "BALANCE");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;

        if (attackOnTransfer) {
            attackOnTransfer = false;
            (bool ok, ) = address(targetVault).call(
                abi.encodeWithSelector(HyperionVault.executeWithdrawal.selector, targetWithdrawalId)
            );
            if (!ok) {
                lastReentrancyBlocked = true;
                // Propagate revert when testing strict revert mode if requested
                revert("REENTRANCY_BLOCKED_ON_TRANSFER");
            }
        }
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(allowance[from][msg.sender] >= amount, "ALLOWANCE");
        require(balanceOf[from] >= amount, "BALANCE");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;

        if (attackOnTransferFrom) {
            attackOnTransferFrom = false;
            (bool ok, bytes memory ret) = address(targetVault).call{value: 0}(
                abi.encodeWithSelector(HyperionVault.depositERC20.selector, address(this), amount, uint64(999))
            );
            if (!ok) {
                lastReentrancyBlocked = true;
                if (ret.length >= 4) {
                    bytes4 sel;
                    assembly {
                        sel := mload(add(ret, 0x20))
                    }
                    lastInnerRevertSelector = sel;
                }
                if (revertOnBlocked) {
                    revert("REENTRANCY_BLOCKED_ON_TRANSFER_FROM");
                }
            }
        }
        return true;
    }
}

/// @title MaliciousETHReentrancyAttacker
/// @notice Attempts cross-function & same-function ETH `receive()` reentrancy against HyperionVault & EscapeHatch.
contract MaliciousETHReentrancyAttacker {
    enum AttackMode {
        NONE,
        REENTER_EXECUTE_WITHDRAWAL,
        REENTER_DEPOSIT_NATIVE,
        REENTER_FORCE_EXIT,
        REENTER_RECLAIM_DEPOSIT
    }

    HyperionVault public immutable vault;
    CensorshipEscapeHatch public immutable escapeHatch;

    AttackMode public mode;
    bytes32 public targetWithdrawalId;
    uint64 public targetDepositSeq;
    uint64 public targetAccountId;
    uint128 public targetLeafBalanceFp;
    MerkleSumLib.ProofStep[] internal _storedProof;

    bytes public capturedRevertData;

    constructor(HyperionVault vault_, CensorshipEscapeHatch escapeHatch_) {
        vault = vault_;
        escapeHatch = escapeHatch_;
    }

    function resetMode() external {
        mode = AttackMode.NONE;
    }

    function setAttackWithdrawal(bytes32 withdrawalId_) external {
        mode = AttackMode.REENTER_EXECUTE_WITHDRAWAL;
        targetWithdrawalId = withdrawalId_;
    }

    function setAttackDepositNative() external {
        mode = AttackMode.REENTER_DEPOSIT_NATIVE;
    }

    function setAttackForceExit(
        uint64 accountId_,
        uint128 leafBalanceFp_,
        MerkleSumLib.ProofStep[] calldata proof_
    ) external {
        mode = AttackMode.REENTER_FORCE_EXIT;
        targetAccountId = accountId_;
        targetLeafBalanceFp = leafBalanceFp_;
        delete _storedProof;
        for (uint256 i = 0; i < proof_.length; ++i) {
            _storedProof.push(proof_[i]);
        }
    }

    function setAttackReclaim(uint64 depositSeq_) external {
        mode = AttackMode.REENTER_RECLAIM_DEPOSIT;
        targetDepositSeq = depositSeq_;
    }

    function depositFromAttacker(uint64 accountId) external payable returns (uint64 seq, uint128 fp) {
        return vault.depositNative{value: msg.value}(accountId);
    }

    function triggerForceExit(
        uint64 accountId,
        uint128 leafBalanceFp,
        MerkleSumLib.ProofStep[] calldata proof
    ) external {
        escapeHatch.forceExit(accountId, address(0), leafBalanceFp, address(this), proof);
    }

    function triggerReclaim(uint64 depositSeq) external {
        escapeHatch.reclaimUnanchoredDeposit(depositSeq, address(this));
    }

    receive() external payable {
        AttackMode m = mode;
        mode = AttackMode.NONE;

        if (m == AttackMode.REENTER_EXECUTE_WITHDRAWAL) {
            (bool ok, bytes memory ret) = address(vault).call(
                abi.encodeWithSelector(HyperionVault.executeWithdrawal.selector, targetWithdrawalId)
            );
            require(ok, string(ret));
        } else if (m == AttackMode.REENTER_DEPOSIT_NATIVE) {
            (bool ok, bytes memory ret) = address(vault).call{value: 1e10}(
                abi.encodeWithSelector(HyperionVault.depositNative.selector, uint64(777))
            );
            require(ok, string(ret));
        } else if (m == AttackMode.REENTER_FORCE_EXIT) {
            (bool ok, bytes memory ret) = address(escapeHatch).call(
                abi.encodeWithSelector(
                    CensorshipEscapeHatch.forceExit.selector,
                    targetAccountId,
                    address(0),
                    targetLeafBalanceFp,
                    address(this),
                    _storedProof
                )
            );
            require(ok, string(ret));
        } else if (m == AttackMode.REENTER_RECLAIM_DEPOSIT) {
            (bool ok, bytes memory ret) = address(escapeHatch).call(
                abi.encodeWithSelector(
                    CensorshipEscapeHatch.reclaimUnanchoredDeposit.selector,
                    targetDepositSeq,
                    address(this)
                )
            );
            require(ok, string(ret));
        }
    }
}
