# HYPERION OMEGA — Targeted Financial & Cryptographic Mutation Testing Audit (`CH20`)

- **Technique**: `Untch-Offutt-Harrold Mutant Schemata (1x rustc + 26 isolated runs)`
- **Total Mutants Injected**: `26`
- **Mutants Killed**: `26 / 26`
- **Surviving Mutants**: `0`
- **Mutation Kill Score**: **`100.00%`** (Mandatory Gate: `>= 95.00%`)
- **Execution Time**: `123.07s`

| Mutant ID | Target Subsystem / Invariant | Verdict | Time (ms) |
|---|---|---|---:|
| `M01_FP_NOTIONAL_SCALE_SHIFT` | CH01/CH02 Fixed-Point Notional | **KILLED** | `243.0` |
| `M02_FEE_TRUNC_ARITHMETIC` | CH01/CH02 Signed Fee/Rebate | **KILLED** | `230.3` |
| `M03_BYPASS_SELF_TRADE_PREVENTION` | CH02 LOB Self-Trade Prevention | **KILLED** | `331.7` |
| `M04_WEAKEN_FROZEN_ACCOUNT_GATE` | CH02/CH06 Aegis Account Freeze | **KILLED** | `417.9` |
| `M05_TAKER_SOLVENCY_BOUNDARY` | CH02 Pre-Trade Balance Check | **KILLED** | `425.4` |
| `M06_INVERT_MAKER_REBATE_SIGN` | CH02 Maker Rebate Settlement | **KILLED** | `369.8` |
| `M07_DROP_MAKER_FEE_FROM_TREASURY` | CH02 Double-Entry Treasury Pool | **KILLED** | `381.1` |
| `M08_RELAX_DOUBLE_ENTRY_EQUALITY` | CH02/CH11 Conservation Invariant | **KILLED** | `448.7` |
| `M09_IGNORE_WAL_CRC32C_BITROT` | CH02/CH08 HYPWAL01 CRC32C Check | **KILLED** | `487.3` |
| `M10_IGNORE_WAL_SEQ_GAP` | CH02/CH08 WAL Monotonic Sequence | **KILLED** | `610.2` |
| `M11_MINORITY_RAFT_QUORUM_SPLITBRAIN` | CH12 5-Node Raft Quorum Formula | **KILLED** | `568.8` |
| `M12_BYPASS_RAFT_PREVOTE_SHIELD` | CH12 Raft PreVote Protocol | **KILLED** | `499.3` |
| `M13_RAFT_VOTE_STALE_LOG_CANDIDATE` | CH12 Raft Log Completeness | **KILLED** | `429.2` |
| `M14_RAFT_STALE_TERM_NOT_INCREMENTED` | CH12 Raft Election Term Monotonicity | **KILLED** | `411.4` |
| `M15_CROSS_MARGIN_BLINDSPOT` | CH14 Maintenance Margin Gate | **KILLED** | `407.3` |
| `M16_INSURANCE_FUND_SIGN_FLIP` | CH14 Stage-3 Insurance Absorption | **KILLED** | `383.8` |
| `M17_OMIT_STAGE4_QUANTILE_ADL` | CH14 Stage-4 Quantile ADL | **KILLED** | `646.3` |
| `M18_BYPASS_EBPF_CIDR_BLOCKLIST` | CH16 eBPF XDP LPM Blocklist | **KILLED** | `794.1` |
| `M19_EBPF_OFF_BY_ONE_RATE_LIMIT` | CH16 eBPF Token-Bucket Limiter | **KILLED** | `452.6` |
| `M20_EBPF_ACCEPT_BAD_WIRE_MAGIC` | CH16 0x4859 Wire Magic Guard | **KILLED** | `406.4` |
| `M21_DISABLE_WASH_RING_DETECTOR` | CH05 Temporal Multigraph Cycle | **KILLED** | `401.4` |
| `M22_BYPASS_WASM_FUEL_METERING` | CH15 Wasmtime Fuel Exhaustion Trap | **KILLED** | `7181.7` |
| `M23_BYPASS_WASM_16MB_MEMORY_CAP` | CH15 Wasmtime 16MiB Linear Cap | **KILLED** | `5560.2` |
| `M24_ALLOW_NEGATIVE_GHOST_LEAF_MSST` | CH13 ZK Merkle-Sum Range Proof | **KILLED** | `4329.6` |
| `M25_ALLOW_INSOLVENT_VAULT_RESERVES` | CH13/CH18 Solvency Inequality | **KILLED** | `5028.4` |
| `M26_IGNORE_L2_4BLOCK_CHAIN_REORG` | CH18 Chain Relayer Reorg Detector | **KILLED** | `4955.1` |
