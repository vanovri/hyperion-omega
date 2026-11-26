#![deny(clippy::float_arithmetic)]

use crate::arena_lob::{OrderKind, Side, StpPolicy};
use crate::engine::{
    hash_idempotency_key, AckStatus, CancelOrderCmd, DeterministicExchangeEngine, EngineSizing,
    FastOrderAck, L3OpCode, MarketRegimeState, ModifyOrderCmd, OutboundRingEvent, RawFillEvent,
    RawL3Delta, SubmitOrderCmd,
};
use crate::fixed::{AssetSymbol, ClientOrderId, InstrumentSymbol};
use crate::ledger::{AccountRiskStatus, CompactJournalEntry, JournalLeg};
use crate::merkle::{hash_execution_fill_leaf, ExecutionFillDigestInput};
use crate::ring::SpscProducer;
use crate::wal::{WalError, WalFrame, WalSyncMode};
use hyperion_proto::{
    market_event_envelope, AccountAssetBalance, AccountStatus, ApplyAccountRiskActionRequest,
    ApplyAccountRiskActionResponse, CancelOrder, CreditDepositRequest, CreditDepositResponse,
    ExecutionFill, GetL2BookSnapshotRequest, GetL2BookSnapshotResponse,
    GetMerkleLedgerProofRequest, GetMerkleLedgerProofResponse, L2BookSnapshot, L3BookDelta,
    LedgerEntryLine, LedgerJournalEntry, MarketEventEnvelope, MarketRegime, MerkleLedgerProof,
    MerkleProofNode, ModifyOrder, OrderAck, OrderSide, OrderType, PriceLevel,
    SelfTradePreventionMode, SubmitOrder, TraceMetadata, VerifySystemLedgerInvariantRequest,
    VerifySystemLedgerInvariantResponse, I128, U128,
};
use prost::Message;
use std::collections::HashMap;
use std::path::Path;

#[inline]
pub fn journal_to_proto(entry: &CompactJournalEntry) -> LedgerJournalEntry {
    let lines = entry
        .legs_slice()
        .iter()
        .map(|leg| LedgerEntryLine {
            account_id: leg.account_id,
            asset: leg.asset.as_str().to_string(),
            sub_account: leg.sub_account.to_string(),
            debit_fp: Some(I128::from(leg.debit_fp)),
            credit_fp: Some(I128::from(leg.credit_fp)),
            balance_after_fp: Some(I128::from(leg.balance_after_fp)),
        })
        .collect();

    LedgerJournalEntry {
        journal_seq: entry.journal_seq,
        engine_seq: entry.engine_seq,
        reference_type: entry.reference_type.to_string(),
        reference_id: entry.reference_id,
        lines,
        ts_ns: entry.ts_ns,
        prev_journal_blake3: entry.prev_journal_blake3.to_vec(),
        journal_blake3: entry.journal_blake3.to_vec(),
    }
}

#[inline]
pub fn fill_to_proto(f: &RawFillEvent, trace: Option<TraceMetadata>) -> ExecutionFill {
    ExecutionFill {
        fill_id: f.fill_id,
        engine_seq: f.engine_seq,
        symbol: f.symbol.as_str().to_string(),
        instrument_id: f.instrument_id,
        maker_order_id: f.maker_order_id,
        taker_order_id: f.taker_order_id,
        maker_account_id: f.maker_account_id,
        taker_account_id: f.taker_account_id,
        aggressor_side: f.aggressor_side as i32,
        price_fp: f.price_fp,
        qty_fp: f.qty_fp,
        notional_fp: Some(U128::from(f.notional_fp)),
        maker_fee_fp: f.maker_fee_fp,
        taker_fee_fp: f.taker_fee_fp,
        exec_ts_ns: f.exec_ts_ns,
        wal_blake3_hash: f.wal_blake3_hash.to_vec(),
        trace,
    }
}

#[inline]
pub fn delta_to_proto(d: &RawL3Delta, trace: Option<TraceMetadata>) -> L3BookDelta {
    L3BookDelta {
        engine_seq: d.engine_seq,
        prev_engine_seq: d.prev_engine_seq,
        symbol: d.symbol.as_str().to_string(),
        instrument_id: d.instrument_id,
        op: d.op as i32,
        side: d.side as i32,
        order_id: d.order_id,
        account_id: d.account_id,
        price_fp: d.price_fp,
        remaining_qty_fp: d.remaining_qty_fp,
        delta_qty_fp: d.delta_qty_fp,
        event_ts_ns: d.event_ts_ns,
        state_root_blake3: d.state_root_blake3.to_vec(),
        trace,
    }
}

#[inline]
fn map_side(side: i32) -> Option<Side> {
    match OrderSide::try_from(side).ok()? {
        OrderSide::Buy => Some(Side::Buy),
        OrderSide::Sell => Some(Side::Sell),
        OrderSide::Unspecified => None,
    }
}

#[inline]
fn map_order_kind(ot: i32) -> Option<OrderKind> {
    match OrderType::try_from(ot).ok()? {
        OrderType::Limit => Some(OrderKind::Limit),
        OrderType::Market => Some(OrderKind::Market),
        OrderType::Ioc => Some(OrderKind::Ioc),
        OrderType::Fok => Some(OrderKind::Fok),
        OrderType::PostOnly => Some(OrderKind::PostOnly),
        OrderType::Unspecified => Some(OrderKind::Limit),
    }
}

#[inline]
fn map_stp(stp: i32) -> StpPolicy {
    match SelfTradePreventionMode::try_from(stp).unwrap_or(SelfTradePreventionMode::None) {
        SelfTradePreventionMode::CancelResting => StpPolicy::CancelResting,
        SelfTradePreventionMode::CancelAggressing => StpPolicy::CancelAggressing,
        SelfTradePreventionMode::CancelBoth => StpPolicy::CancelBoth,
        SelfTradePreventionMode::Unspecified | SelfTradePreventionMode::None => StpPolicy::None,
    }
}

/// Persists all timeline events emitted during a single atomic command plus its terminal `MarketEventEnvelope`
/// into the Blake3 hash-chained `HYPWAL01` log.
fn persist_timeline_to_wal(
    engine: &mut DeterministicExchangeEngine,
    trace: &Option<TraceMetadata>,
) -> Result<[u8; 32], WalError> {
    let timeline: Vec<OutboundRingEvent> = engine.last_timeline().to_vec();
    let mut last_hash = engine.wal_mut().last_blake3();

    for ev in &timeline {
        let (seq, ts, env) = match ev {
            OutboundRingEvent::Journal { symbol, entry } => (
                entry.engine_seq,
                entry.ts_ns,
                MarketEventEnvelope {
                    engine_seq: entry.engine_seq,
                    symbol: symbol.as_str().to_string(),
                    ts_ns: entry.ts_ns,
                    payload: Some(market_event_envelope::Payload::JournalEntry(
                        journal_to_proto(entry),
                    )),
                },
            ),
            OutboundRingEvent::Fill(f) => (
                f.engine_seq,
                f.exec_ts_ns,
                MarketEventEnvelope {
                    engine_seq: f.engine_seq,
                    symbol: f.symbol.as_str().to_string(),
                    ts_ns: f.exec_ts_ns,
                    payload: Some(market_event_envelope::Payload::Fill(fill_to_proto(
                        f,
                        trace.clone(),
                    ))),
                },
            ),
            OutboundRingEvent::L3Delta(d) => (
                d.engine_seq,
                d.event_ts_ns,
                MarketEventEnvelope {
                    engine_seq: d.engine_seq,
                    symbol: d.symbol.as_str().to_string(),
                    ts_ns: d.event_ts_ns,
                    payload: Some(market_event_envelope::Payload::L3Delta(delta_to_proto(
                        d,
                        trace.clone(),
                    ))),
                },
            ),
        };

        let mut buf = std::mem::take(engine.scratch_prost_mut());
        buf.clear();
        env.encode(&mut buf)
            .expect("MarketEventEnvelope encode into Vec<u8> infallible");
        last_hash = engine.wal_mut().append_frame(seq, ts, &buf)?;
        *engine.scratch_prost_mut() = buf;
    }

    Ok(last_hash)
}

fn persist_ack_frame_to_wal(
    engine: &mut DeterministicExchangeEngine,
    ack: &mut OrderAck,
) -> Result<[u8; 32], WalError> {
    let seq = ack.engine_seq;
    let ts = ack.engine_ts_ns;
    let env = MarketEventEnvelope {
        engine_seq: seq,
        symbol: ack.symbol.clone(),
        ts_ns: ts,
        payload: Some(market_event_envelope::Payload::OrderAck(ack.clone())),
    };
    let mut buf = std::mem::take(engine.scratch_prost_mut());
    buf.clear();
    env.encode(&mut buf)
        .expect("OrderAck envelope encode infallible");
    let frame_hash = engine.wal_mut().append_frame(seq, ts, &buf)?;
    *engine.scratch_prost_mut() = buf;
    ack.wal_blake3_hash = frame_hash.to_vec();
    for fill in &mut ack.fills {
        if fill.wal_blake3_hash.is_empty() || fill.wal_blake3_hash.iter().all(|&b| b == 0) {
            fill.wal_blake3_hash = frame_hash.to_vec();
        }
    }
    let _ = engine.maybe_auto_snapshot()?;
    Ok(frame_hash)
}

fn fast_ack_to_proto(
    engine: &DeterministicExchangeEngine,
    fast: &FastOrderAck,
    trace: Option<TraceMetadata>,
) -> OrderAck {
    let fills = engine
        .last_fills()
        .iter()
        .map(|f| fill_to_proto(f, trace.clone()))
        .collect();

    OrderAck {
        engine_seq: fast.engine_seq,
        order_id: fast.order_id,
        client_order_id: fast.client_order_id.as_str().to_string(),
        account_id: fast.account_id,
        symbol: fast.symbol.as_str().to_string(),
        instrument_id: fast.instrument_id,
        status: fast.status as i32,
        reject_reason: fast.reject_reason as i32,
        reject_detail: fast.reject_detail.to_string(),
        filled_qty_fp: fast.filled_qty_fp,
        remaining_qty_fp: fast.remaining_qty_fp,
        avg_fill_price_fp: fast.avg_fill_price_fp,
        engine_ts_ns: fast.engine_ts_ns,
        wal_blake3_hash: fast.wal_blake3_hash.to_vec(),
        fills,
        trace,
    }
}

impl DeterministicExchangeEngine {
    /// Executes `SubmitOrder` and commits all resulting double-entry journals, fills, L3 deltas, and `OrderAck`
    /// to the Blake3 hash-chained `HYPWAL01` log.
    pub fn execute_submit_proto(&mut self, req: SubmitOrder) -> Result<OrderAck, WalError> {
        let Some(side) = map_side(req.side) else {
            let fast = self.submit_order_fast(SubmitOrderCmd {
                client_order_id: ClientOrderId::from_str_trunc(&req.client_order_id),
                account_id: req.account_id,
                instrument_id: req.instrument_id,
                symbol: InstrumentSymbol::from_str_trunc(&req.symbol),
                side: Side::Buy,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 0,
                qty_fp: 0,
                max_slippage_bps: 0,
                ts_ns: req.client_ts_ns,
                idempotency_hash: None,
            });
            let mut ack = fast_ack_to_proto(self, &fast, req.trace);
            ack.status = AckStatus::Rejected as i32;
            persist_ack_frame_to_wal(self, &mut ack)?;
            return Ok(ack);
        };

        let Some(kind) = map_order_kind(req.order_type) else {
            let fast = self.submit_order_fast(SubmitOrderCmd {
                client_order_id: ClientOrderId::from_str_trunc(&req.client_order_id),
                account_id: req.account_id,
                instrument_id: req.instrument_id,
                symbol: InstrumentSymbol::from_str_trunc(&req.symbol),
                side,
                kind: OrderKind::Limit,
                stp_mode: StpPolicy::None,
                price_fp: 0,
                qty_fp: 0,
                max_slippage_bps: 0,
                ts_ns: req.client_ts_ns,
                idempotency_hash: None,
            });
            let mut ack = fast_ack_to_proto(self, &fast, req.trace);
            persist_ack_frame_to_wal(self, &mut ack)?;
            return Ok(ack);
        };

        let cmd = SubmitOrderCmd {
            client_order_id: ClientOrderId::from_str_trunc(&req.client_order_id),
            account_id: req.account_id,
            instrument_id: req.instrument_id,
            symbol: InstrumentSymbol::from_str_trunc(&req.symbol),
            side,
            kind,
            stp_mode: map_stp(req.stp_mode),
            price_fp: req.price_fp,
            qty_fp: req.qty_fp,
            max_slippage_bps: req.max_slippage_bps,
            ts_ns: req.client_ts_ns,
            idempotency_hash: hash_idempotency_key(req.account_id, &req.idempotency_key),
        };

        let fast = self.submit_order_fast(cmd);
        persist_timeline_to_wal(self, &req.trace)?;
        let mut ack = fast_ack_to_proto(self, &fast, req.trace);
        persist_ack_frame_to_wal(self, &mut ack)?;
        Ok(ack)
    }

    pub fn execute_cancel_proto(&mut self, req: CancelOrder) -> Result<OrderAck, WalError> {
        let cmd = CancelOrderCmd {
            order_id: req.order_id,
            client_order_id: ClientOrderId::from_str_trunc(&req.client_order_id),
            account_id: req.account_id,
            instrument_id: req.instrument_id,
            symbol: InstrumentSymbol::from_str_trunc(&req.symbol),
            ts_ns: req.client_ts_ns,
            idempotency_hash: hash_idempotency_key(req.account_id, &req.idempotency_key),
        };
        let fast = self.cancel_order_fast(cmd);
        persist_timeline_to_wal(self, &req.trace)?;
        let mut ack = fast_ack_to_proto(self, &fast, req.trace);
        persist_ack_frame_to_wal(self, &mut ack)?;
        Ok(ack)
    }

    pub fn execute_modify_proto(&mut self, req: ModifyOrder) -> Result<OrderAck, WalError> {
        let cmd = ModifyOrderCmd {
            order_id: req.order_id,
            client_order_id: ClientOrderId::from_str_trunc(&req.client_order_id),
            account_id: req.account_id,
            instrument_id: req.instrument_id,
            symbol: InstrumentSymbol::from_str_trunc(&req.symbol),
            new_price_fp: req.new_price_fp,
            new_qty_fp: req.new_qty_fp,
            ts_ns: req.client_ts_ns,
            idempotency_hash: hash_idempotency_key(req.account_id, &req.idempotency_key),
        };
        let fast = self.modify_order_fast(cmd);
        persist_timeline_to_wal(self, &req.trace)?;
        let mut ack = fast_ack_to_proto(self, &fast, req.trace);
        persist_ack_frame_to_wal(self, &mut ack)?;
        Ok(ack)
    }

    pub fn execute_deposit_proto(
        &mut self,
        req: CreditDepositRequest,
    ) -> Result<CreditDepositResponse, String> {
        let amount = req.amount_fp.map(u128::from).unwrap_or(0);
        let asset = AssetSymbol::from_str_trunc(&req.asset);
        let (seq, journal, leaf, root) = self
            .credit_deposit_fast(req.account_id, asset, amount, req.ts_ns)
            .map_err(|e| e.to_string())?;
        persist_timeline_to_wal(self, &req.trace).map_err(|e| e.to_string())?;
        let _ = self.maybe_auto_snapshot().map_err(|e| e.to_string())?;

        let total = leaf.available_fp.saturating_add(leaf.locked_fp);
        Ok(CreditDepositResponse {
            engine_seq: seq,
            journal_seq: journal.journal_seq,
            updated_balance: Some(AccountAssetBalance {
                asset: req.asset,
                available_fp: Some(I128::from(leaf.available_fp)),
                locked_fp: Some(I128::from(leaf.locked_fp)),
                total_fp: Some(I128::from(total)),
            }),
            state_root_blake3: root.to_vec(),
        })
    }

    pub fn execute_risk_action_proto(
        &mut self,
        req: ApplyAccountRiskActionRequest,
    ) -> Result<ApplyAccountRiskActionResponse, String> {
        let new_status = match AccountStatus::try_from(req.new_status).ok() {
            Some(AccountStatus::Active) => Some(AccountRiskStatus::Active),
            Some(AccountStatus::Throttled) => Some(AccountRiskStatus::Throttled),
            Some(AccountStatus::Frozen) => Some(AccountRiskStatus::Frozen),
            Some(AccountStatus::LiquidationOnly) => Some(AccountRiskStatus::LiquidationOnly),
            _ => None,
        };
        let regime = match MarketRegime::try_from(req.symbol_regime_override).ok() {
            Some(MarketRegime::Normal) => Some(MarketRegimeState::Normal),
            Some(MarketRegime::VolatilityGuard) => Some(MarketRegimeState::VolatilityGuard),
            Some(MarketRegime::PostOnlyAuction) => Some(MarketRegimeState::PostOnlyAuction),
            Some(MarketRegime::Halted) => Some(MarketRegimeState::Halted),
            _ => None,
        };
        let ts_in = req.trace.as_ref().map(|t| t.ingest_ts_ns).unwrap_or(0);
        let (seq, canceled_cnt, eff_status, root) = self
            .apply_risk_action_fast(
                req.account_id,
                new_status,
                regime,
                &req.target_symbol,
                req.cancel_all_open_orders,
                ts_in,
            )
            .map_err(|e| e.to_string())?;

        persist_timeline_to_wal(self, &req.trace).map_err(|e| e.to_string())?;

        // Persist terminal risk control marker as a deterministic `OrderAck` WAL frame
        let status_code = new_status.map(|s| s as u8).unwrap_or(0);
        let regime_code = regime.map(|r| r as u8).unwrap_or(0);
        let mut marker_ack = OrderAck {
            engine_seq: seq,
            order_id: 0,
            client_order_id: format!(
                "RISK_ACTION:{}:{}:{}",
                req.account_id, status_code, regime_code
            ),
            account_id: req.account_id,
            symbol: req.target_symbol.clone(),
            instrument_id: 0,
            status: AckStatus::Accepted as i32,
            reject_reason: 1,
            reject_detail: req.reason,
            filled_qty_fp: 0,
            remaining_qty_fp: 0,
            avg_fill_price_fp: 0,
            engine_ts_ns: self.last_ts_ns_val(),
            wal_blake3_hash: Vec::new(),
            fills: Vec::new(),
            trace: req.trace,
        };
        persist_ack_frame_to_wal(self, &mut marker_ack).map_err(|e| e.to_string())?;

        Ok(ApplyAccountRiskActionResponse {
            applied: true,
            engine_seq: seq,
            canceled_orders_count: canceled_cnt,
            effective_status: eff_status as i32,
            state_root_blake3: root.to_vec(),
        })
    }

    pub fn query_l2_snapshot_proto(
        &mut self,
        req: GetL2BookSnapshotRequest,
    ) -> Option<GetL2BookSnapshotResponse> {
        let sym = if req.symbol.is_empty() {
            "BTC-USDT"
        } else {
            &req.symbol
        };
        let (seq, inst_id, sym_inline, bids, asks, best_bid, best_ask, mid, spread, ts, root) =
            self.l2_snapshot(sym, req.depth as usize)?;
        Some(GetL2BookSnapshotResponse {
            snapshot: Some(L2BookSnapshot {
                engine_seq: seq,
                symbol: sym_inline.as_str().to_string(),
                instrument_id: inst_id,
                bids: bids
                    .into_iter()
                    .map(|l| PriceLevel {
                        price_fp: l.price_fp,
                        total_qty_fp: l.total_qty_fp,
                        order_count: l.order_count,
                    })
                    .collect(),
                asks: asks
                    .into_iter()
                    .map(|l| PriceLevel {
                        price_fp: l.price_fp,
                        total_qty_fp: l.total_qty_fp,
                        order_count: l.order_count,
                    })
                    .collect(),
                best_bid_fp: best_bid,
                best_ask_fp: best_ask,
                mid_price_fp: mid,
                spread_fp: spread,
                snapshot_ts_ns: ts,
                state_root_blake3: root.to_vec(),
            }),
        })
    }

    pub fn query_merkle_proof_proto(
        &mut self,
        req: GetMerkleLedgerProofRequest,
    ) -> GetMerkleLedgerProofResponse {
        let (seq, balances, leaf_hash, state_root, proof_steps, quote_totals, balanced) =
            self.generate_account_merkle_proof(req.account_id, "USDT");
        let user_equity = quote_totals
            .total_available_fp
            .saturating_add(quote_totals.total_locked_fp);
        GetMerkleLedgerProofResponse {
            proof: Some(MerkleLedgerProof {
                engine_seq: seq,
                account_id: req.account_id,
                balances: balances
                    .into_iter()
                    .map(|b| AccountAssetBalance {
                        asset: b.asset.as_str().to_string(),
                        available_fp: Some(I128::from(b.available_fp)),
                        locked_fp: Some(I128::from(b.locked_fp)),
                        total_fp: Some(I128::from(b.available_fp.saturating_add(b.locked_fp))),
                    })
                    .collect(),
                leaf_blake3_hash: leaf_hash.to_vec(),
                state_root_blake3: state_root.to_vec(),
                proof_path: proof_steps
                    .into_iter()
                    .map(|s| MerkleProofNode {
                        sibling_hash: s.sibling_hash.to_vec(),
                        is_left_sibling: s.is_left_sibling,
                        depth: s.depth,
                    })
                    .collect(),
                system_total_deposits_fp: Some(U128::from(quote_totals.total_deposits_fp)),
                system_total_user_equity_fp: Some(U128::from(user_equity)),
                treasury_fee_pool_fp: Some(I128::from(quote_totals.treasury_fee_pool_fp)),
                double_entry_balanced: balanced,
                proof_ts_ns: self.last_ts_ns_val(),
            }),
        }
    }

    pub fn query_system_invariant_proto(
        &mut self,
        req: VerifySystemLedgerInvariantRequest,
    ) -> VerifySystemLedgerInvariantResponse {
        let asset_q = if req.asset.is_empty() {
            "USDT"
        } else {
            &req.asset
        };
        let (seq, totals, inv_ok, uncrossed, root) = self.verify_system_invariants(asset_q);
        VerifySystemLedgerInvariantResponse {
            engine_seq: seq,
            asset: asset_q.to_string(),
            total_deposits_fp: Some(U128::from(totals.total_deposits_fp)),
            total_available_fp: Some(U128::from(totals.total_available_fp)),
            total_locked_fp: Some(U128::from(totals.total_locked_fp)),
            treasury_fee_pool_fp: Some(I128::from(totals.treasury_fee_pool_fp)),
            invariant_holds: inv_ok,
            crossed_book_free: uncrossed,
            state_root_blake3: root.to_vec(),
            verified_ts_ns: self.last_ts_ns_val(),
        }
    }

    /// Opens `${data_dir}` (`HYPSNP01` snapshot + `HYPWAL01` tail WAL), verifies CRC32C & Blake3 chains,
    /// truncates any corrupted incomplete tail bytes cleanly, and reconstructs 100% bit-identical state!
    pub fn recover_from_data_dir(
        sizing: EngineSizing,
        data_dir: impl AsRef<Path>,
        sync_mode: WalSyncMode,
        outbound_ring: Option<SpscProducer<OutboundRingEvent>>,
    ) -> Result<(Self, usize, bool), WalError> {
        let mut order_quote_locks: HashMap<u64, i128> = HashMap::new();
        let mut pending_fill_quote_debits: HashMap<u64, i128> = HashMap::new();

        Self::open_and_recover_with(sizing, data_dir, sync_mode, outbound_ring, |eng, frame| {
            // Seed `order_quote_locks` lazily from any snapshot-loaded resting Buy orders on first tail frame
            if order_quote_locks.is_empty() {
                for b in eng.books() {
                    for o in b.export_resting_orders_deterministic() {
                        if o.side == Side::Buy && o.locked_quote_fp > 0 {
                            order_quote_locks.insert(o.order_id, o.locked_quote_fp);
                        }
                    }
                }
            }
            apply_replayed_wal_frame(
                eng,
                frame,
                &mut order_quote_locks,
                &mut pending_fill_quote_debits,
            )
        })
    }

    /// Replays a slice of `WalFrame`s directly into a fresh in-memory engine (used by deterministic WAL replay tests & Chronos).
    pub fn replay_wal_frames(&mut self, frames: &[WalFrame]) -> Result<(), WalError> {
        let mut order_quote_locks: HashMap<u64, i128> = HashMap::new();
        let mut pending_fill_quote_debits: HashMap<u64, i128> = HashMap::new();
        for b in self.books() {
            for o in b.export_resting_orders_deterministic() {
                if o.side == Side::Buy && o.locked_quote_fp > 0 {
                    order_quote_locks.insert(o.order_id, o.locked_quote_fp);
                }
            }
        }
        for frame in frames {
            apply_replayed_wal_frame(
                self,
                frame,
                &mut order_quote_locks,
                &mut pending_fill_quote_debits,
            )?;
            self.set_replayed_progress(frame.engine_seq, frame.ts_ns, frame.frame_blake3);
        }
        self.state_root();
        Ok(())
    }
}

fn intern_sub_account(s: &str) -> &'static str {
    match s {
        "AVAILABLE" => "AVAILABLE",
        "LOCKED_MARGIN" => "LOCKED_MARGIN",
        "TREASURY_FEE" => "TREASURY_FEE",
        "EXCHANGE_VAULT" => "EXCHANGE_VAULT",
        _ => "AVAILABLE",
    }
}

fn intern_ref_type(s: &str) -> &'static str {
    match s {
        "DEPOSIT" => "DEPOSIT",
        "ORDER_HOLD" => "ORDER_HOLD",
        "TRADE_SETTLEMENT" => "TRADE_SETTLEMENT",
        "MAKER_FILL_RESIDUAL_RELEASE" => "MAKER_FILL_RESIDUAL_RELEASE",
        "TAKER_PRICE_IMPROVEMENT_RELEASE" => "TAKER_PRICE_IMPROVEMENT_RELEASE",
        "STP_TAKER_RELEASE" => "STP_TAKER_RELEASE",
        "IOC_MARKET_REMAINDER_RELEASE" => "IOC_MARKET_REMAINDER_RELEASE",
        "ORDER_CANCEL_RELEASE" => "ORDER_CANCEL_RELEASE",
        "ORDER_MODIFY_SIZE_DOWN_RELEASE" => "ORDER_MODIFY_SIZE_DOWN_RELEASE",
        "ORDER_MODIFY_CANCEL_OLD" => "ORDER_MODIFY_CANCEL_OLD",
        "STP_CANCEL_RESTING" => "STP_CANCEL_RESTING",
        "STP_CANCEL_BOTH_RESTING" => "STP_CANCEL_BOTH_RESTING",
        "RISK_ACTION_CANCEL" => "RISK_ACTION_CANCEL",
        _ => "JOURNAL",
    }
}

fn apply_replayed_wal_frame(
    eng: &mut DeterministicExchangeEngine,
    frame: &WalFrame,
    order_quote_locks: &mut HashMap<u64, i128>,
    pending_fill_quote_debits: &mut HashMap<u64, i128>,
) -> Result<(), WalError> {
    let env = MarketEventEnvelope::decode(frame.payload.as_slice())
        .map_err(|e| WalError::SnapshotCodec(format!("MarketEventEnvelope decode error: {e}")))?;

    match env.payload {
        Some(market_event_envelope::Payload::JournalEntry(j)) => {
            let mut legs = [JournalLeg {
                account_id: 0,
                asset: AssetSymbol::empty(),
                sub_account: "",
                debit_fp: 0,
                credit_fp: 0,
                balance_after_fp: 0,
            }; 8];
            let count = j.lines.len().min(8);
            for (idx, line) in j.lines.iter().take(8).enumerate() {
                let d = line.debit_fp.map(i128::from).unwrap_or(0);
                let c = line.credit_fp.map(i128::from).unwrap_or(0);
                let aft = line.balance_after_fp.map(i128::from).unwrap_or(0);
                legs[idx] = JournalLeg {
                    account_id: line.account_id,
                    asset: AssetSymbol::from_str_trunc(&line.asset),
                    sub_account: intern_sub_account(&line.sub_account),
                    debit_fp: d,
                    credit_fp: c,
                    balance_after_fp: aft,
                };

                // Track exact per-order quote lock changes across any quote asset
                if line.sub_account == "LOCKED_MARGIN" {
                    if j.reference_type == "ORDER_HOLD" && c > 0 {
                        *order_quote_locks.entry(j.reference_id).or_insert(0) += c;
                    } else if j.reference_type == "TRADE_SETTLEMENT" && idx >= 2 && d > 0 {
                        pending_fill_quote_debits.insert(j.reference_id, d);
                    } else if j.reference_type != "TRADE_SETTLEMENT" && d > 0 {
                        if let Some(rem) = order_quote_locks.get_mut(&j.reference_id) {
                            *rem = (*rem - d).max(0);
                        }
                    }
                }
            }
            let mut prev_h = [0u8; 32];
            if j.prev_journal_blake3.len() == 32 {
                prev_h.copy_from_slice(&j.prev_journal_blake3);
            }
            let mut cur_h = [0u8; 32];
            if j.journal_blake3.len() == 32 {
                cur_h.copy_from_slice(&j.journal_blake3);
            }
            let compact = CompactJournalEntry {
                journal_seq: j.journal_seq,
                engine_seq: j.engine_seq,
                reference_type: intern_ref_type(&j.reference_type),
                reference_id: j.reference_id,
                legs,
                leg_count: count as u8,
                ts_ns: j.ts_ns,
                prev_journal_blake3: prev_h,
                journal_blake3: cur_h,
            };
            eng.replay_apply_journal(&compact)
                .map_err(|e| WalError::SnapshotCodec(e.to_string()))?;
        }
        Some(market_event_envelope::Payload::Fill(f)) => {
            if let Some(buyer_debit) = pending_fill_quote_debits.remove(&f.fill_id) {
                let buyer_order_id = if f.aggressor_side == OrderSide::Buy as i32 {
                    f.taker_order_id
                } else {
                    f.maker_order_id
                };
                if let Some(lock_rem) = order_quote_locks.get_mut(&buyer_order_id) {
                    *lock_rem = (*lock_rem - buyer_debit).max(0);
                }
            }
            let leaf = hash_execution_fill_leaf(&ExecutionFillDigestInput {
                fill_id: f.fill_id,
                engine_seq: f.engine_seq,
                instrument_id: f.instrument_id,
                maker_order_id: f.maker_order_id,
                taker_order_id: f.taker_order_id,
                maker_account_id: f.maker_account_id,
                taker_account_id: f.taker_account_id,
                aggressor_side: f.aggressor_side as u8,
                price_fp: f.price_fp,
                qty_fp: f.qty_fp,
                notional_fp: f.notional_fp.map(u128::from).unwrap_or(0),
                maker_fee_fp: f.maker_fee_fp,
                taker_fee_fp: f.taker_fee_fp,
                exec_ts_ns: f.exec_ts_ns,
            });
            eng.replay_append_fill(f.fill_id, f.maker_order_id, f.taker_order_id, leaf);
        }
        Some(market_event_envelope::Payload::L3Delta(d)) => {
            let side = map_side(d.side).unwrap_or(Side::Buy);
            let op = match d.op {
                1 => L3OpCode::Add,
                2 => L3OpCode::Modify,
                3 => L3OpCode::Cancel,
                4 => L3OpCode::Fill,
                _ => L3OpCode::Clear,
            };
            let locked_q = if side == Side::Buy {
                order_quote_locks.get(&d.order_id).copied().unwrap_or(0)
            } else {
                0
            };
            if matches!(op, L3OpCode::Cancel)
                || (matches!(op, L3OpCode::Fill) && d.remaining_qty_fp == 0)
            {
                order_quote_locks.remove(&d.order_id);
            }
            eng.replay_apply_l3_delta(
                d.instrument_id,
                &d.symbol,
                d.engine_seq,
                op,
                side,
                d.order_id,
                d.account_id,
                d.price_fp,
                d.remaining_qty_fp,
                locked_q,
                d.event_ts_ns,
            )?;
        }
        Some(market_event_envelope::Payload::OrderAck(ack)) => {
            eng.replay_apply_order_ack(&ack)?;
        }
        Some(market_event_envelope::Payload::L2Snapshot(_)) | None => {}
    }

    Ok(())
}
