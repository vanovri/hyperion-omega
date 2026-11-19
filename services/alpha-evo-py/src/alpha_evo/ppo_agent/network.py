"""
PyTorch Actor-Critic Neural Architecture with Embedded Welford Observation Normalizer.

Embedding `obs_mean` and `obs_var` directly as `nn.Module` buffers ensures the exported
ONNX graph (`ppo_execution_agent.onnx`) is 100% self-contained with zero external
feature-scaling drift during production inference.
"""

from __future__ import annotations

import math
import torch
import torch.nn as nn
from torch.distributions import Normal

from alpha_evo.lob_gym.env import ACT_DIM, OBS_DIM


def _orthogonal_init(layer: nn.Linear, gain: float = math.sqrt(2.0)) -> nn.Linear:
    nn.init.orthogonal_(layer.weight, gain=gain)
    if layer.bias is not None:
        nn.init.zeros_(layer.bias)
    return layer


class ActorCriticNetwork(nn.Module):
    """
    Dual-trunk Actor-Critic network solving the coupled Avellaneda-Stoikov + Almgren-Chriss
    market-making and optimal execution problem.
    """

    obs_mean: torch.Tensor
    obs_var: torch.Tensor
    obs_count: torch.Tensor

    def __init__(
        self,
        obs_dim: int = OBS_DIM,
        act_dim: int = ACT_DIM,
        hidden_dim: int = 64,
    ) -> None:
        super().__init__()
        self.obs_dim = obs_dim
        self.act_dim = act_dim
        self.hidden_dim = hidden_dim

        self.register_buffer("obs_mean", torch.zeros(obs_dim, dtype=torch.float32))
        self.register_buffer("obs_var", torch.ones(obs_dim, dtype=torch.float32))
        self.register_buffer("obs_count", torch.tensor(1e-4, dtype=torch.float32))

        self.actor_net = nn.Sequential(
            _orthogonal_init(nn.Linear(obs_dim, hidden_dim)),
            nn.Tanh(),
            _orthogonal_init(nn.Linear(hidden_dim, hidden_dim)),
            nn.Tanh(),
            _orthogonal_init(nn.Linear(hidden_dim, act_dim), gain=0.01),
        )

        self.critic_net = nn.Sequential(
            _orthogonal_init(nn.Linear(obs_dim, hidden_dim)),
            nn.Tanh(),
            _orthogonal_init(nn.Linear(hidden_dim, hidden_dim)),
            nn.Tanh(),
            _orthogonal_init(nn.Linear(hidden_dim, 1), gain=1.0),
        )

        # Diagonal Gaussian exploration log-std (clamped during forward pass)
        self.actor_log_std = nn.Parameter(torch.full((act_dim,), -0.55, dtype=torch.float32))

    @torch.no_grad()
    def update_obs_stats(self, batch_obs: torch.Tensor) -> None:
        """Parallel Welford running mean/variance update on `(N, obs_dim)` batch."""
        flat = batch_obs.reshape(-1, self.obs_dim).to(dtype=torch.float32)
        batch_n = float(flat.shape[0])
        if batch_n < 1.0:
            return
        b_mean = flat.mean(dim=0)
        b_var = flat.var(dim=0, unbiased=False)

        delta = b_mean - self.obs_mean
        tot_n = self.obs_count + batch_n
        new_mean = self.obs_mean + delta * (batch_n / tot_n)
        m_a = self.obs_var * self.obs_count
        m_b = b_var * batch_n
        m2 = m_a + m_b + (delta * delta) * (self.obs_count * batch_n / tot_n)
        new_var = torch.clamp(m2 / tot_n, min=1e-4)

        self.obs_mean.copy_(new_mean)
        self.obs_var.copy_(new_var)
        self.obs_count.copy_(tot_n)

    def normalize_obs(self, obs: torch.Tensor) -> torch.Tensor:
        normed = (obs - self.obs_mean) / torch.sqrt(self.obs_var + 1e-5)
        return torch.clamp(normed, -8.0, 8.0)

    def forward(self, obs: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        """
        Deterministic ONNX inference entry point:
            `obs: (B, 14) -> (bounded_action: (B, 4) in [-1, 1], state_value: (B, 1))`
        """
        z = self.normalize_obs(obs)
        raw_mean = self.actor_net(z)
        bounded_action = torch.tanh(raw_mean)
        value = self.critic_net(z)
        return bounded_action, value

    def get_action_and_value(
        self,
        obs: torch.Tensor,
        raw_action: torch.Tensor | None = None,
    ) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
        """
        Sample or evaluate pre-squash Gaussian action `u` and bounded action `a = tanh(u)`
        for PPO policy-gradient updates.
        Returns `(bounded_action, raw_action, log_prob, entropy, value_flat)`.
        """
        z = self.normalize_obs(obs)
        mean = self.actor_net(z)
        log_std = torch.clamp(self.actor_log_std, -2.2, 0.2).expand_as(mean)
        std = torch.exp(log_std)
        dist = Normal(mean, std)

        if raw_action is None:
            raw_action = dist.sample()
        bounded_action = torch.tanh(raw_action)

        # Log-probability under Gaussian (standard CleanRL unscented surrogate in raw space
        # avoids boundary Jacobian numerical blowup while `tanh` bounds env actions cleanly!)
        log_prob = dist.log_prob(raw_action).sum(dim=-1)
        entropy = dist.entropy().sum(dim=-1)
        value = self.critic_net(z).squeeze(-1)
        return bounded_action, raw_action, log_prob, entropy, value

    def get_value(self, obs: torch.Tensor) -> torch.Tensor:
        z = self.normalize_obs(obs)
        return self.critic_net(z).squeeze(-1)
