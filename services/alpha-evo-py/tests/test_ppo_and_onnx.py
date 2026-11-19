"""Tests for PyTorch PPO + GAE exactness, policy improvement, and ONNXRuntime numerical parity."""

from __future__ import annotations

import torch

from alpha_evo.lob_gym.env import LOBEnvConfig
from alpha_evo.ppo_agent.exporter import export_ppo_artifacts
from alpha_evo.ppo_agent.trainer import PPOConfig, compute_gae, train_ppo_agent


def test_gae_exact_hand_calculated_recurrence() -> None:
    rewards = torch.tensor([[1.0, 2.0], [3.0, 0.0]], dtype=torch.float32)
    values = torch.tensor([[0.5, 1.0], [1.5, 0.5]], dtype=torch.float32)
    dones = torch.tensor([[0.0, 1.0], [0.0, 0.0]], dtype=torch.float32)
    next_val = torch.tensor([2.0, 1.0], dtype=torch.float32)

    adv, ret = compute_gae(rewards, values, dones, next_val, gamma=0.9, gae_lambda=0.8)

    # Env 0, t=1: delta_1 = 3.0 + 0.9 * 2.0 * 1 - 1.5 = 3.3 -> A_1 = 3.3
    # Env 0, t=0: delta_0 = 1.0 + 0.9 * 1.5 * 1 - 0.5 = 1.85 -> A_0 = 1.85 + 0.9*0.8*3.3 = 4.226
    assert torch.isclose(adv[1, 0], torch.tensor(3.3), atol=1e-5)
    assert torch.isclose(adv[0, 0], torch.tensor(4.226), atol=1e-5)

    # Env 1, t=0 has done=1.0 -> bootstrap cut off! delta_0 = 2.0 + 0 - 1.0 = 1.0 -> A_0 = 1.0
    assert torch.isclose(adv[0, 1], torch.tensor(1.0), atol=1e-5)
    assert torch.allclose(ret, adv + values, atol=1e-6)


def test_ppo_training_and_onnx_export_parity(tmp_path) -> None:
    ppo_cfg = PPOConfig(
        num_envs=24,
        rollout_steps=64,
        n_iterations=8,
        minibatch_size=384,
        seed=2026,
    )
    env_cfg = LOBEnvConfig(num_envs=24, episode_steps=64, seed=2026)
    res = train_ppo_agent(ppo_cfg=ppo_cfg, env_cfg=env_cfg)

    assert len(res.learning_curve) == 8
    # Trained PPO agent must outperform naive unskewed symmetric market maker on risk-adjusted utility!
    ppo_util = res.benchmark_comparison["ppo_gae_agent"]["mean_utility_reward"]
    naive_util = res.benchmark_comparison["naive_symmetric_mm"]["mean_utility_reward"]
    assert ppo_util > naive_util

    exp = export_ppo_artifacts(res, tmp_path)
    assert exp["onnx_size_bytes"] > 1_000
    assert exp["npz_size_bytes"] > 500
    assert exp["ort_max_action_abs_diff"] < 1e-5
    assert exp["ort_max_value_abs_diff"] < 1e-5
