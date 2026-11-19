# ADR-0002: Blake3 Hash-Chained Merkle Write-Ahead Log & Sparse-Indexed Time-Travel Architecture

- **Status:** Accepted
- **Date:** 2026-11-18
- **Deciders:** Staff Event-Sourcing & Deterministic Simulation Lead, Staff Rust Systems Lead, Distinguished Principal Engineer
- **Affected Subsystems:** `/services/ledger-rs`, `/services/chronos-replay`, `/proto/orderbook.proto`, `/proto/chronos.proto`

---

## Context

Institutional exchanges and post-incident forensic teams require two capabilities that conventional CRUD databases cannot satisfy simultaneously at high throughput:

1. **Tamper-Evident Cryptographic Auditability:** Following market turbulence, network faults, or adversarial wash-trading campaigns, operators must prove cryptographically—without trusting mutable database tables—that no account balance leaked a single atomic unit ($10^{-8}$) and that no historical execution record was retroactively altered or omitted.
2. **Nanosecond Time-Travel & Counterfactual Forking ("What-If" Simulation):** Operators in `terminal-web` and automated post-incident verifiers must seek to any historical `engine_seq` or nanosecond timestamp $T$ in milliseconds, inspect the exact L2/L3 order book and account balances at $T$, and fork an isolated timeline over $[T_1, T_2]$ with modified risk/fee policies (e.g., *"What if Aegis froze syndicate account `X` 30 seconds earlier?"*) to compute exact `StateTreeDiffResponse` deltas.

---

## Decision

We co-design the storage and state-commitment layer across `ledger-rs` and `chronos-replay` around three tightly integrated primitives:

### 1. Binary Framed Write-Ahead Log (WAL) with Dual Integrity (`CRC32C` + Chained `Blake3`)
Every deterministic state transition (`engine_seq`) is serialized into an append-only binary WAL segment frame containing:
- Frame magic, length, `engine_seq`, `event_ts_ns`, and hardware-accelerated **`CRC32C` (Castagnoli)** checksum to detect torn writes, bit-rot, or truncated tail frames during crash recovery.
- A **32-byte chained `Blake3` cryptographic digest**:
  $$H_n = \text{BLAKE3}\bigl(H_{n-1} \,\|\, \text{engine\_seq}_n \,\|\, \text{ts\_ns}_n \,\|\, \text{payload}_n \,\|\, \text{state\_root}_n\bigr)$$
  linking every event monotonically back to genesis ($H_0$) so deleting, reordering, or mutating any historical byte invalidates every subsequent hash in the chain.

### 2. Incremental Binary Blake3 Merkle State Tree
- Account balances (`available_fp`, `locked_fp`, `total_fp` per asset) and system treasury pools are committed as leaves of a canonical sorted binary **Blake3 Merkle Tree**.
- `GetMerkleLedgerProof` generates $O(\log N)$ sibling inclusion proofs (`MerkleLedgerProof`) alongside global conservation totals (`system_total_deposits_fp == system_total_user_equity_fp + treasury_fee_pool_fp`), enabling stateless $O(\log N)$ verification by `aegis-agent` (`query_ledger_merkle_audit`) and external auditors.

### 3. Periodic Checkpointed Snapshots + In-Memory Sparse Jump Index for Chronos
- `ledger-rs` periodically writes immutable point-in-time state snapshots (`Snapshot_K` at `engine_seq = K`) containing full arena LOB state, account balances, and `state_root_blake3`.
- `chronos-replay` maintains an in-memory **Sparse Index** mapping `(engine_seq, event_ts_ns) -> (snapshot_id, wal_segment_offset)`.
- To service `TimeTravelSeekRequest(target = T)`:
  1. Locate the nearest snapshot $S_{\le T}$ in $O(\log M)$ via binary search.
  2. Load $S_{\le T}$ into an isolated copy-on-write sandbox state machine.
  3. Fast-forward the sequential WAL delta frames from $S_{\le T}$ to $T$ at $>500,000\text{ events/s}$ while optionally verifying the Blake3 hash chain.
- To service `TimelineForkRequest([T_1, T_2], policy_override)`:
  1. Reconstruct baseline state at $T_1$ once and clone into two isolated in-memory sandboxes (`Baseline` and `Forked`).
  2. Replay the historical command stream in $[T_1, T_2]$ through both sandboxes—applying `SimulationPolicyOverride` (account freeze/throttle at `effective_from_seq`, fee overrides, max order size caps, or `SelfTradePreventionMode` overrides) exclusively in `Forked`.
  3. Compute exact fixed-point deltas (`StateTreeDiffResponse`: `prevented_wash_volume_fp`, `clearing_price_delta_fp`, `account_diffs`, `l2_level_diffs`). When `policy_override` is empty (noop), `baseline_state_root_blake3 == forked_state_root_blake3` holds identically.

```mermaid
sequenceDiagram
    autonumber
    participant Op as Operator / E2E Crucible
    participant Ch as Chronos Replay (Rust)
    participant Snap as Snapshot Store
    participant WAL as Blake3 Chained WAL

    Op->>Ch: TimelineForkRequest([T1, T2], freeze account=666 at T1)
    Ch->>Snap: Load nearest Snapshot S <= T1
    Ch->>WAL: Stream & verify Blake3 frames (S -> T1)
    Note over Ch: Reconstruct exact state at T1 & clone into Baseline + Fork Sandboxes
    Ch->>WAL: Stream historical commands [T1 .. T2]
    par Deterministic Dual Execution (In-Memory)
        Ch->>Ch: Baseline Sandbox replays raw history -> Root_Base(T2)
    and
        Ch->>Ch: Fork Sandbox applies PolicyOverride -> Root_Fork(T2)
    end
    Ch-->>Op: StateTreeDiffResponse (prevented_wash_volume_fp, account_diffs, latency_us)
```

---

## Alternatives Considered

### Alternative A: SHA-256 / Keccak-256 instead of Blake3
- **Pros:** Ubiquitous FIPS/Ethereum ecosystem familiarity.
- **Cons:** `Blake3` is an authenticated Merkle-tree hash structure by design, exploiting AVX2/AVX-512/NEON SIMD parallelism to achieve $4\text{–}8\times$ higher single-core throughput than `SHA-256` in pure safe Rust while maintaining 256-bit cryptographic collision/preimage security.
- **Verdict:** **Rejected.** `Blake3` minimizes cryptographic overhead on the WAL commit path and fast-forward replay loop.

### Alternative B: Temporal SQL Queries (`AS OF SYSTEM TIME` / Event Table Scans in TimescaleDB)
- **Pros:** Reuses existing PostgreSQL/TimescaleDB infrastructure.
- **Cons:** Reconstructing a full price-time-priority L3 order book and running counterfactual matching rules ("what if order X was rejected at $T_1$—how would subsequent market orders sweep different resting price levels at $T_1 + \Delta$?") cannot be expressed as a static SQL projection because order book matching is path-dependent stateful computation.
- **Verdict:** **Rejected.** Only deterministic state-machine replay over raw ingress commands can compute valid counterfactual market trajectories.

### Alternative C: Replaying from Genesis (`seq = 0`) on Every Seek
- **Pros:** Eliminates snapshot serialization code.
- **Cons:** Seeking to event $10,000,000$ would require $20\text{ seconds}$ instead of $<5\text{ milliseconds}$.
- **Verdict:** **Rejected.** Bounded-interval snapshots + sparse WAL indexing bound worst-case seek latency to $O(\Delta_{\text{snapshot}})$.

---

## Consequences

### Positive
- **Zero-Trust Financial Forensics:** Any silent corruption, disk truncation, or unauthorized ledger edit is caught deterministically by `CRC32C` + `Blake3` chain verification and Merkle root comparison.
- **Sub-10ms Interactive Scrubbing & Counterfactual Proof:** Operators can scrub flash crashes at `0.1x` speed or quantify exact wash-trading volume prevented by earlier SRE intervention without touching live production state.

### Negative / Trade-offs
- **Schema Evolution Discipline:** Snapshot binary/proto layouts and deterministic matching logic must be versioned carefully; changing matching tie-break semantics requires retaining versioned replay reducers for historical epochs.

---

## Empirical Validation (Measured Host Evidence)

- **Implementation:** [`services/ledger-rs/src/wal.rs`](../../services/ledger-rs/src/wal.rs) (`HYPWAL01` + `HYPSNP01`), [`services/ledger-rs/src/merkle.rs`](../../services/ledger-rs/src/merkle.rs) (Depth-6 L1d Radix Account Tree + `LtHash` + Left-to-Right Bagged MMR), [`services/chronos-replay/src/wal.rs`](../../services/chronos-replay/src/wal.rs), and [`services/chronos-replay/src/engine.rs`](../../services/chronos-replay/src/engine.rs).
- **3-Pass Fast-Forward Optimization (`2,016` events, bit-identical root `29452dbb3abe45b4...`):** Pass 1 `1,844,912 ns/ev` (`368,420 B/ev`) → Pass 3 (64B Cache-Aligned Sandbox) **`325 ns/ev` p50 / `1,130 ns/ev` p99 / `0 B/ev` (`5,676×` speedup)**.
- **Sustained Replay Throughput:** **`1,544,308 events/sec`** (`51.7 ms` per `79,864` events) with `0 B` steady-state leak across a `500,000`-event soak (`allocs = 0`).
- **Verification Gates:** `32/32` (`100.00%`) mutation kill score; `test_ch02_ledger_snapshot_v1_and_tail_wal_bit_identical_state_root` and `prop_noop_counterfactual_fork_yields_absolute_zero_diff` prove exact bit-for-bit root equivalence.
