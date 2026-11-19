// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IZKSolvencyVerifier} from "./interfaces/IZKSolvencyVerifier.sol";
import {ISolvencyStateAnchorView, IHyperionVaultSettlement} from "./interfaces/IHyperionSettlement.sol";
import {ReentrancyGuardTransient} from "./libraries/ReentrancyGuardTransient.sol";

/// @title SolvencyStateAnchor
/// @notice Institutional On-Chain Merkle-Sum State Root & Channel 13 ZK-Solvency Commitment Registry.
/// @dev Enforces:
///      1. Strict monotonic epoch continuity (`epoch == latestEpoch + 1`).
///      2. Strict monotonic matching-engine WAL sequence (`engineSeq > latestEngineSeq`).
///      3. Complete asset coverage across all registered Vault custody assets (anti-omission guard).
///      4. Real BN254 Groth16 ZK-Solvency proof verification per asset via `IZKSolvencyVerifier`.
///      5. Hard On-Chain Vault Reserve Ratio Gate: rejects state root transitions whenever
///         `availableBackingReserveRaw < ceil(declaredLiabilityRaw * minReserveRatioBps / 10_000)`.
contract SolvencyStateAnchor is ISolvencyStateAnchorView, ReentrancyGuardTransient {
    // =========================================================================
    // CUSTOM ERRORS
    // =========================================================================
    error UnauthorizedCaller(address caller);
    error ZeroAddressNotAllowed();
    error SettlementFrozenInEscapeMode();
    error EpochSequenceMismatch(uint64 expectedEpoch, uint64 providedEpoch);
    error EngineSeqNotMonotonic(uint64 previousEngineSeq, uint64 providedEngineSeq);
    error InvalidDepositSequenceBounds(uint64 prevProcessed, uint64 providedProcessed, uint64 vaultLatest);
    error AssetDeclarationCountMismatch(uint256 expectedCount, uint256 providedCount);
    error CanonicalAssetOrderMismatch(uint256 index, address expectedAsset, address providedAsset);
    error ReserveRatioBelowMinimum(
        address asset,
        uint256 availableBackingRaw,
        uint256 requiredReserveRaw,
        uint32 minReserveRatioBps
    );
    error InvalidZKSolvencyProof(uint64 epoch, address asset);
    error InvalidReserveRatioBps(uint32 bps);

    // =========================================================================
    // EVENTS
    // =========================================================================
    event StateRootAnchored(
        uint64 indexed epoch,
        uint64 indexed engineSeq,
        uint64 processedDepositSeq,
        bytes32 combinedStateRoot,
        bytes32 walBlake3Hash,
        address indexed relayer
    );

    event AssetEpochCommitted(
        uint64 indexed epoch,
        address indexed asset,
        bytes32 merkleSumRoot,
        bytes32 poseidonRoot,
        uint128 totalLiabilityFp,
        uint256 verifiedReserveRaw
    );

    event RelayerAuthorizationUpdated(address indexed relayer, bool authorized);
    event MinReserveRatioUpdated(uint32 previousBps, uint32 newBps);
    event ZKVerifierUpdated(address indexed previousVerifier, address indexed newVerifier);
    event AnchorFrozenForEscapeHatch(uint64 frozenAtEpoch, uint64 blockNumber);

    // =========================================================================
    // PACKED STORAGE STRUCTS
    // =========================================================================
    struct AssetLiabilityInput {
        address asset;
        bytes32 merkleSumRoot;
        bytes32 poseidonRoot;
        uint128 totalLiabilityFp;
        bytes zkProof;
    }

    /// @dev Packed into compact storage slots (slot 0: 4 x uint64 = 256 bits).
    struct EpochMetadata {
        uint64 epoch;
        uint64 engineSeq;
        uint64 processedDepositSeq;
        uint64 anchoredAtBlock;
        uint64 anchoredAtTimestamp;
        bytes32 combinedStateRoot;
        bytes32 walBlake3Hash;
    }

    struct AssetEpochCommitment {
        bytes32 merkleSumRoot;
        bytes32 poseidonRoot;
        uint128 totalLiabilityFp;
        uint256 verifiedReserveRaw;
    }

    // =========================================================================
    // IMMUTABLES & PACKED STATE
    // =========================================================================
    uint32 public constant BPS_DENOMINATOR = 10_000;
    uint32 public constant MAX_RESERVE_RATIO_BPS = 50_000; // 500%

    IHyperionVaultSettlement public immutable vault;
    address public escapeHatch;

    IZKSolvencyVerifier public zkVerifier;

    uint64 public override latestEpoch;
    uint64 public override latestEngineSeq;
    uint64 public override lastProcessedDepositSeq;
    uint64 public override lastAnchorBlock;

    uint32 public minReserveRatioBps;
    bool public override isEscapeHatchFrozen;
    bool private _escapeHatchConfigured;

    mapping(address => bool) public isAuthorizedRelayer;
    mapping(uint64 => EpochMetadata) internal _epochs;
    mapping(uint64 => mapping(address => AssetEpochCommitment)) internal _assetCommitments;

    // =========================================================================
    // MODIFIERS
    // =========================================================================
    modifier onlyTimelock() {
        if (msg.sender != vault.timelockAddress()) {
            revert UnauthorizedCaller(msg.sender);
        }
        _;
    }

    modifier onlyRelayer() {
        if (!isAuthorizedRelayer[msg.sender]) {
            revert UnauthorizedCaller(msg.sender);
        }
        _;
    }

    constructor(
        address vault_,
        address zkVerifier_,
        address initialRelayer_,
        uint32 minReserveRatioBps_
    ) {
        if (vault_ == address(0) || zkVerifier_ == address(0) || initialRelayer_ == address(0)) {
            revert ZeroAddressNotAllowed();
        }
        if (minReserveRatioBps_ < BPS_DENOMINATOR || minReserveRatioBps_ > MAX_RESERVE_RATIO_BPS) {
            revert InvalidReserveRatioBps(minReserveRatioBps_);
        }

        vault = IHyperionVaultSettlement(vault_);
        zkVerifier = IZKSolvencyVerifier(zkVerifier_);
        minReserveRatioBps = minReserveRatioBps_;
        lastAnchorBlock = uint64(block.number);
        isAuthorizedRelayer[initialRelayer_] = true;

        emit RelayerAuthorizationUpdated(initialRelayer_, true);
        emit MinReserveRatioUpdated(0, minReserveRatioBps_);
    }

    /// @notice One-time wiring for the CensorshipEscapeHatch contract during deployment, or updatable via Timelock.
    function configureEscapeHatch(address escapeHatch_) external {
        if (escapeHatch_ == address(0)) revert ZeroAddressNotAllowed();
        if (_escapeHatchConfigured && msg.sender != vault.timelockAddress()) {
            revert UnauthorizedCaller(msg.sender);
        }
        _escapeHatchConfigured = true;
        escapeHatch = escapeHatch_;
    }

    // =========================================================================
    // CORE EPOCH SOLVENCY ANCHORING
    // =========================================================================
    /// @notice Commits a contiguous Merkle-Sum state root batch & verifies Channel 13 ZK-Solvency receipts.
    function commitStateRoot(
        uint64 epoch,
        uint64 engineSeq,
        uint64 processedDepositSeq,
        bytes32 walBlake3Hash,
        AssetLiabilityInput[] calldata declarations
    ) external nonReentrantTransient onlyRelayer returns (bytes32 combinedStateRoot) {
        _validateBatchHeader(epoch, engineSeq, processedDepositSeq);

        uint256 assetCount = vault.registeredAssetCount();
        if (declarations.length != assetCount) {
            revert AssetDeclarationCountMismatch(assetCount, declarations.length);
        }

        (address[] memory assets, uint256[] memory liabilitiesRaw) = _prepareAssetLiabilities(
            declarations,
            assetCount
        );

        vault.settleEpochAnchor(epoch, lastProcessedDepositSeq, processedDepositSeq, assets, liabilitiesRaw);

        bytes32 rollingDigest = keccak256(abi.encodePacked(epoch, engineSeq, processedDepositSeq, walBlake3Hash));
        for (uint256 i = 0; i < assetCount; ) {
            rollingDigest = _verifyAndCommitAsset(
                epoch,
                engineSeq,
                declarations[i],
                liabilitiesRaw[i],
                rollingDigest
            );
            unchecked {
                ++i;
            }
        }

        combinedStateRoot = rollingDigest;
        latestEpoch = epoch;
        latestEngineSeq = engineSeq;
        lastProcessedDepositSeq = processedDepositSeq;
        lastAnchorBlock = uint64(block.number);

        _epochs[epoch] = EpochMetadata({
            epoch: epoch,
            engineSeq: engineSeq,
            processedDepositSeq: processedDepositSeq,
            anchoredAtBlock: uint64(block.number),
            anchoredAtTimestamp: uint64(block.timestamp),
            combinedStateRoot: combinedStateRoot,
            walBlake3Hash: walBlake3Hash
        });

        emit StateRootAnchored(epoch, engineSeq, processedDepositSeq, combinedStateRoot, walBlake3Hash, msg.sender);
    }

    function _validateBatchHeader(
        uint64 epoch,
        uint64 engineSeq,
        uint64 processedDepositSeq
    ) internal view {
        if (isEscapeHatchFrozen || vault.isEscapeHatchFrozen()) {
            revert SettlementFrozenInEscapeMode();
        }
        uint64 expectedEpoch = latestEpoch + 1;
        if (epoch != expectedEpoch) {
            revert EpochSequenceMismatch(expectedEpoch, epoch);
        }
        if (engineSeq <= latestEngineSeq) {
            revert EngineSeqNotMonotonic(latestEngineSeq, engineSeq);
        }
        uint64 prevDepositSeq = lastProcessedDepositSeq;
        uint64 vaultMaxDepositSeq = vault.latestDepositSeq();
        if (processedDepositSeq < prevDepositSeq || processedDepositSeq > vaultMaxDepositSeq) {
            revert InvalidDepositSequenceBounds(prevDepositSeq, processedDepositSeq, vaultMaxDepositSeq);
        }
    }

    function _prepareAssetLiabilities(
        AssetLiabilityInput[] calldata declarations,
        uint256 assetCount
    ) internal view returns (address[] memory assets, uint256[] memory liabilitiesRaw) {
        assets = new address[](assetCount);
        liabilitiesRaw = new uint256[](assetCount);
        for (uint256 i = 0; i < assetCount; ) {
            AssetLiabilityInput calldata decl = declarations[i];
            address expectedAsset = vault.registeredAssetAt(i);
            if (decl.asset != expectedAsset) {
                revert CanonicalAssetOrderMismatch(i, expectedAsset, decl.asset);
            }
            assets[i] = decl.asset;
            liabilitiesRaw[i] = vault.fpToExactRaw(decl.asset, decl.totalLiabilityFp);
            unchecked {
                ++i;
            }
        }
    }

    function _verifyAndCommitAsset(
        uint64 epoch,
        uint64 engineSeq,
        AssetLiabilityInput calldata decl,
        uint256 liabilityRaw,
        bytes32 prevDigest
    ) internal returns (bytes32 nextDigest) {
        uint256 backingRaw = vault.availableBackingReserveRaw(decl.asset);
        uint32 requiredRatioBps = minReserveRatioBps;
        uint256 requiredRaw = _mulDivCeil(liabilityRaw, requiredRatioBps, BPS_DENOMINATOR);

        if (backingRaw < requiredRaw) {
            revert ReserveRatioBelowMinimum(decl.asset, backingRaw, requiredRaw, requiredRatioBps);
        }

        uint128 backingFp = vault.rawToFloorFp(decl.asset, backingRaw);
        IZKSolvencyVerifier.SolvencyPublicInputs memory pubInputs = IZKSolvencyVerifier.SolvencyPublicInputs({
            epoch: epoch,
            engineSeq: engineSeq,
            asset: decl.asset,
            merkleSumRoot: decl.merkleSumRoot,
            poseidonRoot: decl.poseidonRoot,
            totalLiabilityFp: decl.totalLiabilityFp,
            verifiedVaultReserveFp: backingFp
        });

        if (!zkVerifier.verifySolvencyProof(pubInputs, decl.zkProof)) {
            revert InvalidZKSolvencyProof(epoch, decl.asset);
        }

        _assetCommitments[epoch][decl.asset] = AssetEpochCommitment({
            merkleSumRoot: decl.merkleSumRoot,
            poseidonRoot: decl.poseidonRoot,
            totalLiabilityFp: decl.totalLiabilityFp,
            verifiedReserveRaw: backingRaw
        });

        emit AssetEpochCommitted(
            epoch,
            decl.asset,
            decl.merkleSumRoot,
            decl.poseidonRoot,
            decl.totalLiabilityFp,
            backingRaw
        );

        return
            keccak256(
                abi.encodePacked(
                    prevDigest,
                    decl.asset,
                    decl.merkleSumRoot,
                    decl.poseidonRoot,
                    decl.totalLiabilityFp,
                    backingRaw
                )
            );
    }

    // =========================================================================
    // ESCAPE HATCH & GOVERNANCE CONTROLS
    // =========================================================================
    function markEscapeHatchFrozen() external override {
        if (msg.sender != escapeHatch && msg.sender != address(vault)) {
            revert UnauthorizedCaller(msg.sender);
        }
        if (!isEscapeHatchFrozen) {
            isEscapeHatchFrozen = true;
            emit AnchorFrozenForEscapeHatch(latestEpoch, uint64(block.number));
        }
    }

    function setRelayerAuthorization(address relayer, bool authorized) external onlyTimelock {
        if (relayer == address(0)) revert ZeroAddressNotAllowed();
        isAuthorizedRelayer[relayer] = authorized;
        emit RelayerAuthorizationUpdated(relayer, authorized);
    }

    function setMinReserveRatioBps(uint32 newBps) external onlyTimelock {
        if (newBps < BPS_DENOMINATOR || newBps > MAX_RESERVE_RATIO_BPS) {
            revert InvalidReserveRatioBps(newBps);
        }
        uint32 oldBps = minReserveRatioBps;
        minReserveRatioBps = newBps;
        emit MinReserveRatioUpdated(oldBps, newBps);
    }

    function setZKVerifier(address newVerifier) external onlyTimelock {
        if (newVerifier == address(0)) revert ZeroAddressNotAllowed();
        address old = address(zkVerifier);
        zkVerifier = IZKSolvencyVerifier(newVerifier);
        emit ZKVerifierUpdated(old, newVerifier);
    }

    // =========================================================================
    // VIEW FUNCTIONS
    // =========================================================================
    function getEpochMetadata(uint64 epoch) external view returns (EpochMetadata memory) {
        return _epochs[epoch];
    }

    function getEpochAssetCommitment(
        uint64 epoch,
        address asset
    )
        external
        view
        override
        returns (
            bytes32 merkleSumRoot,
            bytes32 poseidonRoot,
            uint128 totalLiabilityFp,
            uint256 verifiedReserveRaw
        )
    {
        AssetEpochCommitment storage c = _assetCommitments[epoch][asset];
        return (c.merkleSumRoot, c.poseidonRoot, c.totalLiabilityFp, c.verifiedReserveRaw);
    }

    function _mulDivCeil(uint256 a, uint256 b, uint256 denominator) internal pure returns (uint256 result) {
        if (a == 0 || b == 0) return 0;
        uint256 prod = a * b;
        result = prod / denominator;
        if (prod % denominator != 0) {
            unchecked {
                ++result;
            }
        }
    }
}
