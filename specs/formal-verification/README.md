# HYPERION OMEGA — Formal Verification Suite (`Channel 11`)

Executable mathematical proofs and exhaustive state-space model checking for **HYPERION OMEGA**:

## Directory Layout

- `smt/` — **Z3 SMT Theorem Proving Suite (`z3-solver`)**:
  - `theorem1_double_entry_conservation.py`: Proves `UNSAT` over full 64-bit unsigned domain `[1, 2^64 - 1]` for prices/quantities and signed `[-1e6, +1e6]` ppm maker rebates / taker fees (`u128`/`i128` overflow freedom, `QF_BV` two's-complement `bvsdiv == trunc_div`, K-way partial-fill double-entry conservation, rebate-farming immunity vs naive `floor_div`, and inductive locked-collateral sufficiency).
  - `theorem2_margin_liquidation_adl.py`: Proves `UNSAT` for Cross-Margin positive maintenance-margin bankruptcy cushion, zero undetected negative equity, zero-sum open-interest clearinghouse ADL capacity sufficiency, Insurance Fund absorption, and strict profit-leverage quantile-ordered ADL fairness.
  - `theorem3_l2_bridge_no_double_spend.py`: Proves `UNSAT` for EIP-712 `(chain_id, verifying_vault, account_id, withdrawal_nonce, epoch_id)` replay & cross-chain immunity, `K_FINALITY = 12` block reorg rollback safety, and `HyperionVault.sol` + `CensorshipEscapeHatch.sol` forced-exit mutual exclusion.
  - `run_all_smt_proofs.py`: Master Z3 runner + `pytest` test suite + `smt_proof_certificates.json` generator.
  - `overclock_mutation_and_soak.py`: AST-operator mutation harness (`>= 95%` kill gate) + `500,000`-cycle `tracemalloc` zero-leak soak test (`overclock_python_report.json`).
- `tlaplus/` — **Formal TLA+ Specification**:
  - `HyperionOmegaRaftL2.tla` & `HyperionOmegaRaftL2.cfg`: Complete TLA+ specification of coupled Raft + PreVote + Cryptographic Chained-Hash WAL + L2 Settlement Batch Commit (`ElectionSafety`, `LogMatching`, `WalHashChainIntegrity`, `ZeroCommittedDataLoss`, `L2SettlementSafety`, and temporal `PostHealLiveness`).
- `model-checker-rs/` — **Compiled Rust Explicit-State BFS & Liveness Model Checker**:
  - Zero-dependency safe Rust (`#![forbid(unsafe_code)]`) explicit-state model checker exploring `>= 1,250,000` concurrent transitions across asymmetric/symmetric network partitions, sudden leader crashes, persistent WAL recovery, duplicate/dropped/out-of-order RPCs, stale minority-leader writes, and shallow L2 chain reorgs, plus `10,000+` post-partition liveness convergence checks and Extreme Overclock benchmarks (`model_checker_report.json`, `overclock_rust_report.json`).
