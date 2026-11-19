# Hyperion Omega — Channel 19 (`services/alpha-evo-py`)

Autonomous Quantitative AI Research & Reinforcement Learning Optimal Execution Lab:

1. **Vectorized Limit Order Book Gym (`alpha_evo.lob_gym`)**:
   - Farama `Gymnasium` (`HyperionLOBGymEnv`) & batched C-contiguous NumPy (`VectorizedLOBEnv`) market simulator.
   - Real-time FIFO queue position tracking (`queue_ahead_fp`), order-amend priority preservation/loss (`orderbook.proto`), Post-Only crossing rejection, Log-Normal wire latency ring buffer, exact `1e8` fixed-point Channel 02 maker/taker fee conservation, and Almgren-Chriss permanent + temporary market impact.
2. **Genetic Programming Symbolic Alpha Factory (`alpha_evo.gp_alpha`)**:
   - Strictly causal $O(N)$ / sliding-window quantitative operators (`ts_rank`, `ts_zscore`, `ts_decay_linear`, `ts_corr`, `ofi_delta`, `microprice_skew`) with 64-byte cache-line aligned Static Single Assignment (SSA) zero-allocation evaluation.
   - Subtree crossover, point/hoist/subtree mutation, algebraic simplification, and Parsimony Pressure bloat control.
   - Automatic `#![no_std]` Rust / WebAssembly (`wasm32-unknown-unknown`) & JSON compiler targeting Channel 15 (`quant-wasm-host`).
3. **Multi-Agent PPO + GAE Optimal Execution & Market-Making (`alpha_evo.ppo_agent`)**:
   - PyTorch Actor-Critic with embedded Welford `RunningMeanStd` buffers solving the coupled Avellaneda-Stoikov + Almgren-Chriss + quadratic tail-drawdown objective.
   - Verified ONNX export (`ppo_execution_agent.onnx` with `< 1e-5` ONNXRuntime numerical parity) & compact `.npz` weights.
4. **Institutional Anti-Overfitting Validation (`alpha_evo.validation`)**:
   - Purged & Embargoed Walk-Forward CV & Combinatorial Purged K-Fold (`CPCV`).
   - Exact Bailey & López de Prado (2014) Deflated Sharpe Ratio (`DSR`), Probabilistic Sharpe Ratio (`PSR`), Effective Independent Trials (`M_eff`), and Bailey et al. (2017) CSCV Probability of Backtest Overfitting (`PBO`).

## Quickstart

```bash
uv venv --python 3.12
uv pip install --extra-index-url https://download.pytorch.org/whl/cpu -e ".[dev]"
pytest -v
alpha-evo-run
```
