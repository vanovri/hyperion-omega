#![deny(unsafe_code)]
#![deny(clippy::float_arithmetic)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use prost::Message;
use serde::{Deserialize, Serialize};

use crate::error::ChronosError;
use crate::fixed::{
    checked_buyer_reserve_fp, checked_fee_fp, checked_notional_fp, checked_slippage_limit_fp,
    compute_slippage_bps, i128_to_pb, pb_to_u128, u128_to_pb, AssetSymbol, InstrumentSpec,
    DEFAULT_MAKER_FEE_PPM, DEFAULT_TAKER_FEE_PPM,
};
use crate::merkle::{
    build_local_subtree_root_and_proof, hash_account_leaf, hash_execution_fill_leaf,
    hash_merkle_branch, AssetBalanceLeaf, ExecutionFillDigestInput, LobStateAccumulator,
    ACCOUNT_BUCKET_COUNT,
};
use crate::pb::{
    market_event_envelope::Payload as EnvelopePayload, time_travel_seek_request::Target,
    AccountBalanceDiff, AccountPolicyOverride, AccountStatus, ExecutionFill,
    L3BookDelta, L3DeltaOp, LedgerEntryLine, LedgerJournalEntry, MarketEventEnvelope, MarketRegime,
    OrderSide, OrderType, PriceLevelDiff, SelfTradePreventionMode, SimulationPolicyOverride,
    StateTreeDiffResponse, TimeTravelSeekRequest, TimeTravelSeekResponse, TimelineForkRequest,
    TraceMetadata,
};
use crate::state::{ExchangeState, ReconstructedLiquidityMetrics, NULL_SLOT};
use crate::wal::{
    encode_snapshot_bytes, parse_and_verify_snapshot, ChronosWalStore, IndexedWalEvent,
    SegmentBacking, SnapshotRestingOrder, VerifiedSnapshot, WalSegmentWriter,
};

const CMD_TAG_PREFIX: &str = "chronos-cmd:";
const WASH_WINDOW_NS: u64 = 60_000_000_000; // 60-second temporal cycle window

/// Canonical deterministic replay command extracted from WAL frames for counterfactual simulation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplayCommand {
    Deposit {
        engine_seq: u64,
        ts_ns: u64,
        account_id: u64,
        asset: String,
        amount_fp: u128,
    },
    SubmitOrder {
        engine_seq: u64,
        ts_ns: u64,
        order_id: u64,
        client_order_id: String,
        account_id: u64,
        symbol: String,
        instrument_id: u32,
        side: u8,       // 1 = BUY, 2 = SELL
        order_type: i32, // OrderType proto enum
        stp_mode: i32,   // SelfTradePreventionMode proto enum
        price_fp: u64,
        qty_fp: u64,
        max_slippage_bps: u32,
        rest_priority_seq: u64,
        first_fill_seq: u64,
        #[serde(default)]
        fill_seqs: Vec<u64>,
        maker_fee_ppm: i64,
        taker_fee_ppm: i64,
    },
    CancelOrder {
        engine_seq: u64,
        ts_ns: u64,
        order_id: u64,
        account_id: u64,
        symbol: String,
    },
    ModifyOrder {
        engine_seq: u64,
        ts_ns: u64,
        order_id: u64,
        account_id: u64,
        symbol: String,
        new_price_fp: u64,
        new_qty_fp: u64,
        priority_seq: u64,
    },
    RiskAction {
        engine_seq: u64,
        ts_ns: u64,
        account_id: u64,
        new_status: i32,
        cancel_open_orders: bool,
        symbol: String,
    },
}

impl ReplayCommand {
    #[inline]
    pub const fn engine_seq(&self) -> u64 {
        match self {
            Self::Deposit { engine_seq, .. }
            | Self::SubmitOrder { engine_seq, .. }
            | Self::CancelOrder { engine_seq, .. }
            | Self::ModifyOrder { engine_seq, .. }
            | Self::RiskAction { engine_seq, .. } => *engine_seq,
        }
    }

    #[inline]
    pub const fn ts_ns(&self) -> u64 {
        match self {
            Self::Deposit { ts_ns, .. }
            | Self::SubmitOrder { ts_ns, .. }
            | Self::CancelOrder { ts_ns, .. }
            | Self::ModifyOrder { ts_ns, .. }
            | Self::RiskAction { ts_ns, .. } => *ts_ns,
        }
    }
}

/// Detailed Seek output combining Protobuf `TimeTravelSeekResponse`, L3 resting book, and liquidity metrics.
#[derive(Debug, Clone)]
pub struct DetailedSeekOutput {
    pub response: TimeTravelSeekResponse,
    pub l3_orders: Vec<SnapshotRestingOrder>,
    pub liquidity: ReconstructedLiquidityMetrics,
}

/// Stored Counterfactual Fork artifact so UI / VCR (`StreamReplayFrames`) can play back a forked timeline!
#[derive(Clone)]
pub struct ForkedTimelineArtifact {
    pub fork_id: String,
    pub fork_name: String,
    pub symbol: String,
    pub start_engine_seq: u64,
    pub end_engine_seq: u64,
    pub base_state_at_start: ExchangeState,
    pub final_state_at_end: ExchangeState,
    pub events: Vec<IndexedWalEvent>,
    pub diff: StateTreeDiffResponse,
}

/// Returns true iff `SimulationPolicyOverride` has zero active overrides (strict noop).
pub fn is_policy_override_noop(ov: Option<&SimulationPolicyOverride>) -> bool {
    let Some(p) = ov else {
        return true;
    };
    let notional_override_zero = pb_to_u128(p.max_order_notional_fp_override) == 0;
    p.maker_fee_ppm_override.is_none()
        && p.taker_fee_ppm_override.is_none()
        && p.max_order_qty_fp_override.is_none()
        && notional_override_zero
        && (p.stp_mode_override == SelfTradePreventionMode::Unspecified as i32)
        && (p.regime_override == MarketRegime::Unspecified as i32)
        && p.volatility_guard_spread_bps.is_none()
        && p.account_overrides.is_empty()
}

/// Thread-safe Chronos Time-Travel & Counterfactual Forking Engine.
pub struct ChronosEngine {
    store: Arc<ChronosWalStore>,
    forks: RwLock<HashMap<String, Arc<ForkedTimelineArtifact>>>,
    fork_counter: std::sync::atomic::AtomicU64,
}

impl ChronosEngine {
    pub fn new(store: ChronosWalStore) -> Self {
        Self {
            store: Arc::new(store),
            forks: RwLock::new(HashMap::new()),
            fork_counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    #[inline]
    pub fn store(&self) -> &Arc<ChronosWalStore> {
        &self.store
    }

    pub fn get_fork(&self, fork_id: &str) -> Option<Arc<ForkedTimelineArtifact>> {
        self.forks
            .read()
            .ok()
            .and_then(|guard| guard.get(fork_id).cloned())
    }

    pub fn list_forks(&self) -> Vec<StateTreeDiffResponse> {
        self.forks
            .read()
            .map(|g| g.values().map(|f| f.diff.clone()).collect())
            .unwrap_or_default()
    }

    /// Reconstructs exact `ExchangeState` at `target_seq` by loading the nearest verified snapshot
    /// `<= target_seq` and fast-forwarding delta events via `SparseWalIndex`.
    pub fn reconstruct_state_at_seq(
        &self,
        target_seq: u64,
    ) -> Result<(ExchangeState, u64, u64), ChronosError> {
        let (mut state, base_snap_seq) =
            if let Some(snap) = self.store.nearest_snapshot_for_seq(target_seq) {
                (ExchangeState::from_snapshot(snap)?, snap.engine_seq)
            } else {
                (ExchangeState::new(), 0)
            };

        let slice = self.store.events_in_seq_range(base_snap_seq, target_seq);
        let mut replayed = 0u64;
        for ev in slice {
            state.apply_wal_envelope(&ev.envelope)?;
            replayed += 1;
        }
        Ok((state, base_snap_seq, replayed))
    }

    /// Executes `TimeTravelSeek` (by `target_engine_seq` or `target_ts_ns`) and returns L2, L3,
    /// Merkle account proofs, liquidity metrics, and optional raw WAL hash-chain verification.
    pub fn seek(&self, req: &TimeTravelSeekRequest) -> Result<DetailedSeekOutput, ChronosError> {
        let t0 = Instant::now();
        let symbol = if req.symbol.is_empty() {
            "BTC-USDT"
        } else {
            req.symbol.as_str()
        };

        let resolved_seq = match req.target {
            Some(Target::TargetEngineSeq(seq)) => seq.min(self.store.max_engine_seq()),
            Some(Target::TargetTsNs(ts_ns)) => self
                .store
                .resolve_seq_at_or_before_ts(ts_ns)
                .map(|(seq, _)| seq)
                .unwrap_or(0),
            None => self.store.max_engine_seq(),
        };

        let wal_verified = if req.verify_wal_hash_chain {
            self.store.verify_raw_chain_up_to(resolved_seq)?;
            true
        } else {
            self.store.is_chain_verified()
        };

        let (mut state, base_snap_seq, replayed_events) =
            self.reconstruct_state_at_seq(resolved_seq)?;
        let state_root = state.compute_state_root();
        let l2_depth = req.l2_depth as usize;
        let l2_snapshot = state.build_l2_snapshot(symbol, l2_depth);
        let l3_orders = state.build_l3_resting_orders(symbol);
        let liquidity = state.compute_liquidity_metrics(symbol);

        let spec = InstrumentSpec::from_symbol(symbol);
        let account_states = if req.include_account_balances {
            state.build_all_account_merkle_proofs(spec.quote_asset.as_str())
        } else {
            Vec::new()
        };

        let resolved_ts_ns = state.ts_ns;
        let latency_us = t0.elapsed().as_micros() as u64;

        Ok(DetailedSeekOutput {
            response: TimeTravelSeekResponse {
                resolved_engine_seq: state.engine_seq,
                resolved_ts_ns,
                base_snapshot_seq: base_snap_seq,
                replayed_wal_events: replayed_events,
                reconstruction_latency_us: latency_us,
                wal_chain_verified: wal_verified,
                state_root_blake3: state_root.to_vec(),
                l2_snapshot: Some(l2_snapshot),
                account_states,
            },
            l3_orders,
            liquidity,
        })
    }

    /// Executes an isolated Counterfactual "What-If" Timeline Fork over `(T1, T2]` without touching
    /// baseline state, computes the high-precision `StateTreeDiffResponse`, and caches the forked
    /// timeline for interactive Split-Screen VCR playback.
    pub fn fork_timeline(
        &self,
        req: &TimelineForkRequest,
    ) -> Result<StateTreeDiffResponse, ChronosError> {
        let t0 = Instant::now();
        let symbol = if req.symbol.is_empty() {
            "BTC-USDT".to_string()
        } else {
            req.symbol.clone()
        };
        let spec = InstrumentSpec::from_symbol(&symbol);

        let max_seq = self.store.max_engine_seq();
        let start_seq = if req.start_engine_seq > 0 {
            req.start_engine_seq.min(max_seq)
        } else if req.start_ts_ns > 0 {
            self.store
                .resolve_seq_at_or_before_ts(req.start_ts_ns)
                .map(|(s, _)| s)
                .unwrap_or(0)
        } else {
            0
        };

        let end_seq = if req.end_engine_seq > 0 {
            req.end_engine_seq.min(max_seq)
        } else if req.end_ts_ns > 0 {
            self.store
                .resolve_seq_at_or_before_ts(req.end_ts_ns)
                .map(|(s, _)| s)
                .unwrap_or(max_seq)
        } else {
            max_seq
        };

        if end_seq < start_seq {
            return Err(ChronosError::InvalidArgument(format!(
                "end_engine_seq ({end_seq}) must be >= start_engine_seq ({start_seq})"
            )));
        }

        // 1. Reconstruct exact state at T1 (`start_seq`) and isolate two independent clones:
        //    - `baseline_state`: replays actual historical WAL events `(start_seq, end_seq]`
        //    - `sandbox_state`: re-executes historical command stream through counterfactual risk & fee sandbox
        let (state_at_t1, _, _) = self.reconstruct_state_at_seq(start_seq)?;
        let base_state_for_vcr = state_at_t1.clone();
        let mut baseline_state = state_at_t1.clone();
        let mut sandbox_state = state_at_t1;
        baseline_state.clear_window_trackers();
        sandbox_state.clear_window_trackers();
        baseline_state.track_window_fills = true;
        sandbox_state.track_window_fills = true;

        let wal_window = self.store.events_in_seq_range(start_seq, end_seq);
        for ev in wal_window {
            baseline_state.apply_wal_envelope(&ev.envelope)?;
        }

        let commands = extract_commands_from_wal_slice(
            wal_window,
            baseline_state.maker_fee_ppm,
            baseline_state.taker_fee_ppm,
        );

        let policy = req.policy_override.as_ref();
        let mut sandbox = CounterfactualSandbox::new(&mut sandbox_state, policy, start_seq);
        for cmd in &commands {
            sandbox.execute_command(cmd)?;
        }
        let rejected_orders_count = sandbox.rejected_orders_count;
        let forked_events = sandbox.emitted_events;

        // 2. Compute cryptographic state roots at T2
        let baseline_root = baseline_state.compute_state_root();
        let forked_root = sandbox_state.compute_state_root();

        // 3. Clearing price, realized volatility, and max slippage deltas
        let baseline_clearing_price_fp = baseline_state
            .find_book(&symbol)
            .map(|b| b.last_clearing_price_fp)
            .unwrap_or(0);
        let forked_clearing_price_fp = sandbox_state
            .find_book(&symbol)
            .map(|b| b.last_clearing_price_fp)
            .unwrap_or(0);
        let clearing_price_delta_fp =
            (forked_clearing_price_fp as i128 - baseline_clearing_price_fp as i128)
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64;

        let baseline_volatility_bps = baseline_state.window_realized_volatility_bps();
        let forked_volatility_bps = sandbox_state.window_realized_volatility_bps();
        let max_slippage_delta_bps = (sandbox_state.window_max_slippage_bps as i64
            - baseline_state.window_max_slippage_bps as i64)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32;

        // 4. Prevented wash-trading & manipulative volume analysis
        let blocked_accounts: HashSet<u64> = policy
            .map(|p| {
                p.account_overrides
                    .iter()
                    .filter(|ao| {
                        ao.status_override == AccountStatus::Frozen as i32
                            || ao.status_override == AccountStatus::Throttled as i32
                            || ao.rate_limit_rps > 0
                    })
                    .map(|ao| ao.account_id)
                    .collect()
            })
            .unwrap_or_default();

        let (prevented_wash_volume_fp, prevented_fills_count) = compute_prevented_wash_metrics(
            &baseline_state.window_fills,
            &sandbox_state.window_fills,
            &blocked_accounts,
        );

        // 5. Per-account multi-asset balance diffs
        let account_diffs = compute_account_balance_diffs(&baseline_state, &sandbox_state);

        // 6. L2 orderbook depth diffs per price tick
        let l2_level_diffs = compute_l2_level_diffs(&baseline_state, &sandbox_state, &symbol);

        // 7. Treasury fee delta
        let base_treasury = baseline_state.quote_treasury_fee_pool(spec.quote_asset.as_str());
        let fork_treasury = sandbox_state.quote_treasury_fee_pool(spec.quote_asset.as_str());
        let treasury_delta = fork_treasury.saturating_sub(base_treasury);

        let all_diffs_zero = clearing_price_delta_fp == 0
            && baseline_volatility_bps == forked_volatility_bps
            && max_slippage_delta_bps == 0
            && prevented_wash_volume_fp == 0
            && prevented_fills_count == 0
            && rejected_orders_count == 0
            && treasury_delta == 0
            && account_diffs.is_empty()
            && l2_level_diffs.is_empty();

        let state_identical = (baseline_root == forked_root) && all_diffs_zero;

        let seq_num = self
            .fork_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let fork_id = if req.fork_name.is_empty() {
            format!("fork-{seq_num:06}")
        } else {
            let slug: String = req
                .fork_name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            format!("{slug}-{seq_num:04}")
        };

        let computation_latency_us = t0.elapsed().as_micros() as u64;
        let diff = StateTreeDiffResponse {
            fork_id: fork_id.clone(),
            symbol: symbol.clone(),
            start_engine_seq: start_seq,
            end_engine_seq: end_seq,
            baseline_state_root_blake3: baseline_root.to_vec(),
            forked_state_root_blake3: forked_root.to_vec(),
            state_identical,
            baseline_clearing_price_fp,
            forked_clearing_price_fp,
            clearing_price_delta_fp,
            baseline_volatility_bps,
            forked_volatility_bps,
            max_slippage_delta_bps,
            prevented_wash_volume_fp: Some(u128_to_pb(prevented_wash_volume_fp)),
            prevented_fills_count,
            rejected_orders_count,
            treasury_fee_delta_fp: Some(i128_to_pb(treasury_delta)),
            account_diffs,
            l2_level_diffs,
            replayed_events_count: wal_window.len() as u64,
            computation_latency_us,
        };

        if let Ok(mut guard) = self.forks.write() {
            guard.insert(
                fork_id.clone(),
                Arc::new(ForkedTimelineArtifact {
                    fork_id,
                    fork_name: req.fork_name.clone(),
                    symbol,
                    start_engine_seq: start_seq,
                    end_engine_seq: end_seq,
                    base_state_at_start: base_state_for_vcr,
                    final_state_at_end: sandbox_state,
                    events: forked_events,
                    diff: diff.clone(),
                }),
            );
        }

        Ok(diff)
    }
}

// =============================================================================
// COUNTERFACTUAL SANDBOX MATCHING & RISK FILTER ENGINE
// =============================================================================

struct ActiveAccountRule {
    override_cfg: AccountPolicyOverride,
    activated: bool,
}

struct CounterfactualSandbox<'a> {
    state: &'a mut ExchangeState,
    policy: Option<&'a SimulationPolicyOverride>,
    fork_start_seq: u64,
    account_rules: HashMap<u64, ActiveAccountRule>,
    next_synth_seq: u64,
    pub rejected_orders_count: u64,
    pub emitted_events: Vec<IndexedWalEvent>,
    prev_blake3: [u8; 32],
}

impl<'a> CounterfactualSandbox<'a> {
    fn new(
        state: &'a mut ExchangeState,
        policy: Option<&'a SimulationPolicyOverride>,
        fork_start_seq: u64,
    ) -> Self {
        let mut account_rules = HashMap::new();
        if let Some(p) = policy {
            for ao in &p.account_overrides {
                account_rules.insert(
                    ao.account_id,
                    ActiveAccountRule {
                        override_cfg: *ao,
                        activated: false,
                    },
                );
            }
        }
        Self {
            next_synth_seq: state.engine_seq.saturating_add(1),
            state,
            policy,
            fork_start_seq,
            account_rules,
            rejected_orders_count: 0,
            emitted_events: Vec::new(),
            prev_blake3: [0u8; 32],
        }
    }

    fn activate_due_account_overrides(&mut self, cur_seq: u64, cur_ts_ns: u64) -> Result<(), ChronosError> {
        let mut to_cancel_accounts: Vec<u64> = Vec::new();
        let mut status_updates: Vec<(u64, i32)> = Vec::new();

        for (&account_id, rule) in &mut self.account_rules {
            if rule.activated {
                continue;
            }
            let eff_seq = if rule.override_cfg.effective_from_seq == 0 {
                self.fork_start_seq
            } else {
                rule.override_cfg.effective_from_seq
            };
            let seq_due = cur_seq >= eff_seq;
            let ts_due = rule.override_cfg.effective_from_ts_ns == 0
                || cur_ts_ns >= rule.override_cfg.effective_from_ts_ns;

            if seq_due && ts_due {
                rule.activated = true;
                if rule.override_cfg.status_override != AccountStatus::Unspecified as i32 {
                    status_updates.push((account_id, rule.override_cfg.status_override));
                }
                if rule.override_cfg.cancel_resting_orders_on_activation {
                    to_cancel_accounts.push(account_id);
                }
            }
        }

        for (account_id, st) in status_updates {
            let idx = self.state.ensure_account(account_id);
            self.state.accounts[idx as usize].status = st;
        }
        for account_id in to_cancel_accounts {
            self.cancel_all_orders_for_account(account_id, cur_ts_ns)?;
        }
        Ok(())
    }

    fn cancel_all_orders_for_account(&mut self, account_id: u64, ts_ns: u64) -> Result<(), ChronosError> {
        let order_ids: Vec<u64> = self
            .state
            .hot_orders
            .iter()
            .filter(|s| s.active != 0 && s.account_id == account_id && s.remaining_qty_fp > 0)
            .map(|s| s.order_id)
            .collect();

        for oid in order_ids {
            if let Some((hot, cold)) = self.state.remove_resting_order_by_id(oid) {
                let spec = InstrumentSpec::from_symbol(cold.symbol.as_str());
                let unlock_asset = if hot.side == 1 {
                    spec.quote_asset
                } else {
                    spec.base_asset
                };
                if cold.locked_collateral_fp > 0 {
                    self.state
                        .unlock_funds(hot.account_id, unlock_asset, cold.locked_collateral_fp)?;
                }
                self.emit_vcr_event(
                    ts_ns,
                    cold.symbol.as_str(),
                    EnvelopePayload::L3Delta(L3BookDelta {
                        engine_seq: 0,
                        prev_engine_seq: 0,
                        symbol: cold.symbol.as_str().to_string(),
                        instrument_id: hot.instrument_id,
                        op: L3DeltaOp::Cancel as i32,
                        side: if hot.side == 2 {
                            OrderSide::Sell as i32
                        } else {
                            OrderSide::Buy as i32
                        },
                        order_id: oid,
                        account_id,
                        price_fp: hot.price_fp,
                        remaining_qty_fp: 0,
                        delta_qty_fp: -(hot.remaining_qty_fp as i64),
                        event_ts_ns: ts_ns,
                        state_root_blake3: Vec::new(),
                        trace: None,
                    }),
                );
            }
        }
        Ok(())
    }

    fn emit_vcr_event(&mut self, ts_ns: u64, symbol: &str, payload: EnvelopePayload) {
        let seq = self.next_synth_seq;
        self.next_synth_seq += 1;
        let mut env = MarketEventEnvelope {
            engine_seq: seq,
            symbol: symbol.to_string(),
            ts_ns,
            payload: Some(payload),
        };
        if let Some(EnvelopePayload::L3Delta(ref mut d)) = env.payload {
            if d.engine_seq == 0 {
                d.engine_seq = seq;
            }
        }
        let encoded = env.encode_to_vec();
        let frame_hash = crate::wal::compute_wal_frame_blake3(seq, ts_ns, &self.prev_blake3, &encoded);
        let prev = self.prev_blake3;
        self.prev_blake3 = frame_hash;
        self.emitted_events.push(IndexedWalEvent {
            engine_seq: seq,
            ts_ns,
            prev_blake3: prev,
            frame_blake3: frame_hash,
            envelope: env,
        });
    }

    fn execute_command(&mut self, cmd: &ReplayCommand) -> Result<(), ChronosError> {
        let cur_seq = cmd.engine_seq();
        let cur_ts = cmd.ts_ns();
        self.state.engine_seq = cur_seq;
        self.state.ts_ns = cur_ts;

        self.activate_due_account_overrides(cur_seq, cur_ts)?;

        match cmd {
            ReplayCommand::Deposit {
                account_id,
                asset,
                amount_fp,
                ts_ns,
                ..
            } => {
                self.state.credit_deposit(*account_id, asset, *amount_fp)?;
                let _ = ts_ns;
            }
            ReplayCommand::RiskAction {
                account_id,
                new_status,
                cancel_open_orders,
                ts_ns,
                ..
            } => {
                let idx = self.state.ensure_account(*account_id);
                if *new_status != AccountStatus::Unspecified as i32 {
                    self.state.accounts[idx as usize].status = *new_status;
                }
                if *cancel_open_orders {
                    self.cancel_all_orders_for_account(*account_id, *ts_ns)?;
                }
            }
            ReplayCommand::CancelOrder {
                order_id, ts_ns, ..
            } => {
                if let Some((hot, cold)) = self.state.remove_resting_order_by_id(*order_id) {
                    let spec = InstrumentSpec::from_symbol(cold.symbol.as_str());
                    let unlock_asset = if hot.side == 1 {
                        spec.quote_asset
                    } else {
                        spec.base_asset
                    };
                    if cold.locked_collateral_fp > 0 {
                        self.state.unlock_funds(
                            hot.account_id,
                            unlock_asset,
                            cold.locked_collateral_fp,
                        )?;
                    }
                    self.emit_vcr_event(
                        *ts_ns,
                        cold.symbol.as_str(),
                        EnvelopePayload::L3Delta(L3BookDelta {
                            engine_seq: cur_seq,
                            prev_engine_seq: 0,
                            symbol: cold.symbol.as_str().to_string(),
                            instrument_id: hot.instrument_id,
                            op: L3DeltaOp::Cancel as i32,
                            side: if hot.side == 2 {
                                OrderSide::Sell as i32
                            } else {
                                OrderSide::Buy as i32
                            },
                            order_id: *order_id,
                            account_id: hot.account_id,
                            price_fp: hot.price_fp,
                            remaining_qty_fp: 0,
                            delta_qty_fp: -(hot.remaining_qty_fp as i64),
                            event_ts_ns: *ts_ns,
                            state_root_blake3: Vec::new(),
                            trace: None,
                        }),
                    );
                }
            }
            ReplayCommand::ModifyOrder {
                order_id,
                account_id,
                symbol,
                new_price_fp,
                new_qty_fp,
                priority_seq,
                ts_ns,
                ..
            } => {
                let acc_idx = self.state.ensure_account(*account_id);
                if self.state.accounts[acc_idx as usize].status == AccountStatus::Frozen as i32 {
                    self.rejected_orders_count += 1;
                    return Ok(());
                }
                if let Some( max_qty ) = self.policy.and_then(|p| p.max_order_qty_fp_override) {
                    if *new_qty_fp > max_qty {
                        self.rejected_orders_count += 1;
                        return Ok(());
                    }
                }
                if let Some((hot, cold)) = self.state.remove_resting_order_by_id(*order_id) {
                    let spec = InstrumentSpec::from_symbol(symbol);
                    let unlock_asset = if hot.side == 1 {
                        spec.quote_asset
                    } else {
                        spec.base_asset
                    };
                    if cold.locked_collateral_fp > 0 {
                        self.state
                            .unlock_funds(hot.account_id, unlock_asset, cold.locked_collateral_fp)?;
                    }
                    if *new_qty_fp > 0 {
                        let maker_ppm = self
                            .policy
                            .and_then(|p| p.maker_fee_ppm_override)
                            .unwrap_or(self.state.maker_fee_ppm);
                        let taker_ppm = self
                            .policy
                            .and_then(|p| p.taker_fee_ppm_override)
                            .unwrap_or(self.state.taker_fee_ppm);
                        let new_lock = if hot.side == 1 {
                            checked_buyer_reserve_fp(*new_price_fp, *new_qty_fp, maker_ppm, taker_ppm)?
                        } else {
                            *new_qty_fp as i128
                        };
                        if !self.state.lock_funds(*account_id, unlock_asset, new_lock)? {
                            self.rejected_orders_count += 1;
                            return Ok(());
                        }
                        let eff_prio = if *new_price_fp == hot.price_fp
                            && *new_qty_fp <= hot.remaining_qty_fp
                        {
                            hot.priority_seq
                        } else {
                            *priority_seq
                        };
                        self.state.insert_resting_order_raw(
                            *order_id,
                            cold.client_order_id.as_str(),
                            hot.instrument_id,
                            symbol,
                            *account_id,
                            hot.side,
                            *new_price_fp,
                            *new_qty_fp,
                            new_lock,
                            eff_prio,
                            *ts_ns,
                        );
                    }
                }
            }
            ReplayCommand::SubmitOrder {
                ts_ns,
                order_id,
                client_order_id,
                account_id,
                symbol,
                instrument_id,
                side,
                order_type,
                stp_mode,
                price_fp,
                qty_fp,
                max_slippage_bps,
                rest_priority_seq,
                first_fill_seq,
                fill_seqs,
                maker_fee_ppm: cmd_maker_ppm,
                taker_fee_ppm: cmd_taker_ppm,
                ..
            } => {
                self.execute_sandbox_submit(
                    *ts_ns,
                    *order_id,
                    client_order_id,
                    *account_id,
                    symbol,
                    *instrument_id,
                    *side,
                    *order_type,
                    *stp_mode,
                    *price_fp,
                    *qty_fp,
                    *max_slippage_bps,
                    *rest_priority_seq,
                    *first_fill_seq,
                    fill_seqs,
                    *cmd_maker_ppm,
                    *cmd_taker_ppm,
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_sandbox_submit(
        &mut self,
        ts_ns: u64,
        order_id: u64,
        client_order_id: &str,
        account_id: u64,
        symbol: &str,
        instrument_id: u32,
        side: u8,
        order_type: i32,
        stp_mode: i32,
        price_fp: u64,
        qty_fp: u64,
        max_slippage_bps: u32,
        rest_priority_seq: u64,
        first_fill_seq: u64,
        fill_seqs: &[u64],
        cmd_maker_ppm: i64,
        cmd_taker_ppm: i64,
    ) -> Result<(), ChronosError> {
        if qty_fp == 0 {
            self.rejected_orders_count += 1;
            return Ok(());
        }

        // 1. Policy Gate: Market Regime & Account Risk Status & Rate Limit
        let regime = self
            .policy
            .map(|p| p.regime_override)
            .unwrap_or(MarketRegime::Unspecified as i32);
        if regime == MarketRegime::Halted as i32 {
            self.rejected_orders_count += 1;
            return Ok(());
        }

        let acc_idx = self.state.ensure_account(account_id);
        let acc_status = self.state.accounts[acc_idx as usize].status;
        if acc_status == AccountStatus::Frozen as i32 {
            self.rejected_orders_count += 1;
            return Ok(());
        }

        if let Some(rule) = self.account_rules.get(&account_id) {
            if rule.activated && rule.override_cfg.rate_limit_rps > 0 {
                let sec_bucket = ts_ns / 1_000_000_000;
                let acc = &mut self.state.accounts[acc_idx as usize];
                if acc.rate_window_sec != sec_bucket {
                    acc.rate_window_sec = sec_bucket;
                    acc.rate_window_count = 0;
                }
                if acc.rate_window_count >= rule.override_cfg.rate_limit_rps {
                    self.rejected_orders_count += 1;
                    return Ok(());
                }
                acc.rate_window_count += 1;
            }
        }

        // 2. Policy Gate: Order Quantity & Notional Caps
        if let Some(max_qty) = self.policy.and_then(|p| p.max_order_qty_fp_override) {
            if qty_fp > max_qty {
                self.rejected_orders_count += 1;
                return Ok(());
            }
        }

        let b_idx = self.state.ensure_book(symbol);
        let spec = self.state.books[b_idx].spec.clone();
        let ref_book_px = if side == 1 {
            self.state.books[b_idx].best_ask_fp()
        } else {
            self.state.books[b_idx].best_bid_fp()
        };
        let effective_px_for_notional = if price_fp > 0 { price_fp } else { ref_book_px };

        if let Some(p) = self.policy {
            let max_notional = pb_to_u128(p.max_order_notional_fp_override);
            if max_notional > 0 && effective_px_for_notional > 0 {
                let notional = checked_notional_fp(effective_px_for_notional, qty_fp)?;
                if notional > max_notional {
                    self.rejected_orders_count += 1;
                    return Ok(());
                }
            }
        }

        // 3. Policy Gate: Volatility Guard & Post-Only Auction
        let spread_bps = self.state.books[b_idx].spread_bps();
        let vol_guard_triggered = regime == MarketRegime::PostOnlyAuction as i32
            || (regime == MarketRegime::VolatilityGuard as i32 && spread_bps > 50)
            || self
                .policy
                .and_then(|p| p.volatility_guard_spread_bps)
                .map(|max_sp| spread_bps > max_sp)
                .unwrap_or(false);

        let eff_order_type = if vol_guard_triggered && order_type != OrderType::PostOnly as i32 {
            if order_type == OrderType::Market as i32
                || order_type == OrderType::Ioc as i32
                || order_type == OrderType::Fok as i32
            {
                self.rejected_orders_count += 1;
                return Ok(());
            }
            OrderType::PostOnly as i32
        } else {
            order_type
        };

        let eff_stp = match self.policy.map(|p| p.stp_mode_override).unwrap_or(0) {
            x if x != SelfTradePreventionMode::Unspecified as i32 => x,
            _ => stp_mode,
        };

        let maker_ppm = self
            .policy
            .and_then(|p| p.maker_fee_ppm_override)
            .unwrap_or(cmd_maker_ppm);
        let taker_ppm = self
            .policy
            .and_then(|p| p.taker_fee_ppm_override)
            .unwrap_or(cmd_taker_ppm);

        // Determine effective limit price
        let is_buy = side == 1;
        let limit_px = if eff_order_type == OrderType::Market as i32 {
            if ref_book_px == 0 {
                return Ok(());
            }
            if max_slippage_bps > 0 {
                checked_slippage_limit_fp(ref_book_px, max_slippage_bps, is_buy)?
            } else if is_buy {
                u64::MAX / 2
            } else {
                1
            }
        } else {
            price_fp
        };

        // Post-Only check
        if eff_order_type == OrderType::PostOnly as i32 {
            let would_cross = if is_buy {
                let best_ask = self.state.books[b_idx].best_ask_fp();
                best_ask > 0 && limit_px >= best_ask
            } else {
                let best_bid = self.state.books[b_idx].best_bid_fp();
                best_bid > 0 && limit_px <= best_bid
            };
            if would_cross {
                self.rejected_orders_count += 1;
                return Ok(());
            }
        }

        // FOK pre-flight liquidity check
        if eff_order_type == OrderType::Fok as i32 {
            let mut avail_liq: u128 = 0;
            let opp_levels = if is_buy {
                &self.state.books[b_idx].asks
            } else {
                &self.state.books[b_idx].bids
            };
            for lvl in opp_levels.iter().rev() {
                let crosses = if is_buy {
                    lvl.price_fp <= limit_px
                } else {
                    lvl.price_fp >= limit_px
                };
                if !crosses {
                    break;
                }
                let mut cur = lvl.head_slot;
                while cur != NULL_SLOT {
                    let slot = &self.state.hot_orders[cur as usize];
                    if !(eff_stp != SelfTradePreventionMode::None as i32
                        && eff_stp != SelfTradePreventionMode::Unspecified as i32
                        && slot.account_id == account_id)
                    {
                        avail_liq += slot.remaining_qty_fp as u128;
                    }
                    cur = slot.next_slot;
                }
                if avail_liq >= qty_fp as u128 {
                    break;
                }
            }
            if avail_liq < qty_fp as u128 {
                self.rejected_orders_count += 1;
                return Ok(());
            }
        }

        // Lock upfront collateral for Limit/PostOnly/IOC/FOK/Market
        let lock_price_cap = if eff_order_type == OrderType::Market as i32 {
            // For pure market buy without explicit price_fp, cap by available balance during sweep
            price_fp
        } else {
            limit_px
        };

        let mut remaining_locked_fp: i128 = 0;
        if is_buy {
            if lock_price_cap > 0 {
                let needed =
                    checked_buyer_reserve_fp(lock_price_cap, qty_fp, maker_ppm, taker_ppm)?;
                if !self.state.lock_funds(account_id, spec.quote_asset, needed)? {
                    self.rejected_orders_count += 1;
                    return Ok(());
                }
                remaining_locked_fp = needed;
            }
        } else {
            let needed = qty_fp as i128;
            if !self.state.lock_funds(account_id, spec.base_asset, needed)? {
                self.rejected_orders_count += 1;
                return Ok(());
            }
            remaining_locked_fp = needed;
        }

        // 4. Price-Time Priority Matching Loop
        let mut rem_qty = qty_fp;
        let mut fill_idx = 0usize;
        let mut fill_seq_cursor = if first_fill_seq > 0 {
            first_fill_seq
        } else {
            self.state.engine_seq
        };

        while rem_qty > 0 {
            let best_maker_slot = {
                let opp_levels = if is_buy {
                    &self.state.books[b_idx].asks
                } else {
                    &self.state.books[b_idx].bids
                };
                let Some(best_lvl) = opp_levels.last() else {
                    break;
                };
                let crosses = if is_buy {
                    best_lvl.price_fp <= limit_px
                } else {
                    best_lvl.price_fp >= limit_px
                };
                if !crosses {
                    break;
                }
                best_lvl.head_slot
            };

            if best_maker_slot == NULL_SLOT {
                break;
            }

            let maker_hot = self.state.hot_orders[best_maker_slot as usize];
            let maker_cold = self.state.cold_orders[best_maker_slot as usize];

            // Self-Trade Prevention (STP) check
            if maker_hot.account_id == account_id {
                match SelfTradePreventionMode::try_from(eff_stp)
                    .unwrap_or(SelfTradePreventionMode::Unspecified)
                {
                    SelfTradePreventionMode::CancelResting => {
                        self.state.remove_resting_order_by_id(maker_hot.order_id);
                        let unlock_asset = if maker_hot.side == 1 {
                            spec.quote_asset
                        } else {
                            spec.base_asset
                        };
                        if maker_cold.locked_collateral_fp > 0 {
                            self.state.unlock_funds(
                                maker_hot.account_id,
                                unlock_asset,
                                maker_cold.locked_collateral_fp,
                            )?;
                        }
                        continue;
                    }
                    SelfTradePreventionMode::CancelAggressing => {
                        rem_qty = 0;
                        break;
                    }
                    SelfTradePreventionMode::CancelBoth => {
                        self.state.remove_resting_order_by_id(maker_hot.order_id);
                        let unlock_asset = if maker_hot.side == 1 {
                            spec.quote_asset
                        } else {
                            spec.base_asset
                        };
                        if maker_cold.locked_collateral_fp > 0 {
                            self.state.unlock_funds(
                                maker_hot.account_id,
                                unlock_asset,
                                maker_cold.locked_collateral_fp,
                            )?;
                        }
                        rem_qty = 0;
                        break;
                    }
                    _ => {}
                }
            }

            let match_qty = rem_qty.min(maker_hot.remaining_qty_fp);
            let exec_px = maker_hot.price_fp;
            let notional = checked_notional_fp(exec_px, match_qty)?;
            let maker_fee = checked_fee_fp(notional, maker_ppm)?;
            let taker_fee = checked_fee_fp(notional, taker_ppm)?;

            // Check pure-market buyer affordability if no upfront lock was taken
            if is_buy && lock_price_cap == 0 {
                let req_quote = (notional as i128) + (taker_fee as i128);
                let (avail_q, _) =
                    self.state.accounts[acc_idx as usize].get_asset(spec.quote_asset);
                if avail_q < req_quote {
                    break;
                }
            }

            // Exact collateral release preserving exact `checked_buyer_reserve_fp` on resting remainder!
            let new_maker_rem = maker_hot.remaining_qty_fp - match_qty;
            let maker_locked_used = if new_maker_rem == 0 {
                maker_cold.locked_collateral_fp
            } else if maker_hot.side == 1 {
                let keep = checked_buyer_reserve_fp(
                    maker_hot.price_fp,
                    new_maker_rem,
                    maker_ppm,
                    taker_ppm,
                )?;
                maker_cold.locked_collateral_fp.saturating_sub(keep)
            } else {
                match_qty as i128
            };

            let new_taker_rem = rem_qty - match_qty;
            let taker_locked_used = if remaining_locked_fp > 0 {
                if new_taker_rem == 0 {
                    remaining_locked_fp
                } else if is_buy {
                    let keep = checked_buyer_reserve_fp(
                        lock_price_cap,
                        new_taker_rem,
                        maker_ppm,
                        taker_ppm,
                    )?;
                    remaining_locked_fp.saturating_sub(keep)
                } else {
                    match_qty as i128
                }
            } else {
                0
            };

            if is_buy {
                self.state.settle_trade_fill(
                    spec.base_asset,
                    spec.quote_asset,
                    account_id,
                    maker_hot.account_id,
                    taker_locked_used,
                    maker_locked_used,
                    match_qty,
                    notional,
                    taker_fee,
                    maker_fee,
                )?;
            } else {
                self.state.settle_trade_fill(
                    spec.base_asset,
                    spec.quote_asset,
                    maker_hot.account_id,
                    account_id,
                    maker_locked_used,
                    taker_locked_used,
                    match_qty,
                    notional,
                    maker_fee,
                    taker_fee,
                )?;
            }

            remaining_locked_fp -= taker_locked_used;
            rem_qty -= match_qty;

            // Update or remove resting maker order
            let new_maker_rem = maker_hot.remaining_qty_fp - match_qty;
            if new_maker_rem == 0 {
                self.state.remove_resting_order_by_id(maker_hot.order_id);
            } else {
                self.state.hot_orders[best_maker_slot as usize].remaining_qty_fp = new_maker_rem;
                self.state.cold_orders[best_maker_slot as usize].locked_collateral_fp -=
                    maker_locked_used;
                let opp_levels = if is_buy {
                    &mut self.state.books[b_idx].asks
                } else {
                    &mut self.state.books[b_idx].bids
                };
                if let Some(best_lvl) = opp_levels.last_mut() {
                    best_lvl.total_qty_fp = best_lvl.total_qty_fp.saturating_sub(match_qty);
                }
            }

            // Record execution fill in state & Merkle MMR
            let fill_id = self.state.next_fill_id;
            self.state.next_fill_id = fill_id.saturating_add(1);
            let eff_inst_id = if instrument_id != 0 {
                instrument_id
            } else {
                spec.instrument_id
            };

            let ref_for_slip = self.state.books[b_idx].last_clearing_price_fp;
            if ref_for_slip > 0 {
                let slip = compute_slippage_bps(ref_for_slip, exec_px, is_buy);
                if slip > self.state.window_max_slippage_bps {
                    self.state.window_max_slippage_bps = slip;
                }
            }
            self.state.books[b_idx].last_clearing_price_fp = exec_px;
            self.state.books[b_idx].cumulative_base_vol_fp = self.state.books[b_idx]
                .cumulative_base_vol_fp
                .saturating_add(match_qty as u128);
            self.state.books[b_idx].cumulative_quote_vol_fp = self.state.books[b_idx]
                .cumulative_quote_vol_fp
                .saturating_add(notional);
            self.state.window_fill_prices.push(exec_px);

            let cur_fill_seq = fill_seqs
                .get(fill_idx)
                .copied()
                .filter(|&s| s > 0)
                .unwrap_or(fill_seq_cursor);
            fill_idx += 1;

            let fill_pb = ExecutionFill {
                fill_id,
                engine_seq: cur_fill_seq,
                symbol: symbol.to_string(),
                instrument_id: eff_inst_id,
                maker_order_id: maker_hot.order_id,
                taker_order_id: order_id,
                maker_account_id: maker_hot.account_id,
                taker_account_id: account_id,
                aggressor_side: if is_buy {
                    OrderSide::Buy as i32
                } else {
                    OrderSide::Sell as i32
                },
                price_fp: exec_px,
                qty_fp: match_qty,
                notional_fp: Some(u128_to_pb(notional)),
                maker_fee_fp: maker_fee,
                taker_fee_fp: taker_fee,
                exec_ts_ns: ts_ns,
                wal_blake3_hash: Vec::new(),
                trace: None,
            };
            self.state.window_fills.push(fill_pb.clone());

            let leaf = hash_execution_fill_leaf(&ExecutionFillDigestInput {
                fill_id,
                engine_seq: cur_fill_seq,
                instrument_id: eff_inst_id,
                maker_order_id: maker_hot.order_id,
                taker_order_id: order_id,
                maker_account_id: maker_hot.account_id,
                taker_account_id: account_id,
                aggressor_side: side,
                price_fp: exec_px,
                qty_fp: match_qty,
                notional_fp: notional,
                maker_fee_fp: maker_fee,
                taker_fee_fp: taker_fee,
                exec_ts_ns: ts_ns,
            });
            self.state.merkle.append_execution_leaf(leaf);

            self.emit_vcr_event(ts_ns, symbol, EnvelopePayload::Fill(fill_pb));
            self.emit_vcr_event(
                ts_ns,
                symbol,
                EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: cur_fill_seq,
                    prev_engine_seq: 0,
                    symbol: symbol.to_string(),
                    instrument_id: eff_inst_id,
                    op: L3DeltaOp::Fill as i32,
                    side: if maker_hot.side == 2 {
                        OrderSide::Sell as i32
                    } else {
                        OrderSide::Buy as i32
                    },
                    order_id: maker_hot.order_id,
                    account_id: maker_hot.account_id,
                    price_fp: exec_px,
                    remaining_qty_fp: new_maker_rem,
                    delta_qty_fp: -(match_qty as i64),
                    event_ts_ns: ts_ns,
                    state_root_blake3: Vec::new(),
                    trace: None,
                }),
            );
            fill_seq_cursor = fill_seq_cursor.saturating_add(1);
        }

        // 5. Rest unfilled quantity (for LIMIT / POST_ONLY) or refund unused collateral (for IOC / MARKET)
        let rests_on_book = rem_qty > 0
            && (eff_order_type == OrderType::Limit as i32
                || eff_order_type == OrderType::PostOnly as i32
                || eff_order_type == OrderType::Unspecified as i32);

        if rests_on_book && limit_px > 0 {
            let eff_prio = if rest_priority_seq > 0 {
                rest_priority_seq
            } else {
                self.state.engine_seq
            };
            let eff_inst_id = if instrument_id != 0 {
                instrument_id
            } else {
                spec.instrument_id
            };
            self.state.insert_resting_order_raw(
                order_id,
                client_order_id,
                eff_inst_id,
                symbol,
                account_id,
                side,
                limit_px,
                rem_qty,
                remaining_locked_fp,
                eff_prio,
                ts_ns,
            );
            self.emit_vcr_event(
                ts_ns,
                symbol,
                EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: eff_prio,
                    prev_engine_seq: 0,
                    symbol: symbol.to_string(),
                    instrument_id: eff_inst_id,
                    op: L3DeltaOp::Add as i32,
                    side: if is_buy {
                        OrderSide::Buy as i32
                    } else {
                        OrderSide::Sell as i32
                    },
                    order_id,
                    account_id,
                    price_fp: limit_px,
                    remaining_qty_fp: rem_qty,
                    delta_qty_fp: rem_qty as i64,
                    event_ts_ns: ts_ns,
                    state_root_blake3: Vec::new(),
                    trace: None,
                }),
            );
        } else if remaining_locked_fp > 0 {
            let unlock_asset = if is_buy {
                spec.quote_asset
            } else {
                spec.base_asset
            };
            self.state
                .unlock_funds(account_id, unlock_asset, remaining_locked_fp)?;
        }

        Ok(())
    }
}

// =============================================================================
// COMMAND EXTRACTOR & DIFF ANALYZERS
// =============================================================================

/// Extracts canonical `ReplayCommand` sequence from a slice of `IndexedWalEvent`s.
/// Supports both explicit `chronos-cmd:` trace tags AND pure `MarketEventEnvelope` delta inference
/// for external `ledger-rs` `HYPWAL01` segments.
pub fn extract_commands_from_wal_slice(
    events: &[IndexedWalEvent],
    default_maker_ppm: i64,
    default_taker_ppm: i64,
) -> Vec<ReplayCommand> {
    let mut commands = Vec::new();
    let mut i = 0usize;

    while i < events.len() {
        let ev = &events[i];
        if let Some(tag_cmd) = try_extract_tagged_command(&ev.envelope) {
            commands.push(tag_cmd);
            // Skip companion frames belonging to the same tagged correlation batch
            let corr = extract_correlation_id(&ev.envelope).unwrap_or("").to_string();
            i += 1;
            while i < events.len() {
                let is_same_corr = extract_correlation_id(&events[i].envelope) == Some(corr.as_str());
                let is_companion_journal = matches!(
                    &events[i].envelope.payload,
                    Some(EnvelopePayload::JournalEntry(j))
                        if !j.reference_type.eq_ignore_ascii_case("DEPOSIT")
                );
                if is_same_corr || is_companion_journal {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        }

        // Pure MarketEventEnvelope inference for external `ledger-rs` WAL segments
        match &ev.envelope.payload {
            Some(EnvelopePayload::JournalEntry(j))
                if j.reference_type.eq_ignore_ascii_case("DEPOSIT") =>
            {
                for line in &j.lines {
                    if line.sub_account == "AVAILABLE" {
                        let credit = crate::fixed::pb_to_i128(line.credit_fp);
                        let debit = crate::fixed::pb_to_i128(line.debit_fp);
                        if credit > debit {
                            commands.push(ReplayCommand::Deposit {
                                engine_seq: ev.engine_seq,
                                ts_ns: ev.ts_ns,
                                account_id: line.account_id,
                                asset: line.asset.clone(),
                                amount_fp: (credit - debit) as u128,
                            });
                        }
                    }
                }
                i += 1;
            }
            Some(EnvelopePayload::Fill(first_fill)) => {
                // Coalesce consecutive fills + trailing L3 ADD for the same `taker_order_id`
                let taker_oid = first_fill.taker_order_id;
                let taker_acc = first_fill.taker_account_id;
                let sym = if first_fill.symbol.is_empty() {
                    ev.envelope.symbol.clone()
                } else {
                    first_fill.symbol.clone()
                };
                let inst_id = first_fill.instrument_id;
                let side = if first_fill.aggressor_side == OrderSide::Sell as i32 {
                    2u8
                } else {
                    1u8
                };
                let first_fill_seq = first_fill.engine_seq.max(ev.engine_seq);
                let mut fill_seqs = Vec::new();
                let mut total_qty = 0u64;
                let mut worst_px = first_fill.price_fp;
                let mut rest_px = 0u64;
                let mut rest_prio = 0u64;
                let mut maker_ppm = default_maker_ppm;
                let mut taker_ppm = default_taker_ppm;

                let n0 = pb_to_u128(first_fill.notional_fp);
                if n0 >= 100_000_000 {
                    maker_ppm = ((first_fill.maker_fee_fp as i128 * 1_000_000) / n0 as i128) as i64;
                    taker_ppm = ((first_fill.taker_fee_fp as i128 * 1_000_000) / n0 as i128) as i64;
                }

                while i < events.len() {
                    match &events[i].envelope.payload {
                        Some(EnvelopePayload::Fill(f)) if f.taker_order_id == taker_oid => {
                            fill_seqs.push(f.engine_seq.max(events[i].engine_seq));
                            total_qty = total_qty.saturating_add(f.qty_fp);
                            worst_px = if side == 1 {
                                worst_px.max(f.price_fp)
                            } else {
                                worst_px.min(f.price_fp)
                            };
                            i += 1;
                        }
                        Some(EnvelopePayload::L3Delta(d))
                            if d.op == L3DeltaOp::Fill as i32 =>
                        {
                            i += 1;
                        }
                        Some(EnvelopePayload::L3Delta(d))
                            if d.op == L3DeltaOp::Add as i32 && d.order_id == taker_oid =>
                        {
                            total_qty = total_qty.saturating_add(d.remaining_qty_fp);
                            rest_px = d.price_fp;
                            rest_prio = d.engine_seq;
                            i += 1;
                        }
                        Some(EnvelopePayload::JournalEntry(j))
                            if !j.reference_type.eq_ignore_ascii_case("DEPOSIT") =>
                        {
                            i += 1;
                        }
                        Some(EnvelopePayload::OrderAck(ack)) if ack.order_id == taker_oid => {
                            i += 1;
                            break;
                        }
                        _ => break,
                    }
                }

                let (order_type, limit_px) = if rest_px > 0 {
                    (OrderType::Limit as i32, rest_px)
                } else {
                    (OrderType::Ioc as i32, worst_px)
                };

                commands.push(ReplayCommand::SubmitOrder {
                    engine_seq: ev.engine_seq,
                    ts_ns: ev.ts_ns,
                    order_id: taker_oid,
                    client_order_id: format!("wal-ord-{taker_oid}"),
                    account_id: taker_acc,
                    symbol: sym,
                    instrument_id: inst_id,
                    side,
                    order_type,
                    stp_mode: SelfTradePreventionMode::CancelResting as i32,
                    price_fp: limit_px,
                    qty_fp: total_qty,
                    max_slippage_bps: 0,
                    rest_priority_seq: rest_prio,
                    first_fill_seq,
                    fill_seqs,
                    maker_fee_ppm: maker_ppm,
                    taker_fee_ppm: taker_ppm,
                });
            }
            Some(EnvelopePayload::L3Delta(d)) => {
                let op = L3DeltaOp::try_from(d.op).unwrap_or(L3DeltaOp::Unspecified);
                let sym = if d.symbol.is_empty() {
                    ev.envelope.symbol.clone()
                } else {
                    d.symbol.clone()
                };
                match op {
                    L3DeltaOp::Add => {
                        commands.push(ReplayCommand::SubmitOrder {
                            engine_seq: ev.engine_seq,
                            ts_ns: ev.ts_ns,
                            order_id: d.order_id,
                            client_order_id: format!("wal-ord-{}", d.order_id),
                            account_id: d.account_id,
                            symbol: sym,
                            instrument_id: d.instrument_id,
                            side: if d.side == OrderSide::Sell as i32 {
                                2
                            } else {
                                1
                            },
                            order_type: OrderType::Limit as i32,
                            stp_mode: SelfTradePreventionMode::CancelResting as i32,
                            price_fp: d.price_fp,
                            qty_fp: d.remaining_qty_fp,
                            max_slippage_bps: 0,
                            rest_priority_seq: d.engine_seq,
                            first_fill_seq: d.engine_seq,
                            fill_seqs: Vec::new(),
                            maker_fee_ppm: default_maker_ppm,
                            taker_fee_ppm: default_taker_ppm,
                        });
                    }
                    L3DeltaOp::Cancel => {
                        commands.push(ReplayCommand::CancelOrder {
                            engine_seq: ev.engine_seq,
                            ts_ns: ev.ts_ns,
                            order_id: d.order_id,
                            account_id: d.account_id,
                            symbol: sym,
                        });
                    }
                    L3DeltaOp::Modify => {
                        commands.push(ReplayCommand::ModifyOrder {
                            engine_seq: ev.engine_seq,
                            ts_ns: ev.ts_ns,
                            order_id: d.order_id,
                            account_id: d.account_id,
                            symbol: sym,
                            new_price_fp: d.price_fp,
                            new_qty_fp: d.remaining_qty_fp,
                            priority_seq: d.engine_seq,
                        });
                    }
                    _ => {}
                }
                i += 1;
            }
            Some(EnvelopePayload::OrderAck(ack)) => {
                if let Some(rest) = ack.client_order_id.strip_prefix("RISK_ACTION:") {
                    let parts: Vec<&str> = rest.split(':').collect();
                    if parts.len() == 3 {
                        let acc_id: u64 = parts[0].parse().unwrap_or(0);
                        let st_u8: i32 = parts[1].parse().unwrap_or(0);
                        if acc_id != 0 && st_u8 != 0 {
                            commands.push(ReplayCommand::RiskAction {
                                engine_seq: ev.engine_seq,
                                ts_ns: ev.ts_ns,
                                account_id: acc_id,
                                new_status: st_u8,
                                cancel_open_orders: false,
                                symbol: ack.symbol.clone(),
                            });
                        }
                    }
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    commands
}

fn extract_correlation_id(env: &MarketEventEnvelope) -> Option<&str> {
    let trace = match &env.payload {
        Some(EnvelopePayload::L3Delta(d)) => d.trace.as_ref(),
        Some(EnvelopePayload::Fill(f)) => f.trace.as_ref(),
        Some(EnvelopePayload::OrderAck(a)) => a.trace.as_ref(),
        _ => None,
    }?;
    if trace.correlation_id.is_empty() {
        None
    } else {
        Some(trace.correlation_id.as_str())
    }
}

fn try_extract_tagged_command(env: &MarketEventEnvelope) -> Option<ReplayCommand> {
    let trace = match &env.payload {
        Some(EnvelopePayload::L3Delta(d)) => d.trace.as_ref(),
        Some(EnvelopePayload::Fill(f)) => f.trace.as_ref(),
        Some(EnvelopePayload::OrderAck(a)) => a.trace.as_ref(),
        _ => None,
    }?;
    let raw = trace.source_service.strip_prefix(CMD_TAG_PREFIX)?;
    serde_json::from_str::<ReplayCommand>(raw).ok()
}

/// Detects circular wash-trading & self-trade fills prevented in the Counterfactual Fork.
fn compute_prevented_wash_metrics(
    baseline_fills: &[ExecutionFill],
    forked_fills: &[ExecutionFill],
    blocked_accounts: &HashSet<u64>,
) -> (u128, u64) {
    if baseline_fills.is_empty() {
        return (0, 0);
    }

    // Build multiset key `(maker_order_id, taker_order_id, price_fp, qty_fp)` of fills present in fork
    let mut fork_counts: HashMap<(u64, u64, u64, u64), usize> =
        HashMap::with_capacity(forked_fills.len());
    for f in forked_fills {
        *fork_counts
            .entry((f.maker_order_id, f.taker_order_id, f.price_fp, f.qty_fp))
            .or_insert(0) += 1;
    }

    // Identify baseline fills participating in self-trades, blocked syndicate accounts, or directed temporal cycles (k <= 6)
    let mut adj: HashMap<u64, Vec<(u64, u64, usize)>> = HashMap::new();
    let mut cyclic_indices: HashSet<usize> = HashSet::new();

    for (idx, f) in baseline_fills.iter().enumerate() {
        if f.maker_account_id == f.taker_account_id
            || blocked_accounts.contains(&f.maker_account_id)
            || blocked_accounts.contains(&f.taker_account_id)
        {
            cyclic_indices.insert(idx);
        }

        // Prune expired edges outside 60s window & check if path `taker -> ... -> maker` exists (cycle <= 6!)
        let min_ts = f.exec_ts_ns.saturating_sub(WASH_WINDOW_NS);
        let mut path_edges = Vec::new();
        let mut visited = HashSet::new();
        if dfs_find_path(
            &adj,
            f.taker_account_id,
            f.maker_account_id,
            min_ts,
            5,
            &mut visited,
            &mut path_edges,
        ) {
            cyclic_indices.insert(idx);
            for e_idx in path_edges {
                cyclic_indices.insert(e_idx);
            }
        }
        adj.entry(f.maker_account_id)
            .or_default()
            .push((f.taker_account_id, f.exec_ts_ns, idx));
    }

    let mut prevented_vol_fp: u128 = 0;
    let mut prevented_count: u64 = 0;

    for (idx, f) in baseline_fills.iter().enumerate() {
        let key = (f.maker_order_id, f.taker_order_id, f.price_fp, f.qty_fp);
        if let Some(cnt) = fork_counts.get_mut(&key) {
            if *cnt > 0 {
                *cnt -= 1;
                continue;
            }
        }
        if cyclic_indices.contains(&idx) {
            prevented_vol_fp = prevented_vol_fp.saturating_add(f.qty_fp as u128);
            prevented_count += 1;
        }
    }

    (prevented_vol_fp, prevented_count)
}

fn dfs_find_path(
    adj: &HashMap<u64, Vec<(u64, u64, usize)>>,
    cur: u64,
    target: u64,
    min_ts: u64,
    rem_depth: usize,
    visited: &mut HashSet<u64>,
    path_edges: &mut Vec<usize>,
) -> bool {
    if rem_depth == 0 {
        return false;
    }
    visited.insert(cur);
    if let Some(neighbors) = adj.get(&cur) {
        for &(nxt, ts, edge_idx) in neighbors.iter().rev() {
            if ts < min_ts {
                continue;
            }
            if nxt == target {
                path_edges.push(edge_idx);
                return true;
            }
            if !visited.contains(&nxt) {
                path_edges.push(edge_idx);
                if dfs_find_path(adj, nxt, target, min_ts, rem_depth - 1, visited, path_edges) {
                    return true;
                }
                path_edges.pop();
            }
        }
    }
    visited.remove(&cur);
    false
}

fn compute_account_balance_diffs(
    baseline: &ExchangeState,
    forked: &ExchangeState,
) -> Vec<AccountBalanceDiff> {
    let mut keys: Vec<(u64, AssetSymbol)> = Vec::new();
    for acc in &baseline.accounts {
        if acc.account_id == 0 {
            continue;
        }
        for b in acc.sorted_balances() {
            keys.push((acc.account_id, b.asset));
        }
    }
    for acc in &forked.accounts {
        if acc.account_id == 0 {
            continue;
        }
        for b in acc.sorted_balances() {
            keys.push((acc.account_id, b.asset));
        }
    }
    keys.sort_unstable();
    keys.dedup();

    let mut out = Vec::new();
    for (account_id, asset) in keys {
        let base_tot = baseline
            .account_index
            .get(account_id)
            .map(|idx| {
                let (av, lk) = baseline.accounts[idx as usize].get_asset(asset);
                av.saturating_add(lk)
            })
            .unwrap_or(0);
        let fork_tot = forked
            .account_index
            .get(account_id)
            .map(|idx| {
                let (av, lk) = forked.accounts[idx as usize].get_asset(asset);
                av.saturating_add(lk)
            })
            .unwrap_or(0);
        let delta = fork_tot.saturating_sub(base_tot);
        if delta != 0 {
            out.push(AccountBalanceDiff {
                account_id,
                asset: asset.as_str().to_string(),
                baseline_total_fp: Some(i128_to_pb(base_tot)),
                forked_total_fp: Some(i128_to_pb(fork_tot)),
                delta_total_fp: Some(i128_to_pb(delta)),
            });
        }
    }
    out
}

fn compute_l2_level_diffs(
    baseline: &ExchangeState,
    forked: &ExchangeState,
    symbol: &str,
) -> Vec<PriceLevelDiff> {
    let mut out = Vec::new();
    let base_book = baseline.find_book(symbol);
    let fork_book = forked.find_book(symbol);

    // Bids (descending order in diff report)
    let mut bid_prices: Vec<u64> = Vec::new();
    if let Some(b) = base_book {
        bid_prices.extend(b.bids.iter().map(|l| l.price_fp));
    }
    if let Some(f) = fork_book {
        bid_prices.extend(f.bids.iter().map(|l| l.price_fp));
    }
    bid_prices.sort_unstable_by(|a, b| b.cmp(a));
    bid_prices.dedup();

    for px in bid_prices {
        let b_qty = base_book
            .and_then(|b| b.find_level_idx(1, px).ok().map(|i| b.bids[i].total_qty_fp))
            .unwrap_or(0);
        let f_qty = fork_book
            .and_then(|f| f.find_level_idx(1, px).ok().map(|i| f.bids[i].total_qty_fp))
            .unwrap_or(0);
        if b_qty != f_qty {
            let delta = (f_qty as i128 - b_qty as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            out.push(PriceLevelDiff {
                side: OrderSide::Buy as i32,
                price_fp: px,
                baseline_qty_fp: b_qty,
                forked_qty_fp: f_qty,
                delta_qty_fp: delta,
            });
        }
    }

    // Asks (ascending order in diff report)
    let mut ask_prices: Vec<u64> = Vec::new();
    if let Some(b) = base_book {
        ask_prices.extend(b.asks.iter().map(|l| l.price_fp));
    }
    if let Some(f) = fork_book {
        ask_prices.extend(f.asks.iter().map(|l| l.price_fp));
    }
    ask_prices.sort_unstable();
    ask_prices.dedup();

    for px in ask_prices {
        let b_qty = base_book
            .and_then(|b| b.find_level_idx(2, px).ok().map(|i| b.asks[i].total_qty_fp))
            .unwrap_or(0);
        let f_qty = fork_book
            .and_then(|f| f.find_level_idx(2, px).ok().map(|i| f.asks[i].total_qty_fp))
            .unwrap_or(0);
        if b_qty != f_qty {
            let delta = (f_qty as i128 - b_qty as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            out.push(PriceLevelDiff {
                side: OrderSide::Sell as i32,
                price_fp: px,
                baseline_qty_fp: b_qty,
                forked_qty_fp: f_qty,
                delta_qty_fp: delta,
            });
        }
    }

    out
}

// =============================================================================
// DETERMINISTIC HISTORICAL SCENARIO BUILDER (WAL + PERIODIC SNAPSHOT WRITER)
// =============================================================================

/// Produces cryptographically chained `HYPWAL01` segments and periodic `HYPSNP01` snapshots
/// from high-level exchange commands, used for deterministic testing, benchmarks, and standalone boot.
pub struct HistoricalTimelineRecorder {
    state: ExchangeState,
    writer: WalSegmentWriter,
    segments: Vec<(Option<std::path::PathBuf>, SegmentBacking)>,
    snapshots: Vec<VerifiedSnapshot>,
    snapshot_interval: u64,
    last_snapshot_seq: u64,
    segment_max_frames: usize,
    frames_in_current_segment: usize,
    journal_seq: u64,
    corr_counter: u64,
    maker_fee_ppm: i64,
    taker_fee_ppm: i64,
}

impl HistoricalTimelineRecorder {
    pub fn new(snapshot_interval: u64, maker_fee_ppm: i64, taker_fee_ppm: i64) -> Self {
        let mut state = ExchangeState::new();
        state.maker_fee_ppm = maker_fee_ppm;
        state.taker_fee_ppm = taker_fee_ppm;
        Self {
            state,
            writer: WalSegmentWriter::new(),
            segments: Vec::new(),
            snapshots: Vec::new(),
            snapshot_interval,
            last_snapshot_seq: 0,
            segment_max_frames: 50_000,
            frames_in_current_segment: 0,
            journal_seq: 1,
            corr_counter: 1,
            maker_fee_ppm,
            taker_fee_ppm,
        }
    }

    pub fn set_segment_max_frames(&mut self, max_frames: usize) {
        self.segment_max_frames = max_frames.max(16);
    }

    #[inline]
    pub fn current_state_mut(&mut self) -> &mut ExchangeState {
        &mut self.state
    }

    #[inline]
    pub const fn next_engine_seq(&self) -> u64 {
        self.writer.next_seq()
    }

    fn push_envelope(&mut self, env: MarketEventEnvelope) -> Result<u64, ChronosError> {
        let (seq, _) = self.writer.append_envelope(env.clone())?;
        self.state.engine_seq = seq;
        self.state.ts_ns = env.ts_ns;
        self.frames_in_current_segment += 1;

        if self.frames_in_current_segment >= self.segment_max_frames {
            let next_seq = self.writer.next_seq();
            let tip = self.writer.tip_blake3();
            let finished = std::mem::replace(
                &mut self.writer,
                WalSegmentWriter::with_chain_anchor(next_seq, tip),
            );
            self.segments
                .push((None, SegmentBacking::Memory(finished.finish_bytes())));
            self.frames_in_current_segment = 0;
        }
        Ok(seq)
    }

    fn maybe_capture_snapshot(&mut self) -> Result<(), ChronosError> {
        if self.snapshot_interval > 0
            && self.state.engine_seq.saturating_sub(self.last_snapshot_seq)
                >= self.snapshot_interval
        {
            let seq = self.capture_snapshot()?;
            self.last_snapshot_seq = seq;
        }
        Ok(())
    }

    pub fn capture_snapshot(&mut self) -> Result<u64, ChronosError> {
        let root = self.state.compute_state_root();
        let payload = self.state.to_snapshot_payload();
        let bytes = encode_snapshot_bytes(self.state.engine_seq, self.state.ts_ns, root, &payload)?;
        let verified = parse_and_verify_snapshot(&bytes, None)?;
        let seq = verified.engine_seq;
        if self.snapshots.last().map(|s| s.engine_seq) != Some(seq) {
            self.snapshots.push(verified);
        }
        Ok(seq)
    }

    pub fn record_deposit(
        &mut self,
        account_id: u64,
        asset: &str,
        amount_fp: u128,
        ts_ns: u64,
    ) -> Result<u64, ChronosError> {
        self.state.credit_deposit(account_id, asset, amount_fp)?;
        let acc_idx = self.state.ensure_account(account_id);
        let sym = AssetSymbol::from_str_trunc(asset);
        let (avail, _) = self.state.accounts[acc_idx as usize].get_asset(sym);

        let j_seq = self.journal_seq;
        self.journal_seq += 1;
        let seq = self.writer.next_seq();
        let env = MarketEventEnvelope {
            engine_seq: seq,
            symbol: "BTC-USDT".to_string(),
            ts_ns,
            payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                journal_seq: j_seq,
                engine_seq: seq,
                reference_type: "DEPOSIT".to_string(),
                reference_id: account_id,
                lines: vec![LedgerEntryLine {
                    account_id,
                    asset: asset.to_string(),
                    sub_account: "AVAILABLE".to_string(),
                    debit_fp: Some(i128_to_pb(0)),
                    credit_fp: Some(i128_to_pb(amount_fp as i128)),
                    balance_after_fp: Some(i128_to_pb(avail)),
                }],
                ts_ns,
                prev_journal_blake3: Vec::new(),
                journal_blake3: Vec::new(),
            })),
        };
        let out_seq = self.push_envelope(env)?;
        self.maybe_capture_snapshot()?;
        Ok(out_seq)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_submit_order(
        &mut self,
        order_id: u64,
        account_id: u64,
        symbol: &str,
        side: u8,
        order_type: OrderType,
        stp_mode: SelfTradePreventionMode,
        price_fp: u64,
        qty_fp: u64,
        ts_ns: u64,
    ) -> Result<u64, ChronosError> {
        let corr_id = format!("corr-{}", self.corr_counter);
        self.corr_counter += 1;

        let b_idx = self.state.ensure_book(symbol);
        let spec = self.state.books[b_idx].spec.clone();
        let is_buy = side == 1;
        let maker_ppm = self.maker_fee_ppm;
        let taker_ppm = self.taker_fee_ppm;

        let limit_px = if order_type == OrderType::Market {
            if is_buy {
                u64::MAX / 2
            } else {
                1
            }
        } else {
            price_fp
        };

        // Pre-lock collateral
        let mut remaining_locked_fp: i128 = 0;
        if is_buy {
            if price_fp > 0 {
                let reserve = checked_buyer_reserve_fp(price_fp, qty_fp, maker_ppm, taker_ppm)?;
                if !self.state.lock_funds(account_id, spec.quote_asset, reserve)? {
                    return Ok(self.state.engine_seq);
                }
                remaining_locked_fp = reserve;
            }
        } else {
            let reserve = qty_fp as i128;
            if !self.state.lock_funds(account_id, spec.base_asset, reserve)? {
                return Ok(self.state.engine_seq);
            }
            remaining_locked_fp = reserve;
        }

        // Execute matching and collect WAL events to emit atomically
        let cmd_start_seq = self.writer.next_seq();
        let mut rem_qty = qty_fp;
        let mut pending_fills: Vec<(ExecutionFill, u64, u8, u64, u64)> = Vec::new();
        let mut stp_cancels: Vec<(u64, u64, u8, u64, u64)> = Vec::new();

        while rem_qty > 0 {
            let best_maker_slot = {
                let opp_levels = if is_buy {
                    &self.state.books[b_idx].asks
                } else {
                    &self.state.books[b_idx].bids
                };
                let Some(best_lvl) = opp_levels.last() else {
                    break;
                };
                let crosses = if is_buy {
                    best_lvl.price_fp <= limit_px
                } else {
                    best_lvl.price_fp >= limit_px
                };
                if !crosses {
                    break;
                }
                best_lvl.head_slot
            };
            if best_maker_slot == NULL_SLOT {
                break;
            }

            let maker_hot = self.state.hot_orders[best_maker_slot as usize];
            let maker_cold = self.state.cold_orders[best_maker_slot as usize];

            if maker_hot.account_id == account_id && stp_mode != SelfTradePreventionMode::None {
                self.state.remove_resting_order_by_id(maker_hot.order_id);
                let unlock_asset = if maker_hot.side == 1 {
                    spec.quote_asset
                } else {
                    spec.base_asset
                };
                if maker_cold.locked_collateral_fp > 0 {
                    self.state.unlock_funds(
                        maker_hot.account_id,
                        unlock_asset,
                        maker_cold.locked_collateral_fp,
                    )?;
                }
                stp_cancels.push((
                    maker_hot.order_id,
                    maker_hot.account_id,
                    maker_hot.side,
                    maker_hot.price_fp,
                    maker_hot.remaining_qty_fp,
                ));
                if stp_mode == SelfTradePreventionMode::CancelAggressing
                    || stp_mode == SelfTradePreventionMode::CancelBoth
                {
                    rem_qty = 0;
                    break;
                }
                continue;
            }

            let match_qty = rem_qty.min(maker_hot.remaining_qty_fp);
            let exec_px = maker_hot.price_fp;
            let notional = checked_notional_fp(exec_px, match_qty)?;
            let maker_fee = checked_fee_fp(notional, maker_ppm)?;
            let taker_fee = checked_fee_fp(notional, taker_ppm)?;

            let new_maker_rem = maker_hot.remaining_qty_fp - match_qty;
            let maker_locked_used = if new_maker_rem == 0 {
                maker_cold.locked_collateral_fp
            } else if maker_hot.side == 1 {
                let keep = checked_buyer_reserve_fp(
                    maker_hot.price_fp,
                    new_maker_rem,
                    maker_ppm,
                    taker_ppm,
                )?;
                maker_cold.locked_collateral_fp.saturating_sub(keep)
            } else {
                match_qty as i128
            };
            let new_taker_rem = rem_qty - match_qty;
            let taker_locked_used = if remaining_locked_fp > 0 {
                if new_taker_rem == 0 {
                    remaining_locked_fp
                } else if is_buy {
                    let keep = checked_buyer_reserve_fp(
                        price_fp,
                        new_taker_rem,
                        maker_ppm,
                        taker_ppm,
                    )?;
                    remaining_locked_fp.saturating_sub(keep)
                } else {
                    match_qty as i128
                }
            } else {
                0
            };

            if is_buy {
                self.state.settle_trade_fill(
                    spec.base_asset,
                    spec.quote_asset,
                    account_id,
                    maker_hot.account_id,
                    taker_locked_used,
                    maker_locked_used,
                    match_qty,
                    notional,
                    taker_fee,
                    maker_fee,
                )?;
            } else {
                self.state.settle_trade_fill(
                    spec.base_asset,
                    spec.quote_asset,
                    maker_hot.account_id,
                    account_id,
                    maker_locked_used,
                    taker_locked_used,
                    match_qty,
                    notional,
                    maker_fee,
                    taker_fee,
                )?;
            }

            remaining_locked_fp -= taker_locked_used;
            rem_qty -= match_qty;

            let new_maker_rem = maker_hot.remaining_qty_fp - match_qty;
            if new_maker_rem == 0 {
                self.state.remove_resting_order_by_id(maker_hot.order_id);
            } else {
                self.state.hot_orders[best_maker_slot as usize].remaining_qty_fp = new_maker_rem;
                self.state.cold_orders[best_maker_slot as usize].locked_collateral_fp -=
                    maker_locked_used;
                let opp_levels = if is_buy {
                    &mut self.state.books[b_idx].asks
                } else {
                    &mut self.state.books[b_idx].bids
                };
                if let Some(best_lvl) = opp_levels.last_mut() {
                    best_lvl.total_qty_fp = best_lvl.total_qty_fp.saturating_sub(match_qty);
                }
            }

            let fill_id = self.state.next_fill_id;
            self.state.next_fill_id = fill_id.saturating_add(1);
            self.state.books[b_idx].last_clearing_price_fp = exec_px;
            self.state.books[b_idx].cumulative_base_vol_fp = self.state.books[b_idx]
                .cumulative_base_vol_fp
                .saturating_add(match_qty as u128);
            self.state.books[b_idx].cumulative_quote_vol_fp = self.state.books[b_idx]
                .cumulative_quote_vol_fp
                .saturating_add(notional);

            let fill = ExecutionFill {
                fill_id,
                engine_seq: 0, // assigned below
                symbol: symbol.to_string(),
                instrument_id: spec.instrument_id,
                maker_order_id: maker_hot.order_id,
                taker_order_id: order_id,
                maker_account_id: maker_hot.account_id,
                taker_account_id: account_id,
                aggressor_side: if is_buy {
                    OrderSide::Buy as i32
                } else {
                    OrderSide::Sell as i32
                },
                price_fp: exec_px,
                qty_fp: match_qty,
                notional_fp: Some(u128_to_pb(notional)),
                maker_fee_fp: maker_fee,
                taker_fee_fp: taker_fee,
                exec_ts_ns: ts_ns,
                wal_blake3_hash: Vec::new(),
                trace: None,
            };
            pending_fills.push((fill, maker_hot.order_id, maker_hot.side, new_maker_rem, match_qty));
        }

        let rests = rem_qty > 0
            && (order_type == OrderType::Limit || order_type == OrderType::PostOnly)
            && limit_px > 0;
        if !rests && remaining_locked_fp > 0 {
            let unlock_asset = if is_buy {
                spec.quote_asset
            } else {
                spec.base_asset
            };
            self.state
                .unlock_funds(account_id, unlock_asset, remaining_locked_fp)?;
        }

        // Compute exact sequence numbers for first fill & resting ADD so sandbox noop matches 100%!
        // Layout of frames emitted for this command:
        //   1. STP cancel L3 deltas (if any): `stp_cancels.len()` frames
        //   2. For each fill `k`: 1 Fill frame + 1 Maker L3Delta(FILL) frame -> wait: `first_fill_seq`
        //      Wait: in `CounterfactualSandbox::execute_sandbox_submit`, fill `k` hashes `fill_seq_cursor = first_fill_seq + k`!
        //      Let's emit ALL `Fill` frames contiguously starting at `first_fill_seq` (`first_fill_seq + 0`, `first_fill_seq + 1`, ...),
        //      followed by Maker `L3Delta(FILL)` frames, followed by LedgerJournalEntry, followed by Taker `L3Delta(ADD)` at `rest_priority_seq`!
        let first_fill_seq = cmd_start_seq + (stp_cancels.len() as u64);
        let fills_count = pending_fills.len() as u64;
        let has_journal = !pending_fills.is_empty() || rests || !stp_cancels.is_empty();
        // Emit resting L3Delta(ADD) BEFORE the consolidated JournalEntry so EVERY command batch
        // begins with a `chronos-cmd:` tagged frame (`stp_cancels`, `Fill`, `L3Delta(ADD)`, or `OrderAck`)!
        let rest_priority_seq = if rests {
            cmd_start_seq + (stp_cancels.len() as u64) + (fills_count * 2)
        } else {
            0
        };

        if rests {
            self.state.insert_resting_order_raw(
                order_id,
                &format!("ord-{order_id}"),
                spec.instrument_id,
                symbol,
                account_id,
                side,
                limit_px,
                rem_qty,
                remaining_locked_fp,
                rest_priority_seq,
                ts_ns,
            );
        }

        // Embed deterministic command tag on the very first frame of this batch
        let cmd_descriptor = ReplayCommand::SubmitOrder {
            engine_seq: cmd_start_seq,
            ts_ns,
            order_id,
            client_order_id: format!("ord-{order_id}"),
            account_id,
            symbol: symbol.to_string(),
            instrument_id: spec.instrument_id,
            side,
            order_type: order_type as i32,
            stp_mode: stp_mode as i32,
            price_fp,
            qty_fp,
            max_slippage_bps: 0,
            rest_priority_seq,
            first_fill_seq,
            fill_seqs: Vec::new(),
            maker_fee_ppm: maker_ppm,
            taker_fee_ppm: taker_ppm,
        };
        let cmd_tag_str = format!(
            "{CMD_TAG_PREFIX}{}",
            serde_json::to_string(&cmd_descriptor)?
        );
        let mut tag_attached = false;
        let make_trace = |attach_cmd: bool| -> Option<TraceMetadata> {
            Some(TraceMetadata {
                trace_id: String::new(),
                span_id: String::new(),
                correlation_id: corr_id.clone(),
                source_service: if attach_cmd {
                    cmd_tag_str.clone()
                } else {
                    "ledger-rs".to_string()
                },
                ingest_ts_ns: ts_ns,
            })
        };

        // (a) Emit STP cancel frames
        for &(c_oid, c_acc, c_side, c_px, c_qty) in &stp_cancels {
            let seq = self.writer.next_seq();
            let tr = make_trace(!tag_attached);
            tag_attached = true;
            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: seq,
                    prev_engine_seq: 0,
                    symbol: symbol.to_string(),
                    instrument_id: spec.instrument_id,
                    op: L3DeltaOp::Cancel as i32,
                    side: if c_side == 2 {
                        OrderSide::Sell as i32
                    } else {
                        OrderSide::Buy as i32
                    },
                    order_id: c_oid,
                    account_id: c_acc,
                    price_fp: c_px,
                    remaining_qty_fp: 0,
                    delta_qty_fp: -(c_qty as i64),
                    event_ts_ns: ts_ns,
                    state_root_blake3: Vec::new(),
                    trace: tr,
                })),
            })?;
        }

        // (b) Emit Fill frames contiguously (`first_fill_seq + k`) & record leaves in recorder's Merkle state
        for (mut fill, _, _, _, _) in pending_fills.iter().cloned() {
            let seq = self.writer.next_seq();
            fill.engine_seq = seq;
            fill.trace = make_trace(!tag_attached);
            tag_attached = true;

            let notional = pb_to_u128(fill.notional_fp);
            let leaf = hash_execution_fill_leaf(&ExecutionFillDigestInput {
                fill_id: fill.fill_id,
                engine_seq: seq,
                instrument_id: spec.instrument_id,
                maker_order_id: fill.maker_order_id,
                taker_order_id: fill.taker_order_id,
                maker_account_id: fill.maker_account_id,
                taker_account_id: fill.taker_account_id,
                aggressor_side: side,
                price_fp: fill.price_fp,
                qty_fp: fill.qty_fp,
                notional_fp: notional,
                maker_fee_fp: fill.maker_fee_fp,
                taker_fee_fp: fill.taker_fee_fp,
                exec_ts_ns: ts_ns,
            });
            self.state.merkle.append_execution_leaf(leaf);

            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::Fill(fill)),
            })?;
        }

        // (c) Emit Maker L3Delta(FILL) frames
        for (fill, m_oid, m_side, m_rem, m_qty) in &pending_fills {
            let seq = self.writer.next_seq();
            let tr = make_trace(!tag_attached);
            tag_attached = true;
            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: seq,
                    prev_engine_seq: 0,
                    symbol: symbol.to_string(),
                    instrument_id: spec.instrument_id,
                    op: L3DeltaOp::Fill as i32,
                    side: if *m_side == 2 {
                        OrderSide::Sell as i32
                    } else {
                        OrderSide::Buy as i32
                    },
                    order_id: *m_oid,
                    account_id: fill.maker_account_id,
                    price_fp: fill.price_fp,
                    remaining_qty_fp: *m_rem,
                    delta_qty_fp: -(*m_qty as i64),
                    event_ts_ns: ts_ns,
                    state_root_blake3: Vec::new(),
                    trace: tr,
                })),
            })?;
        }

        // (d) Emit resting L3Delta(ADD) BEFORE consolidated JournalEntry so EVERY command batch starts with `chronos-cmd:`!
        if rests {
            let seq = self.writer.next_seq();
            debug_assert_eq!(seq, rest_priority_seq);
            let tr = make_trace(!tag_attached);
            let _ = tag_attached;
            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                    engine_seq: seq,
                    prev_engine_seq: 0,
                    symbol: symbol.to_string(),
                    instrument_id: spec.instrument_id,
                    op: L3DeltaOp::Add as i32,
                    side: if is_buy {
                        OrderSide::Buy as i32
                    } else {
                        OrderSide::Sell as i32
                    },
                    order_id,
                    account_id,
                    price_fp: limit_px,
                    remaining_qty_fp: rem_qty,
                    delta_qty_fp: rem_qty as i64,
                    event_ts_ns: ts_ns,
                    state_root_blake3: Vec::new(),
                    trace: tr,
                })),
            })?;
        } else if !tag_attached {
            // Unfilled IOC / Market order that didn't rest: emit OrderAck so command is still recorded
            let seq = self.writer.next_seq();
            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::OrderAck(crate::pb::OrderAck {
                    engine_seq: seq,
                    order_id,
                    client_order_id: format!("ord-{order_id}"),
                    account_id,
                    symbol: symbol.to_string(),
                    instrument_id: spec.instrument_id,
                    status: crate::pb::OrderStatus::Expired as i32,
                    reject_reason: crate::pb::RejectReason::None as i32,
                    reject_detail: String::new(),
                    filled_qty_fp: 0,
                    remaining_qty_fp: 0,
                    avg_fill_price_fp: 0,
                    engine_ts_ns: ts_ns,
                    wal_blake3_hash: Vec::new(),
                    fills: Vec::new(),
                    trace: make_trace(true),
                })),
            })?;
        }

        // (e) Emit consolidated Double-Entry LedgerJournalEntry syncing ONLY actual asset slots + treasury
        if has_journal {
            let mut touched_accounts = vec![account_id];
            for (f, _, _, _, _) in &pending_fills {
                if !touched_accounts.contains(&f.maker_account_id) {
                    touched_accounts.push(f.maker_account_id);
                }
            }
            for &(_, c_acc, _, _, _) in &stp_cancels {
                if !touched_accounts.contains(&c_acc) {
                    touched_accounts.push(c_acc);
                }
            }
            let mut lines = Vec::new();
            for acc_id in touched_accounts {
                let a_idx = self.state.ensure_account(acc_id);
                let existing_assets: Vec<crate::merkle::AssetBalanceLeaf> =
                    self.state.accounts[a_idx as usize].sorted_balances().to_vec();
                for bal in existing_assets {
                    lines.push(LedgerEntryLine {
                        account_id: acc_id,
                        asset: bal.asset.as_str().to_string(),
                        sub_account: "AVAILABLE".to_string(),
                        debit_fp: None,
                        credit_fp: None,
                        balance_after_fp: Some(i128_to_pb(bal.available_fp)),
                    });
                    lines.push(LedgerEntryLine {
                        account_id: acc_id,
                        asset: bal.asset.as_str().to_string(),
                        sub_account: "LOCKED_MARGIN".to_string(),
                        debit_fp: None,
                        credit_fp: None,
                        balance_after_fp: Some(i128_to_pb(bal.locked_fp)),
                    });
                }
            }
            let treasury_q = self.state.quote_treasury_fee_pool(spec.quote_asset.as_str());
            lines.push(LedgerEntryLine {
                account_id: 0,
                asset: spec.quote_asset.as_str().to_string(),
                sub_account: "TREASURY_FEE".to_string(),
                debit_fp: None,
                credit_fp: None,
                balance_after_fp: Some(i128_to_pb(treasury_q)),
            });

            let seq = self.writer.next_seq();
            let j_seq = self.journal_seq;
            self.journal_seq += 1;
            self.push_envelope(MarketEventEnvelope {
                engine_seq: seq,
                symbol: symbol.to_string(),
                ts_ns,
                payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                    journal_seq: j_seq,
                    engine_seq: seq,
                    reference_type: "ORDER_SETTLEMENT".to_string(),
                    reference_id: order_id,
                    lines,
                    ts_ns,
                    prev_journal_blake3: Vec::new(),
                    journal_blake3: Vec::new(),
                })),
            })?;
        }

        self.maybe_capture_snapshot()?;
        Ok(self.state.engine_seq)
    }

    pub fn record_cancel_order(
        &mut self,
        order_id: u64,
        symbol: &str,
        ts_ns: u64,
    ) -> Result<u64, ChronosError> {
        let Some((hot, cold)) = self.state.remove_resting_order_by_id(order_id) else {
            return Ok(self.state.engine_seq);
        };
        let spec = InstrumentSpec::from_symbol(symbol);
        let unlock_asset = if hot.side == 1 {
            spec.quote_asset
        } else {
            spec.base_asset
        };
        if cold.locked_collateral_fp > 0 {
            self.state
                .unlock_funds(hot.account_id, unlock_asset, cold.locked_collateral_fp)?;
        }

        let corr_id = format!("corr-{}", self.corr_counter);
        self.corr_counter += 1;
        let cmd_seq = self.writer.next_seq();
        let cmd_desc = ReplayCommand::CancelOrder {
            engine_seq: cmd_seq,
            ts_ns,
            order_id,
            account_id: hot.account_id,
            symbol: symbol.to_string(),
        };
        let tag = format!("{CMD_TAG_PREFIX}{}", serde_json::to_string(&cmd_desc)?);

        self.push_envelope(MarketEventEnvelope {
            engine_seq: cmd_seq,
            symbol: symbol.to_string(),
            ts_ns,
            payload: Some(EnvelopePayload::L3Delta(L3BookDelta {
                engine_seq: cmd_seq,
                prev_engine_seq: 0,
                symbol: symbol.to_string(),
                instrument_id: hot.instrument_id,
                op: L3DeltaOp::Cancel as i32,
                side: if hot.side == 2 {
                    OrderSide::Sell as i32
                } else {
                    OrderSide::Buy as i32
                },
                order_id,
                account_id: hot.account_id,
                price_fp: hot.price_fp,
                remaining_qty_fp: 0,
                delta_qty_fp: -(hot.remaining_qty_fp as i64),
                event_ts_ns: ts_ns,
                state_root_blake3: Vec::new(),
                trace: Some(TraceMetadata {
                    trace_id: String::new(),
                    span_id: String::new(),
                    correlation_id: corr_id.clone(),
                    source_service: tag,
                    ingest_ts_ns: ts_ns,
                }),
            })),
        })?;

        let a_idx = self.state.ensure_account(hot.account_id);
        let (av, lk) = self.state.accounts[a_idx as usize].get_asset(unlock_asset);
        let seq = self.writer.next_seq();
        let j_seq = self.journal_seq;
        self.journal_seq += 1;
        self.push_envelope(MarketEventEnvelope {
            engine_seq: seq,
            symbol: symbol.to_string(),
            ts_ns,
            payload: Some(EnvelopePayload::JournalEntry(LedgerJournalEntry {
                journal_seq: j_seq,
                engine_seq: seq,
                reference_type: "ORDER_CANCEL".to_string(),
                reference_id: order_id,
                lines: vec![
                    LedgerEntryLine {
                        account_id: hot.account_id,
                        asset: unlock_asset.as_str().to_string(),
                        sub_account: "AVAILABLE".to_string(),
                        debit_fp: None,
                        credit_fp: None,
                        balance_after_fp: Some(i128_to_pb(av)),
                    },
                    LedgerEntryLine {
                        account_id: hot.account_id,
                        asset: unlock_asset.as_str().to_string(),
                        sub_account: "LOCKED_MARGIN".to_string(),
                        debit_fp: None,
                        credit_fp: None,
                        balance_after_fp: Some(i128_to_pb(lk)),
                    },
                ],
                ts_ns,
                prev_journal_blake3: Vec::new(),
                journal_blake3: Vec::new(),
            })),
        })?;

        self.maybe_capture_snapshot()?;
        Ok(self.state.engine_seq)
    }

    pub fn build_store(mut self, sparse_stride: usize) -> Result<(ChronosWalStore, ExchangeState), ChronosError> {
        if self.frames_in_current_segment > 0 {
            self.segments
                .push((None, SegmentBacking::Memory(self.writer.finish_bytes())));
        }
        let store = ChronosWalStore::from_backings_and_snapshots(
            self.segments,
            self.snapshots,
            sparse_stride,
        )?;
        Ok((store, self.state))
    }

    /// Writes generated segments (`wal/segment_XXXXXXXX.wal`) and snapshots (`snapshots/snap_XXXXXXXXXXXXXXXX.snap`)
    /// to disk for `open_data_dir` testing and live startup seeding.
    pub fn write_to_data_dir(mut self, data_dir: &std::path::Path) -> Result<(), ChronosError> {
        let wal_dir = data_dir.join("wal");
        let snap_dir = data_dir.join("snapshots");
        std::fs::create_dir_all(&wal_dir)?;
        std::fs::create_dir_all(&snap_dir)?;

        if self.frames_in_current_segment > 0 {
            self.segments
                .push((None, SegmentBacking::Memory(self.writer.finish_bytes())));
        }

        for (idx, (_, backing)) in self.segments.iter().enumerate() {
            let p = wal_dir.join(format!("segment_{:08}.wal", idx + 1));
            std::fs::write(p, backing.as_slice())?;
        }
        for snap in &self.snapshots {
            let p = snap_dir.join(format!("snap_{:016}.snap", snap.engine_seq));
            let bytes = encode_snapshot_bytes(
                snap.engine_seq,
                snap.ts_ns,
                snap.state_root_blake3,
                &snap.payload,
            )?;
            std::fs::write(p, bytes)?;
        }
        Ok(())
    }
}

/// Builds a rich default multi-regime historical market scenario (Deposits + Market Makers +
/// Circular Wash-Trading Ring `666 -> 667 -> 668 -> 666` + Flash-Crash Sweep) when starting
/// `chronos-replay` without an existing `ledger-rs` WAL directory.
pub fn build_default_demo_scenario() -> Result<ChronosWalStore, ChronosError> {
    let mut rec =
        HistoricalTimelineRecorder::new(250, DEFAULT_MAKER_FEE_PPM, DEFAULT_TAKER_FEE_PPM);
    let base_ts: u64 = 1_795_010_000_000_000_000;
    let mut ts = base_ts;

    // Seed institutional & syndicate accounts
    for acc in [1001u64, 1002, 1003, 1004, 666, 667, 668] {
        rec.record_deposit(acc, "USDT", 500_000_000_000_000, ts)?; // 5,000,000 USDT
        ts += 1_000_000;
        rec.record_deposit(acc, "BTC", 10_000_000_000, ts)?; // 100 BTC
        ts += 1_000_000;
    }

    let mut next_oid = 1u64;
    let mid_px: u64 = 6_850_000_000_000; // 68,500.00 USDT

    // Phase 1: Deep two-sided institutional book
    for lvl in 1..=20u64 {
        let bid_px = mid_px - lvl * 1_000_000_000; // -10 USDT steps
        let ask_px = mid_px + lvl * 1_000_000_000; // +10 USDT steps
        rec.record_submit_order(
            next_oid,
            1001,
            "BTC-USDT",
            1,
            OrderType::Limit,
            SelfTradePreventionMode::CancelResting,
            bid_px,
            50_000_000, // 0.5 BTC
            ts,
        )?;
        next_oid += 1;
        ts += 5_000_000;

        rec.record_submit_order(
            next_oid,
            1002,
            "BTC-USDT",
            2,
            OrderType::Limit,
            SelfTradePreventionMode::CancelResting,
            ask_px,
            50_000_000, // 0.5 BTC
            ts,
        )?;
        next_oid += 1;
        ts += 5_000_000;
    }

    // Phase 2: Organic trading + Circular Wash-Trading Syndicate (`666 -> 667 -> 668 -> 666`)
    ts += 35_000_000_000; // +35 seconds so "block 30s earlier" hits cleanly before attack!
    for round in 0..15u64 {
        let wash_px = mid_px + (round % 3) * 100_000_000;
        // 666 sells to 667
        rec.record_submit_order(
            next_oid,
            666,
            "BTC-USDT",
            2,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 1_000_000;
        rec.record_submit_order(
            next_oid,
            667,
            "BTC-USDT",
            1,
            OrderType::Ioc,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 1_000_000;

        // 667 sells to 668
        rec.record_submit_order(
            next_oid,
            667,
            "BTC-USDT",
            2,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 1_000_000;
        rec.record_submit_order(
            next_oid,
            668,
            "BTC-USDT",
            1,
            OrderType::Ioc,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 1_000_000;

        // 668 sells back to 666 (closes k=3 wash ring!)
        rec.record_submit_order(
            next_oid,
            668,
            "BTC-USDT",
            2,
            OrderType::Limit,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 1_000_000;
        rec.record_submit_order(
            next_oid,
            666,
            "BTC-USDT",
            1,
            OrderType::Ioc,
            SelfTradePreventionMode::None,
            wash_px,
            25_000_000,
            ts,
        )?;
        next_oid += 1;
        ts += 2_000_000;
    }

    // Phase 3: Large Flash-Crash Sell Sweep by account 1004 (sweeping 6 BTC across multiple bid levels)
    rec.record_submit_order(
        next_oid,
        1004,
        "BTC-USDT",
        2,
        OrderType::Ioc,
        SelfTradePreventionMode::CancelResting,
        mid_px - 25_000_000_000,
        600_000_000, // 6.0 BTC flash-crash dump
        ts,
    )?;

    let (store, _) = rec.build_store(64)?;
    Ok(store)
}

// =============================================================================
// 3-PASS MICRO-OPTIMIZATION BENCHMARK IMPLEMENTATIONS (`02-extreme-overclock-protocol.md`)
// =============================================================================

/// **Pass 1 (Baseline Reference)**: Heap-allocated `HashMap`/`BTreeMap` + `String` keys +
/// eager per-event Merkle tree recomputation on every single delta event.
pub fn reconstruct_pass1_baseline(events: &[IndexedWalEvent]) -> Result<[u8; 32], ChronosError> {
    #[derive(Clone)]
    struct NaiveOrder {
        instrument_id: u32,
        account_id: u64,
        side: u8,
        price_fp: u64,
        qty_fp: u64,
        prio: u64,
    }

    let mut orders: HashMap<u64, NaiveOrder> = HashMap::new();
    let mut bids: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut asks: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut balances: HashMap<u64, BTreeMap<String, (i128, i128)>> = HashMap::new();
    let mut exec_leaves: Vec<[u8; 32]> = Vec::new();
    let empty_hash = *blake3::hash(b"HYPERION_MERKLE_EMPTY_V1").as_bytes();
    let mut latest_root = [0u8; 32];

    for ev in events {
        match &ev.envelope.payload {
            Some(EnvelopePayload::L3Delta(d)) => {
                let side = if d.side == OrderSide::Sell as i32 { 2 } else { 1 };
                let op = L3DeltaOp::try_from(d.op).unwrap_or(L3DeltaOp::Unspecified);
                match op {
                    L3DeltaOp::Add => {
                        if d.remaining_qty_fp > 0 {
                            orders.insert(
                                d.order_id,
                                NaiveOrder {
                                    instrument_id: if d.instrument_id != 0 {
                                        d.instrument_id
                                    } else {
                                        1
                                    },
                                    account_id: d.account_id,
                                    side,
                                    price_fp: d.price_fp,
                                    qty_fp: d.remaining_qty_fp,
                                    prio: d.engine_seq,
                                },
                            );
                            if side == 1 {
                                bids.entry(d.price_fp).or_default().push(d.order_id);
                            } else {
                                asks.entry(d.price_fp).or_default().push(d.order_id);
                            }
                        }
                    }
                    L3DeltaOp::Fill => {
                        if d.remaining_qty_fp == 0 {
                            if let Some(prev) = orders.remove(&d.order_id) {
                                let map = if prev.side == 1 { &mut bids } else { &mut asks };
                                if let Some(q) = map.get_mut(&prev.price_fp) {
                                    q.retain(|&x| x != d.order_id);
                                    if q.is_empty() {
                                        map.remove(&prev.price_fp);
                                    }
                                }
                            }
                        } else if let Some(ord) = orders.get_mut(&d.order_id) {
                            ord.qty_fp = d.remaining_qty_fp;
                        }
                    }
                    L3DeltaOp::Cancel => {
                        if let Some(prev) = orders.remove(&d.order_id) {
                            let map = if prev.side == 1 { &mut bids } else { &mut asks };
                            if let Some(q) = map.get_mut(&prev.price_fp) {
                                q.retain(|&x| x != d.order_id);
                                if q.is_empty() {
                                    map.remove(&prev.price_fp);
                                }
                            }
                        }
                    }
                    L3DeltaOp::Modify => {
                        if d.remaining_qty_fp == 0 {
                            orders.remove(&d.order_id);
                        } else if let Some(ord) = orders.get_mut(&d.order_id) {
                            if ord.price_fp != d.price_fp || d.remaining_qty_fp > ord.qty_fp {
                                ord.prio = d.engine_seq;
                            }
                            ord.price_fp = d.price_fp;
                            ord.qty_fp = d.remaining_qty_fp;
                        }
                    }
                    _ => {}
                }
            }
            Some(EnvelopePayload::Fill(f)) => {
                let notional = {
                    let n = pb_to_u128(f.notional_fp);
                    if n > 0 {
                        n
                    } else {
                        checked_notional_fp(f.price_fp, f.qty_fp)?
                    }
                };
                exec_leaves.push(hash_execution_fill_leaf(&ExecutionFillDigestInput {
                    fill_id: f.fill_id,
                    engine_seq: if f.engine_seq > 0 {
                        f.engine_seq
                    } else {
                        ev.engine_seq
                    },
                    instrument_id: if f.instrument_id != 0 {
                        f.instrument_id
                    } else {
                        1
                    },
                    maker_order_id: f.maker_order_id,
                    taker_order_id: f.taker_order_id,
                    maker_account_id: f.maker_account_id,
                    taker_account_id: f.taker_account_id,
                    aggressor_side: if f.aggressor_side == OrderSide::Sell as i32 {
                        2
                    } else {
                        1
                    },
                    price_fp: f.price_fp,
                    qty_fp: f.qty_fp,
                    notional_fp: notional,
                    maker_fee_fp: f.maker_fee_fp,
                    taker_fee_fp: f.taker_fee_fp,
                    exec_ts_ns: f.exec_ts_ns,
                }));
            }
            Some(EnvelopePayload::JournalEntry(j)) => {
                for line in &j.lines {
                    let after = crate::fixed::pb_to_i128(line.balance_after_fp);
                    let credit = crate::fixed::pb_to_i128(line.credit_fp);
                    let debit = crate::fixed::pb_to_i128(line.debit_fp);
                    if line.sub_account == "AVAILABLE" || line.sub_account == "LOCKED_MARGIN" {
                        let acct = balances.entry(line.account_id).or_default();
                        let entry = acct.entry(line.asset.clone()).or_insert((0, 0));
                        if line.sub_account == "AVAILABLE" {
                            if line.balance_after_fp.is_some() {
                                entry.0 = after;
                            } else {
                                entry.0 += credit - debit;
                            }
                        } else if line.balance_after_fp.is_some() {
                            entry.1 = after;
                        } else {
                            entry.1 += credit - debit;
                        }
                    } else if line.sub_account == "TREASURY_FEE" {
                        let cur = balances
                            .get(&0)
                            .and_then(|m| m.get(&line.asset))
                            .map(|&(av, _)| av)
                            .unwrap_or(0);
                        let next = if line.balance_after_fp.is_some() {
                            after
                        } else {
                            cur + credit - debit
                        };
                        if next != 0 || balances.contains_key(&0) {
                            let acct = balances.entry(0).or_default();
                            let entry = acct.entry(line.asset.clone()).or_insert((0, 0));
                            entry.0 = next;
                        }
                    }
                }
            }
            _ => {}
        }

        // Baseline bottleneck: recompute LOB hash + account leaves eagerly on each delta step
        let mut lob_acc = LobStateAccumulator::new();
        for (&oid, o) in &orders {
            lob_acc.add_lanes(LobStateAccumulator::order_state_lanes(
                o.instrument_id,
                oid,
                o.account_id,
                o.side,
                o.price_fp,
                o.qty_fp,
                o.prio,
            ));
        }
        let mut buckets: Vec<Vec<(u64, [u8; 32])>> = vec![Vec::new(); ACCOUNT_BUCKET_COUNT];
        for (&acc_id, assets) in &balances {
            let leaves: Vec<AssetBalanceLeaf> = assets
                .iter()
                .map(|(k, &(av, lk))| AssetBalanceLeaf {
                    asset: AssetSymbol::from_str_trunc(k),
                    available_fp: av,
                    locked_fp: lk,
                })
                .collect();
            let h = hash_account_leaf(acc_id, &leaves);
            let b_idx = crate::merkle::account_bucket_index(acc_id);
            buckets[b_idx].push((acc_id, h));
        }
        let mut tree = vec![empty_hash; ACCOUNT_BUCKET_COUNT * 2];
        for (b_idx, bucket) in buckets.iter_mut().enumerate() {
            if !bucket.is_empty() {
                bucket.sort_by_key(|&(id, _)| id);
                let hashes: Vec<[u8; 32]> = bucket.iter().map(|&(_, h)| h).collect();
                tree[ACCOUNT_BUCKET_COUNT + b_idx] =
                    build_local_subtree_root_and_proof(&hashes, None, &empty_hash).0;
            }
        }
        for idx in (1..ACCOUNT_BUCKET_COUNT).rev() {
            tree[idx] = hash_merkle_branch(&tree[idx * 2], &tree[idx * 2 + 1]);
        }
        let exec_root = build_local_subtree_root_and_proof(&exec_leaves, None, &empty_hash).0;
        let right = hash_merkle_branch(&lob_acc.commitment_hash(), &exec_root);
        latest_root = hash_merkle_branch(&tree[1], &right);
    }

    Ok(latest_root)
}

/// **Pass 2 (Zero-Copy / Deferred Merkle Refactor)**: Unaligned combined 160-byte `OrderSlot` +
/// `BTreeMap` price levels + coalesced dirty-account Merkle computation at target `T`.
pub fn reconstruct_pass2_zerocopy(events: &[IndexedWalEvent]) -> Result<[u8; 32], ChronosError> {
    #[derive(Clone)]
    struct CombinedOrderSlot {
        order_id: u64,
        account_id: u64,
        instrument_id: u32,
        side: u8,
        active: bool,
        price_fp: u64,
        remaining_qty_fp: u64,
        priority_seq: u64,
        _client_id: [u8; 48],
        _symbol: [u8; 24],
    }

    let mut slots: Vec<CombinedOrderSlot> = Vec::with_capacity(events.len().min(8192));
    let mut free: Vec<u32> = Vec::new();
    let mut index = crate::state::FlatU64Map::with_capacity(4096);
    let mut bids: BTreeMap<u64, u64> = BTreeMap::new();
    let mut asks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut accounts: Vec<crate::state::AccountRecord> = Vec::with_capacity(256);
    let mut acc_index = crate::state::FlatU64Map::with_capacity(256);
    let mut merkle = crate::merkle::StateCommitmentTree::new();

    for ev in events {
        match &ev.envelope.payload {
            Some(EnvelopePayload::L3Delta(d)) => {
                let side = if d.side == OrderSide::Sell as i32 { 2 } else { 1 };
                let op = L3DeltaOp::try_from(d.op).unwrap_or(L3DeltaOp::Unspecified);
                match op {
                    L3DeltaOp::Add if d.remaining_qty_fp > 0 => {
                        let rec = CombinedOrderSlot {
                            order_id: d.order_id,
                            account_id: d.account_id,
                            instrument_id: if d.instrument_id != 0 {
                                d.instrument_id
                            } else {
                                1
                            },
                            side,
                            active: true,
                            price_fp: d.price_fp,
                            remaining_qty_fp: d.remaining_qty_fp,
                            priority_seq: d.engine_seq,
                            _client_id: [0u8; 48],
                            _symbol: [0u8; 24],
                        };
                        let idx = if let Some(s) = free.pop() {
                            slots[s as usize] = rec;
                            s
                        } else {
                            let s = slots.len() as u32;
                            slots.push(rec);
                            s
                        };
                        index.insert(d.order_id, idx);
                        let book = if side == 1 { &mut bids } else { &mut asks };
                        *book.entry(d.price_fp).or_insert(0) += d.remaining_qty_fp;
                    }
                    L3DeltaOp::Fill => {
                        if d.remaining_qty_fp == 0 {
                            if let Some(s) = index.remove(d.order_id) {
                                let prev = &mut slots[s as usize];
                                prev.active = false;
                                let book = if prev.side == 1 { &mut bids } else { &mut asks };
                                if let Some(q) = book.get_mut(&prev.price_fp) {
                                    *q = q.saturating_sub(prev.remaining_qty_fp);
                                    if *q == 0 {
                                        book.remove(&prev.price_fp);
                                    }
                                }
                                free.push(s);
                            }
                        } else if let Some(s) = index.get(d.order_id) {
                            let prev = &mut slots[s as usize];
                            let diff = prev.remaining_qty_fp.saturating_sub(d.remaining_qty_fp);
                            prev.remaining_qty_fp = d.remaining_qty_fp;
                            let book = if prev.side == 1 { &mut bids } else { &mut asks };
                            if let Some(q) = book.get_mut(&prev.price_fp) {
                                *q = q.saturating_sub(diff);
                            }
                        }
                    }
                    L3DeltaOp::Cancel => {
                        if let Some(s) = index.remove(d.order_id) {
                            let prev = &mut slots[s as usize];
                            prev.active = false;
                            let book = if prev.side == 1 { &mut bids } else { &mut asks };
                            if let Some(q) = book.get_mut(&prev.price_fp) {
                                *q = q.saturating_sub(prev.remaining_qty_fp);
                                if *q == 0 {
                                    book.remove(&prev.price_fp);
                                }
                            }
                            free.push(s);
                        }
                    }
                    L3DeltaOp::Modify => {
                        if let Some(s) = index.get(d.order_id) {
                            let prev = &mut slots[s as usize];
                            if prev.price_fp != d.price_fp
                                || d.remaining_qty_fp > prev.remaining_qty_fp
                            {
                                prev.priority_seq = d.engine_seq;
                            }
                            prev.price_fp = d.price_fp;
                            prev.remaining_qty_fp = d.remaining_qty_fp;
                        }
                    }
                    _ => {}
                }
            }
            Some(EnvelopePayload::Fill(f)) => {
                let notional = {
                    let n = pb_to_u128(f.notional_fp);
                    if n > 0 {
                        n
                    } else {
                        checked_notional_fp(f.price_fp, f.qty_fp)?
                    }
                };
                merkle.append_execution_leaf(hash_execution_fill_leaf(&ExecutionFillDigestInput {
                    fill_id: f.fill_id,
                    engine_seq: if f.engine_seq > 0 {
                        f.engine_seq
                    } else {
                        ev.engine_seq
                    },
                    instrument_id: if f.instrument_id != 0 {
                        f.instrument_id
                    } else {
                        1
                    },
                    maker_order_id: f.maker_order_id,
                    taker_order_id: f.taker_order_id,
                    maker_account_id: f.maker_account_id,
                    taker_account_id: f.taker_account_id,
                    aggressor_side: if f.aggressor_side == OrderSide::Sell as i32 {
                        2
                    } else {
                        1
                    },
                    price_fp: f.price_fp,
                    qty_fp: f.qty_fp,
                    notional_fp: notional,
                    maker_fee_fp: f.maker_fee_fp,
                    taker_fee_fp: f.taker_fee_fp,
                    exec_ts_ns: f.exec_ts_ns,
                }));
            }
            Some(EnvelopePayload::JournalEntry(j)) => {
                for line in &j.lines {
                    let after = crate::fixed::pb_to_i128(line.balance_after_fp);
                    let credit = crate::fixed::pb_to_i128(line.credit_fp);
                    let debit = crate::fixed::pb_to_i128(line.debit_fp);
                    if line.sub_account == "AVAILABLE" || line.sub_account == "LOCKED_MARGIN" {
                        let a_idx = if let Some(idx) = acc_index.get(line.account_id) {
                            idx
                        } else {
                            let idx = accounts.len() as u32;
                            accounts.push(crate::state::AccountRecord::new(line.account_id));
                            acc_index.insert(line.account_id, idx);
                            idx
                        };
                        let sym = AssetSymbol::from_str_trunc(&line.asset);
                        let bal = accounts[a_idx as usize].get_or_insert_asset_mut(sym)?;
                        if line.sub_account == "AVAILABLE" {
                            if line.balance_after_fp.is_some() {
                                bal.available_fp = after;
                            } else {
                                bal.available_fp += credit - debit;
                            }
                        } else if line.balance_after_fp.is_some() {
                            bal.locked_fp = after;
                        } else {
                            bal.locked_fp += credit - debit;
                        }
                    } else if line.sub_account == "TREASURY_FEE" {
                        let sym = AssetSymbol::from_str_trunc(&line.asset);
                        let cur = acc_index
                            .get(0)
                            .map(|idx| accounts[idx as usize].get_asset(sym).0)
                            .unwrap_or(0);
                        let next = if line.balance_after_fp.is_some() {
                            after
                        } else {
                            cur + credit - debit
                        };
                        if next != 0 || acc_index.get(0).is_some() {
                            let a_idx = if let Some(idx) = acc_index.get(0) {
                                idx
                            } else {
                                let idx = accounts.len() as u32;
                                accounts.push(crate::state::AccountRecord::new(0));
                                acc_index.insert(0, idx);
                                idx
                            };
                            let bal = accounts[a_idx as usize].get_or_insert_asset_mut(sym)?;
                            bal.available_fp = next;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    for acc in &accounts {
        merkle.upsert_account_leaf(
            acc.account_id,
            hash_account_leaf(acc.account_id, acc.sorted_balances()),
        );
    }
    let mut lob_acc = LobStateAccumulator::new();
    for s in &slots {
        if s.active && s.remaining_qty_fp > 0 {
            lob_acc.add_lanes(LobStateAccumulator::order_state_lanes(
                s.instrument_id,
                s.order_id,
                s.account_id,
                s.side,
                s.price_fp,
                s.remaining_qty_fp,
                s.priority_seq,
            ));
        }
    }
    Ok(merkle.commit_state_root(&lob_acc))
}

/// **Pass 3 (Cache-Line Aligned Final — Production Hot Path)**:
/// Uses `#[repr(C, align(64))] HotOrderSlot` (64-byte L1 cache line hot/cold split),
/// contiguous 32-byte `PriceLevelHead` (`0` BTreeMap heap nodes), pre-warmed `ExchangeState`,
/// and single-shot target Merkle commitment (`0 B/op` steady-state heap allocations!).
pub fn reconstruct_pass3_cache_aligned(
    state: &mut ExchangeState,
    events: &[IndexedWalEvent],
) -> Result<[u8; 32], ChronosError> {
    state.reset_keep_capacity();
    for ev in events {
        state.apply_wal_envelope(&ev.envelope)?;
    }
    Ok(state.compute_state_root())
}
