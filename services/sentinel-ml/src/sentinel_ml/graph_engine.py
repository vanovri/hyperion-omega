"""Streaming Temporal Transaction Multigraph & Bounded Tarjan-Johnson Wash-Trading Detector.

Implements:
1. In-memory directed sliding-window temporal multigraph mapping maker -> taker and
   economic seller -> buyer execution edges with exact fixed-point (1e8) price/qty/notional
   attributes and nanosecond inter-arrival deltas.
2. Incremental real-time cycle detector combining bidirectional hop pruning,
   Tarjan's Strongly Connected Components (SCC), and Depth-Aware Johnson's simple
   cycle algorithm bounded at depth k <= 6 with exponential temporal decay scoring.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass
import heapq
import math
from typing import Literal

from sentinel_ml.fixed_point import PPM_SCALE, compute_notional_fp, ratio_to_ppm, require_uint64

SideLiteral = Literal["BUY", "SELL"]
INF_DEPTH: int = 1_000_000


@dataclass(frozen=True, slots=True)
class TradeExecutionEdge:
    """Directed trade execution edge aligned 1:1 with hyperion.v1.TransactionGraphEdge."""

    edge_id: int
    fill_id: int
    engine_seq: int
    symbol: str
    maker_account_id: int
    taker_account_id: int
    seller_account_id: int
    buyer_account_id: int
    aggressor_side: SideLiteral
    price_fp: int
    qty_fp: int
    notional_fp: int
    exec_ts_ns: int
    prev_interaction_delta_ns: int

    def temporal_decay_ppm(self, ref_ts_ns: int, tau_ns: int) -> int:
        if ref_ts_ns <= self.exec_ts_ns:
            return PPM_SCALE
        decay = math.exp(-(ref_ts_ns - self.exec_ts_ns) / tau_ns)
        return ratio_to_ppm(decay, clamp_unit=True)


@dataclass(frozen=True, slots=True)
class DetectedWashCycle:
    """Forensic proof of a circular wash-trading ring aligned with hyperion.v1.WashTradingCycle."""

    cycle_id: str
    symbol: str
    participant_account_ids: tuple[int, ...]
    cycle_edges: tuple[TradeExecutionEdge, ...]
    cycle_length: int
    circular_volume_fp: int
    circular_notional_fp: int
    net_position_delta_fp: int
    cycle_duration_ns: int
    volume_symmetry_ppm: int
    wash_score: float
    temporal_decay: float
    causal_order_ratio: float
    price_tightness: float
    detected_ts_ns: int
    orientation: str


@dataclass(frozen=True, slots=True)
class SubgraphSnapshot:
    """Extracted ego-subgraph around a target suspect account for SRE MCP & Force Graph UI."""

    center_account_id: int
    symbol: str
    account_ids: tuple[int, ...]
    edges: tuple[TradeExecutionEdge, ...]
    recent_cycles: tuple[DetectedWashCycle, ...]
    snapshot_ts_ns: int


class _DirectedMultigraphIndex:
    """Integer-indexed directed multigraph adjacency structure for sub-millisecond traversal."""

    __slots__ = ("out_neighbors", "in_neighbors", "pair_edges", "last_pair_ts")

    def __init__(self) -> None:
        self.out_neighbors: list[set[int]] = []
        self.in_neighbors: list[set[int]] = []
        self.pair_edges: dict[tuple[int, int], deque[TradeExecutionEdge]] = {}
        self.last_pair_ts: dict[tuple[int, int], int] = {}

    def ensure_node(self, idx: int) -> None:
        while len(self.out_neighbors) <= idx:
            self.out_neighbors.append(set())
            self.in_neighbors.append(set())

    def add_edge(self, u: int, v: int, edge: TradeExecutionEdge) -> None:
        self.ensure_node(max(u, v))
        key = (u, v)
        dq = self.pair_edges.get(key)
        if dq is None:
            dq = deque()
            self.pair_edges[key] = dq
            self.out_neighbors[u].add(v)
            self.in_neighbors[v].add(u)
        dq.append(edge)
        self.last_pair_ts[key] = max(edge.exec_ts_ns, self.last_pair_ts.get(key, 0))

    def pop_oldest_edge(self, u: int, v: int, edge: TradeExecutionEdge) -> None:
        key = (u, v)
        dq = self.pair_edges.get(key)
        if dq is None:
            return
        if dq and dq[0] is edge:
            dq.popleft()
        else:
            try:
                dq.remove(edge)
            except ValueError:
                return
        if not dq:
            del self.pair_edges[key]
            self.last_pair_ts.pop(key, None)
            self.out_neighbors[u].discard(v)
            self.in_neighbors[v].discard(u)


class SlidingTemporalMultigraph:
    """In-memory sliding-window temporal transaction multigraph with k <= 6 cycle detection."""

    def __init__(
        self,
        window_ns: int = 60_000_000_000,
        max_cycle_depth: int = 6,
        decay_tau_ns: int = 15_000_000_000,
        min_wash_score: float = 0.25,
        max_cycles_per_trade: int = 16,
    ) -> None:
        if not (1 <= max_cycle_depth <= 6):
            raise ValueError(f"max_cycle_depth must satisfy 1 <= k <= 6, got {max_cycle_depth}")
        if window_ns <= 0 or decay_tau_ns <= 0:
            raise ValueError("window_ns and decay_tau_ns must be positive integers")

        self.window_ns: int = int(window_ns)
        self.max_cycle_depth: int = int(max_cycle_depth)
        self.decay_tau_ns: int = int(decay_tau_ns)
        self.min_wash_score: float = float(min_wash_score)
        self.max_cycles_per_trade: int = int(max_cycles_per_trade)

        self._acct_to_idx: dict[int, int] = {}
        self._idx_to_acct: list[int] = []

        self._exec_graph = _DirectedMultigraphIndex()
        self._flow_graph = _DirectedMultigraphIndex()

        self._edge_window: deque[tuple[int, int, int, int, int, TradeExecutionEdge]] = deque()
        self._watermark_ns: int = 0
        self._edge_seq: int = 0
        self._cycle_seq: int = 0

        self._acct_counterparties: dict[int, dict[int, int]] = {}
        self._acct_traded_vol_fp: dict[int, int] = {}
        self._acct_wash_state: dict[int, tuple[float, int, int]] = {}
        self._recent_cycles_by_acct: dict[int, deque[DetectedWashCycle]] = {}

    @property
    def active_edge_count(self) -> int:
        return len(self._edge_window)

    @property
    def watermark_ns(self) -> int:
        return self._watermark_ns

    def _intern_account(self, account_id: int) -> int:
        idx = self._acct_to_idx.get(account_id)
        if idx is None:
            idx = len(self._idx_to_acct)
            self._acct_to_idx[account_id] = idx
            self._idx_to_acct.append(account_id)
            self._exec_graph.ensure_node(idx)
            self._flow_graph.ensure_node(idx)
        return idx

    def evict_expired(self, current_watermark_ns: int) -> int:
        """Evict all edges strictly older than current_watermark_ns - window_ns."""
        if current_watermark_ns > self._watermark_ns:
            self._watermark_ns = current_watermark_ns
        cutoff_ns = self._watermark_ns - self.window_ns
        evicted = 0
        while self._edge_window and self._edge_window[0][0] < cutoff_ns:
            _, m_idx, t_idx, s_idx, b_idx, edge = self._edge_window.popleft()
            self._exec_graph.pop_oldest_edge(m_idx, t_idx, edge)
            self._flow_graph.pop_oldest_edge(s_idx, b_idx, edge)
            self._decrement_counterparty(m_idx, t_idx)
            self._decrement_counterparty(t_idx, m_idx)
            prev_m = self._acct_traded_vol_fp.get(m_idx, 0)
            self._acct_traded_vol_fp[m_idx] = max(0, prev_m - edge.qty_fp)
            if t_idx != m_idx:
                prev_t = self._acct_traded_vol_fp.get(t_idx, 0)
                self._acct_traded_vol_fp[t_idx] = max(0, prev_t - edge.qty_fp)
            evicted += 1
        return evicted

    def _increment_counterparty(self, u: int, v: int) -> None:
        cp = self._acct_counterparties.get(u)
        if cp is None:
            cp = {}
            self._acct_counterparties[u] = cp
        cp[v] = cp.get(v, 0) + 1

    def _decrement_counterparty(self, u: int, v: int) -> None:
        cp = self._acct_counterparties.get(u)
        if cp is None:
            return
        cnt = cp.get(v, 0) - 1
        if cnt <= 0:
            cp.pop(v, None)
            if not cp:
                self._acct_counterparties.pop(u, None)
        else:
            cp[v] = cnt

    def ingest_trade(
        self,
        *,
        fill_id: int,
        symbol: str,
        maker_account_id: int,
        taker_account_id: int,
        price_fp: int,
        qty_fp: int,
        exec_ts_ns: int,
        aggressor_side: SideLiteral = "BUY",
        engine_seq: int = 0,
    ) -> tuple[TradeExecutionEdge | None, list[DetectedWashCycle]]:
        """Insert a trade execution edge, evict expired edges, and detect k <= 6 wash cycles."""
        require_uint64(maker_account_id, "maker_account_id", allow_zero=False)
        require_uint64(taker_account_id, "taker_account_id", allow_zero=False)
        require_uint64(price_fp, "price_fp", allow_zero=False)
        require_uint64(qty_fp, "qty_fp", allow_zero=False)
        require_uint64(exec_ts_ns, "exec_ts_ns", allow_zero=True)
        require_uint64(fill_id, "fill_id", allow_zero=True)
        require_uint64(engine_seq, "engine_seq", allow_zero=True)
        if aggressor_side not in ("BUY", "SELL"):
            raise ValueError(f"aggressor_side must be 'BUY' or 'SELL', got {aggressor_side!r}")
        if not symbol:
            raise ValueError("symbol must be non-empty")

        effective_watermark = max(self._watermark_ns, exec_ts_ns)
        if exec_ts_ns < effective_watermark - self.window_ns:
            return None, []

        m_idx = self._intern_account(maker_account_id)
        t_idx = self._intern_account(taker_account_id)
        if aggressor_side == "BUY":
            seller_id, buyer_id = maker_account_id, taker_account_id
            s_idx, b_idx = m_idx, t_idx
        else:
            seller_id, buyer_id = taker_account_id, maker_account_id
            s_idx, b_idx = t_idx, m_idx

        prev_pair_ts = self._exec_graph.last_pair_ts.get((m_idx, t_idx))
        prev_delta_ns = max(0, exec_ts_ns - prev_pair_ts) if prev_pair_ts is not None else 0

        self._edge_seq += 1
        notional_fp = compute_notional_fp(price_fp, qty_fp)
        edge = TradeExecutionEdge(
            edge_id=self._edge_seq,
            fill_id=fill_id,
            engine_seq=engine_seq,
            symbol=symbol,
            maker_account_id=maker_account_id,
            taker_account_id=taker_account_id,
            seller_account_id=seller_id,
            buyer_account_id=buyer_id,
            aggressor_side=aggressor_side,
            price_fp=price_fp,
            qty_fp=qty_fp,
            notional_fp=notional_fp,
            exec_ts_ns=exec_ts_ns,
            prev_interaction_delta_ns=prev_delta_ns,
        )

        if self._edge_window and exec_ts_ns < self._edge_window[-1][0]:
            ins_pos = len(self._edge_window)
            while ins_pos > 0 and self._edge_window[ins_pos - 1][0] > exec_ts_ns:
                ins_pos -= 1
            self._edge_window.insert(ins_pos, (exec_ts_ns, m_idx, t_idx, s_idx, b_idx, edge))
        else:
            self._edge_window.append((exec_ts_ns, m_idx, t_idx, s_idx, b_idx, edge))

        self._exec_graph.add_edge(m_idx, t_idx, edge)
        self._flow_graph.add_edge(s_idx, b_idx, edge)
        self._increment_counterparty(m_idx, t_idx)
        self._increment_counterparty(t_idx, m_idx)
        self._acct_traded_vol_fp[m_idx] = self._acct_traded_vol_fp.get(m_idx, 0) + qty_fp
        if t_idx != m_idx:
            self._acct_traded_vol_fp[t_idx] = self._acct_traded_vol_fp.get(t_idx, 0) + qty_fp

        self.evict_expired(effective_watermark)

        seen_canonical: set[tuple[int, ...]] = set()
        detected: list[DetectedWashCycle] = []

        flow_cycles = self._detect_cycles_on_edge(
            self._flow_graph, s_idx, b_idx, self.max_cycle_depth
        )
        for cyc in flow_cycles:
            canon = self._canonical_cycle_key(cyc)
            if canon in seen_canonical:
                continue
            seen_canonical.add(canon)
            scored = self._score_cycle(self._flow_graph, cyc, edge, "asset_flow")
            if scored is not None and scored.wash_score >= self.min_wash_score:
                detected.append(scored)
                if len(detected) >= self.max_cycles_per_trade:
                    break

        if (m_idx, t_idx) != (s_idx, b_idx) and len(detected) < self.max_cycles_per_trade:
            exec_cycles = self._detect_cycles_on_edge(
                self._exec_graph, m_idx, t_idx, self.max_cycle_depth
            )
            for cyc in exec_cycles:
                canon = self._canonical_cycle_key(cyc)
                if canon in seen_canonical:
                    continue
                seen_canonical.add(canon)
                scored = self._score_cycle(self._exec_graph, cyc, edge, "maker_taker")
                if scored is not None and scored.wash_score >= self.min_wash_score:
                    detected.append(scored)
                    if len(detected) >= self.max_cycles_per_trade:
                        break

        if detected:
            detected.sort(key=lambda c: c.wash_score, reverse=True)
            for cyc_obj in detected:
                for acct_id in cyc_obj.participant_account_ids:
                    a_idx = self._acct_to_idx[acct_id]
                    prev_score, _, prev_ts = self._acct_wash_state.get(a_idx, (0.0, 0, 0))
                    dt_prev = max(0, exec_ts_ns - prev_ts)
                    decayed_prev = prev_score * math.exp(-dt_prev / self.decay_tau_ns)
                    if cyc_obj.wash_score >= decayed_prev:
                        self._acct_wash_state[a_idx] = (
                            cyc_obj.wash_score,
                            cyc_obj.cycle_length,
                            exec_ts_ns,
                        )
                    hist = self._recent_cycles_by_acct.get(a_idx)
                    if hist is None:
                        hist = deque(maxlen=8)
                        self._recent_cycles_by_acct[a_idx] = hist
                    hist.append(cyc_obj)

        return edge, detected

    @staticmethod
    def _canonical_cycle_key(cycle_nodes: tuple[int, ...]) -> tuple[int, ...]:
        if len(cycle_nodes) <= 1:
            return cycle_nodes
        min_pos = min(range(len(cycle_nodes)), key=cycle_nodes.__getitem__)
        return cycle_nodes[min_pos:] + cycle_nodes[:min_pos]

    @staticmethod
    def _tarjan_scc_containing(
        out_neighbors: list[set[int]],
        candidate_nodes: set[int],
        anchor_u: int,
        anchor_v: int,
    ) -> set[int]:
        """Iterative Tarjan's Strongly Connected Components restricted to candidate_nodes."""
        if anchor_u not in candidate_nodes or anchor_v not in candidate_nodes:
            return set()
        index_counter = 0
        indices: dict[int, int] = {}
        lowlink: dict[int, int] = {}
        on_stack: set[int] = set()
        tarjan_stack: list[int] = []

        indices[anchor_u] = index_counter
        lowlink[anchor_u] = index_counter
        index_counter += 1
        tarjan_stack.append(anchor_u)
        on_stack.add(anchor_u)

        call_stack: list[tuple[int, list[int], int]] = [
            (anchor_u, [w for w in out_neighbors[anchor_u] if w in candidate_nodes], 0)
        ]
        while call_stack:
            node, nbrs, pos = call_stack[-1]
            if pos < len(nbrs):
                call_stack[-1] = (node, nbrs, pos + 1)
                w = nbrs[pos]
                if w not in indices:
                    indices[w] = index_counter
                    lowlink[w] = index_counter
                    index_counter += 1
                    tarjan_stack.append(w)
                    on_stack.add(w)
                    nxt_list = [nxt for nxt in out_neighbors[w] if nxt in candidate_nodes]
                    call_stack.append((w, nxt_list, 0))
                elif w in on_stack and indices[w] < lowlink[node]:
                    lowlink[node] = indices[w]
            else:
                call_stack.pop()
                if call_stack:
                    parent = call_stack[-1][0]
                    if lowlink[node] < lowlink[parent]:
                        lowlink[parent] = lowlink[node]
                if lowlink[node] == indices[node]:
                    scc: set[int] = set()
                    while True:
                        member = tarjan_stack.pop()
                        on_stack.discard(member)
                        scc.add(member)
                        if member == node:
                            break
                    if anchor_u in scc and anchor_v in scc:
                        return scc
        return set()

    def _detect_cycles_on_edge(
        self,
        graph: _DirectedMultigraphIndex,
        u: int,
        v: int,
        max_depth: int,
    ) -> list[tuple[int, ...]]:
        """Exact incremental Tarjan-SCC + Depth-Aware Johnson simple cycle search on edge u -> v."""
        if u == v:
            return [(u,)]
        if max_depth < 2:
            return []

        max_rem_hops = max_depth - 1

        # 1a. Backward BFS from u up to max_rem_hops
        dist_to_u: dict[int, int] = {u: 0}
        bfs_q: deque[int] = deque([u])
        while bfs_q:
            curr = bfs_q.popleft()
            d = dist_to_u[curr]
            if d >= max_rem_hops:
                continue
            nd = d + 1
            for pred in graph.in_neighbors[curr]:
                if pred not in dist_to_u:
                    dist_to_u[pred] = nd
                    bfs_q.append(pred)

        if v not in dist_to_u:
            return []

        # 1b. Forward BFS from v pruned by dist_from_v[x] + dist_to_u[x] <= max_rem_hops
        dist_from_v: dict[int, int] = {v: 0}
        fwd_q: deque[int] = deque([v])
        while fwd_q:
            curr = fwd_q.popleft()
            d = dist_from_v[curr]
            if d >= max_rem_hops:
                continue
            nd = d + 1
            for nxt in graph.out_neighbors[curr]:
                if nxt == u:
                    continue
                rem = dist_to_u.get(nxt)
                if rem is not None and nd + rem <= max_rem_hops and nxt not in dist_from_v:
                    dist_from_v[nxt] = nd
                    fwd_q.append(nxt)

        candidate_nodes: set[int] = {u} | set(dist_from_v.keys())

        # 2. Tarjan SCC isolation
        scc_nodes = self._tarjan_scc_containing(graph.out_neighbors, candidate_nodes, u, v)
        if not scc_nodes:
            return []

        scc_adj: dict[int, list[int]] = {
            node: sorted(w for w in graph.out_neighbors[node] if w in scc_nodes)
            for node in scc_nodes
        }

        # 3. Depth-Aware Johnson's circuit enumeration anchored at edge (u -> v)
        blocked_depth: dict[int, int] = {node: INF_DEPTH for node in scc_nodes}
        unblock_map: dict[int, set[int]] = {node: set() for node in scc_nodes}
        on_path: set[int] = {u, v}
        path: list[int] = [u, v]
        cycles: list[tuple[int, ...]] = []

        def unblock(node: int) -> None:
            stack_u = [node]
            while stack_u:
                curr = stack_u.pop()
                if blocked_depth[curr] < INF_DEPTH:
                    blocked_depth[curr] = INF_DEPTH
                    for dep in unblock_map[curr]:
                        if blocked_depth[dep] < INF_DEPTH:
                            stack_u.append(dep)
                    unblock_map[curr].clear()

        def circuit(curr: int, depth: int) -> bool:
            found_cycle = False
            for nxt in scc_adj[curr]:
                if nxt == u:
                    cycles.append(tuple(path))
                    found_cycle = True
                elif nxt not in on_path and depth < max_depth - 1:
                    if depth + 1 + dist_to_u[nxt] > max_depth:
                        continue
                    if depth + 1 >= blocked_depth[nxt]:
                        continue
                    on_path.add(nxt)
                    path.append(nxt)
                    if circuit(nxt, depth + 1):
                        found_cycle = True
                    path.pop()
                    on_path.remove(nxt)

            if found_cycle:
                unblock(curr)
            else:
                if depth < blocked_depth[curr]:
                    blocked_depth[curr] = depth
                for nxt in scc_adj[curr]:
                    if depth + 1 + dist_to_u[nxt] <= max_depth:
                        unblock_map[nxt].add(curr)
            return found_cycle

        circuit(v, 1)
        return cycles

    def _score_cycle(
        self,
        graph: _DirectedMultigraphIndex,
        cycle_nodes: tuple[int, ...],
        trigger_edge: TradeExecutionEdge,
        orientation: str,
    ) -> DetectedWashCycle | None:
        """Select coherent multigraph edges along cycle_nodes and compute forensic wash score."""
        depth = len(cycle_nodes)
        if depth == 1:
            acct_id = self._idx_to_acct[cycle_nodes[0]]
            self._cycle_seq += 1
            return DetectedWashCycle(
                cycle_id=f"WASH-{self._cycle_seq:08d}",
                symbol=trigger_edge.symbol,
                participant_account_ids=(acct_id,),
                cycle_edges=(trigger_edge,),
                cycle_length=1,
                circular_volume_fp=trigger_edge.qty_fp,
                circular_notional_fp=trigger_edge.notional_fp,
                net_position_delta_fp=0,
                cycle_duration_ns=0,
                volume_symmetry_ppm=PPM_SCALE,
                wash_score=1.0,
                temporal_decay=1.0,
                causal_order_ratio=1.0,
                price_tightness=1.0,
                detected_ts_ns=trigger_edge.exec_ts_ns,
                orientation=orientation,
            )

        leg_edges_rev: list[TradeExecutionEdge] = [trigger_edge]
        cursor_ts = trigger_edge.exec_ts_ns
        for idx_pos in range(depth - 1, 0, -1):
            src = cycle_nodes[idx_pos]
            dst = cycle_nodes[(idx_pos + 1) % depth]
            dq = graph.pair_edges.get((src, dst))
            if not dq:
                return None
            best_edge: TradeExecutionEdge | None = None
            for cand in reversed(dq):
                if cand.symbol != trigger_edge.symbol:
                    continue
                if cand.exec_ts_ns <= cursor_ts:
                    best_edge = cand
                    break
            if best_edge is None:
                for cand in reversed(dq):
                    if cand.symbol == trigger_edge.symbol:
                        best_edge = cand
                        break
            if best_edge is None:
                return None
            leg_edges_rev.append(best_edge)
            cursor_ts = best_edge.exec_ts_ns

        chron_edges = tuple(reversed(leg_edges_rev))
        chron_accounts = tuple(
            self._idx_to_acct[cycle_nodes[(i + 1) % depth]] for i in range(depth)
        )

        timestamps = [e.exec_ts_ns for e in chron_edges]
        duration_ns = max(timestamps) - min(timestamps)
        temporal_decay = math.exp(-duration_ns / self.decay_tau_ns)

        causal_steps = sum(
            1
            for i in range(depth - 1)
            if chron_edges[i].exec_ts_ns <= chron_edges[i + 1].exec_ts_ns
        )
        causal_ratio = causal_steps / max(1, depth - 1)

        quantities = [e.qty_fp for e in chron_edges]
        vol_symmetry = min(quantities) / max(quantities)

        inflow: dict[int, int] = {}
        outflow: dict[int, int] = {}
        for e in chron_edges:
            if orientation == "asset_flow":
                src_a, dst_a = e.seller_account_id, e.buyer_account_id
            else:
                src_a, dst_a = e.maker_account_id, e.taker_account_id
            outflow[src_a] = outflow.get(src_a, 0) + e.qty_fp
            inflow[dst_a] = inflow.get(dst_a, 0) + e.qty_fp

        total_qty_fp = sum(quantities)
        net_delta_fp = sum(abs(inflow[a] - outflow[a]) for a in inflow)
        net_retention_score = max(0.0, 1.0 - (net_delta_fp / (2 * total_qty_fp)))

        prices = [e.price_fp for e in chron_edges]
        mean_price = sum(prices) / depth
        rel_price_disp = (max(prices) - min(prices)) / max(1.0, mean_price)
        price_tightness = math.exp(-50.0 * rel_price_disp)

        wash_score = float(
            min(
                1.0,
                temporal_decay
                * (
                    0.35 * vol_symmetry
                    + 0.30 * net_retention_score
                    + 0.20 * causal_ratio
                    + 0.15 * price_tightness
                ),
            )
        )

        self._cycle_seq += 1
        return DetectedWashCycle(
            cycle_id=f"WASH-{self._cycle_seq:08d}",
            symbol=trigger_edge.symbol,
            participant_account_ids=chron_accounts,
            cycle_edges=chron_edges,
            cycle_length=depth,
            circular_volume_fp=total_qty_fp,
            circular_notional_fp=sum(e.notional_fp for e in chron_edges),
            net_position_delta_fp=net_delta_fp,
            cycle_duration_ns=duration_ns,
            volume_symmetry_ppm=ratio_to_ppm(vol_symmetry, clamp_unit=True),
            wash_score=wash_score,
            temporal_decay=temporal_decay,
            causal_order_ratio=causal_ratio,
            price_tightness=price_tightness,
            detected_ts_ns=trigger_edge.exec_ts_ns,
            orientation=orientation,
        )

    def get_account_graph_metrics(
        self, account_id: int, now_ns: int | None = None
    ) -> tuple[float, float, float]:
        """Return (wash_cycle_score, wash_cycle_depth_norm, counterparty_concentration)."""
        idx = self._acct_to_idx.get(account_id)
        if idx is None:
            return 0.0, 0.0, 0.0
        ts_ref = self._watermark_ns if now_ns is None else now_ns

        raw_score, depth, last_ts = self._acct_wash_state.get(idx, (0.0, 0, 0))
        if raw_score > 0.0 and ts_ref > last_ts:
            wash_score = raw_score * math.exp(-(ts_ref - last_ts) / self.decay_tau_ns)
        else:
            wash_score = raw_score
        depth_norm = (depth / float(self.max_cycle_depth)) if wash_score > 0.05 else 0.0

        cp = self._acct_counterparties.get(idx)
        if not cp:
            cp_conc = 0.0
        else:
            total_trades = sum(cp.values())
            hhi = sum((cnt / total_trades) ** 2 for cnt in cp.values())
            sample_conf = min(1.0, total_trades / 6.0)
            cp_conc = float(hhi * sample_conf)

        return float(wash_score), float(depth_norm), cp_conc

    def get_account_traded_volume_fp(self, account_id: int) -> int:
        idx = self._acct_to_idx.get(account_id)
        if idx is None:
            return 0
        return self._acct_traded_vol_fp.get(idx, 0)

    def extract_subgraph(
        self,
        center_account_id: int,
        *,
        symbol: str = "",
        max_hops: int = 2,
        lookback_window_ns: int = 0,
        max_edges: int = 64,
    ) -> SubgraphSnapshot:
        """Extract recent directed ego-subgraph around center_account_id for alerts & SRE MCP."""
        idx = self._acct_to_idx.get(center_account_id)
        if idx is None:
            return SubgraphSnapshot(
                center_account_id=center_account_id,
                symbol=symbol,
                account_ids=(center_account_id,),
                edges=(),
                recent_cycles=(),
                snapshot_ts_ns=self._watermark_ns,
            )

        visited: set[int] = {idx}
        frontier: set[int] = {idx}
        for _ in range(max(1, max_hops)):
            nxt_frontier: set[int] = set()
            for node in frontier:
                nxt_frontier.update(self._flow_graph.out_neighbors[node])
                nxt_frontier.update(self._flow_graph.in_neighbors[node])
            nxt_frontier -= visited
            visited.update(nxt_frontier)
            frontier = nxt_frontier
            if not frontier:
                break

        min_ts = (self._watermark_ns - lookback_window_ns) if lookback_window_ns > 0 else 0
        collected_edges: list[TradeExecutionEdge] = []
        for u in visited:
            for v in self._flow_graph.out_neighbors[u]:
                if v in visited:
                    dq = self._flow_graph.pair_edges.get((u, v))
                    if dq:
                        taken = 0
                        for e in reversed(dq):
                            if symbol and e.symbol != symbol:
                                continue
                            if e.exec_ts_ns < min_ts:
                                break
                            collected_edges.append(e)
                            taken += 1
                            if taken >= max_edges:
                                break

        if len(collected_edges) > max_edges:
            trimmed_edges = tuple(
                heapq.nlargest(max_edges, collected_edges, key=lambda e: e.exec_ts_ns)
            )
        else:
            collected_edges.sort(key=lambda e: e.exec_ts_ns, reverse=True)
            trimmed_edges = tuple(collected_edges)
        recent_cycles = tuple(
            c
            for c in self._recent_cycles_by_acct.get(idx, ())
            if (not symbol or c.symbol == symbol) and c.detected_ts_ns >= min_ts
        )
        sorted_accounts = tuple(sorted(self._idx_to_acct[n] for n in visited))
        return SubgraphSnapshot(
            center_account_id=center_account_id,
            symbol=symbol,
            account_ids=sorted_accounts,
            edges=trimmed_edges,
            recent_cycles=recent_cycles,
            snapshot_ts_ns=self._watermark_ns,
        )

    def enumerate_reference_cycles_on_edge(
        self,
        u_account_id: int,
        v_account_id: int,
        *,
        max_depth: int | None = None,
        orientation: Literal["asset_flow", "maker_taker"] = "asset_flow",
    ) -> set[tuple[int, ...]]:
        """Reference brute-force simple cycle enumerator for Hypothesis property verification."""
        k = self.max_cycle_depth if max_depth is None else max_depth
        u = self._acct_to_idx.get(u_account_id)
        v = self._acct_to_idx.get(v_account_id)
        if u is None or v is None:
            return set()
        graph = self._flow_graph if orientation == "asset_flow" else self._exec_graph
        if v not in graph.out_neighbors[u]:
            return set()
        if u == v:
            return {(u_account_id,)}

        results: set[tuple[int, ...]] = set()
        path = [u, v]
        visited = {u, v}

        def dfs(curr: int, depth: int) -> None:
            for nxt in graph.out_neighbors[curr]:
                if nxt == u:
                    accts = tuple(self._idx_to_acct[n] for n in path)
                    min_i = min(range(len(accts)), key=accts.__getitem__)
                    results.add(accts[min_i:] + accts[:min_i])
                elif nxt not in visited and depth < k - 1:
                    visited.add(nxt)
                    path.append(nxt)
                    dfs(nxt, depth + 1)
                    path.pop()
                    visited.remove(nxt)

        dfs(v, 1)
        return results
