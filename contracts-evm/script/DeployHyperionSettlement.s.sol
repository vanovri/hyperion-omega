// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Script} from "forge-std/Script.sol";
import {HyperionVault} from "../src/HyperionVault.sol";
import {SolvencyStateAnchor} from "../src/SolvencyStateAnchor.sol";
import {CensorshipEscapeHatch} from "../src/CensorshipEscapeHatch.sol";
import {Groth16BN254SolvencyVerifier} from "../src/Groth16BN254SolvencyVerifier.sol";

/// @title DeployHyperionSettlement
/// @notice Deterministic deployment script for Channel 18 institutional L2 Settlement Suite.
contract DeployHyperionSettlement is Script {
    struct DeploymentConfig {
        address governanceProposer;
        address guardian;
        address sequencerSigner;
        address chainRelayer;
        uint64 withdrawalCooldownSec;
        uint64 escapeTimeoutBlocks;
        uint256 timelockDelaySec;
        uint32 minReserveRatioBps;
        Groth16BN254SolvencyVerifier.VerifyingKey vk;
    }

    function deploySuite(
        DeploymentConfig memory cfg,
        address[] memory initialAssets,
        uint8[] memory initialDecimals,
        uint192[] memory initialDailyLimits,
        uint256[] memory initialLargeThresholds
    )
        public
        returns (
            Groth16BN254SolvencyVerifier verifier,
            HyperionVault vault,
            SolvencyStateAnchor anchor,
            CensorshipEscapeHatch escapeHatch
        )
    {
        verifier = new Groth16BN254SolvencyVerifier(cfg.vk);
        vault = new HyperionVault(
            cfg.governanceProposer,
            cfg.guardian,
            cfg.sequencerSigner,
            cfg.withdrawalCooldownSec,
            cfg.escapeTimeoutBlocks,
            cfg.timelockDelaySec
        );
        anchor = new SolvencyStateAnchor(address(vault), address(verifier), cfg.chainRelayer, cfg.minReserveRatioBps);
        escapeHatch = new CensorshipEscapeHatch(address(vault), address(anchor), cfg.escapeTimeoutBlocks);

        anchor.configureEscapeHatch(address(escapeHatch));
        vault.initializeSettlementSuite(
            address(anchor),
            address(escapeHatch),
            initialAssets,
            initialDecimals,
            initialDailyLimits,
            initialLargeThresholds
        );
    }
}
