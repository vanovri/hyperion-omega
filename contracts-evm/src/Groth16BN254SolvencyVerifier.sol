// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {IZKSolvencyVerifier} from "./interfaces/IZKSolvencyVerifier.sol";

/// @title Groth16BN254SolvencyVerifier
/// @notice Real BN254 (`alt_bn128`) Groth16 Zero-Knowledge Solvency Verifier using EVM precompiles
///         `0x06` (`ecAdd`), `0x07` (`ecMul`), and `0x08` (`ecPairing`).
/// @dev Verifies Channel 13 `ZKSolvencyReceipt` over BN254:
///      1. Enforces public circuit inequality `totalLiabilityFp <= verifiedVaultReserveFp` and non-zero roots.
///      2. Hashes canonical public inputs modulo BN254 scalar field `R_MODULUS` to prevent field aliasing.
///      3. Validates `A, C in G1` lie on `y^2 == x^3 + 3 (mod P_MODULUS)` and `B in G2` coordinates `< P_MODULUS`.
///      4. Computes `vk_x = IC0 + publicInputScalar * IC1` via `ecMul(0x07)` and `ecAdd(0x06)`.
///      5. Checks bilinear pairing equation `e(-A, B) * e(alpha, beta) * e(vk_x, gamma) * e(C, delta) == 1`
///         via `ecPairing(0x08)`.
contract Groth16BN254SolvencyVerifier is IZKSolvencyVerifier {
    error InvalidProofByteLength(uint256 length);
    error InsolventPublicInputs(uint128 liabilityFp, uint128 reserveFp);
    error EmptyCommitmentRoot();
    error PointCoordinateOutOfField();
    error G1PointNotOnCurve();
    error PrecompileCallFailed(uint8 precompile);

    /// @dev BN254 base field prime p
    uint256 public constant P_MODULUS =
        21888242871839275222246405745257275088696311157297823662689037894645226208583;

    /// @dev BN254 scalar field order r
    uint256 public constant R_MODULUS =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;

    struct G1Point {
        uint256 x;
        uint256 y;
    }

    /// @dev EIP-197 ordering: imaginary part (`x1`/`y1`) precedes real part (`x0`/`y0`).
    struct G2Point {
        uint256 x1;
        uint256 x0;
        uint256 y1;
        uint256 y0;
    }

    struct VerifyingKey {
        G1Point alpha1;
        G2Point beta2;
        G2Point gamma2;
        G2Point delta2;
        G1Point ic0;
        G1Point ic1;
    }

    VerifyingKey internal _vk;

    constructor(VerifyingKey memory vk_) {
        _validateG1OnCurve(vk_.alpha1);
        _validateG1OnCurve(vk_.ic0);
        _validateG1OnCurve(vk_.ic1);
        _validateG2InField(vk_.beta2);
        _validateG2InField(vk_.gamma2);
        _validateG2InField(vk_.delta2);
        _vk = vk_;
    }

    function getVerifyingKey() external view returns (VerifyingKey memory) {
        return _vk;
    }

    /// @notice Computes the BN254 scalar field element binding all Channel 13 public solvency commitments.
    function computePublicInputScalar(SolvencyPublicInputs calldata inputs) public pure returns (uint256 scalar) {
        bytes32 digest = keccak256(
            abi.encode(
                inputs.epoch,
                inputs.engineSeq,
                inputs.asset,
                inputs.merkleSumRoot,
                inputs.poseidonRoot,
                inputs.totalLiabilityFp,
                inputs.verifiedVaultReserveFp
            )
        );
        scalar = uint256(digest) % R_MODULUS;
        if (scalar == 0) {
            scalar = 1;
        }
    }

    /// @inheritdoc IZKSolvencyVerifier
    function verifySolvencyProof(
        SolvencyPublicInputs calldata inputs,
        bytes calldata zkProof
    ) external view override returns (bool valid) {
        if (zkProof.length != 256) {
            revert InvalidProofByteLength(zkProof.length);
        }
        if (inputs.merkleSumRoot == bytes32(0) || inputs.poseidonRoot == bytes32(0)) {
            revert EmptyCommitmentRoot();
        }
        if (inputs.totalLiabilityFp > inputs.verifiedVaultReserveFp) {
            revert InsolventPublicInputs(inputs.totalLiabilityFp, inputs.verifiedVaultReserveFp);
        }

        (G1Point memory a, G2Point memory b, G1Point memory c) = _decodeProof(zkProof);
        if (!_isG1OnCurve(a) || !_isG1OnCurve(c) || !_isG2InField(b)) {
            return false;
        }

        uint256 pubScalar = computePublicInputScalar(inputs);
        VerifyingKey memory vk = _vk;

        // vk_x = IC0 + pubScalar * IC1
        G1Point memory scaledIc1 = _ecMul(vk.ic1, pubScalar);
        G1Point memory vkX = _ecAdd(vk.ic0, scaledIc1);

        // Negate A in G1: -A = (A.x, P_MODULUS - A.y)
        G1Point memory negA = G1Point({x: a.x, y: a.y == 0 ? 0 : P_MODULUS - a.y});

        return _ecPairing4(negA, b, vk.alpha1, vk.beta2, vkX, vk.gamma2, c, vk.delta2);
    }

    function _decodeProof(
        bytes calldata zkProof
    ) internal pure returns (G1Point memory a, G2Point memory b, G1Point memory c) {
        assembly ("memory-safe") {
            let off := zkProof.offset
            mstore(a, calldataload(off))
            mstore(add(a, 0x20), calldataload(add(off, 0x20)))

            mstore(b, calldataload(add(off, 0x40)))
            mstore(add(b, 0x20), calldataload(add(off, 0x60)))
            mstore(add(b, 0x40), calldataload(add(off, 0x80)))
            mstore(add(b, 0x60), calldataload(add(off, 0xa0)))

            mstore(c, calldataload(add(off, 0xc0)))
            mstore(add(c, 0x20), calldataload(add(off, 0xe0)))
        }
    }

    function _isG1OnCurve(G1Point memory p) internal pure returns (bool) {
        if (p.x >= P_MODULUS || p.y >= P_MODULUS) return false;
        if (p.x == 0 && p.y == 0) return false; // Reject point at infinity in proof elements
        uint256 lhs = mulmod(p.y, p.y, P_MODULUS);
        uint256 x2 = mulmod(p.x, p.x, P_MODULUS);
        uint256 rhs = addmod(mulmod(x2, p.x, P_MODULUS), 3, P_MODULUS);
        return lhs == rhs;
    }

    function _isG2InField(G2Point memory p) internal pure returns (bool) {
        if (p.x0 >= P_MODULUS || p.x1 >= P_MODULUS || p.y0 >= P_MODULUS || p.y1 >= P_MODULUS) {
            return false;
        }
        if (p.x0 == 0 && p.x1 == 0 && p.y0 == 0 && p.y1 == 0) {
            return false;
        }
        return true;
    }

    function _validateG1OnCurve(G1Point memory p) internal pure {
        if (!_isG1OnCurve(p)) revert G1PointNotOnCurve();
    }

    function _validateG2InField(G2Point memory p) internal pure {
        if (!_isG2InField(p)) revert PointCoordinateOutOfField();
    }

    function _ecAdd(G1Point memory p1, G1Point memory p2) internal view returns (G1Point memory r) {
        bool ok;
        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore(m, mload(p1))
            mstore(add(m, 0x20), mload(add(p1, 0x20)))
            mstore(add(m, 0x40), mload(p2))
            mstore(add(m, 0x60), mload(add(p2, 0x20)))
            let sOk := staticcall(gas(), 0x06, m, 0x80, r, 0x40)
            ok := and(sOk, eq(returndatasize(), 0x40))
        }
        if (!ok) revert PrecompileCallFailed(0x06);
    }

    function _ecMul(G1Point memory p, uint256 s) internal view returns (G1Point memory r) {
        bool ok;
        assembly ("memory-safe") {
            let m := mload(0x40)
            mstore(m, mload(p))
            mstore(add(m, 0x20), mload(add(p, 0x20)))
            mstore(add(m, 0x40), s)
            let sOk := staticcall(gas(), 0x07, m, 0x60, r, 0x40)
            ok := and(sOk, eq(returndatasize(), 0x40))
        }
        if (!ok) revert PrecompileCallFailed(0x07);
    }

    function _ecPairing4(
        G1Point memory a1,
        G2Point memory a2,
        G1Point memory b1,
        G2Point memory b2,
        G1Point memory c1,
        G2Point memory c2,
        G1Point memory d1,
        G2Point memory d2
    ) internal view returns (bool pairedOk) {
        bool callOk;
        assembly ("memory-safe") {
            let m := mload(0x40)
            // Pair 1 (0x00..0xbf)
            mstore(m, mload(a1))
            mstore(add(m, 0x20), mload(add(a1, 0x20)))
            mstore(add(m, 0x40), mload(a2))
            mstore(add(m, 0x60), mload(add(a2, 0x20)))
            mstore(add(m, 0x80), mload(add(a2, 0x40)))
            mstore(add(m, 0xa0), mload(add(a2, 0x60)))

            // Pair 2 (0xc0..0x17f)
            mstore(add(m, 0xc0), mload(b1))
            mstore(add(m, 0xe0), mload(add(b1, 0x20)))
            mstore(add(m, 0x100), mload(b2))
            mstore(add(m, 0x120), mload(add(b2, 0x20)))
            mstore(add(m, 0x140), mload(add(b2, 0x40)))
            mstore(add(m, 0x160), mload(add(b2, 0x60)))

            // Pair 3 (0x180..0x23f)
            mstore(add(m, 0x180), mload(c1))
            mstore(add(m, 0x1a0), mload(add(c1, 0x20)))
            mstore(add(m, 0x1c0), mload(c2))
            mstore(add(m, 0x1e0), mload(add(c2, 0x20)))
            mstore(add(m, 0x200), mload(add(c2, 0x40)))
            mstore(add(m, 0x220), mload(add(c2, 0x60)))

            // Pair 4 (0x240..0x2ff)
            mstore(add(m, 0x240), mload(d1))
            mstore(add(m, 0x260), mload(add(d1, 0x20)))
            mstore(add(m, 0x280), mload(d2))
            mstore(add(m, 0x2a0), mload(add(d2, 0x20)))
            mstore(add(m, 0x2c0), mload(add(d2, 0x40)))
            mstore(add(m, 0x2e0), mload(add(d2, 0x60)))

            let sOk := staticcall(gas(), 0x08, m, 0x300, 0x00, 0x20)
            callOk := and(sOk, eq(returndatasize(), 0x20))
            pairedOk := and(callOk, eq(mload(0x00), 1))
        }
    }
}
