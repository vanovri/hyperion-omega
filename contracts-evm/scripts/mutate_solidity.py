#!/usr/bin/env python3
"""Programmatic Solidity Operator Mutation Testing Harness for HYPERION Channel 18."""

from __future__ import annotations

import os
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FORGE = pathlib.Path(r"C:\Users\ardhy\.foundry\bin\forge.exe")

# Deterministic high-value security/financial mutation sites across all core contracts & libraries.
MUTATIONS: list[tuple[str, str, str, str]] = [
    # HyperionVault.sol
    (
        "src/HyperionVault.sol",
        "if (actualReceived != rawAmount) {",
        "if (actualReceived == rawAmount) {",
        "Vault fee-on-transfer exact delta check",
    ),
    (
        "src/HyperionVault.sol",
        "if (nextWithdrawnFp > leafBalanceFp) {",
        "if (nextWithdrawnFp < leafBalanceFp) {",
        "Vault per-account MSST leaf cap check",
    ),
    (
        "src/HyperionVault.sol",
        "if (intent.rawAmount > surplusRaw) {",
        "if (intent.rawAmount < surplusRaw) {",
        "Vault protocol surplus withdrawal bound",
    ),
    (
        "src/HyperionVault.sol",
        "if (block.timestamp < w.unlockTimestamp) {",
        "if (block.timestamp <= w.unlockTimestamp) {",
        "Vault whale cooldown exact unlock boundary (< vs <=)",
    ),
    (
        "src/HyperionVault.sol",
        "if (intent.chainId != block.chainid) {",
        "if (intent.chainId == block.chainid) {",
        "Vault cross-chain EIP-712 chainId check",
    ),
    (
        "src/HyperionVault.sol",
        "if (block.timestamp > intent.deadline) {",
        "if (block.timestamp < intent.deadline) {",
        "Vault EIP-712 deadline expiration check",
    ),
    (
        "src/HyperionVault.sol",
        "if (nextUsed > uint256(limitRaw)) {",
        "if (nextUsed < uint256(limitRaw)) {",
        "Vault 24h circuit-breaker rate limit check",
    ),
    (
        "src/HyperionVault.sol",
        "remainingAnchoredLiabilitiesRaw[intent.asset] -= intent.rawAmount;",
        "remainingAnchoredLiabilitiesRaw[intent.asset] += intent.rawAmount;",
        "Vault liability bucket subtraction",
    ),
    (
        "src/HyperionVault.sol",
        "if (_assets[target].supported || value > 0) {",
        "if (false && (_assets[target].supported || value > 0)) {",
        "Vault timelock custody-bypass anti-rug guard",
    ),
    (
        "src/HyperionVault.sol",
        "if (readyAt <= _TIMELOCK_DONE_TIMESTAMP || block.timestamp < readyAt) {",
        "if (readyAt <= _TIMELOCK_DONE_TIMESTAMP || block.timestamp <= readyAt) {",
        "Vault timelock exact readyTimestamp boundary (< vs <=)",
    ),
    # SolvencyStateAnchor.sol
    (
        "src/SolvencyStateAnchor.sol",
        "if (epoch != expectedEpoch) {",
        "if (epoch == expectedEpoch) {",
        "Anchor contiguous epoch sequence check",
    ),
    (
        "src/SolvencyStateAnchor.sol",
        "if (engineSeq <= latestEngineSeq) {",
        "if (engineSeq < latestEngineSeq) {",
        "Anchor strict monotonic engineSeq check (<= vs <)",
    ),
    (
        "src/SolvencyStateAnchor.sol",
        "if (backingRaw < requiredRaw) {",
        "if (backingRaw <= requiredRaw) {",
        "Anchor exact reserve ratio floor boundary (< vs <=)",
    ),
    (
        "src/SolvencyStateAnchor.sol",
        "if (!zkVerifier.verifySolvencyProof(pubInputs, decl.zkProof)) {",
        "if (zkVerifier.verifySolvencyProof(pubInputs, decl.zkProof)) {",
        "Anchor BN254 Groth16 proof gate",
    ),
    # CensorshipEscapeHatch.sol
    (
        "src/CensorshipEscapeHatch.sol",
        "if (block.number <= uint256(lastLive) + uint256(timeoutBlocks)) {",
        "if (block.number < uint256(lastLive) + uint256(timeoutBlocks)) {",
        "EscapeHatch exact block timeout boundary (<= vs <)",
    ),
    (
        "src/CensorshipEscapeHatch.sol",
        "if (leafBalanceFp <= alreadyWithdrawnFp) {",
        "if (leafBalanceFp == 0 && alreadyWithdrawnFp > 0) {",
        "EscapeHatch double-exit / post-EIP712 deduction guard",
    ),
    (
        "src/CensorshipEscapeHatch.sol",
        "exitedFp = leafBalanceFp - alreadyWithdrawnFp;",
        "exitedFp = leafBalanceFp;",
        "EscapeHatch net remaining balance subtraction",
    ),
    (
        "src/CensorshipEscapeHatch.sol",
        "if (msg.sender != d.depositor) {",
        "if (msg.sender == address(0)) {",
        "EscapeHatch unanchored deposit original depositor check",
    ),
    # Libraries & Verifier
    (
        "src/libraries/EIP712Lib.sol",
        "if (uint256(s) > SECP256K1_HALF_N || uint256(s) == 0 || uint256(r) == 0) {",
        "if (uint256(s) == 0 || uint256(r) == 0) {",
        "EIP-2 ECDSA upper-half s malleability check",
    ),
    (
        "src/libraries/MerkleSumLib.sol",
        "if (currentHash != expectedRootHash || currentSum != expectedRootSumFp) {",
        "if (currentHash != expectedRootHash) {",
        "MSST root sum equality check",
    ),
    (
        "src/Groth16BN254SolvencyVerifier.sol",
        "if (inputs.totalLiabilityFp > inputs.verifiedVaultReserveFp) {",
        "if (false) {",
        "Verifier public solvency inequality guard",
    ),
]


def main() -> int:
    killed = 0
    survived = 0
    total = len(MUTATIONS)

    env = os.environ.copy()
    env["FOUNDRY_OPTIMIZER"] = "false"
    env["FOUNDRY_VIA_IR"] = "true"

    for idx, (rel_path, target, replacement, desc) in enumerate(MUTATIONS, 1):
        fpath = ROOT / rel_path
        orig = fpath.read_text(encoding="utf-8")
        if target not in orig:
            print(f"[ERROR] Target not found in {rel_path}: {target}", file=sys.stderr)
            return 1

        mutated = orig.replace(target, replacement, 1)
        fpath.write_text(mutated, encoding="utf-8")
        try:
            proc = subprocess.run(
                [
                    str(FORGE),
                    "test",
                    "--no-dynamic-test-linking",
                    "--skip",
                    "Invariant",
                    "--skip",
                    "Handler",
                    "--skip",
                    "Benchmark",
                    "--match-contract",
                    "HyperionSettlementUnitTest",
                ],
                cwd=str(ROOT),
                env=env,
                capture_output=True,
                text=True,
                timeout=300,
            )
            if proc.returncode != 0:
                killed += 1
                print(f"[{idx:02d}/{total:02d}] KILLED   | {rel_path} :: {desc}")
            else:
                survived += 1
                print(f"[{idx:02d}/{total:02d}] SURVIVED | {rel_path} :: {desc}")
        finally:
            fpath.write_text(orig, encoding="utf-8")

    score = (killed / total) * 100.0 if total else 100.0
    print(f"\n=== SOLIDITY MUTATION SUMMARY: Killed={killed}/{total} (Survived={survived}) | Score={score:.2f}% ===")
    return 0 if score >= 95.0 else 2


if __name__ == "__main__":
    raise SystemExit(main())

