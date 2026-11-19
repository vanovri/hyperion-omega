// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {ISolvencyStateAnchorView, IHyperionVaultSettlement} from "./interfaces/IHyperionSettlement.sol";
import {ReentrancyGuardTransient} from "./libraries/ReentrancyGuardTransient.sol";
import {SafeTransferLib} from "./libraries/SafeTransferLib.sol";
import {MerkleSumLib} from "./libraries/MerkleSumLib.sol";
import {EIP712Lib} from "./libraries/EIP712Lib.sol";

/// @title HyperionVault
/// @notice Institutional L2 Settlement Custody Vault for HYPERION.
/// @dev Handles:
///      1. Lossless Fixed-Point (`1e8` atomic ticks) Native ETH & ERC-20 deposits with fee-on-transfer protection.
///      2. Structured EIP-712 signed withdrawal queue with cross-chain replay protection, EIP-2 non-malleability,
///         per-account nonce nullifiers, deadline expiration, and Merkle-Sum liability binding.
///      3. Emergency rolling-window daily withdrawal rate limit (Circuit Breaker) & Guardian pause.
///      4. Self-contained Governance Timelock with custody-token anti-rug protection.
contract HyperionVault is IHyperionVaultSettlement, ReentrancyGuardTransient {
    // =========================================================================
    // CUSTOM ERRORS
    // =========================================================================
    error UnauthorizedCaller(address caller);
    error ZeroAddressNotAllowed();
    error ZeroAmountNotAllowed();
    error InvalidAccountId();
    error AssetNotSupported(address asset);
    error AssetAlreadyRegistered(address asset);
    error MaxAssetsReached(uint256 maxAssets);
    error AssetDepositsPaused(address asset);
    error InvalidAssetDecimals(uint8 decimals);
    error UnrepresentableRawDust(uint256 rawAmount, uint256 scaleUnit);
    error SubTokenPrecisionDust(uint128 amountFp, uint256 divisor);
    error RawFpMismatch(uint256 providedRaw, uint256 expectedRaw);
    error FeeOnTransferTokenUnsupported(uint256 expectedDelta, uint256 actualDelta);
    error EmergencyPaused();
    error EscapeHatchModeActive();
    error EscapeHatchNotActive();
    error CrossChainReplayRejected(uint256 intentChainId, uint256 currentChainId);
    error WithdrawalDeadlineExpired(uint64 deadline, uint256 currentTimestamp);
    error StaleOrUnanchoredEpoch(uint64 intentEpoch, uint64 latestEpoch);
    error AccountNonceAlreadyUsed(uint64 accountId, uint64 nonce);
    error NullifierAlreadyConsumed(bytes32 nullifierHash);
    error WithdrawalAlreadyExists(bytes32 withdrawalId);
    error WithdrawalNotQueued(bytes32 withdrawalId);
    error WithdrawalCooldownActive(bytes32 withdrawalId, uint64 unlockTimestamp, uint256 currentTimestamp);
    error UnauthorizedSequencerSigner(address recoveredSigner);
    error ExceedsAnchoredAccountBalance(uint64 accountId, uint128 requestedCumulativeFp, uint128 leafBalanceFp);
    error ExceedsProtocolSurplus(address asset, uint256 requestedRaw, uint256 availableSurplusRaw);
    error DailyWithdrawalLimitExceeded(address asset, uint256 requestedRaw, uint256 remainingWindowRaw);
    error DepositAlreadyAnchored(uint64 depositSeq, uint64 lastAnchoredSeq);
    error DepositAlreadyReclaimed(uint64 depositSeq);
    error InvalidDepositSeq(uint64 depositSeq);
    error TimelockDelayBelowMinimum(uint256 providedDelay, uint256 minDelay);
    error TimelockDelayAboveMaximum(uint256 providedDelay, uint256 maxDelay);
    error TimelockOperationAlreadyScheduled(bytes32 opId);
    error TimelockOperationNotReady(bytes32 opId, uint256 readyTimestamp, uint256 currentTimestamp);
    error TimelockOperationExpired(bytes32 opId, uint256 expiryTimestamp, uint256 currentTimestamp);
    error TimelockPredecessorNotExecuted(bytes32 predecessor);
    error TimelockCustodyBypassForbidden(address target, uint256 value);
    error TimelockCallReverted(bytes returnData);
    error AmountFpOverflow(uint256 computedFp);

    // =========================================================================
    // EVENTS
    // =========================================================================
    event AssetRegistered(address indexed asset, uint40 indexed assetId, uint8 decimals, uint192 dailyLimitRaw);
    event AssetConfigUpdated(address indexed asset, bool depositsPaused, uint192 dailyLimitRaw, uint256 largeThresholdRaw);
    event DepositReceived(
        uint64 indexed depositSeq,
        uint64 indexed accountId,
        address indexed asset,
        address depositor,
        uint256 rawAmount,
        uint128 amountFp
    );
    event WithdrawalQueued(
        bytes32 indexed withdrawalId,
        uint64 indexed accountId,
        address indexed asset,
        address recipient,
        uint256 rawAmount,
        uint128 amountFp,
        uint64 stateEpoch,
        uint64 nonce,
        uint64 unlockTimestamp
    );
    event WithdrawalExecuted(
        bytes32 indexed withdrawalId,
        uint64 indexed accountId,
        address indexed asset,
        address recipient,
        uint256 rawAmount
    );
    event EscapeHatchExitPaid(
        uint64 indexed frozenEpoch,
        uint64 indexed accountId,
        address indexed asset,
        address recipient,
        uint128 exitedFp,
        uint256 rawPaid
    );
    event UnanchoredDepositRefunded(
        uint64 indexed depositSeq,
        uint64 indexed accountId,
        address indexed asset,
        address recipient,
        uint256 rawRefunded
    );
    event SequencerSignerUpdated(address indexed signer, bool authorized);
    event EmergencyPauseChanged(bool paused, address indexed triggeredBy);
    event EscapeHatchFreezeLatched(uint64 blockNumber);
    event TimelockScheduled(
        bytes32 indexed opId,
        address indexed target,
        uint256 value,
        bytes data,
        bytes32 predecessor,
        uint256 readyTimestamp
    );
    event TimelockExecuted(bytes32 indexed opId, address indexed target, uint256 value);
    event TimelockCancelled(bytes32 indexed opId);

    // =========================================================================
    // CONSTANTS & IMMUTABLES
    // =========================================================================
    uint8 public constant HYPERION_FP_DECIMALS = 8;
    uint256 public constant MAX_REGISTERED_ASSETS = 16;
    uint256 public constant MIN_TIMELOCK_DELAY = 2 days;
    uint256 public constant MAX_TIMELOCK_DELAY = 30 days;
    uint256 public constant TIMELOCK_GRACE_PERIOD = 7 days;
    uint256 private constant _TIMELOCK_DONE_TIMESTAMP = 1;

    bytes32 private constant _EIP712_NAME_HASH = keccak256("HyperionSettlementVault");
    bytes32 private constant _EIP712_VERSION_HASH = keccak256("1");

    uint256 private immutable _CACHED_CHAIN_ID;
    bytes32 private immutable _CACHED_DOMAIN_SEPARATOR;
    uint64 public immutable override deploymentBlock;

    // =========================================================================
    // PACKED STORAGE STRUCTS
    // =========================================================================
    /// @dev Exact 1-slot (256-bit) packed asset configuration: 8 + 8 + 8 + 40 + 192 = 256 bits.
    struct AssetConfig {
        bool supported;
        bool depositsPaused;
        uint8 decimals;
        uint40 assetId;
        uint192 dailyLimitRaw;
    }

    /// @dev Exact 1-slot (256-bit) packed 24h circuit-breaker window: 64 + 192 = 256 bits.
    struct DailyRateWindow {
        uint64 dayIndex;
        uint192 withdrawnRaw;
    }

    enum WithdrawalStatus {
        NONE,
        QUEUED,
        EXECUTED
    }

    struct QueuedWithdrawal {
        address recipient;
        uint64 accountId;
        WithdrawalStatus status;
        address asset;
        uint64 unlockTimestamp;
        uint128 amountFp;
        uint256 rawAmount;
    }

    // =========================================================================
    // STATE VARIABLES
    // =========================================================================
    address public governanceProposer;
    address public guardian;
    ISolvencyStateAnchorView public stateAnchor;
    address public escapeHatch;

    uint64 public override latestDepositSeq;
    uint64 public withdrawalCooldownSec;
    uint64 public escapeHatchTimeoutBlocks;
    bool public emergencyPaused;
    bool public override isEscapeHatchFrozen;
    bool private _protocolWired;

    uint256 public timelockMinDelay;
    mapping(bytes32 => uint256) public timelockTimestamps;

    address[] internal _registeredAssets;
    mapping(address => AssetConfig) internal _assets;
    mapping(address => uint256) public largeWithdrawalThresholdRaw;
    mapping(address => DailyRateWindow) internal _dailyWindows;

    /// @notice Segregated custody buckets guaranteeing Eternal Solvency Invariant.
    mapping(address => uint256) public unanchoredDepositsRaw;
    mapping(address => uint256) public queuedWithdrawalsRaw;
    mapping(address => uint256) public remainingAnchoredLiabilitiesRaw;

    mapping(uint64 => DepositReceipt) internal _deposits;
    mapping(bytes32 => QueuedWithdrawal) internal _withdrawals;
    mapping(address => bool) public isSequencerSigner;
    mapping(uint64 => mapping(uint64 => bool)) public usedAccountNonces;
    mapping(bytes32 => bool) public usedNullifiers;
    mapping(uint64 => mapping(uint64 => mapping(address => uint128))) public override epochAccountWithdrawnFp;

    // =========================================================================
    // MODIFIERS
    // =========================================================================
    modifier onlyTimelock() {
        if (msg.sender != address(this)) {
            revert UnauthorizedCaller(msg.sender);
        }
        _;
    }

    constructor(
        address governanceProposer_,
        address guardian_,
        address initialSequencerSigner_,
        uint64 withdrawalCooldownSec_,
        uint64 escapeHatchTimeoutBlocks_,
        uint256 timelockMinDelay_
    ) {
        if (
            governanceProposer_ == address(0) ||
            guardian_ == address(0) ||
            initialSequencerSigner_ == address(0)
        ) {
            revert ZeroAddressNotAllowed();
        }
        if (timelockMinDelay_ < MIN_TIMELOCK_DELAY) {
            revert TimelockDelayBelowMinimum(timelockMinDelay_, MIN_TIMELOCK_DELAY);
        }
        if (timelockMinDelay_ > MAX_TIMELOCK_DELAY) {
            revert TimelockDelayAboveMaximum(timelockMinDelay_, MAX_TIMELOCK_DELAY);
        }
        if (escapeHatchTimeoutBlocks_ == 0) {
            revert ZeroAmountNotAllowed();
        }

        governanceProposer = governanceProposer_;
        guardian = guardian_;
        withdrawalCooldownSec = withdrawalCooldownSec_;
        escapeHatchTimeoutBlocks = escapeHatchTimeoutBlocks_;
        timelockMinDelay = timelockMinDelay_;
        isSequencerSigner[initialSequencerSigner_] = true;

        deploymentBlock = uint64(block.number);
        _CACHED_CHAIN_ID = block.chainid;
        _CACHED_DOMAIN_SEPARATOR = EIP712Lib.computeDomainSeparator(
            _EIP712_NAME_HASH,
            _EIP712_VERSION_HASH,
            block.chainid,
            address(this)
        );

        emit SequencerSignerUpdated(initialSequencerSigner_, true);
    }

    /// @notice Wires SolvencyStateAnchor, CensorshipEscapeHatch, and initial assets once at deployment.
    function initializeSettlementSuite(
        address stateAnchor_,
        address escapeHatch_,
        address[] calldata initialAssets,
        uint8[] calldata decimalsList,
        uint192[] calldata dailyLimitsRaw,
        uint256[] calldata largeThresholdsRaw
    ) external {
        if (_protocolWired || msg.sender != governanceProposer) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (stateAnchor_ == address(0) || escapeHatch_ == address(0)) {
            revert ZeroAddressNotAllowed();
        }
        _protocolWired = true;
        stateAnchor = ISolvencyStateAnchorView(stateAnchor_);
        escapeHatch = escapeHatch_;

        uint256 len = initialAssets.length;
        for (uint256 i = 0; i < len; ) {
            _registerAssetInternal(
                initialAssets[i],
                decimalsList[i],
                dailyLimitsRaw[i],
                largeThresholdsRaw[i]
            );
            unchecked {
                ++i;
            }
        }
    }

    // =========================================================================
    // DEPOSIT FLOWS (NATIVE ETH & ERC-20)
    // =========================================================================
    /// @notice Deposits Native ETH (`asset == address(0)`) into L2 settlement custody.
    function depositNative(
        uint64 accountId
    ) external payable nonReentrantTransient returns (uint64 depositSeq, uint128 amountFp) {
        return _recordDeposit(address(0), msg.value, accountId, msg.sender);
    }

    /// @notice Deposits supported ERC-20 tokens into L2 settlement custody with balance-delta verification.
    function depositERC20(
        address asset,
        uint256 rawAmount,
        uint64 accountId
    ) external nonReentrantTransient returns (uint64 depositSeq, uint128 amountFp) {
        if (asset == address(0)) revert AssetNotSupported(asset);
        uint256 balBefore = SafeTransferLib.balanceOf(asset, address(this));
        SafeTransferLib.safeTransferFrom(asset, msg.sender, address(this), rawAmount);
        uint256 balAfter = SafeTransferLib.balanceOf(asset, address(this));
        uint256 actualReceived = balAfter - balBefore;
        if (actualReceived != rawAmount) {
            revert FeeOnTransferTokenUnsupported(rawAmount, actualReceived);
        }
        return _recordDeposit(asset, rawAmount, accountId, msg.sender);
    }

    function _recordDeposit(
        address asset,
        uint256 rawAmount,
        uint64 accountId,
        address depositor
    ) internal returns (uint64 depositSeq, uint128 amountFp) {
        if (emergencyPaused) revert EmergencyPaused();
        if (_isEscapeHatchTriggered()) revert EscapeHatchModeActive();
        if (accountId == 0) revert InvalidAccountId();
        if (rawAmount == 0) revert ZeroAmountNotAllowed();

        AssetConfig memory cfg = _assets[asset];
        if (!cfg.supported) revert AssetNotSupported(asset);
        if (cfg.depositsPaused) revert AssetDepositsPaused(asset);

        amountFp = _rawToExactFp(cfg.decimals, rawAmount);
        depositSeq = ++latestDepositSeq;
        unanchoredDepositsRaw[asset] += rawAmount;

        _deposits[depositSeq] = DepositReceipt({
            depositor: depositor,
            accountId: accountId,
            blockNumber: uint32(block.number),
            asset: asset,
            reclaimed: false,
            amountFp: amountFp,
            rawAmount: rawAmount
        });

        emit DepositReceived(depositSeq, accountId, asset, depositor, rawAmount, amountFp);
    }

    // =========================================================================
    // STRUCTURED EIP-712 SIGNED WITHDRAWAL QUEUE
    // =========================================================================
    /// @notice Enqueues an EIP-712 Sequencer-authorized withdrawal bound to the active Merkle-Sum Epoch Root.
    /// @dev Protects other depositors even under full Sequencer key compromise by verifying the account's
    ///      committed Merkle-Sum leaf cap on-chain (`cumulativeEpochWithdrawnFp <= leafBalanceFp`).
    function queueWithdrawal(
        EIP712Lib.WithdrawalIntent calldata intent,
        bytes calldata sequencerSig,
        address leafOwner,
        uint128 leafBalanceFp,
        MerkleSumLib.ProofStep[] calldata leafProof
    ) external nonReentrantTransient returns (uint64 unlockTimestamp) {
        if (leafOwner == address(0)) revert ZeroAddressNotAllowed();
        _validateWithdrawalCommon(intent, sequencerSig);

        (bytes32 epochRoot, , uint128 epochTotalLiabilityFp, ) = stateAnchor.getEpochAssetCommitment(
            intent.stateEpoch,
            intent.asset
        );

        MerkleSumLib.verifyInclusion(
            epochRoot,
            epochTotalLiabilityFp,
            intent.accountId,
            leafOwner,
            intent.asset,
            leafBalanceFp,
            leafProof
        );

        uint128 prevWithdrawnFp = epochAccountWithdrawnFp[intent.stateEpoch][intent.accountId][intent.asset];
        uint128 nextWithdrawnFp = prevWithdrawnFp + intent.amountFp;
        if (nextWithdrawnFp > leafBalanceFp) {
            revert ExceedsAnchoredAccountBalance(intent.accountId, nextWithdrawnFp, leafBalanceFp);
        }

        _consumeDailyWithdrawalLimit(intent.asset, intent.rawAmount);

        bytes32 nullifier = computeWithdrawalNullifier(
            intent.chainId,
            address(this),
            intent.accountId,
            intent.nonce,
            intent.stateEpoch
        );
        usedNullifiers[nullifier] = true;
        usedAccountNonces[intent.accountId][intent.nonce] = true;
        epochAccountWithdrawnFp[intent.stateEpoch][intent.accountId][intent.asset] = nextWithdrawnFp;

        // Move exact raw backing from remainingAnchoredLiabilitiesRaw into segregated queuedWithdrawalsRaw
        remainingAnchoredLiabilitiesRaw[intent.asset] -= intent.rawAmount;
        queuedWithdrawalsRaw[intent.asset] += intent.rawAmount;

        unlockTimestamp = _enqueueWithdrawalRecord(intent);
    }

    /// @notice Enqueues an EIP-712 Sequencer-authorized withdrawal funded strictly from unencumbered Protocol Surplus
    ///         (e.g., intra-epoch trading PnL or treasury fee withdrawals prior to the next epoch anchor).
    /// @dev Mathematically incapable of touching `unanchoredDepositsRaw`, `queuedWithdrawalsRaw`, or
    ///      `remainingAnchoredLiabilitiesRaw`.
    function queueSurplusWithdrawal(
        EIP712Lib.WithdrawalIntent calldata intent,
        bytes calldata sequencerSig
    ) external nonReentrantTransient returns (uint64 unlockTimestamp) {
        _validateWithdrawalCommon(intent, sequencerSig);

        uint256 surplusRaw = protocolSurplusRaw(intent.asset);
        if (intent.rawAmount > surplusRaw) {
            revert ExceedsProtocolSurplus(intent.asset, intent.rawAmount, surplusRaw);
        }

        _consumeDailyWithdrawalLimit(intent.asset, intent.rawAmount);
        bytes32 nullifier = computeWithdrawalNullifier(
            intent.chainId,
            address(this),
            intent.accountId,
            intent.nonce,
            intent.stateEpoch
        );
        usedNullifiers[nullifier] = true;
        usedAccountNonces[intent.accountId][intent.nonce] = true;
        queuedWithdrawalsRaw[intent.asset] += intent.rawAmount;

        unlockTimestamp = _enqueueWithdrawalRecord(intent);
    }

    /// @notice Finalizes and transfers a queued withdrawal once its cooldown timestamp has elapsed.
    /// @dev Already-segregated queued withdrawals remain claimable even if Escape Hatch activates later,
    ///      as their 1:1 backing is isolated in `queuedWithdrawalsRaw`.
    function executeWithdrawal(bytes32 withdrawalId) external nonReentrantTransient {
        if (emergencyPaused && !_isEscapeHatchTriggered()) {
            revert EmergencyPaused();
        }

        QueuedWithdrawal storage w = _withdrawals[withdrawalId];
        if (w.status != WithdrawalStatus.QUEUED) {
            revert WithdrawalNotQueued(withdrawalId);
        }
        if (block.timestamp < w.unlockTimestamp) {
            revert WithdrawalCooldownActive(withdrawalId, w.unlockTimestamp, block.timestamp);
        }

        // Checks-Effects-Interactions (CEI) + EIP-1153 ReentrancyGuardTransient
        w.status = WithdrawalStatus.EXECUTED;
        address asset = w.asset;
        address recipient = w.recipient;
        uint256 rawAmount = w.rawAmount;
        uint64 accountId = w.accountId;

        queuedWithdrawalsRaw[asset] -= rawAmount;
        _payoutRaw(asset, recipient, rawAmount);

        emit WithdrawalExecuted(withdrawalId, accountId, asset, recipient, rawAmount);
    }

    function _validateWithdrawalCommon(
        EIP712Lib.WithdrawalIntent calldata intent,
        bytes calldata sequencerSig
    ) internal view {
        if (emergencyPaused) revert EmergencyPaused();
        if (_isEscapeHatchTriggered()) revert EscapeHatchModeActive();
        if (intent.chainId != block.chainid) {
            revert CrossChainReplayRejected(intent.chainId, block.chainid);
        }
        if (block.timestamp > intent.deadline) {
            revert WithdrawalDeadlineExpired(intent.deadline, block.timestamp);
        }
        if (intent.recipient == address(0)) revert ZeroAddressNotAllowed();
        if (intent.accountId == 0) revert InvalidAccountId();
        if (intent.amountFp == 0 || intent.rawAmount == 0) revert ZeroAmountNotAllowed();

        AssetConfig memory cfg = _assets[intent.asset];
        if (!cfg.supported) revert AssetNotSupported(intent.asset);

        uint256 expectedRaw = _fpToExactRaw(cfg.decimals, intent.amountFp);
        if (intent.rawAmount != expectedRaw) {
            revert RawFpMismatch(intent.rawAmount, expectedRaw);
        }

        uint64 currentEpoch = stateAnchor.latestEpoch();
        if (currentEpoch == 0 || intent.stateEpoch != currentEpoch) {
            revert StaleOrUnanchoredEpoch(intent.stateEpoch, currentEpoch);
        }

        if (usedAccountNonces[intent.accountId][intent.nonce]) {
            revert AccountNonceAlreadyUsed(intent.accountId, intent.nonce);
        }
        bytes32 nullifier = computeWithdrawalNullifier(
            intent.chainId,
            address(this),
            intent.accountId,
            intent.nonce,
            intent.stateEpoch
        );
        if (usedNullifiers[nullifier]) {
            revert NullifierAlreadyConsumed(nullifier);
        }
        if (_withdrawals[intent.withdrawalId].status != WithdrawalStatus.NONE) {
            revert WithdrawalAlreadyExists(intent.withdrawalId);
        }

        bytes32 digest = EIP712Lib.toTypedDataHash(domainSeparator(), EIP712Lib.hashWithdrawalIntent(intent));
        address recovered = EIP712Lib.recoverStrict(digest, sequencerSig);
        if (!isSequencerSigner[recovered]) {
            revert UnauthorizedSequencerSigner(recovered);
        }
    }

    function _enqueueWithdrawalRecord(
        EIP712Lib.WithdrawalIntent calldata intent
    ) internal returns (uint64 unlockTimestamp) {
        uint64 delay = intent.rawAmount >= largeWithdrawalThresholdRaw[intent.asset]
            ? withdrawalCooldownSec
            : 0;
        unlockTimestamp = uint64(block.timestamp) + delay;

        _withdrawals[intent.withdrawalId] = QueuedWithdrawal({
            recipient: intent.recipient,
            accountId: intent.accountId,
            status: WithdrawalStatus.QUEUED,
            asset: intent.asset,
            unlockTimestamp: unlockTimestamp,
            amountFp: intent.amountFp,
            rawAmount: intent.rawAmount
        });

        emit WithdrawalQueued(
            intent.withdrawalId,
            intent.accountId,
            intent.asset,
            intent.recipient,
            intent.rawAmount,
            intent.amountFp,
            intent.stateEpoch,
            intent.nonce,
            unlockTimestamp
        );
    }

    function _consumeDailyWithdrawalLimit(address asset, uint256 rawAmount) internal {
        uint192 limitRaw = _assets[asset].dailyLimitRaw;
        uint64 today = uint64(block.timestamp / 1 days);
        DailyRateWindow memory win = _dailyWindows[asset];

        uint256 usedToday = win.dayIndex == today ? uint256(win.withdrawnRaw) : 0;
        uint256 nextUsed = usedToday + rawAmount;
        if (nextUsed > uint256(limitRaw)) {
            uint256 remaining = uint256(limitRaw) > usedToday ? uint256(limitRaw) - usedToday : 0;
            revert DailyWithdrawalLimitExceeded(asset, rawAmount, remaining);
        }

        _dailyWindows[asset] = DailyRateWindow({dayIndex: today, withdrawnRaw: uint192(nextUsed)});
    }

    // =========================================================================
    // SOLVENCY ANCHOR & ESCAPE HATCH HOOKS
    // =========================================================================
    /// @inheritdoc IHyperionVaultSettlement
    function settleEpochAnchor(
        uint64 /* epoch */,
        uint64 prevProcessedDepositSeq,
        uint64 newProcessedDepositSeq,
        address[] calldata assets,
        uint256[] calldata liabilitiesRaw
    ) external override {
        if (msg.sender != address(stateAnchor)) {
            revert UnauthorizedCaller(msg.sender);
        }

        uint64 headSeq = latestDepositSeq;
        if (newProcessedDepositSeq == headSeq) {
            // Fast O(1) graduation when anchoring up to latest deposit head
            uint256 n = assets.length;
            for (uint256 i = 0; i < n; ) {
                unanchoredDepositsRaw[assets[i]] = 0;
                unchecked {
                    ++i;
                }
            }
        } else if (newProcessedDepositSeq > prevProcessedDepositSeq) {
            uint64 deltaCount = newProcessedDepositSeq - prevProcessedDepositSeq;
            uint64 tailCount = headSeq - newProcessedDepositSeq;

            if (deltaCount <= tailCount) {
                for (uint64 s = prevProcessedDepositSeq + 1; s <= newProcessedDepositSeq; ) {
                    DepositReceipt storage d = _deposits[s];
                    unanchoredDepositsRaw[d.asset] -= d.rawAmount;
                    unchecked {
                        ++s;
                    }
                }
            } else {
                uint256 n = assets.length;
                for (uint256 i = 0; i < n; ) {
                    unanchoredDepositsRaw[assets[i]] = 0;
                    unchecked {
                        ++i;
                    }
                }
                for (uint64 s = newProcessedDepositSeq + 1; s <= headSeq; ) {
                    DepositReceipt storage d = _deposits[s];
                    unanchoredDepositsRaw[d.asset] += d.rawAmount;
                    unchecked {
                        ++s;
                    }
                }
            }
        }

        uint256 len = assets.length;
        for (uint256 i = 0; i < len; ) {
            remainingAnchoredLiabilitiesRaw[assets[i]] = liabilitiesRaw[i];
            unchecked {
                ++i;
            }
        }
    }

    /// @inheritdoc IHyperionVaultSettlement
    function activateEscapeHatchFreeze() external override {
        if (msg.sender != escapeHatch) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (!isEscapeHatchFrozen) {
            isEscapeHatchFrozen = true;
            stateAnchor.markEscapeHatchFrozen();
            emit EscapeHatchFreezeLatched(uint64(block.number));
        }
    }

    /// @inheritdoc IHyperionVaultSettlement
    function payoutEscapeHatchExit(
        uint64 frozenEpoch,
        uint64 accountId,
        address asset,
        uint128 claimableFp,
        address recipient
    ) external override nonReentrantTransient returns (uint256 rawPaid) {
        if (msg.sender != escapeHatch) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (!isEscapeHatchFrozen) revert EscapeHatchNotActive();
        if (recipient == address(0)) revert ZeroAddressNotAllowed();

        AssetConfig memory cfg = _assets[asset];
        if (!cfg.supported) revert AssetNotSupported(asset);

        rawPaid = _fpToFloorRaw(cfg.decimals, claimableFp);
        if (rawPaid == 0) revert ZeroAmountNotAllowed();

        epochAccountWithdrawnFp[frozenEpoch][accountId][asset] += claimableFp;
        uint256 liabilityDeduct = rawPaid <= remainingAnchoredLiabilitiesRaw[asset]
            ? rawPaid
            : remainingAnchoredLiabilitiesRaw[asset];
        remainingAnchoredLiabilitiesRaw[asset] -= liabilityDeduct;

        _payoutRaw(asset, recipient, rawPaid);
        emit EscapeHatchExitPaid(frozenEpoch, accountId, asset, recipient, claimableFp, rawPaid);
    }

    /// @inheritdoc IHyperionVaultSettlement
    function payoutUnanchoredDepositRefund(
        uint64 depositSeq,
        uint64 lastAnchoredDepositSeq,
        address recipient
    ) external override nonReentrantTransient returns (uint256 rawRefunded) {
        if (msg.sender != escapeHatch) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (!isEscapeHatchFrozen) revert EscapeHatchNotActive();
        if (depositSeq == 0 || depositSeq > latestDepositSeq) {
            revert InvalidDepositSeq(depositSeq);
        }
        if (depositSeq <= lastAnchoredDepositSeq) {
            revert DepositAlreadyAnchored(depositSeq, lastAnchoredDepositSeq);
        }

        DepositReceipt storage d = _deposits[depositSeq];
        if (d.reclaimed) revert DepositAlreadyReclaimed(depositSeq);
        if (recipient == address(0)) revert ZeroAddressNotAllowed();

        d.reclaimed = true;
        rawRefunded = d.rawAmount;
        address asset = d.asset;
        uint64 accountId = d.accountId;

        unanchoredDepositsRaw[asset] -= rawRefunded;
        _payoutRaw(asset, recipient, rawRefunded);

        emit UnanchoredDepositRefunded(depositSeq, accountId, asset, recipient, rawRefunded);
    }

    // =========================================================================
    // GOVERNANCE TIMELOCK & EMERGENCY CIRCUIT BREAKER
    // =========================================================================
    function computeTimelockOperationId(
        address target,
        uint256 value,
        bytes calldata data,
        bytes32 predecessor,
        bytes32 salt
    ) public pure returns (bytes32) {
        return keccak256(abi.encode(target, value, data, predecessor, salt));
    }

    function scheduleTimelockOperation(
        address target,
        uint256 value,
        bytes calldata data,
        bytes32 predecessor,
        bytes32 salt,
        uint256 delay
    ) external returns (bytes32 opId) {
        if (msg.sender != governanceProposer) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (target == address(0)) revert ZeroAddressNotAllowed();
        if (_assets[target].supported || value > 0) {
            revert TimelockCustodyBypassForbidden(target, value);
        }
        if (delay < timelockMinDelay) {
            revert TimelockDelayBelowMinimum(delay, timelockMinDelay);
        }
        if (delay > MAX_TIMELOCK_DELAY) {
            revert TimelockDelayAboveMaximum(delay, MAX_TIMELOCK_DELAY);
        }

        opId = computeTimelockOperationId(target, value, data, predecessor, salt);
        if (timelockTimestamps[opId] != 0) {
            revert TimelockOperationAlreadyScheduled(opId);
        }

        uint256 readyAt = block.timestamp + delay;
        timelockTimestamps[opId] = readyAt;
        emit TimelockScheduled(opId, target, value, data, predecessor, readyAt);
    }

    function cancelTimelockOperation(bytes32 opId) external {
        if (msg.sender != governanceProposer && msg.sender != guardian) {
            revert UnauthorizedCaller(msg.sender);
        }
        uint256 ts = timelockTimestamps[opId];
        if (ts <= _TIMELOCK_DONE_TIMESTAMP) {
            revert TimelockOperationNotReady(opId, ts, block.timestamp);
        }
        delete timelockTimestamps[opId];
        emit TimelockCancelled(opId);
    }

    function executeTimelockOperation(
        address target,
        uint256 value,
        bytes calldata data,
        bytes32 predecessor,
        bytes32 salt
    ) external nonReentrantTransient returns (bytes memory result) {
        if (msg.sender != governanceProposer) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (_assets[target].supported || value > 0) {
            revert TimelockCustodyBypassForbidden(target, value);
        }
        if (predecessor != bytes32(0) && timelockTimestamps[predecessor] != _TIMELOCK_DONE_TIMESTAMP) {
            revert TimelockPredecessorNotExecuted(predecessor);
        }

        bytes32 opId = computeTimelockOperationId(target, value, data, predecessor, salt);
        uint256 readyAt = timelockTimestamps[opId];
        if (readyAt <= _TIMELOCK_DONE_TIMESTAMP || block.timestamp < readyAt) {
            revert TimelockOperationNotReady(opId, readyAt, block.timestamp);
        }
        if (block.timestamp > readyAt + TIMELOCK_GRACE_PERIOD) {
            revert TimelockOperationExpired(opId, readyAt + TIMELOCK_GRACE_PERIOD, block.timestamp);
        }

        timelockTimestamps[opId] = _TIMELOCK_DONE_TIMESTAMP;
        (bool ok, bytes memory ret) = target.call(data);
        if (!ok) {
            revert TimelockCallReverted(ret);
        }
        emit TimelockExecuted(opId, target, value);
        return ret;
    }

    /// @notice Guardian emergency circuit breaker (immediate pause; unpause requires Timelock or Guardian).
    function setEmergencyPause(bool paused) external {
        if (paused) {
            if (msg.sender != guardian && msg.sender != address(this)) {
                revert UnauthorizedCaller(msg.sender);
            }
        } else {
            if (msg.sender != address(this)) {
                revert UnauthorizedCaller(msg.sender);
            }
        }
        emergencyPaused = paused;
        emit EmergencyPauseChanged(paused, msg.sender);
    }

    function registerAsset(
        address asset,
        uint8 decimals,
        uint192 dailyLimitRaw,
        uint256 largeThresholdRaw
    ) external onlyTimelock {
        _registerAssetInternal(asset, decimals, dailyLimitRaw, largeThresholdRaw);
    }

    function updateAssetConfig(
        address asset,
        bool depositsPaused,
        uint192 dailyLimitRaw,
        uint256 largeThresholdRaw
    ) external onlyTimelock {
        AssetConfig storage cfg = _assets[asset];
        if (!cfg.supported) revert AssetNotSupported(asset);
        if (dailyLimitRaw == 0) revert ZeroAmountNotAllowed();
        cfg.depositsPaused = depositsPaused;
        cfg.dailyLimitRaw = dailyLimitRaw;
        largeWithdrawalThresholdRaw[asset] = largeThresholdRaw;
        emit AssetConfigUpdated(asset, depositsPaused, dailyLimitRaw, largeThresholdRaw);
    }

    function setSequencerSigner(address signer, bool authorized) external onlyTimelock {
        if (signer == address(0)) revert ZeroAddressNotAllowed();
        isSequencerSigner[signer] = authorized;
        emit SequencerSignerUpdated(signer, authorized);
    }

    function setWithdrawalCooldown(uint64 newCooldownSec) external onlyTimelock {
        withdrawalCooldownSec = newCooldownSec;
    }

    function setEscapeHatchTimeoutBlocks(uint64 newTimeoutBlocks) external onlyTimelock {
        if (newTimeoutBlocks == 0) revert ZeroAmountNotAllowed();
        escapeHatchTimeoutBlocks = newTimeoutBlocks;
    }

    // =========================================================================
    // VIEWS & PRECISION CONVERSIONS
    // =========================================================================
    function timelockAddress() external view override returns (address) {
        return address(this);
    }

    /// @notice Computes canonical 32-byte nullifier matching `settlement_l2.proto` & Z3 Theorem 3:
    ///         keccak256(chainId || verifyingVault || accountId || withdrawalNonce || stateEpoch)
    function computeWithdrawalNullifier(
        uint256 chainId,
        address verifyingVault,
        uint64 accountId,
        uint64 nonce,
        uint64 stateEpoch
    ) public pure returns (bytes32) {
        return keccak256(abi.encodePacked(chainId, verifyingVault, accountId, nonce, stateEpoch));
    }

    function domainSeparator() public view returns (bytes32) {
        if (block.chainid == _CACHED_CHAIN_ID) {
            return _CACHED_DOMAIN_SEPARATOR;
        }
        return
            EIP712Lib.computeDomainSeparator(
                _EIP712_NAME_HASH,
                _EIP712_VERSION_HASH,
                block.chainid,
                address(this)
            );
    }

    function registeredAssetCount() external view override returns (uint256) {
        return _registeredAssets.length;
    }

    function registeredAssetAt(uint256 index) external view override returns (address) {
        return _registeredAssets[index];
    }

    function getAssetConfig(address asset) external view returns (AssetConfig memory) {
        return _assets[asset];
    }

    function getDepositReceipt(uint64 depositSeq) external view override returns (DepositReceipt memory) {
        return _deposits[depositSeq];
    }

    function getQueuedWithdrawal(bytes32 withdrawalId) external view returns (QueuedWithdrawal memory) {
        return _withdrawals[withdrawalId];
    }

    function actualAssetBalanceRaw(address asset) public view returns (uint256) {
        if (asset == address(0)) {
            return address(this).balance;
        }
        return SafeTransferLib.balanceOf(asset, address(this));
    }

    /// @notice Total segregated obligations across all three custody buckets.
    function totalProtectedClaimsRaw(address asset) public view returns (uint256) {
        return
            unanchoredDepositsRaw[asset] +
            queuedWithdrawalsRaw[asset] +
            remainingAnchoredLiabilitiesRaw[asset];
    }

    /// @notice Available Vault reserve backing anchored epoch liabilities (`actual - unanchored - queued`).
    function availableBackingReserveRaw(address asset) public view override returns (uint256) {
        uint256 actual = actualAssetBalanceRaw(asset);
        uint256 encumbered = unanchoredDepositsRaw[asset] + queuedWithdrawalsRaw[asset];
        return actual > encumbered ? actual - encumbered : 0;
    }

    /// @notice Unencumbered protocol surplus strictly above 100% of all user claims.
    function protocolSurplusRaw(address asset) public view returns (uint256) {
        uint256 actual = actualAssetBalanceRaw(asset);
        uint256 claims = totalProtectedClaimsRaw(asset);
        return actual > claims ? actual - claims : 0;
    }

    function remainingDailyWithdrawalCapacityRaw(address asset) external view returns (uint256) {
        uint192 limitRaw = _assets[asset].dailyLimitRaw;
        DailyRateWindow memory win = _dailyWindows[asset];
        uint64 today = uint64(block.timestamp / 1 days);
        uint256 used = win.dayIndex == today ? uint256(win.withdrawnRaw) : 0;
        return uint256(limitRaw) > used ? uint256(limitRaw) - used : 0;
    }

    function fpToExactRaw(address asset, uint128 amountFp) external view override returns (uint256) {
        AssetConfig memory cfg = _assets[asset];
        if (!cfg.supported) revert AssetNotSupported(asset);
        return _fpToExactRaw(cfg.decimals, amountFp);
    }

    function rawToFloorFp(address asset, uint256 rawAmount) external view override returns (uint128) {
        AssetConfig memory cfg = _assets[asset];
        if (!cfg.supported) revert AssetNotSupported(asset);
        uint256 fp;
        if (cfg.decimals == HYPERION_FP_DECIMALS) {
            fp = rawAmount;
        } else if (cfg.decimals > HYPERION_FP_DECIMALS) {
            fp = rawAmount / (10 ** (cfg.decimals - HYPERION_FP_DECIMALS));
        } else {
            fp = rawAmount * (10 ** (HYPERION_FP_DECIMALS - cfg.decimals));
        }
        return fp > type(uint128).max ? type(uint128).max : uint128(fp);
    }

    // =========================================================================
    // INTERNAL HELPERS
    // =========================================================================
    function _isEscapeHatchTriggered() internal view returns (bool) {
        if (isEscapeHatchFrozen) return true;
        if (address(stateAnchor) == address(0)) return false;
        uint64 refBlock = stateAnchor.latestEpoch() == 0 ? deploymentBlock : stateAnchor.lastAnchorBlock();
        return block.number > uint256(refBlock) + uint256(escapeHatchTimeoutBlocks);
    }

    function _registerAssetInternal(
        address asset,
        uint8 decimals,
        uint192 dailyLimitRaw,
        uint256 largeThresholdRaw
    ) internal {
        if (_assets[asset].supported) revert AssetAlreadyRegistered(asset);
        if (_registeredAssets.length >= MAX_REGISTERED_ASSETS) {
            revert MaxAssetsReached(MAX_REGISTERED_ASSETS);
        }
        if (asset == address(0) && decimals != 18) {
            revert InvalidAssetDecimals(decimals);
        }
        if (decimals > 24) {
            revert InvalidAssetDecimals(decimals);
        }
        if (dailyLimitRaw == 0) revert ZeroAmountNotAllowed();

        uint40 assetId = uint40(_registeredAssets.length + 1);
        _registeredAssets.push(asset);
        _assets[asset] = AssetConfig({
            supported: true,
            depositsPaused: false,
            decimals: decimals,
            assetId: assetId,
            dailyLimitRaw: dailyLimitRaw
        });
        largeWithdrawalThresholdRaw[asset] = largeThresholdRaw;

        emit AssetRegistered(asset, assetId, decimals, dailyLimitRaw);
    }

    function _rawToExactFp(uint8 decimals, uint256 rawAmount) internal pure returns (uint128) {
        uint256 fp;
        if (decimals == HYPERION_FP_DECIMALS) {
            fp = rawAmount;
        } else if (decimals > HYPERION_FP_DECIMALS) {
            uint256 scaleUnit = 10 ** (decimals - HYPERION_FP_DECIMALS);
            if (rawAmount % scaleUnit != 0) {
                revert UnrepresentableRawDust(rawAmount, scaleUnit);
            }
            fp = rawAmount / scaleUnit;
        } else {
            uint256 factor = 10 ** (HYPERION_FP_DECIMALS - decimals);
            fp = rawAmount * factor;
        }
        if (fp > type(uint128).max) revert AmountFpOverflow(fp);
        return uint128(fp);
    }

    function _fpToExactRaw(uint8 decimals, uint128 amountFp) internal pure returns (uint256) {
        if (decimals == HYPERION_FP_DECIMALS) {
            return uint256(amountFp);
        } else if (decimals > HYPERION_FP_DECIMALS) {
            return uint256(amountFp) * (10 ** (decimals - HYPERION_FP_DECIMALS));
        } else {
            uint256 divisor = 10 ** (HYPERION_FP_DECIMALS - decimals);
            if (uint256(amountFp) % divisor != 0) {
                revert SubTokenPrecisionDust(amountFp, divisor);
            }
            return uint256(amountFp) / divisor;
        }
    }

    function _fpToFloorRaw(uint8 decimals, uint128 amountFp) internal pure returns (uint256) {
        if (decimals == HYPERION_FP_DECIMALS) {
            return uint256(amountFp);
        } else if (decimals > HYPERION_FP_DECIMALS) {
            return uint256(amountFp) * (10 ** (decimals - HYPERION_FP_DECIMALS));
        } else {
            uint256 divisor = 10 ** (HYPERION_FP_DECIMALS - decimals);
            return uint256(amountFp) / divisor;
        }
    }

    function _payoutRaw(address asset, address recipient, uint256 rawAmount) internal {
        if (asset == address(0)) {
            SafeTransferLib.safeTransferETH(recipient, rawAmount);
        } else {
            SafeTransferLib.safeTransfer(asset, recipient, rawAmount);
        }
    }
}
