//! Deterministic Replicated Double-Entry Ledger State Machine for HYPERION CH12.
//! Enforces strict fixed-point integer arithmetic (`1e8` scale, zero floats) and the
//! global Double-Entry Conservation Law (`00-hyperion-contract.md` §5.1 & §5.2):
//! `Sum_u(Available_{u,A} + Locked_{u,A}) + TreasuryFeePool_A == TotalDeposits_A - TotalWithdrawals_A`.

use crate::types::{
    CommandApplyReceipt, LedgerCommand, LedgerReadQuery, LedgerReadResult, LogIndex,
    FP_SCALE_U128, PPM_SCALE_I128,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};

pub const IDEMPOTENCY_WINDOW_CAP: usize = 4_096;
const STATE_MACHINE_MAGIC: &[u8; 8] = b"HYPSM01\0";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBucket {
    pub available_fp: i128,
    pub locked_fp: i128,
}

impl AssetBucket {
    #[inline]
    pub fn total_fp(&self) -> Option<i128> {
        self.available_fp.checked_add(self.locked_fp)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplicatedLedgerStateMachine {
    pub last_applied_index: LogIndex,
    pub journal_seq: u64,
    /// Canonical deterministic sorted map `(account_id, asset) -> AssetBucket`.
    pub accounts: BTreeMap<(u64, String), AssetBucket>,
    pub total_deposits: BTreeMap<String, u128>,
    pub total_withdrawals: BTreeMap<String, u128>,
    pub treasury_fee_pool: BTreeMap<String, i128>,
    /// Bounded O(1) fixed-capacity idempotency registry preventing duplicate execution on retry.
    pub idempotency_log: HashMap<String, CommandApplyReceipt>,
    pub idempotency_fifo: VecDeque<String>,
    cached_state_root: [u8; 32],
}

impl ReplicatedLedgerStateMachine {
    pub fn new() -> Self {
        let mut sm = Self {
            idempotency_log: HashMap::with_capacity(IDEMPOTENCY_WINDOW_CAP * 2),
            idempotency_fifo: VecDeque::with_capacity(IDEMPOTENCY_WINDOW_CAP + 64),
            ..Self::default()
        };
        sm.recompute_state_root();
        sm
    }

    #[inline]
    pub fn state_root_blake3(&self) -> [u8; 32] {
        self.cached_state_root
    }

    /// Canonical, fixed-width little-endian binary state-machine snapshot encoder (`HYPSM01\0`).
    /// Eliminates JSON non-string map-key hazards and guarantees constant steady-state size.
    pub fn serialize_deterministic(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            64 + self.accounts.len() * 48 + self.idempotency_fifo.len() * 64,
        );
        out.extend_from_slice(STATE_MACHINE_MAGIC);
        out.extend_from_slice(&self.last_applied_index.to_le_bytes());
        out.extend_from_slice(&self.journal_seq.to_le_bytes());

        // Accounts in canonical sorted BTreeMap order
        out.extend_from_slice(&(self.accounts.len() as u32).to_le_bytes());
        for ((acct_id, asset), bucket) in &self.accounts {
            out.extend_from_slice(&acct_id.to_le_bytes());
            put_str(&mut out, asset);
            out.extend_from_slice(&bucket.available_fp.to_le_bytes());
            out.extend_from_slice(&bucket.locked_fp.to_le_bytes());
        }

        // System asset totals in canonical sorted order
        out.extend_from_slice(&(self.total_deposits.len() as u32).to_le_bytes());
        for (asset, dep) in &self.total_deposits {
            put_str(&mut out, asset);
            out.extend_from_slice(&dep.to_le_bytes());
        }
        out.extend_from_slice(&(self.total_withdrawals.len() as u32).to_le_bytes());
        for (asset, wd) in &self.total_withdrawals {
            put_str(&mut out, asset);
            out.extend_from_slice(&wd.to_le_bytes());
        }
        out.extend_from_slice(&(self.treasury_fee_pool.len() as u32).to_le_bytes());
        for (asset, fee) in &self.treasury_fee_pool {
            put_str(&mut out, asset);
            out.extend_from_slice(&fee.to_le_bytes());
        }

        // Idempotency receipts strictly in deterministic FIFO order!
        out.extend_from_slice(&(self.idempotency_fifo.len() as u32).to_le_bytes());
        for key in &self.idempotency_fifo {
            if let Some(r) = self.idempotency_log.get(key) {
                put_str(&mut out, &r.idempotency_key);
                out.extend_from_slice(&r.journal_seq.to_le_bytes());
                out.push(u8::from(r.applied));
                out.push(u8::from(r.duplicate_noop));
                match &r.reject_reason {
                    Some(msg) => {
                        out.push(1);
                        put_str(&mut out, msg);
                    }
                    None => out.push(0),
                }
                out.extend_from_slice(&r.state_root_blake3);
            }
        }
        out
    }

    pub fn deserialize_deterministic(bytes: &[u8]) -> Result<Self, String> {
        let mut cur = BinCursor::new(bytes);
        let magic = cur.read_exact(8)?;
        if magic != STATE_MACHINE_MAGIC {
            return Err("invalid HYPSM01 state machine snapshot magic".to_string());
        }
        let last_applied_index = cur.read_u64()?;
        let journal_seq = cur.read_u64()?;

        let acct_cnt = cur.read_u32()? as usize;
        let mut accounts = BTreeMap::new();
        for _ in 0..acct_cnt {
            let acct_id = cur.read_u64()?;
            let asset = cur.read_str()?;
            let available_fp = cur.read_i128()?;
            let locked_fp = cur.read_i128()?;
            accounts.insert(
                (acct_id, asset),
                AssetBucket {
                    available_fp,
                    locked_fp,
                },
            );
        }

        let dep_cnt = cur.read_u32()? as usize;
        let mut total_deposits = BTreeMap::new();
        for _ in 0..dep_cnt {
            let asset = cur.read_str()?;
            let val = cur.read_u128()?;
            total_deposits.insert(asset, val);
        }

        let wd_cnt = cur.read_u32()? as usize;
        let mut total_withdrawals = BTreeMap::new();
        for _ in 0..wd_cnt {
            let asset = cur.read_str()?;
            let val = cur.read_u128()?;
            total_withdrawals.insert(asset, val);
        }

        let fee_cnt = cur.read_u32()? as usize;
        let mut treasury_fee_pool = BTreeMap::new();
        for _ in 0..fee_cnt {
            let asset = cur.read_str()?;
            let val = cur.read_i128()?;
            treasury_fee_pool.insert(asset, val);
        }

        let idem_cnt = cur.read_u32()? as usize;
        let mut idempotency_log = HashMap::with_capacity((IDEMPOTENCY_WINDOW_CAP * 2).max(idem_cnt));
        let mut idempotency_fifo = VecDeque::with_capacity((IDEMPOTENCY_WINDOW_CAP + 64).max(idem_cnt));
        for _ in 0..idem_cnt {
            let key = cur.read_str()?;
            let r_seq = cur.read_u64()?;
            let applied = cur.read_u8()? != 0;
            let duplicate_noop = cur.read_u8()? != 0;
            let has_reason = cur.read_u8()? != 0;
            let reject_reason = if has_reason {
                Some(cur.read_str()?)
            } else {
                None
            };
            let root_slice = cur.read_exact(32)?;
            let mut state_root_blake3 = [0u8; 32];
            state_root_blake3.copy_from_slice(root_slice);

            idempotency_fifo.push_back(key.clone());
            idempotency_log.insert(
                key.clone(),
                CommandApplyReceipt {
                    idempotency_key: key,
                    journal_seq: r_seq,
                    applied,
                    duplicate_noop,
                    reject_reason,
                    state_root_blake3,
                },
            );
        }

        let mut sm = Self {
            last_applied_index,
            journal_seq,
            accounts,
            total_deposits,
            total_withdrawals,
            treasury_fee_pool,
            idempotency_log,
            idempotency_fifo,
            cached_state_root: [0u8; 32],
        };
        sm.recompute_state_root();
        Ok(sm)
    }

    pub fn apply_journal_batch(
        &mut self,
        log_index: LogIndex,
        commands: &[LedgerCommand],
    ) -> Vec<CommandApplyReceipt> {
        let mut receipts = Vec::with_capacity(commands.len());
        for cmd in commands {
            receipts.push(self.apply_single_command(cmd));
        }
        self.last_applied_index = log_index;
        self.recompute_state_root();
        let root = self.cached_state_root;
        for r in &mut receipts {
            r.state_root_blake3 = root;
            if !r.duplicate_noop && !self.idempotency_log.contains_key(&r.idempotency_key) {
                self.idempotency_fifo.push_back(r.idempotency_key.clone());
            }
            self.idempotency_log.insert(r.idempotency_key.clone(), r.clone());
        }
        while self.idempotency_fifo.len() > IDEMPOTENCY_WINDOW_CAP {
            if let Some(oldest_key) = self.idempotency_fifo.pop_front() {
                self.idempotency_log.remove(&oldest_key);
            }
        }
        receipts
    }

    pub fn advance_noop_or_config(&mut self, log_index: LogIndex) {
        if log_index > self.last_applied_index {
            self.last_applied_index = log_index;
            self.recompute_state_root();
        }
    }

    fn apply_single_command(&mut self, cmd: &LedgerCommand) -> CommandApplyReceipt {
        let idem_key = cmd.idempotency_key().to_string();
        if let Some(existing) = self.idempotency_log.get(&idem_key) {
            let mut dup = existing.clone();
            dup.duplicate_noop = true;
            return dup;
        }

        let res = match cmd {
            LedgerCommand::CreditDeposit {
                account_id,
                asset,
                amount_fp,
                ..
            } => self.exec_deposit(*account_id, asset, *amount_fp),
            LedgerCommand::Withdraw {
                account_id,
                asset,
                amount_fp,
                ..
            } => self.exec_withdraw(*account_id, asset, *amount_fp),
            LedgerCommand::Transfer {
                from_account,
                to_account,
                asset,
                amount_fp,
                ..
            } => self.exec_transfer(*from_account, *to_account, asset, *amount_fp),
            LedgerCommand::LockMargin {
                account_id,
                asset,
                amount_fp,
                ..
            } => self.exec_lock_margin(*account_id, asset, *amount_fp),
            LedgerCommand::ReleaseMargin {
                account_id,
                asset,
                amount_fp,
                ..
            } => self.exec_release_margin(*account_id, asset, *amount_fp),
            LedgerCommand::SettleFill {
                base_asset,
                quote_asset,
                maker_account,
                taker_account,
                buyer_is_taker,
                price_fp,
                qty_fp,
                maker_fee_ppm,
                taker_fee_ppm,
                ..
            } => self.exec_settle_fill(
                base_asset,
                quote_asset,
                *maker_account,
                *taker_account,
                *buyer_is_taker,
                *price_fp,
                *qty_fp,
                *maker_fee_ppm,
                *taker_fee_ppm,
            ),
        };

        match res {
            Ok(()) => {
                self.journal_seq = self.journal_seq.saturating_add(1);
                CommandApplyReceipt {
                    idempotency_key: idem_key,
                    journal_seq: self.journal_seq,
                    applied: true,
                    duplicate_noop: false,
                    reject_reason: None,
                    state_root_blake3: self.cached_state_root,
                }
            }
            Err(reason) => CommandApplyReceipt {
                idempotency_key: idem_key,
                journal_seq: self.journal_seq,
                applied: false,
                duplicate_noop: false,
                reject_reason: Some(reason),
                state_root_blake3: self.cached_state_root,
            },
        }
    }

    fn exec_deposit(&mut self, account_id: u64, asset: &str, amount_fp: u128) -> Result<(), String> {
        if amount_fp == 0 {
            return Err("deposit amount_fp must be > 0".to_string());
        }
        let delta = i128::try_from(amount_fp).map_err(|_| "deposit overflow i128".to_string())?;
        let new_total_dep = self
            .total_deposits
            .get(asset)
            .copied()
            .unwrap_or(0)
            .checked_add(amount_fp)
            .ok_or_else(|| "system total_deposits overflow".to_string())?;

        let entry = self
            .accounts
            .entry((account_id, asset.to_string()))
            .or_default();
        entry.available_fp = entry
            .available_fp
            .checked_add(delta)
            .ok_or_else(|| "account available_fp overflow".to_string())?;
        self.total_deposits.insert(asset.to_string(), new_total_dep);
        Ok(())
    }

    fn exec_withdraw(
        &mut self,
        account_id: u64,
        asset: &str,
        amount_fp: u128,
    ) -> Result<(), String> {
        if amount_fp == 0 {
            return Err("withdraw amount_fp must be > 0".to_string());
        }
        let delta = i128::try_from(amount_fp).map_err(|_| "withdraw overflow i128".to_string())?;
        let key = (account_id, asset.to_string());
        let cur_avail = self.accounts.get(&key).map(|b| b.available_fp).unwrap_or(0);
        if cur_avail < delta {
            return Err("insufficient available balance for withdrawal".to_string());
        }
        let new_total_wd = self
            .total_withdrawals
            .get(asset)
            .copied()
            .unwrap_or(0)
            .checked_add(amount_fp)
            .ok_or_else(|| "system total_withdrawals overflow".to_string())?;
        let bucket = self.accounts.entry(key).or_default();
        bucket.available_fp = bucket
            .available_fp
            .checked_sub(delta)
            .ok_or_else(|| "withdraw underflow".to_string())?;
        self.total_withdrawals
            .insert(asset.to_string(), new_total_wd);
        Ok(())
    }

    fn exec_transfer(
        &mut self,
        from_account: u64,
        to_account: u64,
        asset: &str,
        amount_fp: u128,
    ) -> Result<(), String> {
        if amount_fp == 0 {
            return Err("transfer amount_fp must be > 0".to_string());
        }
        if from_account == to_account {
            return Ok(());
        }
        let delta = i128::try_from(amount_fp).map_err(|_| "transfer overflow i128".to_string())?;
        let from_key = (from_account, asset.to_string());
        let to_key = (to_account, asset.to_string());
        let from_avail = self
            .accounts
            .get(&from_key)
            .map(|b| b.available_fp)
            .unwrap_or(0);
        if from_avail < delta {
            return Err("insufficient available balance for transfer".to_string());
        }
        let to_avail = self
            .accounts
            .get(&to_key)
            .map(|b| b.available_fp)
            .unwrap_or(0);
        let next_to = to_avail
            .checked_add(delta)
            .ok_or_else(|| "recipient balance overflow".to_string())?;
        let next_from = from_avail
            .checked_sub(delta)
            .ok_or_else(|| "sender balance underflow".to_string())?;

        self.accounts.entry(from_key).or_default().available_fp = next_from;
        self.accounts.entry(to_key).or_default().available_fp = next_to;
        Ok(())
    }

    fn exec_lock_margin(
        &mut self,
        account_id: u64,
        asset: &str,
        amount_fp: u128,
    ) -> Result<(), String> {
        if amount_fp == 0 {
            return Err("lock amount_fp must be > 0".to_string());
        }
        let delta = i128::try_from(amount_fp).map_err(|_| "lock overflow i128".to_string())?;
        let key = (account_id, asset.to_string());
        let b = self.accounts.get(&key).copied().unwrap_or_default();
        if b.available_fp < delta {
            return Err("insufficient available balance to lock margin".to_string());
        }
        let next_avail = b
            .available_fp
            .checked_sub(delta)
            .ok_or_else(|| "available underflow".to_string())?;
        let next_locked = b
            .locked_fp
            .checked_add(delta)
            .ok_or_else(|| "locked overflow".to_string())?;
        self.accounts.insert(
            key,
            AssetBucket {
                available_fp: next_avail,
                locked_fp: next_locked,
            },
        );
        Ok(())
    }

    fn exec_release_margin(
        &mut self,
        account_id: u64,
        asset: &str,
        amount_fp: u128,
    ) -> Result<(), String> {
        if amount_fp == 0 {
            return Err("release amount_fp must be > 0".to_string());
        }
        let delta = i128::try_from(amount_fp).map_err(|_| "release overflow i128".to_string())?;
        let key = (account_id, asset.to_string());
        let b = self.accounts.get(&key).copied().unwrap_or_default();
        if b.locked_fp < delta {
            return Err("insufficient locked balance to release".to_string());
        }
        let next_locked = b
            .locked_fp
            .checked_sub(delta)
            .ok_or_else(|| "locked underflow".to_string())?;
        let next_avail = b
            .available_fp
            .checked_add(delta)
            .ok_or_else(|| "available overflow".to_string())?;
        self.accounts.insert(
            key,
            AssetBucket {
                available_fp: next_avail,
                locked_fp: next_locked,
            },
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_settle_fill(
        &mut self,
        base_asset: &str,
        quote_asset: &str,
        maker_account: u64,
        taker_account: u64,
        buyer_is_taker: bool,
        price_fp: u64,
        qty_fp: u64,
        maker_fee_ppm: i64,
        taker_fee_ppm: i64,
    ) -> Result<(), String> {
        if price_fp == 0 || qty_fp == 0 {
            return Err("price_fp and qty_fp must be > 0".to_string());
        }
        if maker_account == taker_account {
            return Err("self-trade prevented at ledger settlement".to_string());
        }

        // Exact `00-hyperion-contract.md` §5.1 formulas:
        let notional_u128 = (u128::from(price_fp)
            .checked_mul(u128::from(qty_fp))
            .ok_or_else(|| "notional_fp u128 overflow".to_string())?)
            / FP_SCALE_U128;
        let notional_fp =
            i128::try_from(notional_u128).map_err(|_| "notional_fp i128 overflow".to_string())?;
        let base_qty_i128 = i128::from(qty_fp);

        let maker_fee_fp = (notional_fp
            .checked_mul(i128::from(maker_fee_ppm))
            .ok_or_else(|| "maker_fee overflow".to_string())?)
            / PPM_SCALE_I128;
        let taker_fee_fp = (notional_fp
            .checked_mul(i128::from(taker_fee_ppm))
            .ok_or_else(|| "taker_fee overflow".to_string())?)
            / PPM_SCALE_I128;

        let (buyer_id, seller_id, buyer_fee_fp, seller_fee_fp) = if buyer_is_taker {
            (taker_account, maker_account, taker_fee_fp, maker_fee_fp)
        } else {
            (maker_account, taker_account, maker_fee_fp, taker_fee_fp)
        };

        let buyer_quote_cost = notional_fp
            .checked_add(buyer_fee_fp)
            .ok_or_else(|| "buyer_quote_cost overflow".to_string())?;
        let seller_quote_credit = notional_fp
            .checked_sub(seller_fee_fp)
            .ok_or_else(|| "seller_quote_credit overflow".to_string())?;
        let treasury_delta = buyer_fee_fp
            .checked_add(seller_fee_fp)
            .ok_or_else(|| "treasury_delta overflow".to_string())?;

        let buyer_quote_key = (buyer_id, quote_asset.to_string());
        let seller_base_key = (seller_id, base_asset.to_string());
        let buyer_base_key = (buyer_id, base_asset.to_string());
        let seller_quote_key = (seller_id, quote_asset.to_string());

        let mut buyer_quote = self
            .accounts
            .get(&buyer_quote_key)
            .copied()
            .unwrap_or_default();
        let mut seller_base = self
            .accounts
            .get(&seller_base_key)
            .copied()
            .unwrap_or_default();
        let mut buyer_base = self
            .accounts
            .get(&buyer_base_key)
            .copied()
            .unwrap_or_default();
        let mut seller_quote = self
            .accounts
            .get(&seller_quote_key)
            .copied()
            .unwrap_or_default();

        // Deduct from locked first if margin-locked, otherwise from available.
        deduct_locked_then_available(&mut buyer_quote, buyer_quote_cost, "buyer quote")?;
        deduct_locked_then_available(&mut seller_base, base_qty_i128, "seller base")?;

        buyer_base.available_fp = buyer_base
            .available_fp
            .checked_add(base_qty_i128)
            .ok_or_else(|| "buyer base credit overflow".to_string())?;
        seller_quote.available_fp = seller_quote
            .available_fp
            .checked_add(seller_quote_credit)
            .ok_or_else(|| "seller quote credit overflow".to_string())?;

        let next_treasury = self
            .treasury_fee_pool
            .get(quote_asset)
            .copied()
            .unwrap_or(0)
            .checked_add(treasury_delta)
            .ok_or_else(|| "treasury pool overflow".to_string())?;

        self.accounts.insert(buyer_quote_key, buyer_quote);
        self.accounts.insert(seller_base_key, seller_base);
        self.accounts.insert(buyer_base_key, buyer_base);
        self.accounts.insert(seller_quote_key, seller_quote);
        self.treasury_fee_pool
            .insert(quote_asset.to_string(), next_treasury);
        Ok(())
    }

    pub fn execute_read(&self, query: &LedgerReadQuery) -> LedgerReadResult {
        match query {
            LedgerReadQuery::GetAccountBalance { account_id, asset } => {
                let b = self
                    .accounts
                    .get(&(*account_id, asset.clone()))
                    .copied()
                    .unwrap_or_default();
                LedgerReadResult::AccountBalance {
                    account_id: *account_id,
                    asset: asset.clone(),
                    available_fp: b.available_fp,
                    locked_fp: b.locked_fp,
                    total_fp: b.available_fp.saturating_add(b.locked_fp),
                    at_index: self.last_applied_index,
                    state_root_blake3: self.cached_state_root,
                }
            }
            LedgerReadQuery::GetSystemConservation { asset } => {
                let (dep, wd, avail, locked, fee, ok) = self.audit_asset_conservation(asset);
                LedgerReadResult::SystemConservation {
                    asset: asset.clone(),
                    total_deposits_fp: dep,
                    total_withdrawals_fp: wd,
                    total_available_fp: avail,
                    total_locked_fp: locked,
                    treasury_fee_pool_fp: fee,
                    balanced: ok,
                    at_index: self.last_applied_index,
                    state_root_blake3: self.cached_state_root,
                }
            }
            LedgerReadQuery::GetLastJournalSeq => LedgerReadResult::LastJournalSeq {
                journal_seq: self.journal_seq,
                at_index: self.last_applied_index,
                state_root_blake3: self.cached_state_root,
            },
        }
    }

    /// Exact verification of `00-hyperion-contract.md` §5.2 Double-Entry Conservation Law for `asset`.
    pub fn audit_asset_conservation(&self, asset: &str) -> (u128, u128, i128, i128, i128, bool) {
        let dep = self.total_deposits.get(asset).copied().unwrap_or(0);
        let wd = self.total_withdrawals.get(asset).copied().unwrap_or(0);
        let fee = self.treasury_fee_pool.get(asset).copied().unwrap_or(0);
        let mut avail_sum: i128 = 0;
        let mut locked_sum: i128 = 0;
        let mut overflow = false;

        for ((_, acct_asset), bucket) in &self.accounts {
            if acct_asset == asset {
                match avail_sum.checked_add(bucket.available_fp) {
                    Some(v) => avail_sum = v,
                    None => overflow = true,
                }
                match locked_sum.checked_add(bucket.locked_fp) {
                    Some(v) => locked_sum = v,
                    None => overflow = true,
                }
            }
        }

        let lhs = avail_sum
            .checked_add(locked_sum)
            .and_then(|v| v.checked_add(fee));
        let rhs = i128::try_from(dep)
            .ok()
            .and_then(|d| i128::try_from(wd).ok().and_then(|w| d.checked_sub(w)));
        let balanced = !overflow && lhs.is_some() && lhs == rhs;
        (dep, wd, avail_sum, locked_sum, fee, balanced)
    }

    /// Verify conservation law simultaneously across ALL assets present in the ledger.
    pub fn verify_all_assets_conserved(&self) -> bool {
        let mut assets: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for s in self.total_deposits.keys() {
            assets.insert(s.as_str());
        }
        for s in self.total_withdrawals.keys() {
            assets.insert(s.as_str());
        }
        for s in self.treasury_fee_pool.keys() {
            assets.insert(s.as_str());
        }
        for (_, s) in self.accounts.keys() {
            assets.insert(s.as_str());
        }
        for asset in assets {
            let (_, _, _, _, _, ok) = self.audit_asset_conservation(asset);
            if !ok {
                return false;
            }
        }
        true
    }

    fn recompute_state_root(&mut self) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"HYPERION_CH12_LEDGER_SM_V1");
        hasher.update(&self.last_applied_index.to_le_bytes());
        hasher.update(&self.journal_seq.to_le_bytes());

        for ((acct, asset), bucket) in &self.accounts {
            if bucket.available_fp == 0 && bucket.locked_fp == 0 {
                continue;
            }
            hasher.update(&acct.to_le_bytes());
            hasher.update(&(asset.len() as u32).to_le_bytes());
            hasher.update(asset.as_bytes());
            hasher.update(&bucket.available_fp.to_le_bytes());
            hasher.update(&bucket.locked_fp.to_le_bytes());
        }
        for (asset, dep) in &self.total_deposits {
            hasher.update(b"DEP:");
            hasher.update(asset.as_bytes());
            hasher.update(&dep.to_le_bytes());
        }
        for (asset, wd) in &self.total_withdrawals {
            hasher.update(b"WD:");
            hasher.update(asset.as_bytes());
            hasher.update(&wd.to_le_bytes());
        }
        for (asset, fee) in &self.treasury_fee_pool {
            if *fee == 0 {
                continue;
            }
            hasher.update(b"FEE:");
            hasher.update(asset.as_bytes());
            hasher.update(&fee.to_le_bytes());
        }
        self.cached_state_root = *hasher.finalize().as_bytes();
    }
}

fn deduct_locked_then_available(
    bucket: &mut AssetBucket,
    amount: i128,
    label: &str,
) -> Result<(), String> {
    if amount < 0 {
        // Negative cost means net rebate! Credit directly to available_fp.
        let rebate = amount
            .checked_neg()
            .ok_or_else(|| format!("{label} rebate overflow"))?;
        bucket.available_fp = bucket
            .available_fp
            .checked_add(rebate)
            .ok_or_else(|| format!("{label} rebate credit overflow"))?;
        return Ok(());
    }
    let from_locked = bucket.locked_fp.min(amount);
    let rem = amount - from_locked;
    if bucket.available_fp < rem {
        return Err(format!("insufficient {label} balance"));
    }
    bucket.locked_fp -= from_locked;
    bucket.available_fp -= rem;
    Ok(())
}

#[inline]
fn put_str(dst: &mut Vec<u8>, s: &str) {
    dst.extend_from_slice(&(s.len() as u32).to_le_bytes());
    dst.extend_from_slice(s.as_bytes());
}

struct BinCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BinCursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| "snapshot cursor overflow".to_string())?;
        if end > self.buf.len() {
            return Err("unexpected EOF decoding HYPSM01 state machine snapshot".to_string());
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let b = self.read_exact(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, String> {
        let b = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn read_u128(&mut self) -> Result<u128, String> {
        let b = self.read_exact(16)?;
        let mut arr = [0u8; 16];
        arr.copy_from_slice(b);
        Ok(u128::from_le_bytes(arr))
    }

    fn read_i128(&mut self) -> Result<i128, String> {
        let b = self.read_exact(16)?;
        let mut arr = [0u8; 16];
        arr.copy_from_slice(b);
        Ok(i128::from_le_bytes(arr))
    }

    fn read_str(&mut self) -> Result<String, String> {
        let len = self.read_u32()? as usize;
        let b = self.read_exact(len)?;
        std::str::from_utf8(b)
            .map(|s| s.to_string())
            .map_err(|e| format!("invalid UTF-8 in HYPSM01 snapshot: {e}"))
    }
}
