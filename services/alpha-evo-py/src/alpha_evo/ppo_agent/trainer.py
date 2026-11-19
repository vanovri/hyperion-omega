"""
Proximal Policy Optimization (PPO) + Generalized Advantage Estimation (GAE) Trainer
& Out-of-Sample Institutional Execution Baselines (Avellaneda-Stoikov, TWAP, Symmetric MM).
"""

from __future__ import annotations

from dataclasses import dataclass
import time
from typing import Any, Callable
import numpy as np
from numpy.typing import NDArray
import torch
import torch.nn as nn

from alpha_evo.lob_gym.env import ACT_DIM, LOBEnvConfig, OBS_DIM, VectorizedLOBEnv
from alpha_evo.ppo_agent.network import ActorCriticNetwork


@dataclass(slots=True)
class PPOConfig:
    """Hyperparameters for Vectorized PPO + GAE optimal execution training."""

    num_envs: int = 64
    rollout_steps: int = 128
    n_iterations: int = 36
    ppo_epochs: int = 4
    minibatch_size: int = 1_024
    learning_rate: float = 2.5e-3
    gamma: float = 0.992
    gae_lambda: float = 0.95
    clip_eps: float = 0.20
    vf_coef: float = 0.50
    ent_coef: float = 0.006
    max_grad_norm: float = 0.50
    target_kl: float = 0.03
    hidden_dim: int = 64
    seed: int = 202619


@dataclass(slots=True)
class PPOTrainingResult:
    """Trained PPO model, learning curve telemetry, and out-of-sample benchmark comparison."""

    model: ActorCriticNetwork
    learning_curve: list[dict[str, float | int]]
    benchmark_comparison: dict[str, dict[str, float]]
    wall_time_sec: float


def compute_gae(
    rewards: torch.Tensor,
    values: torch.Tensor,
    dones: torch.Tensor,
    next_value: torch.Tensor,
    gamma: float = 0.992,
    gae_lambda: float = 0.95,
) -> tuple[torch.Tensor, torch.Tensor]:
    """
    Exact Generalized Advantage Estimation (Schulman et al., ICLR 2016):
        delta_t = r_t + gamma * V(s_{t+1}) * (1 - done_t) - V(s_t)
        A_t = sum_{l=0}^{T-t-1} (gamma * lambda)^l * delta_{t+l}
        R_t = A_t + V(s_t)

    Shapes:
        `rewards`: (T, B), `values`: (T, B), `dones`: (T, B) float {0, 1}, `next_value`: (B,)
    """
    T, B = rewards.shape
    advantages = torch.zeros((T, B), dtype=rewards.dtype, device=rewards.device)
    last_gae = torch.zeros(B, dtype=rewards.dtype, device=rewards.device)

    for t in reversed(range(T)):
        next_non_terminal = 1.0 - dones[t]
        next_val = next_value if t == T - 1 else values[t + 1]
        delta = rewards[t] + gamma * next_val * next_non_terminal - values[t]
        last_gae = delta + gamma * gae_lambda * next_non_terminal * last_gae
        advantages[t] = last_gae

    returns = advantages + values
    return advantages, returns


def _avellaneda_stoikov_policy(obs: NDArray[np.float32]) -> NDArray[np.float32]:
    """
    Analytical Avellaneda-Stoikov (2008) reservation-price & optimal-spread baseline:
      r(s, q, t) = s - q * gamma * sigma^2 * (T - t)
      delta*     = gamma * sigma^2 * (T - t) + (2 / gamma) * ln(1 + gamma / k)
    """
    b = obs.shape[0]
    act = np.zeros((b, ACT_DIM), dtype=np.float32)
    q_norm = obs[:, 0]
    rem_t = obs[:, 1]
    sigma_norm = obs[:, 7]

    # Skew strongly against inventory proportional to remaining time & variance
    skew_ctrl = np.clip(-0.85 * q_norm * (0.35 + rem_t) * (0.5 + sigma_norm), -1.0, 1.0)
    # Widen half-spread when volatility increases
    spread_ctrl = np.clip(-0.45 + 0.55 * sigma_norm, -1.0, 1.0)
    size_ctrl = np.full(b, 0.25, dtype=np.float32)
    # Mild terminal inventory unwind if inventory is extreme near end of horizon
    urg_ctrl = np.where((rem_t < 0.15) & (np.abs(q_norm) > 0.55), -0.80 * np.sign(q_norm), 0.0).astype(np.float32)

    act[:, 0] = skew_ctrl
    act[:, 1] = spread_ctrl
    act[:, 2] = size_ctrl
    act[:, 3] = urg_ctrl
    return act


def _naive_symmetric_mm_policy(obs: NDArray[np.float32]) -> NDArray[np.float32]:
    """Naive fixed-spread zero-skew symmetric market maker (ignores inventory & alpha)."""
    b = obs.shape[0]
    act = np.zeros((b, ACT_DIM), dtype=np.float32)
    act[:, 0] = 0.0
    act[:, 1] = -0.25   # Narrow fixed spread (~2 ticks) -> vulnerable to adverse pick-off & inventory drift!
    act[:, 2] = 0.40
    act[:, 3] = 0.0
    return act


def _twap_almgren_chriss_policy(obs: NDArray[np.float32]) -> NDArray[np.float32]:
    """Static passive-wide + constant-rate inventory flatten baseline."""
    b = obs.shape[0]
    act = np.zeros((b, ACT_DIM), dtype=np.float32)
    q_norm = obs[:, 0]
    act[:, 0] = np.clip(-0.50 * q_norm, -1.0, 1.0)
    act[:, 1] = 0.45    # Wide conservative spread
    act[:, 2] = -0.30
    act[:, 3] = np.where(np.abs(q_norm) > 0.25, -0.78 * np.sign(q_norm), 0.0).astype(np.float32)
    return act


def evaluate_policy_rollout(
    policy_fn: Callable[[NDArray[np.float32]], NDArray[np.float32]],
    env_cfg: LOBEnvConfig,
    n_eval_episodes_batches: int = 3,
    eval_seed: int = 999_019,
    alpha_signal_fn: Callable[[dict[str, NDArray[np.float64]]], NDArray[np.float64]] | None = None,
) -> dict[str, float]:
    """Evaluate a deterministic policy on unseen out-of-sample trajectories."""
    cfg_eval = LOBEnvConfig(
        symbol=env_cfg.symbol,
        num_envs=env_cfg.num_envs,
        episode_steps=env_cfg.episode_steps,
        step_ms=env_cfg.step_ms,
        latency_log_mean=env_cfg.latency_log_mean,
        latency_log_std=env_cfg.latency_log_std,
        latency_tick_ms=env_cfg.latency_tick_ms,
        min_latency_steps=env_cfg.min_latency_steps,
        max_latency_steps=env_cfg.max_latency_steps,
        maker_fee_ppm=env_cfg.maker_fee_ppm,
        taker_fee_ppm=env_cfg.taker_fee_ppm,
        post_only=env_cfg.post_only,
        base_quote_lots=env_cfg.base_quote_lots,
        max_inventory_lots=env_cfg.max_inventory_lots,
        initial_target_exec_lots=env_cfg.initial_target_exec_lots,
        base_book_depth_lots=env_cfg.base_book_depth_lots,
        base_spread_ticks=env_cfg.base_spread_ticks,
        base_sigma_ticks=env_cfg.base_sigma_ticks,
        temp_impact_eta_ticks=env_cfg.temp_impact_eta_ticks,
        temp_impact_beta=env_cfg.temp_impact_beta,
        temp_impact_resilience=env_cfg.temp_impact_resilience,
        perm_impact_gamma_ticks=env_cfg.perm_impact_gamma_ticks,
        inv_risk_aversion_gamma=env_cfg.inv_risk_aversion_gamma,
        ac_exec_urgency_eta=env_cfg.ac_exec_urgency_eta,
        tail_drawdown_threshold_quote=env_cfg.tail_drawdown_threshold_quote,
        tail_drawdown_lambda=env_cfg.tail_drawdown_lambda,
        terminal_inv_penalty_kappa=env_cfg.terminal_inv_penalty_kappa,
        enable_competing_mm=env_cfg.enable_competing_mm,
        seed=eval_seed,
    )
    env = VectorizedLOBEnv(cfg_eval, alpha_signal_fn=alpha_signal_fn)

    ep_returns: list[float] = []
    ep_net_pnls: list[float] = []
    ep_max_dds: list[float] = []
    ep_rms_invs: list[float] = []
    ep_step_pnls: list[float] = []

    for batch_idx in range(n_eval_episodes_batches):
        obs, _ = env.reset(seed=eval_seed + batch_idx * 101)
        cum_rew = np.zeros(cfg_eval.num_envs, dtype=np.float64)
        inv_sq_acc = np.zeros(cfg_eval.num_envs, dtype=np.float64)
        max_dd = np.zeros(cfg_eval.num_envs, dtype=np.float64)

        for _ in range(cfg_eval.episode_steps):
            act = policy_fn(obs)
            obs, rew, _, _, info = env.step(act)
            cum_rew += rew.astype(np.float64)
            inv_sq_acc += info["inventory_norm"] ** 2
            max_dd = np.maximum(max_dd, info["drawdown_quote"])
            ep_step_pnls.append(float(np.mean(info["step_pnl_quote"])))

        ep_returns.extend(cum_rew.tolist())
        ep_net_pnls.extend(info["net_pnl_quote"].tolist())
        ep_max_dds.extend(max_dd.tolist())
        ep_rms_invs.extend(np.sqrt(inv_sq_acc / cfg_eval.episode_steps).tolist())

    pnl_arr = np.asarray(ep_net_pnls, dtype=np.float64)
    sd_ep = float(np.std(pnl_arr, ddof=1)) if pnl_arr.size > 1 else 1.0
    sharpe_ep = (float(np.mean(pnl_arr)) / max(sd_ep, 1e-8)) * np.sqrt(252.0)

    return {
        "mean_utility_reward": round(float(np.mean(ep_returns)), 4),
        "mean_net_pnl_usdt": round(float(np.mean(ep_net_pnls)), 4),
        "episode_sharpe_annualized": round(float(sharpe_ep), 3),
        "rms_inventory_fraction": round(float(np.mean(ep_rms_invs)), 4),
        "p95_tail_drawdown_usdt": round(float(np.percentile(ep_max_dds, 95)), 4),
        "mean_max_drawdown_usdt": round(float(np.mean(ep_max_dds)), 4),
    }


def evaluate_execution_baselines(
    trained_model: ActorCriticNetwork,
    env_cfg: LOBEnvConfig,
    alpha_signal_fn: Callable[[dict[str, NDArray[np.float64]]], NDArray[np.float64]] | None = None,
) -> dict[str, dict[str, float]]:
    """Run head-to-head out-of-sample evaluation across PPO and 3 classical quant baselines."""
    trained_model.eval()

    def _ppo_det_policy(obs_np: NDArray[np.float32]) -> NDArray[np.float32]:
        with torch.no_grad():
            t_obs = torch.as_tensor(obs_np, dtype=torch.float32)
            act_t, _ = trained_model(t_obs)
            return act_t.cpu().numpy()

    return {
        "ppo_gae_agent": evaluate_policy_rollout(_ppo_det_policy, env_cfg, alpha_signal_fn=alpha_signal_fn),
        "avellaneda_stoikov_analytical": evaluate_policy_rollout(
            _avellaneda_stoikov_policy, env_cfg, alpha_signal_fn=alpha_signal_fn
        ),
        "naive_symmetric_mm": evaluate_policy_rollout(
            _naive_symmetric_mm_policy, env_cfg, alpha_signal_fn=alpha_signal_fn
        ),
        "twap_almgren_chriss_static": evaluate_policy_rollout(
            _twap_almgren_chriss_policy, env_cfg, alpha_signal_fn=alpha_signal_fn
        ),
    }


def train_ppo_agent(
    ppo_cfg: PPOConfig | None = None,
    env_cfg: LOBEnvConfig | None = None,
    alpha_signal_fn: Callable[[dict[str, NDArray[np.float64]]], NDArray[np.float64]] | None = None,
) -> PPOTrainingResult:
    """
    Train Actor-Critic Optimal Execution & Market-Making Policy via Clipped PPO + GAE(lambda).
    """
    t0 = time.perf_counter()
    cfg = ppo_cfg or PPOConfig()
    torch.manual_seed(cfg.seed)
    np.random.seed(cfg.seed)

    if env_cfg is None:
        env_cfg = LOBEnvConfig(
            num_envs=cfg.num_envs,
            episode_steps=cfg.rollout_steps,
            seed=cfg.seed,
        )
    else:
        env_cfg = LOBEnvConfig(
            symbol=env_cfg.symbol,
            num_envs=cfg.num_envs,
            episode_steps=cfg.rollout_steps,
            step_ms=env_cfg.step_ms,
            latency_log_mean=env_cfg.latency_log_mean,
            latency_log_std=env_cfg.latency_log_std,
            latency_tick_ms=env_cfg.latency_tick_ms,
            min_latency_steps=env_cfg.min_latency_steps,
            max_latency_steps=env_cfg.max_latency_steps,
            maker_fee_ppm=env_cfg.maker_fee_ppm,
            taker_fee_ppm=env_cfg.taker_fee_ppm,
            post_only=env_cfg.post_only,
            base_quote_lots=env_cfg.base_quote_lots,
            max_inventory_lots=env_cfg.max_inventory_lots,
            initial_target_exec_lots=env_cfg.initial_target_exec_lots,
            base_book_depth_lots=env_cfg.base_book_depth_lots,
            base_spread_ticks=env_cfg.base_spread_ticks,
            base_sigma_ticks=env_cfg.base_sigma_ticks,
            temp_impact_eta_ticks=env_cfg.temp_impact_eta_ticks,
            temp_impact_beta=env_cfg.temp_impact_beta,
            temp_impact_resilience=env_cfg.temp_impact_resilience,
            perm_impact_gamma_ticks=env_cfg.perm_impact_gamma_ticks,
            inv_risk_aversion_gamma=env_cfg.inv_risk_aversion_gamma,
            ac_exec_urgency_eta=env_cfg.ac_exec_urgency_eta,
            tail_drawdown_threshold_quote=env_cfg.tail_drawdown_threshold_quote,
            tail_drawdown_lambda=env_cfg.tail_drawdown_lambda,
            terminal_inv_penalty_kappa=env_cfg.terminal_inv_penalty_kappa,
            enable_competing_mm=env_cfg.enable_competing_mm,
            seed=cfg.seed,
        )

    env = VectorizedLOBEnv(env_cfg, alpha_signal_fn=alpha_signal_fn)
    model = ActorCriticNetwork(obs_dim=OBS_DIM, act_dim=ACT_DIM, hidden_dim=cfg.hidden_dim)
    optimizer = torch.optim.Adam(model.parameters(), lr=cfg.learning_rate, eps=1e-5)

    T = cfg.rollout_steps
    B = cfg.num_envs
    batch_total = T * B

    obs_buf = torch.zeros((T, B, OBS_DIM), dtype=torch.float32)
    raw_act_buf = torch.zeros((T, B, ACT_DIM), dtype=torch.float32)
    logprob_buf = torch.zeros((T, B), dtype=torch.float32)
    rew_buf = torch.zeros((T, B), dtype=torch.float32)
    done_buf = torch.zeros((T, B), dtype=torch.float32)
    val_buf = torch.zeros((T, B), dtype=torch.float32)

    learning_curve: list[dict[str, float | int]] = []

    for it in range(1, cfg.n_iterations + 1):
        # Linear LR annealing
        frac = 1.0 - (it - 1.0) / float(max(cfg.n_iterations, 1))
        for pg in optimizer.param_groups:
            pg["lr"] = cfg.learning_rate * max(0.10, frac)

        obs_np, _ = env.reset(seed=cfg.seed + it * 37)
        ep_pnl_acc = np.zeros(B, dtype=np.float64)
        ep_inv_pen_acc = np.zeros(B, dtype=np.float64)
        ep_tail_pen_acc = np.zeros(B, dtype=np.float64)
        ep_max_dd = np.zeros(B, dtype=np.float64)

        model.eval()
        for t in range(T):
            obs_t = torch.as_tensor(obs_np, dtype=torch.float32)
            model.update_obs_stats(obs_t)
            obs_buf[t] = obs_t

            with torch.no_grad():
                bounded_act, raw_act, logprob, _, val = model.get_action_and_value(obs_t)

            raw_act_buf[t] = raw_act
            logprob_buf[t] = logprob
            val_buf[t] = val

            next_obs_np, rew_np, term_np, trunc_np, info = env.step(bounded_act.numpy())
            done_np = np.logical_or(term_np, trunc_np).astype(np.float32)

            rew_buf[t] = torch.as_tensor(rew_np, dtype=torch.float32)
            done_buf[t] = torch.as_tensor(done_np, dtype=torch.float32)

            ep_pnl_acc += info["step_pnl_quote"]
            ep_inv_pen_acc += info["inv_penalty"]
            ep_tail_pen_acc += info["tail_dd_penalty"]
            ep_max_dd = np.maximum(ep_max_dd, info["drawdown_quote"])
            obs_np = next_obs_np

        with torch.no_grad():
            next_val = model.get_value(torch.as_tensor(obs_np, dtype=torch.float32))
            advantages, returns = compute_gae(
                rew_buf,
                val_buf,
                done_buf,
                next_val,
                gamma=cfg.gamma,
                gae_lambda=cfg.gae_lambda,
            )

        # Flatten rollout batch
        b_obs = obs_buf.reshape(batch_total, OBS_DIM)
        b_raw_act = raw_act_buf.reshape(batch_total, ACT_DIM)
        b_logprob = logprob_buf.reshape(batch_total)
        b_adv = advantages.reshape(batch_total)
        b_ret = returns.reshape(batch_total)
        b_val = val_buf.reshape(batch_total)

        model.train()
        approx_kl_val = 0.0
        pg_loss_val = 0.0
        vf_loss_val = 0.0
        ent_val = 0.0
        mb_size = min(cfg.minibatch_size, batch_total)

        for _epoch in range(cfg.ppo_epochs):
            perm = torch.randperm(batch_total)
            stop_kl = False
            for start in range(0, batch_total, mb_size):
                mb_idx = perm[start : start + mb_size]
                _, _, new_logprob, entropy, new_val = model.get_action_and_value(
                    b_obs[mb_idx],
                    raw_action=b_raw_act[mb_idx],
                )
                logratio = new_logprob - b_logprob[mb_idx]
                ratio = torch.exp(logratio)

                with torch.no_grad():
                    approx_kl = float(((ratio - 1.0) - logratio).mean().item())
                    approx_kl_val = approx_kl
                if approx_kl > 1.5 * cfg.target_kl:
                    stop_kl = True
                    break

                mb_adv = b_adv[mb_idx]
                mb_adv = (mb_adv - mb_adv.mean()) / (mb_adv.std() + 1e-8)

                pg_loss1 = -mb_adv * ratio
                pg_loss2 = -mb_adv * torch.clamp(ratio, 1.0 - cfg.clip_eps, 1.0 + cfg.clip_eps)
                pg_loss = torch.max(pg_loss1, pg_loss2).mean()

                # Clipped value loss
                v_clipped = b_val[mb_idx] + torch.clamp(
                    new_val - b_val[mb_idx], -cfg.clip_eps, cfg.clip_eps
                )
                v_loss_unclipped = (new_val - b_ret[mb_idx]) ** 2
                v_loss_clipped = (v_clipped - b_ret[mb_idx]) ** 2
                v_loss = 0.5 * torch.max(v_loss_unclipped, v_loss_clipped).mean()

                ent_loss = entropy.mean()
                loss = pg_loss + cfg.vf_coef * v_loss - cfg.ent_coef * ent_loss

                optimizer.zero_grad()
                loss.backward()
                nn.utils.clip_grad_norm_(model.parameters(), cfg.max_grad_norm)
                optimizer.step()

                pg_loss_val = float(pg_loss.item())
                vf_loss_val = float(v_loss.item())
                ent_val = float(ent_loss.item())

            if stop_kl:
                break

        y_pred = b_val.numpy()
        y_true = b_ret.numpy()
        var_y = float(np.var(y_true))
        explained_var = 1.0 - float(np.var(y_true - y_pred)) / max(var_y, 1e-8)

        ep_total_reward = float(rew_buf.sum(dim=0).mean().item())
        learning_curve.append(
            {
                "iteration": it,
                "timesteps": it * batch_total,
                "mean_episode_reward": round(ep_total_reward, 4),
                "mean_spread_pnl_usdt": round(float(np.mean(ep_pnl_acc)), 4),
                "mean_inv_penalty": round(float(np.mean(ep_inv_pen_acc)), 4),
                "mean_tail_dd_penalty": round(float(np.mean(ep_tail_pen_acc)), 4),
                "p95_drawdown_usdt": round(float(np.percentile(ep_max_dd, 95)), 4),
                "policy_loss": round(pg_loss_val, 5),
                "value_loss": round(vf_loss_val, 5),
                "entropy": round(ent_val, 4),
                "approx_kl": round(approx_kl_val, 6),
                "explained_variance": round(explained_var, 4),
            }
        )

    assert bool(np.all(env.double_entry_residual_fp == 0)), "CRITICAL: Double-Entry Ledger Conservation Violated!"

    benchmarks = evaluate_execution_baselines(model, env_cfg, alpha_signal_fn=alpha_signal_fn)
    elapsed = time.perf_counter() - t0
    return PPOTrainingResult(
        model=model,
        learning_curve=learning_curve,
        benchmark_comparison=benchmarks,
        wall_time_sec=round(elapsed, 3),
    )
