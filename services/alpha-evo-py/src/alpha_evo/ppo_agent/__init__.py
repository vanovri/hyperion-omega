"""PyTorch Multi-Agent PPO + GAE Optimal Execution & Market-Making Suite."""

from alpha_evo.ppo_agent.exporter import export_ppo_artifacts
from alpha_evo.ppo_agent.network import ActorCriticNetwork
from alpha_evo.ppo_agent.trainer import (
    PPOConfig,
    PPOTrainingResult,
    compute_gae,
    evaluate_execution_baselines,
    train_ppo_agent,
)

__all__ = [
    "ActorCriticNetwork",
    "PPOConfig",
    "PPOTrainingResult",
    "compute_gae",
    "train_ppo_agent",
    "evaluate_execution_baselines",
    "export_ppo_artifacts",
]
