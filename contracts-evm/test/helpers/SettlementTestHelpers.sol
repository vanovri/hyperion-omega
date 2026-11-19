// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Groth16BN254SolvencyVerifier} from "../../src/Groth16BN254SolvencyVerifier.sol";
import {IZKSolvencyVerifier} from "../../src/interfaces/IZKSolvencyVerifier.sol";
import {MerkleSumLib} from "../../src/libraries/MerkleSumLib.sol";

/// @title BN254Groth16TestProver
/// @notice Exact BN254 (`alt_bn128`) Groth16 Toxic-Waste Keygen & Witness Prover over F_p and F_{p^2}.
/// @dev Produces genuine cryptographic proofs `(A in G1, B in G2, C in G1)` verified by EVM precompiles
///      `0x06`, `0x07`, and `0x08` with linearly independent G2 elements `beta2, gamma2, delta2`.
library BN254Groth16TestProver {
    uint256 internal constant P =
        21888242871839275222246405745257275088696311157297823662689037894645226208583;
    uint256 internal constant R =
        21888242871839275222246405745257275088548364400416034343698204186575808495617;

    // Canonical EIP-197 BN254 G1 generator (1, 2)
    uint256 internal constant G1_X = 1;
    uint256 internal constant G1_Y = 2;

    // Canonical EIP-197 BN254 G2 generator over F_p[i]/(i^2 + 1)
    uint256 internal constant G2_X1 =
        11559732032986387107991004021392285783925812861821192530917403151452391805634;
    uint256 internal constant G2_X0 =
        10857046999023057135944570762232829481370756359578518086990519993285655852781;
    uint256 internal constant G2_Y1 =
        4082367875863433681332203403145435568316851327593401208105741076214120093531;
    uint256 internal constant G2_Y0 =
        8495653923123431417604973247489272438418190587263600148770280649306958101930;

    // Deterministic setup trapdoor scalars in Z_r^*
    uint256 internal constant TAU_ALPHA = 13;
    uint256 internal constant TAU_BETA = 5;
    uint256 internal constant TAU_GAMMA = 7;
    uint256 internal constant TAU_DELTA = 11;
    uint256 internal constant IC0_SCALAR = 17;
    uint256 internal constant IC1_SCALAR = 23;

    function buildVerifyingKey() internal view returns (Groth16BN254SolvencyVerifier.VerifyingKey memory vk) {
        Groth16BN254SolvencyVerifier.G1Point memory g1 = Groth16BN254SolvencyVerifier.G1Point(G1_X, G1_Y);
        Groth16BN254SolvencyVerifier.G2Point memory g2 = Groth16BN254SolvencyVerifier.G2Point(
            G2_X1,
            G2_X0,
            G2_Y1,
            G2_Y0
        );

        vk.alpha1 = g1Mul(g1, TAU_ALPHA);
        vk.beta2 = g2MulSmall(g2, TAU_BETA);
        vk.gamma2 = g2MulSmall(g2, TAU_GAMMA);
        vk.delta2 = g2MulSmall(g2, TAU_DELTA);
        vk.ic0 = g1Mul(g1, IC0_SCALAR);
        vk.ic1 = g1Mul(g1, IC1_SCALAR);
    }

    /// @notice Generates a valid 256-byte BN254 Groth16 proof `(A, B, C)` satisfying:
    ///         e(-A, B) * e(alpha, beta) * e(vk_x, gamma) * e(C, delta) == 1
    function proveSolvency(
        IZKSolvencyVerifier.SolvencyPublicInputs memory inputs,
        uint256 blindingNonce
    ) internal view returns (bytes memory proofBytes) {
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
        uint256 pubScalar = uint256(digest) % R;
        if (pubScalar == 0) pubScalar = 1;

        // Choose non-zero randomized A = rA * G1, B = TAU_BETA * G2
        uint256 rA = (uint256(keccak256(abi.encodePacked("HYPERION_ZK_BLIND", blindingNonce, pubScalar))) % (R - 2)) + 2;
        Groth16BN254SolvencyVerifier.G1Point memory g1 = Groth16BN254SolvencyVerifier.G1Point(G1_X, G1_Y);
        Groth16BN254SolvencyVerifier.G2Point memory g2 = Groth16BN254SolvencyVerifier.G2Point(
            G2_X1,
            G2_X0,
            G2_Y1,
            G2_Y0
        );

        Groth16BN254SolvencyVerifier.G1Point memory a = g1Mul(g1, rA);
        Groth16BN254SolvencyVerifier.G2Point memory b = g2MulSmall(g2, TAU_BETA);

        // Exponent balance in Z_r:
        // rA * TAU_BETA == TAU_ALPHA * TAU_BETA + (IC0_SCALAR + pubScalar * IC1_SCALAR) * TAU_GAMMA + cScalar * TAU_DELTA (mod R)
        uint256 lhs = mulmod(rA, TAU_BETA, R);
        uint256 alphaBeta = mulmod(TAU_ALPHA, TAU_BETA, R);
        uint256 vkExp = addmod(IC0_SCALAR, mulmod(pubScalar, IC1_SCALAR, R), R);
        uint256 vkGamma = mulmod(vkExp, TAU_GAMMA, R);
        uint256 rhsKnown = addmod(alphaBeta, vkGamma, R);

        uint256 diff = addmod(lhs, R - rhsKnown, R);
        uint256 invDelta = modExp(TAU_DELTA, R - 2, R);
        uint256 cScalar = mulmod(diff, invDelta, R);
        if (cScalar == 0) {
            rA = addmod(rA, 1, R);
            a = g1Mul(g1, rA);
            lhs = mulmod(rA, TAU_BETA, R);
            diff = addmod(lhs, R - rhsKnown, R);
            cScalar = mulmod(diff, invDelta, R);
        }

        Groth16BN254SolvencyVerifier.G1Point memory c = g1Mul(g1, cScalar);
        proofBytes = abi.encode(a.x, a.y, b.x1, b.x0, b.y1, b.y0, c.x, c.y);
    }

    function g1Mul(
        Groth16BN254SolvencyVerifier.G1Point memory p,
        uint256 s
    ) internal view returns (Groth16BN254SolvencyVerifier.G1Point memory r) {
        bool ok;
        assembly {
            let m := mload(0x40)
            mstore(m, mload(p))
            mstore(add(m, 0x20), mload(add(p, 0x20)))
            mstore(add(m, 0x40), s)
            ok := staticcall(gas(), 0x07, m, 0x60, r, 0x40)
        }
        require(ok, "G1_ECMUL_FAILED");
    }

    /// @dev Exact affine double-and-add on BN254 G2 over F_{p^2} = F_p[i]/(i^2 + 1).
    function g2MulSmall(
        Groth16BN254SolvencyVerifier.G2Point memory base,
        uint256 k
    ) internal view returns (Groth16BN254SolvencyVerifier.G2Point memory acc) {
        require(k > 0, "ZERO_SCALAR");
        bool hasAcc = false;
        Groth16BN254SolvencyVerifier.G2Point memory cur = base;
        uint256 n = k;
        while (n > 0) {
            if ((n & 1) == 1) {
                if (!hasAcc) {
                    acc = cur;
                    hasAcc = true;
                } else {
                    acc = g2Add(acc, cur);
                }
            }
            n >>= 1;
            if (n > 0) {
                cur = g2Double(cur);
            }
        }
    }

    function g2Double(
        Groth16BN254SolvencyVerifier.G2Point memory p1
    ) internal view returns (Groth16BN254SolvencyVerifier.G2Point memory r) {
        // m = 3 * x^2 / (2 * y) in F_{p^2}
        (uint256 xSq0, uint256 xSq1) = fp2Mul(p1.x0, p1.x1, p1.x0, p1.x1);
        uint256 num0 = mulmod(3, xSq0, P);
        uint256 num1 = mulmod(3, xSq1, P);
        uint256 den0 = mulmod(2, p1.y0, P);
        uint256 den1 = mulmod(2, p1.y1, P);
        (uint256 invDen0, uint256 invDen1) = fp2Inv(den0, den1);
        (uint256 m0, uint256 m1) = fp2Mul(num0, num1, invDen0, invDen1);

        (uint256 mSq0, uint256 mSq1) = fp2Mul(m0, m1, m0, m1);
        (uint256 twoX0, uint256 twoX1) = fp2Add(p1.x0, p1.x1, p1.x0, p1.x1);
        (uint256 x3_0, uint256 x3_1) = fp2Sub(mSq0, mSq1, twoX0, twoX1);

        (uint256 dx0, uint256 dx1) = fp2Sub(p1.x0, p1.x1, x3_0, x3_1);
        (uint256 mdx0, uint256 mdx1) = fp2Mul(m0, m1, dx0, dx1);
        (uint256 y3_0, uint256 y3_1) = fp2Sub(mdx0, mdx1, p1.y0, p1.y1);

        return Groth16BN254SolvencyVerifier.G2Point({x1: x3_1, x0: x3_0, y1: y3_1, y0: y3_0});
    }

    function g2Add(
        Groth16BN254SolvencyVerifier.G2Point memory p1,
        Groth16BN254SolvencyVerifier.G2Point memory p2
    ) internal view returns (Groth16BN254SolvencyVerifier.G2Point memory r) {
        if (p1.x0 == p2.x0 && p1.x1 == p2.x1 && p1.y0 == p2.y0 && p1.y1 == p2.y1) {
            return g2Double(p1);
        }
        (uint256 dy0, uint256 dy1) = fp2Sub(p2.y0, p2.y1, p1.y0, p1.y1);
        (uint256 dx0, uint256 dx1) = fp2Sub(p2.x0, p2.x1, p1.x0, p1.x1);
        (uint256 invDx0, uint256 invDx1) = fp2Inv(dx0, dx1);
        (uint256 m0, uint256 m1) = fp2Mul(dy0, dy1, invDx0, invDx1);

        (uint256 mSq0, uint256 mSq1) = fp2Mul(m0, m1, m0, m1);
        (uint256 t0, uint256 t1) = fp2Sub(mSq0, mSq1, p1.x0, p1.x1);
        (uint256 x3_0, uint256 x3_1) = fp2Sub(t0, t1, p2.x0, p2.x1);

        (uint256 sx0, uint256 sx1) = fp2Sub(p1.x0, p1.x1, x3_0, x3_1);
        (uint256 msx0, uint256 msx1) = fp2Mul(m0, m1, sx0, sx1);
        (uint256 y3_0, uint256 y3_1) = fp2Sub(msx0, msx1, p1.y0, p1.y1);

        return Groth16BN254SolvencyVerifier.G2Point({x1: x3_1, x0: x3_0, y1: y3_1, y0: y3_0});
    }

    function fp2Add(uint256 a0, uint256 a1, uint256 b0, uint256 b1) internal pure returns (uint256, uint256) {
        return (addmod(a0, b0, P), addmod(a1, b1, P));
    }

    function fp2Sub(uint256 a0, uint256 a1, uint256 b0, uint256 b1) internal pure returns (uint256, uint256) {
        return (addmod(a0, P - (b0 % P), P), addmod(a1, P - (b1 % P), P));
    }

    function fp2Mul(uint256 a0, uint256 a1, uint256 b0, uint256 b1) internal pure returns (uint256 c0, uint256 c1) {
        uint256 a0b0 = mulmod(a0, b0, P);
        uint256 a1b1 = mulmod(a1, b1, P);
        c0 = addmod(a0b0, P - a1b1, P);
        c1 = addmod(mulmod(a0, b1, P), mulmod(a1, b0, P), P);
    }

    function fp2Inv(uint256 a0, uint256 a1) internal view returns (uint256 inv0, uint256 inv1) {
        // (a0 - a1*i) / (a0^2 + a1^2) mod P
        uint256 norm = addmod(mulmod(a0, a0, P), mulmod(a1, a1, P), P);
        uint256 invNorm = modExp(norm, P - 2, P);
        inv0 = mulmod(a0, invNorm, P);
        inv1 = mulmod(addmod(P, P - (a1 % P), P), invNorm, P);
    }

    function modExp(uint256 base, uint256 exponent, uint256 modulus) internal view returns (uint256 out) {
        bool ok;
        assembly {
            let m := mload(0x40)
            mstore(m, 0x20)
            mstore(add(m, 0x20), 0x20)
            mstore(add(m, 0x40), 0x20)
            mstore(add(m, 0x60), base)
            mstore(add(m, 0x80), exponent)
            mstore(add(m, 0xa0), modulus)
            ok := staticcall(gas(), 0x05, m, 0xc0, 0x00, 0x20)
            out := mload(0x00)
        }
        require(ok, "MODEXP_FAILED");
    }
}

/// @title MerkleSumTreeHelper
/// @notice Deterministic off-chain MSST builder producing exact roots and inclusion proofs for testing & fuzzing.
library MerkleSumTreeHelper {
    struct LeafInput {
        uint64 accountId;
        address owner;
        address asset;
        uint128 balanceFp;
    }

    function buildTree(
        LeafInput[] memory leaves
    ) internal pure returns (bytes32 rootHash, uint128 rootSumFp, bytes32[][] memory levelHashes, uint128[][] memory levelSums) {
        uint256 n = leaves.length;
        require(n > 0 && (n & (n - 1)) == 0, "LEAVES_MUST_BE_POWER_OF_TWO");

        uint256 depth = 0;
        uint256 tmp = n;
        while (tmp > 1) {
            tmp >>= 1;
            ++depth;
        }

        levelHashes = new bytes32[][](depth + 1);
        levelSums = new uint128[][](depth + 1);
        levelHashes[0] = new bytes32[](n);
        levelSums[0] = new uint128[](n);

        for (uint256 i = 0; i < n; ++i) {
            levelHashes[0][i] = MerkleSumLib.computeLeafHash(
                leaves[i].accountId,
                leaves[i].owner,
                leaves[i].asset,
                leaves[i].balanceFp
            );
            levelSums[0][i] = leaves[i].balanceFp;
        }

        uint256 width = n;
        for (uint256 d = 0; d < depth; ++d) {
            uint256 nextWidth = width >> 1;
            levelHashes[d + 1] = new bytes32[](nextWidth);
            levelSums[d + 1] = new uint128[](nextWidth);
            for (uint256 j = 0; j < nextWidth; ++j) {
                (bytes32 pHash, uint128 pSum) = MerkleSumLib.hashInternalNode(
                    levelHashes[d][2 * j],
                    levelSums[d][2 * j],
                    levelHashes[d][2 * j + 1],
                    levelSums[d][2 * j + 1]
                );
                levelHashes[d + 1][j] = pHash;
                levelSums[d + 1][j] = pSum;
            }
            width = nextWidth;
        }

        rootHash = levelHashes[depth][0];
        rootSumFp = levelSums[depth][0];
    }

    function extractProof(
        bytes32[][] memory levelHashes,
        uint128[][] memory levelSums,
        uint256 leafIndex
    ) internal pure returns (MerkleSumLib.ProofStep[] memory proof) {
        uint256 depth = levelHashes.length - 1;
        proof = new MerkleSumLib.ProofStep[](depth);
        uint256 idx = leafIndex;
        for (uint256 d = 0; d < depth; ++d) {
            bool isRightNode = (idx & 1) == 1;
            uint256 sibIdx = isRightNode ? idx - 1 : idx + 1;
            proof[d] = MerkleSumLib.ProofStep({
                siblingHash: levelHashes[d][sibIdx],
                siblingSumFp: levelSums[d][sibIdx],
                isLeftSibling: isRightNode
            });
            idx >>= 1;
        }
    }
}
