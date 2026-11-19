"""
Strictly Causal Quantitative Microstructure & Time-Series Operators (`alpha_evo.gp_alpha.operators`).

Every `ts_*` and microstructure operator obeys the Strict Causal Invariant:
    For all t in [0, N-1]:  op(X[:t+1])[-1] == op(X)[t]  (zero lookahead leakage!)

Includes 64-byte cache-line aligned buffer allocation (`aligned_empty_64`) and in-place `out=`
kernels used by Pass 3 of the Static Single Assignment (SSA) register evaluator.
"""

from __future__ import annotations

import numpy as np
from numpy.lib.stride_tricks import sliding_window_view
from numpy.typing import NDArray

EPS: float = 1e-8
CLIP_BOUND: float = 10.0


def aligned_empty_64(shape: tuple[int, ...] | int, dtype: np.dtype | type = np.float64) -> NDArray[np.float64]:
    """
    Allocate a 64-byte CPU cache-line aligned contiguous NumPy array (AVX-512 / L1 cache line friendly).
    Guarantees `arr.ctypes.data % 64 == 0`.
    """
    dt = np.dtype(dtype)
    if isinstance(shape, int):
        shape = (shape,)
    n_elems = int(np.prod(shape))
    n_bytes = n_elems * dt.itemsize
    raw = np.empty(n_bytes + 64, dtype=np.uint8)
    offset = (-raw.ctypes.data) % 64
    aligned_view = raw[offset : offset + n_bytes].view(dt)
    return aligned_view.reshape(shape)


def _sanitize(arr: NDArray[np.float64], out: NDArray[np.float64] | None = None) -> NDArray[np.float64]:
    """Replace NaN/Inf with 0.0 and bound extreme magnitudes for numerical stability."""
    if out is None:
        res = np.nan_to_num(arr, nan=0.0, posinf=CLIP_BOUND, neginf=-CLIP_BOUND)
        return np.clip(res, -CLIP_BOUND, CLIP_BOUND)
    np.nan_to_num(arr, copy=False, nan=0.0, posinf=CLIP_BOUND, neginf=-CLIP_BOUND)
    np.clip(arr, -CLIP_BOUND, CLIP_BOUND, out=out)
    return out


def protected_div(
    x: NDArray[np.float64],
    y: NDArray[np.float64],
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """Numerically guarded division returning 0.0 where `|y| <= 1e-6`."""
    if out is None:
        out = np.empty_like(x, dtype=np.float64)
    valid = np.abs(y) > 1e-6
    out.fill(0.0)
    np.divide(x, y, out=out, where=valid)
    return _sanitize(out, out=out)


def ts_zscore(
    x: NDArray[np.float64],
    window: int = 20,
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Strictly causal O(N) rolling Z-Score over `window` bars:
        z_t = (x_t - mean(x_{t-w+1..t})) / (std(x_{t-w+1..t}) + eps)

    Uses causal initial-value shift `u = x - x[0]` (strictly causal — depends ONLY on t=0!)
    before cumulative prefix sums to prevent catastrophic cancellation on large offsets.
    """
    arr = np.asarray(x, dtype=np.float64)
    n = arr.shape[0]
    if out is None:
        out = np.empty(n, dtype=np.float64)
    if n == 0:
        return out
    w = max(2, int(window))

    # Shift strictly by causal anchor `arr[0]` (never `np.mean(arr)` which would peek ahead!)
    u = arr - arr[0]
    cs1 = np.empty(n + 1, dtype=np.float64)
    cs2 = np.empty(n + 1, dtype=np.float64)
    cs1[0] = 0.0
    cs2[0] = 0.0
    np.cumsum(u, out=cs1[1:])
    np.cumsum(u * u, out=cs2[1:])

    idx = np.arange(1, n + 1, dtype=np.int64)
    start = np.maximum(0, idx - w)
    cnt = (idx - start).astype(np.float64)

    sum1 = cs1[idx] - cs1[start]
    sum2 = cs2[idx] - cs2[start]
    mean_u = sum1 / cnt
    var_u = np.maximum(0.0, (sum2 / cnt) - mean_u * mean_u)
    std_u = np.sqrt(var_u)

    valid = (cnt >= 2.0) & (std_u > EPS)
    out.fill(0.0)
    np.divide(u - mean_u, std_u, out=out, where=valid)
    return _sanitize(out, out=out)


def ts_decay_linear(
    x: NDArray[np.float64],
    window: int = 20,
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Strictly causal O(N) linearly decaying weighted moving average over `window` bars:
        d_t = sum_{k=0}^{m-1} (m - k) * x_{t - k} / (m * (m + 1) / 2),  m = min(t + 1, window)

    Computed in exact O(N) time (independent of `window`!) via double prefix-sum identity:
        sum_{j=s}^{t} (j - s + 1) x_j = sum_{j=s}^{t} (j + 1) x_j - s * sum_{j=s}^{t} x_j
    """
    arr = np.asarray(x, dtype=np.float64)
    n = arr.shape[0]
    if out is None:
        out = np.empty(n, dtype=np.float64)
    if n == 0:
        return out
    w = max(1, int(window))
    if w == 1:
        np.copyto(out, arr)
        return _sanitize(out, out=out)

    j_plus_1 = np.arange(1, n + 1, dtype=np.float64)
    cs_x = np.empty(n + 1, dtype=np.float64)
    cs_jx = np.empty(n + 1, dtype=np.float64)
    cs_x[0] = 0.0
    cs_jx[0] = 0.0
    np.cumsum(arr, out=cs_x[1:])
    np.cumsum(j_plus_1 * arr, out=cs_jx[1:])

    idx = np.arange(1, n + 1, dtype=np.int64)
    s = np.maximum(0, idx - w)
    m = (idx - s).astype(np.float64)

    weighted_sum = (cs_jx[idx] - cs_jx[s]) - s.astype(np.float64) * (cs_x[idx] - cs_x[s])
    denom = 0.5 * m * (m + 1.0)
    np.divide(weighted_sum, denom, out=out)
    return _sanitize(out, out=out)


def ts_rank(
    x: NDArray[np.float64],
    window: int = 20,
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Strictly causal rolling fractional rank scaled to `[-1.0, +1.0]` over `window` bars:
        rank_t = 2.0 * (count(x_j < x_t) + 0.5 * count(x_j == x_t) - 0.5) / m - 1.0
    Vectorized over sliding windows without Python loops for `t >= window - 1`.
    """
    arr = np.asarray(x, dtype=np.float64)
    n = arr.shape[0]
    if out is None:
        out = np.empty(n, dtype=np.float64)
    if n == 0:
        return out
    w = max(2, int(window))
    out[0] = 0.0

    warm_end = min(n, w - 1)
    for t in range(1, warm_end):
        seg = arr[: t + 1]
        cur = arr[t]
        less = float(np.sum(seg < cur))
        eq = float(np.sum(seg == cur))
        m = float(t + 1)
        out[t] = 2.0 * ((less + 0.5 * (eq - 1.0)) / (m - 1.0)) - 1.0

    if n >= w:
        win = sliding_window_view(arr, w)              # shape (n - w + 1, w)
        cur_col = win[:, -1:]                          # shape (n - w + 1, 1)
        less_cnt = np.sum(win < cur_col, axis=1).astype(np.float64)
        eq_cnt = np.sum(win == cur_col, axis=1).astype(np.float64)
        out[w - 1 :] = 2.0 * ((less_cnt + 0.5 * (eq_cnt - 1.0)) / float(w - 1)) - 1.0

    return _sanitize(out, out=out)


def ts_corr(
    x: NDArray[np.float64],
    y: NDArray[np.float64],
    window: int = 20,
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Strictly causal O(N) rolling Pearson correlation coefficient `rho_{t,w}(x, y) in [-1, +1]`.
    Uses causal `x[0]` and `y[0]` shifts for numerical precision.
    """
    ax = np.asarray(x, dtype=np.float64)
    ay = np.asarray(y, dtype=np.float64)
    n = ax.shape[0]
    if out is None:
        out = np.empty(n, dtype=np.float64)
    if n == 0:
        return out
    w = max(3, int(window))

    ux = ax - ax[0]
    uy = ay - ay[0]

    cs_x = np.zeros(n + 1, dtype=np.float64)
    cs_y = np.zeros(n + 1, dtype=np.float64)
    cs_xx = np.zeros(n + 1, dtype=np.float64)
    cs_yy = np.zeros(n + 1, dtype=np.float64)
    cs_xy = np.zeros(n + 1, dtype=np.float64)

    np.cumsum(ux, out=cs_x[1:])
    np.cumsum(uy, out=cs_y[1:])
    np.cumsum(ux * ux, out=cs_xx[1:])
    np.cumsum(uy * uy, out=cs_yy[1:])
    np.cumsum(ux * uy, out=cs_xy[1:])

    idx = np.arange(1, n + 1, dtype=np.int64)
    s = np.maximum(0, idx - w)
    cnt = (idx - s).astype(np.float64)

    sx = cs_x[idx] - cs_x[s]
    sy = cs_y[idx] - cs_y[s]
    sxx = cs_xx[idx] - cs_xx[s]
    syy = cs_yy[idx] - cs_yy[s]
    sxy = cs_xy[idx] - cs_xy[s]

    cov = (sxy / cnt) - (sx / cnt) * (sy / cnt)
    vx = np.maximum(0.0, (sxx / cnt) - (sx / cnt) ** 2)
    vy = np.maximum(0.0, (syy / cnt) - (sy / cnt) ** 2)
    denom = np.sqrt(vx * vy)

    valid = (cnt >= 3.0) & (denom > EPS)
    out.fill(0.0)
    np.divide(cov, denom, out=out, where=valid)
    np.clip(out, -1.0, 1.0, out=out)
    return out


def ofi_delta(
    x: NDArray[np.float64],
    window: int = 10,
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Strictly causal Order Flow Imbalance impulse delta over `window` bars:
        ofi_delta_t = x_t - mean(x_{t-w..t-1})
    Captures sudden order-flow acceleration / liquidity sweep shocks relative to recent baseline.
    """
    arr = np.asarray(x, dtype=np.float64)
    n = arr.shape[0]
    if out is None:
        out = np.empty(n, dtype=np.float64)
    if n == 0:
        return out
    w = max(1, int(window))

    cs = np.zeros(n + 1, dtype=np.float64)
    np.cumsum(arr, out=cs[1:])

    t_idx = np.arange(n, dtype=np.int64)
    s = np.maximum(0, t_idx - w)
    cnt = np.maximum(1, t_idx - s).astype(np.float64)
    prior_mean = (cs[t_idx] - cs[s]) / cnt
    prior_mean[0] = arr[0]

    np.subtract(arr, prior_mean, out=out)
    return _sanitize(out, out=out)


def compute_cks_ofi_from_quotes(
    bid_px: NDArray[np.float64],
    bid_sz: NDArray[np.float64],
    ask_px: NDArray[np.float64],
    ask_sz: NDArray[np.float64],
) -> NDArray[np.float64]:
    """
    Exact Cont-Kukanov-Stoikov (2014) Level-1 Order Flow Imbalance event vector:
        e_t = I(P^b_t >= P^b_{t-1}) q^b_t - I(P^b_t <= P^b_{t-1}) q^b_{t-1}
            - I(P^a_t <= P^a_{t-1}) q^a_t + I(P^a_t >= P^a_{t-1}) q^a_{t-1}
    """
    bp = np.asarray(bid_px, dtype=np.float64)
    bs = np.asarray(bid_sz, dtype=np.float64)
    ap = np.asarray(ask_px, dtype=np.float64)
    az = np.asarray(ask_sz, dtype=np.float64)
    n = bp.shape[0]
    if n == 0:
        return np.empty(0, dtype=np.float64)

    bp_prev = np.empty_like(bp)
    bs_prev = np.empty_like(bs)
    ap_prev = np.empty_like(ap)
    az_prev = np.empty_like(az)
    bp_prev[0], bp_prev[1:] = bp[0], bp[:-1]
    bs_prev[0], bs_prev[1:] = bs[0], bs[:-1]
    ap_prev[0], ap_prev[1:] = ap[0], ap[:-1]
    az_prev[0], az_prev[1:] = az[0], az[:-1]

    e_b = np.where(bp >= bp_prev, bs, 0.0) - np.where(bp <= bp_prev, bs_prev, 0.0)
    e_a = np.where(ap <= ap_prev, az, 0.0) - np.where(ap >= ap_prev, az_prev, 0.0)
    ofi = e_b - e_a
    ofi[0] = 0.0
    return ofi


def microprice_skew(
    bid_px: NDArray[np.float64],
    bid_sz: NDArray[np.float64],
    ask_px: NDArray[np.float64],
    ask_sz: NDArray[np.float64],
    out: NDArray[np.float64] | None = None,
) -> NDArray[np.float64]:
    """
    Exact Stoikov (2018) Volume-Weighted Microprice Skew normalized by half-spread:
        M_t = (P^b_t * q^a_t + P^a_t * q^b_t) / (q^b_t + q^a_t)
        P^mid_t = 0.5 * (P^b_t + P^a_t)
        skew_t = (M_t - P^mid_t) / (0.5 * max(P^a_t - P^b_t, eps))  in [-1.0, +1.0]
    """
    bp = np.asarray(bid_px, dtype=np.float64)
    bs = np.maximum(0.0, np.asarray(bid_sz, dtype=np.float64))
    ap = np.asarray(ask_px, dtype=np.float64)
    az = np.maximum(0.0, np.asarray(ask_sz, dtype=np.float64))
    if out is None:
        out = np.empty_like(bp, dtype=np.float64)

    tot_sz = bs + az
    half_sp = 0.5 * np.maximum(ap - bp, EPS)
    mid = 0.5 * (bp + ap)

    valid = (tot_sz > EPS) & (ap > bp)
    micro = np.where(valid, (bp * az + ap * bs) / np.maximum(tot_sz, EPS), mid)
    out.fill(0.0)
    np.divide(micro - mid, half_sp, out=out, where=valid)
    np.clip(out, -1.0, 1.0, out=out)
    return out
