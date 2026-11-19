#![deny(clippy::float_arithmetic)]

use crate::arena_lob::{FixedU64Map, NULL_IDX};
use crate::fixed::AssetSymbol;
use crate::merkle::{hash_account_leaf, AssetBalanceLeaf, IncrementalMerkleState};
use thiserror::Error;

pub const TREASURY_ACCOUNT_ID: u64 = 0;
pub const MAX_INLINE_ASSETS_PER_ACCOUNT: usize = 8;

pub const SUB_ACCOUNT_AVAILABLE: &str = "AVAILABLE";
pub const SUB_ACCOUNT_LOCKED: &str = "LOCKED_MARGIN";
pub const SUB_ACCOUNT_TREASURY: &str = "TREASURY_FEE";
pub const SUB_ACCOUNT_OMNIBUS_VAULT: &str = "EXCHANGE_VAULT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AccountRiskStatus {
    Active = 1,
    Throttled = 2,
    Frozen = 3,
    LiquidationOnly = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LedgerError {
    #[error("insufficient available balance")]
    InsufficientBalance,
    #[error("insufficient locked margin balance")]
    InsufficientLockedBalance,
    #[error("ledger fixed-point overflow or underflow")]
    ArithmeticOverflow,
    #[error("double-entry journal debit/credit imbalance")]
    UnbalancedJournal,
    #[error("account slab capacity exceeded")]
    AccountCapacityExceeded,
    #[error("asset slot capacity per account exceeded")]
    AssetCapacityExceeded,
    #[error("invalid non-positive deposit amount")]
    InvalidAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalLeg {
    pub account_id: u64,
    pub asset: AssetSymbol,
    pub sub_account: &'static str,
    pub debit_fp: i128,
    pub credit_fp: i128,
    pub balance_after_fp: i128,
}

/// Stack-allocated zero-heap-allocation balanced Double-Entry Journal record (up to 8 legs per atomic transition).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactJournalEntry {
    pub journal_seq: u64,
    pub engine_seq: u64,
    pub reference_type: &'static str,
    pub reference_id: u64,
    pub legs: [JournalLeg; 8],
    pub leg_count: u8,
    pub ts_ns: u64,
    pub prev_journal_blake3: [u8; 32],
    pub journal_blake3: [u8; 32],
}

impl CompactJournalEntry {
    #[inline(always)]
    pub fn legs_slice(&self) -> &[JournalLeg] {
        &self.legs[..self.leg_count as usize]
    }
}

#[derive(Clone, Copy)]
pub struct AccountSlot {
    pub account_id: u64,
    pub status: AccountRiskStatus,
    pub assets: [AssetBalanceLeaf; MAX_INLINE_ASSETS_PER_ACCOUNT],
    pub asset_count: u8,
    /// Head of intrusive doubly-linked list of resting orders across an instrument (`NULL_IDX` if none).
    /// Note: per-account open order cancellation iterates cleanly across instruments via per-account index or scan.
    pub open_order_count: u32,
}

impl Default for AccountSlot {
    fn default() -> Self {
        Self {
            account_id: 0,
            status: AccountRiskStatus::Active,
            assets: [AssetBalanceLeaf {
                asset: AssetSymbol::empty(),
                available_fp: 0,
                locked_fp: 0,
            }; MAX_INLINE_ASSETS_PER_ACCOUNT],
            asset_count: 0,
            open_order_count: 0,
        }
    }
}

impl AccountSlot {
    #[inline]
    pub fn assets_slice(&self) -> &[AssetBalanceLeaf] {
        &self.assets[..self.asset_count as usize]
    }

    #[inline]
    pub fn find_asset_pos(&self, asset: AssetSymbol) -> Option<usize> {
        self.assets_slice()
            .binary_search_by_key(&asset, |b| b.asset)
            .ok()
    }

    #[inline]
    pub fn get_or_create_asset_mut(
        &mut self,
        asset: AssetSymbol,
    ) -> Result<&mut AssetBalanceLeaf, LedgerError> {
        let slice = &self.assets[..self.asset_count as usize];
        match slice.binary_search_by_key(&asset, |b| b.asset) {
            Ok(pos) => Ok(&mut self.assets[pos]),
            Err(insert_pos) => {
                let count = self.asset_count as usize;
                if count >= MAX_INLINE_ASSETS_PER_ACCOUNT {
                    return Err(LedgerError::AssetCapacityExceeded);
                }
                for i in (insert_pos..count).rev() {
                    self.assets[i + 1] = self.assets[i];
                }
                self.assets[insert_pos] = AssetBalanceLeaf {
                    asset,
                    available_fp: 0,
                    locked_fp: 0,
                };
                self.asset_count += 1;
                Ok(&mut self.assets[insert_pos])
            }
        }
    }

    #[inline]
    pub fn leaf_hash(&self) -> [u8; 32] {
        hash_account_leaf(self.account_id, self.assets_slice())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetGlobalTotals {
    pub asset: AssetSymbol,
    pub total_deposits_fp: u128,
    pub total_available_fp: u128,
    pub total_locked_fp: u128,
    pub treasury_fee_pool_fp: i128,
}

/// Zero-allocation Double-Entry Cryptographic Ledger with Blake3 hash-chained journals.
///
/// Accounting Convention (Liability/Equity perspective against Exchange Omnibus Vault Asset):
/// - User `AVAILABLE`, User `LOCKED_MARGIN`, and `TREASURY_FEE` are Credit-normal accounts:
///   * Increasing a user's `AVAILABLE` or `LOCKED_MARGIN` posts a **CREDIT** (`credit_fp > 0`).
///   * Decreasing a user's `AVAILABLE` or `LOCKED_MARGIN` posts a **DEBIT** (`debit_fp > 0`).
/// - Exchange `EXCHANGE_VAULT` (Omnibus Custody Asset) is Debit-normal:
///   * External `CreditDeposit(amount)` posts **DEBIT `EXCHANGE_VAULT`** (`+amount`) and **CREDIT `User:AVAILABLE`** (`+amount`).
/// - Every journal entry strictly satisfies:
///   `Sum(leg.debit_fp) == Sum(leg.credit_fp)` across all legs!
const KEY_JOURNAL_CHAIN: [u8; 32] = *b"HYPERION_JOURNAL_CHAIN_KEY_V0001";

#[derive(Clone)]
pub struct DoubleEntryLedger {
    accounts: Box<[AccountSlot]>,
    account_count: u32,
    account_index: FixedU64Map,
    asset_totals: Vec<AssetGlobalTotals>,
    journal_seq: u64,
    last_journal_blake3: [u8; 32],
    merkle_batch_active: bool,
    dirty_account_slots: Vec<u32>,
    dirty_slot_flags: Box<[u8]>,
}

impl DoubleEntryLedger {
    pub fn with_capacity(max_accounts: usize) -> Self {
        let cap = max_accounts.max(16);
        let mut s = Self {
            accounts: vec![AccountSlot::default(); cap].into_boxed_slice(),
            account_count: 0,
            account_index: FixedU64Map::with_capacity(cap),
            asset_totals: Vec::with_capacity(16),
            journal_seq: 0,
            last_journal_blake3: [0u8; 32],
            merkle_batch_active: false,
            dirty_account_slots: Vec::with_capacity(cap.min(256)),
            dirty_slot_flags: vec![0u8; cap].into_boxed_slice(),
        };
        // Reserve slot 0 for TREASURY_ACCOUNT_ID = 0
        s.accounts[0] = AccountSlot {
            account_id: TREASURY_ACCOUNT_ID,
            ..AccountSlot::default()
        };
        s.account_index
            .insert(TREASURY_ACCOUNT_ID, 0)
            .expect("treasury slot init");
        s.account_count = 1;
        s
    }

    pub fn clear(&mut self) {
        for slot in &mut self.accounts[..self.account_count as usize] {
            *slot = AccountSlot::default();
        }
        self.account_index.clear();
        self.accounts[0].account_id = TREASURY_ACCOUNT_ID;
        self.account_index
            .insert(TREASURY_ACCOUNT_ID, 0)
            .expect("treasury slot reset");
        self.account_count = 1;
        self.asset_totals.clear();
        self.journal_seq = 0;
        self.last_journal_blake3 = [0u8; 32];
        self.merkle_batch_active = false;
        for &idx in &self.dirty_account_slots {
            self.dirty_slot_flags[idx as usize] = 0;
        }
        self.dirty_account_slots.clear();
    }

    /// Defers per-account Blake3 leaf hashing until `flush_merkle_batch` so multi-fill sweeps hash
    /// Buyer and Treasury accounts once per command instead of once per fill leg.
    #[inline(always)]
    pub fn begin_merkle_batch(&mut self) {
        self.merkle_batch_active = true;
    }

    #[inline]
    pub fn flush_merkle_batch(&mut self, merkle: &mut IncrementalMerkleState) {
        self.merkle_batch_active = false;
        for i in 0..self.dirty_account_slots.len() {
            let acc_idx = self.dirty_account_slots[i] as usize;
            self.dirty_slot_flags[acc_idx] = 0;
            let acc = &self.accounts[acc_idx];
            let digest = acc.leaf_hash();
            merkle.upsert_account_leaf(acc.account_id, digest);
        }
        self.dirty_account_slots.clear();
    }

    #[inline(always)]
    fn record_account_merkle_update(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        acc_idx: u32,
    ) {
        if self.merkle_batch_active {
            let idx = acc_idx as usize;
            if self.dirty_slot_flags[idx] == 0 {
                self.dirty_slot_flags[idx] = 1;
                self.dirty_account_slots.push(acc_idx);
            }
        } else {
            let acc = &self.accounts[acc_idx as usize];
            let digest = acc.leaf_hash();
            merkle.upsert_account_leaf(acc.account_id, digest);
        }
    }

    #[inline(always)]
    pub const fn journal_seq(&self) -> u64 {
        self.journal_seq
    }

    #[inline(always)]
    pub const fn last_journal_blake3(&self) -> [u8; 32] {
        self.last_journal_blake3
    }

    #[inline(always)]
    pub fn active_accounts_slice(&self) -> &[AccountSlot] {
        &self.accounts[..self.account_count as usize]
    }

    #[inline]
    pub fn get_account(&self, account_id: u64) -> Option<&AccountSlot> {
        let idx = self.account_index.get(account_id)?;
        Some(&self.accounts[idx as usize])
    }

    #[inline]
    pub fn get_or_create_account_idx(&mut self, account_id: u64) -> Result<u32, LedgerError> {
        if let Some(idx) = self.account_index.get(account_id) {
            return Ok(idx);
        }
        let idx = self.account_count;
        if (idx as usize) >= self.accounts.len() {
            return Err(LedgerError::AccountCapacityExceeded);
        }
        self.accounts[idx as usize] = AccountSlot {
            account_id,
            status: AccountRiskStatus::Active,
            ..AccountSlot::default()
        };
        self.account_index
            .insert(account_id, idx)
            .map_err(|_| LedgerError::AccountCapacityExceeded)?;
        self.account_count += 1;
        Ok(idx)
    }

    pub fn set_account_status(
        &mut self,
        account_id: u64,
        status: AccountRiskStatus,
    ) -> Result<AccountRiskStatus, LedgerError> {
        let idx = self.get_or_create_account_idx(account_id)?;
        self.accounts[idx as usize].status = status;
        Ok(status)
    }

    #[inline]
    pub fn account_status(&self, account_id: u64) -> AccountRiskStatus {
        self.get_account(account_id)
            .map(|a| a.status)
            .unwrap_or(AccountRiskStatus::Active)
    }

    #[inline]
    pub fn available_balance(&self, account_id: u64, asset: AssetSymbol) -> i128 {
        let Some(acc) = self.get_account(account_id) else {
            return 0;
        };
        let Some(pos) = acc.find_asset_pos(asset) else {
            return 0;
        };
        acc.assets[pos].available_fp
    }

    #[inline]
    pub fn locked_balance(&self, account_id: u64, asset: AssetSymbol) -> i128 {
        let Some(acc) = self.get_account(account_id) else {
            return 0;
        };
        let Some(pos) = acc.find_asset_pos(asset) else {
            return 0;
        };
        acc.assets[pos].locked_fp
    }

    #[inline]
    fn get_or_create_asset_totals_mut(&mut self, asset: AssetSymbol) -> &mut AssetGlobalTotals {
        match self.asset_totals.binary_search_by_key(&asset, |t| t.asset) {
            Ok(pos) => &mut self.asset_totals[pos],
            Err(pos) => {
                self.asset_totals.insert(
                    pos,
                    AssetGlobalTotals {
                        asset,
                        total_deposits_fp: 0,
                        total_available_fp: 0,
                        total_locked_fp: 0,
                        treasury_fee_pool_fp: 0,
                    },
                );
                &mut self.asset_totals[pos]
            }
        }
    }

    pub fn asset_totals(&self, asset: AssetSymbol) -> AssetGlobalTotals {
        match self.asset_totals.binary_search_by_key(&asset, |t| t.asset) {
            Ok(pos) => self.asset_totals[pos],
            Err(_) => AssetGlobalTotals {
                asset,
                total_deposits_fp: 0,
                total_available_fp: 0,
                total_locked_fp: 0,
                treasury_fee_pool_fp: 0,
            },
        }
    }

    pub fn all_asset_totals(&self) -> &[AssetGlobalTotals] {
        &self.asset_totals
    }

    /// Computes O(A) exact ground-truth audit across all accounts to verify zero 1e-8 drift.
    pub fn verify_asset_conservation(&self, asset: AssetSymbol) -> (AssetGlobalTotals, bool) {
        let tracked = self.asset_totals(asset);
        let mut sum_user_avail: i128 = 0;
        let mut sum_user_locked: i128 = 0;
        let mut treasury_bal: i128 = 0;

        for (i, acc) in self.active_accounts_slice().iter().enumerate() {
            if let Some(pos) = acc.find_asset_pos(asset) {
                let b = acc.assets[pos];
                if i == 0 && acc.account_id == TREASURY_ACCOUNT_ID {
                    treasury_bal = b.available_fp;
                } else {
                    if b.available_fp < 0 || b.locked_fp < 0 {
                        return (tracked, false);
                    }
                    sum_user_avail = match sum_user_avail.checked_add(b.available_fp) {
                        Some(v) => v,
                        None => return (tracked, false),
                    };
                    sum_user_locked = match sum_user_locked.checked_add(b.locked_fp) {
                        Some(v) => v,
                        None => return (tracked, false),
                    };
                }
            }
        }

        let Some(user_sum) = sum_user_avail.checked_add(sum_user_locked) else {
            return (tracked, false);
        };
        let Some(lhs) = user_sum.checked_add(treasury_bal) else {
            return (tracked, false);
        };
        let rhs = tracked.total_deposits_fp as i128;
        let exact_match = lhs == rhs
            && (sum_user_avail as u128) == tracked.total_available_fp
            && (sum_user_locked as u128) == tracked.total_locked_fp
            && treasury_bal == tracked.treasury_fee_pool_fp;

        (tracked, exact_match)
    }

    #[inline]
    fn seal_journal(
        &mut self,
        engine_seq: u64,
        reference_type: &'static str,
        reference_id: u64,
        legs: [JournalLeg; 8],
        leg_count: u8,
        ts_ns: u64,
    ) -> Result<CompactJournalEntry, LedgerError> {
        let mut sum_debit: i128 = 0;
        let mut sum_credit: i128 = 0;
        for leg in &legs[..leg_count as usize] {
            if leg.debit_fp < 0 || leg.credit_fp < 0 {
                return Err(LedgerError::UnbalancedJournal);
            }
            sum_debit = sum_debit
                .checked_add(leg.debit_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            sum_credit = sum_credit
                .checked_add(leg.credit_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }
        if sum_debit != sum_credit {
            return Err(LedgerError::UnbalancedJournal);
        }

        self.journal_seq = self
            .journal_seq
            .checked_add(1)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let prev = self.last_journal_blake3;

        let mut buf = [0u8; 704];
        let mut pos = 0usize;
        buf[pos..pos + 8].copy_from_slice(&self.journal_seq.to_le_bytes());
        pos += 8;
        buf[pos..pos + 8].copy_from_slice(&engine_seq.to_le_bytes());
        pos += 8;
        let ref_b = reference_type.as_bytes();
        let ref_len = ref_b.len().min(32);
        buf[pos] = ref_len as u8;
        pos += 1;
        buf[pos..pos + ref_len].copy_from_slice(&ref_b[..ref_len]);
        pos += ref_len;
        buf[pos..pos + 8].copy_from_slice(&reference_id.to_le_bytes());
        pos += 8;
        buf[pos..pos + 8].copy_from_slice(&ts_ns.to_le_bytes());
        pos += 8;
        buf[pos..pos + 32].copy_from_slice(&prev);
        pos += 32;
        buf[pos] = leg_count;
        pos += 1;
        for leg in &legs[..leg_count as usize] {
            buf[pos..pos + 8].copy_from_slice(&leg.account_id.to_le_bytes());
            pos += 8;
            let ab = leg.asset.as_bytes();
            buf[pos] = ab.len() as u8;
            pos += 1;
            buf[pos..pos + ab.len()].copy_from_slice(ab);
            pos += ab.len();
            let sub_tag: u8 = match leg.sub_account {
                SUB_ACCOUNT_AVAILABLE => 1,
                SUB_ACCOUNT_LOCKED => 2,
                SUB_ACCOUNT_TREASURY => 3,
                SUB_ACCOUNT_OMNIBUS_VAULT => 4,
                _ => 0,
            };
            buf[pos] = sub_tag;
            pos += 1;
            buf[pos..pos + 16].copy_from_slice(&leg.debit_fp.to_le_bytes());
            pos += 16;
            buf[pos..pos + 16].copy_from_slice(&leg.credit_fp.to_le_bytes());
            pos += 16;
            buf[pos..pos + 16].copy_from_slice(&leg.balance_after_fp.to_le_bytes());
            pos += 16;
        }
        let digest = *blake3::keyed_hash(&KEY_JOURNAL_CHAIN, &buf[..pos]).as_bytes();
        self.last_journal_blake3 = digest;

        Ok(CompactJournalEntry {
            journal_seq: self.journal_seq,
            engine_seq,
            reference_type,
            reference_id,
            legs,
            leg_count,
            ts_ns,
            prev_journal_blake3: prev,
            journal_blake3: digest,
        })
    }

    #[inline(always)]
    fn empty_leg() -> JournalLeg {
        JournalLeg {
            account_id: 0,
            asset: AssetSymbol::empty(),
            sub_account: "",
            debit_fp: 0,
            credit_fp: 0,
            balance_after_fp: 0,
        }
    }

    /// Credits a user deposit: DEBIT `EXCHANGE_VAULT` (+amount), CREDIT `User:AVAILABLE` (+amount).
    #[allow(clippy::too_many_arguments)]
    pub fn credit_deposit(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        engine_seq: u64,
        reference_id: u64,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: u128,
        ts_ns: u64,
    ) -> Result<(AssetBalanceLeaf, CompactJournalEntry), LedgerError> {
        if amount_fp == 0 || amount_fp > i128::MAX as u128 || account_id == TREASURY_ACCOUNT_ID {
            return Err(LedgerError::InvalidAmount);
        }
        let amt_i128 = amount_fp as i128;
        let acc_idx = self.get_or_create_account_idx(account_id)?;
        let cur_avail = {
            let slot = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            slot.available_fp
        };
        let new_avail = cur_avail
            .checked_add(amt_i128)
            .ok_or(LedgerError::ArithmeticOverflow)?;

        let new_vault_total = {
            let totals = self.get_or_create_asset_totals_mut(asset);
            let td = totals
                .total_deposits_fp
                .checked_add(amount_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            let ta = totals
                .total_available_fp
                .checked_add(amount_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            if td > i128::MAX as u128 {
                return Err(LedgerError::ArithmeticOverflow);
            }
            totals.total_deposits_fp = td;
            totals.total_available_fp = ta;
            td as i128
        };

        let updated_leaf = {
            let slot = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            slot.available_fp = new_avail;
            *slot
        };
        self.record_account_merkle_update(merkle, acc_idx);

        let mut legs = [Self::empty_leg(); 8];
        legs[0] = JournalLeg {
            account_id: TREASURY_ACCOUNT_ID,
            asset,
            sub_account: SUB_ACCOUNT_OMNIBUS_VAULT,
            debit_fp: amt_i128,
            credit_fp: 0,
            balance_after_fp: new_vault_total,
        };
        legs[1] = JournalLeg {
            account_id,
            asset,
            sub_account: SUB_ACCOUNT_AVAILABLE,
            debit_fp: 0,
            credit_fp: amt_i128,
            balance_after_fp: new_avail,
        };
        let journal = self.seal_journal(engine_seq, "DEPOSIT", reference_id, legs, 2, ts_ns)?;
        Ok((updated_leaf, journal))
    }

    /// Reserves margin/collateral when placing an order:
    /// DEBIT `User:AVAILABLE` (amount), CREDIT `User:LOCKED_MARGIN` (amount).
    #[allow(clippy::too_many_arguments)]
    pub fn lock_funds(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        engine_seq: u64,
        order_id: u64,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: i128,
        ts_ns: u64,
    ) -> Result<Option<CompactJournalEntry>, LedgerError> {
        if amount_fp == 0 {
            return Ok(None);
        }
        if amount_fp < 0 {
            return Err(LedgerError::InvalidAmount);
        }
        let acc_idx = self.get_or_create_account_idx(account_id)?;
        let (cur_avail, cur_locked) = {
            let b = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            (b.available_fp, b.locked_fp)
        };
        if cur_avail < amount_fp {
            return Err(LedgerError::InsufficientBalance);
        }
        let new_avail = cur_avail
            .checked_sub(amount_fp)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let new_locked = cur_locked
            .checked_add(amount_fp)
            .ok_or(LedgerError::ArithmeticOverflow)?;

        {
            let totals = self.get_or_create_asset_totals_mut(asset);
            let amt_u128 = amount_fp as u128;
            totals.total_available_fp = totals
                .total_available_fp
                .checked_sub(amt_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            totals.total_locked_fp = totals
                .total_locked_fp
                .checked_add(amt_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }

        {
            let b = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            b.available_fp = new_avail;
            b.locked_fp = new_locked;
        }
        self.record_account_merkle_update(merkle, acc_idx);

        let mut legs = [Self::empty_leg(); 8];
        legs[0] = JournalLeg {
            account_id,
            asset,
            sub_account: SUB_ACCOUNT_AVAILABLE,
            debit_fp: amount_fp,
            credit_fp: 0,
            balance_after_fp: new_avail,
        };
        legs[1] = JournalLeg {
            account_id,
            asset,
            sub_account: SUB_ACCOUNT_LOCKED,
            debit_fp: 0,
            credit_fp: amount_fp,
            balance_after_fp: new_locked,
        };
        let j = self.seal_journal(engine_seq, "ORDER_HOLD", order_id, legs, 2, ts_ns)?;
        Ok(Some(j))
    }

    /// Releases unused locked funds (on order cancel, IOC remainder, or price-improvement residual):
    /// DEBIT `User:LOCKED_MARGIN` (amount), CREDIT `User:AVAILABLE` (amount).
    #[allow(clippy::too_many_arguments)]
    pub fn unlock_funds(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        engine_seq: u64,
        order_id: u64,
        account_id: u64,
        asset: AssetSymbol,
        amount_fp: i128,
        reference_type: &'static str,
        ts_ns: u64,
    ) -> Result<Option<CompactJournalEntry>, LedgerError> {
        if amount_fp == 0 {
            return Ok(None);
        }
        if amount_fp < 0 {
            return Err(LedgerError::InvalidAmount);
        }
        let acc_idx = self.get_or_create_account_idx(account_id)?;
        let (cur_avail, cur_locked) = {
            let b = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            (b.available_fp, b.locked_fp)
        };
        if cur_locked < amount_fp {
            return Err(LedgerError::InsufficientLockedBalance);
        }
        let new_locked = cur_locked
            .checked_sub(amount_fp)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let new_avail = cur_avail
            .checked_add(amount_fp)
            .ok_or(LedgerError::ArithmeticOverflow)?;

        {
            let totals = self.get_or_create_asset_totals_mut(asset);
            let amt_u128 = amount_fp as u128;
            totals.total_locked_fp = totals
                .total_locked_fp
                .checked_sub(amt_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            totals.total_available_fp = totals
                .total_available_fp
                .checked_add(amt_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }

        {
            let b = self.accounts[acc_idx as usize].get_or_create_asset_mut(asset)?;
            b.locked_fp = new_locked;
            b.available_fp = new_avail;
        }
        self.record_account_merkle_update(merkle, acc_idx);

        let mut legs = [Self::empty_leg(); 8];
        legs[0] = JournalLeg {
            account_id,
            asset,
            sub_account: SUB_ACCOUNT_LOCKED,
            debit_fp: amount_fp,
            credit_fp: 0,
            balance_after_fp: new_locked,
        };
        legs[1] = JournalLeg {
            account_id,
            asset,
            sub_account: SUB_ACCOUNT_AVAILABLE,
            debit_fp: 0,
            credit_fp: amount_fp,
            balance_after_fp: new_avail,
        };
        let j = self.seal_journal(engine_seq, reference_type, order_id, legs, 2, ts_ns)?;
        Ok(Some(j))
    }

    /// Atomically settles a matched trade fill in a single balanced multi-leg Double-Entry Journal:
    /// - Base Asset:
    ///   * DEBIT `Seller:LOCKED_MARGIN(base)` by `qty_fp`
    ///   * CREDIT `Buyer:AVAILABLE(base)` by `qty_fp`
    /// - Quote Asset:
    ///   * DEBIT `Buyer:LOCKED_MARGIN(quote)` by `notional_fp + buyer_fee_fp`
    ///   * CREDIT `Seller:AVAILABLE(quote)` by `notional_fp - seller_fee_fp`
    ///   * CREDIT/DEBIT `Treasury:TREASURY_FEE(quote)` by `buyer_fee_fp + seller_fee_fp`
    ///
    /// Returns `(buyer_quote_spent_from_lock, journal_entry)`.
    #[allow(clippy::too_many_arguments)]
    pub fn settle_fill(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        engine_seq: u64,
        fill_id: u64,
        buyer_account_id: u64,
        seller_account_id: u64,
        base_asset: AssetSymbol,
        quote_asset: AssetSymbol,
        qty_fp: u64,
        notional_fp: u128,
        buyer_fee_fp: i64,
        seller_fee_fp: i64,
        ts_ns: u64,
    ) -> Result<(i128, CompactJournalEntry), LedgerError> {
        let base_amt = qty_fp as i128;
        if notional_fp > i128::MAX as u128 {
            return Err(LedgerError::ArithmeticOverflow);
        }
        let notional_i128 = notional_fp as i128;
        let buyer_quote_debit = notional_i128
            .checked_add(buyer_fee_fp as i128)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let seller_quote_credit = notional_i128
            .checked_sub(seller_fee_fp as i128)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let net_treasury_fee = (buyer_fee_fp as i128)
            .checked_add(seller_fee_fp as i128)
            .ok_or(LedgerError::ArithmeticOverflow)?;

        if buyer_quote_debit < 0 || seller_quote_credit < 0 {
            return Err(LedgerError::ArithmeticOverflow);
        }

        let buyer_idx = self.get_or_create_account_idx(buyer_account_id)?;
        let seller_idx = self.get_or_create_account_idx(seller_account_id)?;

        // Pre-validate locked balances before mutating any state
        {
            let seller_base = self.accounts[seller_idx as usize].get_or_create_asset_mut(base_asset)?;
            if seller_base.locked_fp < base_amt {
                return Err(LedgerError::InsufficientLockedBalance);
            }
        }
        {
            let buyer_quote = self.accounts[buyer_idx as usize].get_or_create_asset_mut(quote_asset)?;
            if buyer_quote.locked_fp < buyer_quote_debit {
                return Err(LedgerError::InsufficientLockedBalance);
            }
        }

        // 1. Mutate Base Asset (Seller Locked -> Buyer Available)
        let seller_base_locked_after = {
            let sb = self.accounts[seller_idx as usize].get_or_create_asset_mut(base_asset)?;
            sb.locked_fp = sb
                .locked_fp
                .checked_sub(base_amt)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            sb.locked_fp
        };
        let buyer_base_avail_after = {
            let bb = self.accounts[buyer_idx as usize].get_or_create_asset_mut(base_asset)?;
            bb.available_fp = bb
                .available_fp
                .checked_add(base_amt)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            bb.available_fp
        };
        {
            let base_totals = self.get_or_create_asset_totals_mut(base_asset);
            let q_u128 = qty_fp as u128;
            base_totals.total_locked_fp = base_totals
                .total_locked_fp
                .checked_sub(q_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            base_totals.total_available_fp = base_totals
                .total_available_fp
                .checked_add(q_u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }

        // 2. Mutate Quote Asset (Buyer Locked -> Seller Available + Treasury Fee)
        let buyer_quote_locked_after = {
            let bq = self.accounts[buyer_idx as usize].get_or_create_asset_mut(quote_asset)?;
            bq.locked_fp = bq
                .locked_fp
                .checked_sub(buyer_quote_debit)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            bq.locked_fp
        };
        let seller_quote_avail_after = {
            let sq = self.accounts[seller_idx as usize].get_or_create_asset_mut(quote_asset)?;
            sq.available_fp = sq
                .available_fp
                .checked_add(seller_quote_credit)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            sq.available_fp
        };
        let treasury_quote_after = {
            let tq = self.accounts[0].get_or_create_asset_mut(quote_asset)?;
            tq.available_fp = tq
                .available_fp
                .checked_add(net_treasury_fee)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            tq.available_fp
        };
        {
            let quote_totals = self.get_or_create_asset_totals_mut(quote_asset);
            quote_totals.total_locked_fp = quote_totals
                .total_locked_fp
                .checked_sub(buyer_quote_debit as u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            quote_totals.total_available_fp = quote_totals
                .total_available_fp
                .checked_add(seller_quote_credit as u128)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            quote_totals.treasury_fee_pool_fp = quote_totals
                .treasury_fee_pool_fp
                .checked_add(net_treasury_fee)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }

        // Mark touched Merkle leaves dirty (coalesced until end of command)
        self.record_account_merkle_update(merkle, buyer_idx);
        if seller_idx != buyer_idx {
            self.record_account_merkle_update(merkle, seller_idx);
        }
        if net_treasury_fee != 0 {
            self.record_account_merkle_update(merkle, 0);
        }

        let mut legs = [Self::empty_leg(); 8];
        let mut lc = 0usize;
        legs[lc] = JournalLeg {
            account_id: seller_account_id,
            asset: base_asset,
            sub_account: SUB_ACCOUNT_LOCKED,
            debit_fp: base_amt,
            credit_fp: 0,
            balance_after_fp: seller_base_locked_after,
        };
        lc += 1;
        legs[lc] = JournalLeg {
            account_id: buyer_account_id,
            asset: base_asset,
            sub_account: SUB_ACCOUNT_AVAILABLE,
            debit_fp: 0,
            credit_fp: base_amt,
            balance_after_fp: buyer_base_avail_after,
        };
        lc += 1;

        if buyer_quote_debit > 0 || seller_quote_credit > 0 || net_treasury_fee != 0 {
            legs[lc] = JournalLeg {
                account_id: buyer_account_id,
                asset: quote_asset,
                sub_account: SUB_ACCOUNT_LOCKED,
                debit_fp: buyer_quote_debit,
                credit_fp: 0,
                balance_after_fp: buyer_quote_locked_after,
            };
            lc += 1;
            legs[lc] = JournalLeg {
                account_id: seller_account_id,
                asset: quote_asset,
                sub_account: SUB_ACCOUNT_AVAILABLE,
                debit_fp: 0,
                credit_fp: seller_quote_credit,
                balance_after_fp: seller_quote_avail_after,
            };
            lc += 1;
            if net_treasury_fee > 0 {
                legs[lc] = JournalLeg {
                    account_id: TREASURY_ACCOUNT_ID,
                    asset: quote_asset,
                    sub_account: SUB_ACCOUNT_TREASURY,
                    debit_fp: 0,
                    credit_fp: net_treasury_fee,
                    balance_after_fp: treasury_quote_after,
                };
                lc += 1;
            } else if net_treasury_fee < 0 {
                legs[lc] = JournalLeg {
                    account_id: TREASURY_ACCOUNT_ID,
                    asset: quote_asset,
                    sub_account: SUB_ACCOUNT_TREASURY,
                    debit_fp: -net_treasury_fee,
                    credit_fp: 0,
                    balance_after_fp: treasury_quote_after,
                };
                lc += 1;
            }
        }

        let _ = NULL_IDX;
        let journal = self.seal_journal(
            engine_seq,
            "TRADE_SETTLEMENT",
            fill_id,
            legs,
            lc as u8,
            ts_ns,
        )?;
        Ok((buyer_quote_debit, journal))
    }

    /// Restores full deterministic state during `HYPSNP01` snapshot load.
    pub fn restore_from_snapshot_parts(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        journal_seq: u64,
        last_journal_blake3: [u8; 32],
        accounts: &[(u64, AccountRiskStatus, Vec<AssetBalanceLeaf>)],
        totals: &[AssetGlobalTotals],
    ) -> Result<(), LedgerError> {
        self.clear();
        self.journal_seq = journal_seq;
        self.last_journal_blake3 = last_journal_blake3;
        self.asset_totals = totals.to_vec();
        self.asset_totals.sort_by_key(|t| t.asset);

        for (account_id, status, balances) in accounts {
            let idx = self.get_or_create_account_idx(*account_id)?;
            self.accounts[idx as usize].status = *status;
            for b in balances {
                let slot = self.accounts[idx as usize].get_or_create_asset_mut(b.asset)?;
                slot.available_fp = b.available_fp;
                slot.locked_fp = b.locked_fp;
            }
            if !balances.is_empty() {
                let h = self.accounts[idx as usize].leaf_hash();
                merkle.upsert_account_leaf(*account_id, h);
            }
        }
        Ok(())
    }

    /// Replays a committed `CompactJournalEntry` from `HYPWAL01` tail recovery, restoring exact balances,
    /// global asset totals, journal sequence, and Blake3 journal hash chain.
    pub fn apply_replayed_journal(
        &mut self,
        merkle: &mut IncrementalMerkleState,
        entry: &CompactJournalEntry,
    ) -> Result<(), LedgerError> {
        if entry.prev_journal_blake3 != self.last_journal_blake3 {
            return Err(LedgerError::UnbalancedJournal);
        }
        let mut sum_d: i128 = 0;
        let mut sum_c: i128 = 0;
        for leg in entry.legs_slice() {
            sum_d = sum_d
                .checked_add(leg.debit_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
            sum_c = sum_c
                .checked_add(leg.credit_fp)
                .ok_or(LedgerError::ArithmeticOverflow)?;
        }
        if sum_d != sum_c {
            return Err(LedgerError::UnbalancedJournal);
        }

        for leg in entry.legs_slice() {
            match leg.sub_account {
                SUB_ACCOUNT_OMNIBUS_VAULT => {
                    let totals = self.get_or_create_asset_totals_mut(leg.asset);
                    totals.total_deposits_fp = leg.balance_after_fp.max(0) as u128;
                }
                SUB_ACCOUNT_AVAILABLE => {
                    let idx = self.get_or_create_account_idx(leg.account_id)?;
                    {
                        let slot = self.accounts[idx as usize].get_or_create_asset_mut(leg.asset)?;
                        slot.available_fp = leg.balance_after_fp;
                    }
                    let h = self.accounts[idx as usize].leaf_hash();
                    merkle.upsert_account_leaf(leg.account_id, h);

                    let totals = self.get_or_create_asset_totals_mut(leg.asset);
                    let cur = totals.total_available_fp as i128;
                    let next = cur
                        .checked_add(leg.credit_fp)
                        .and_then(|v| v.checked_sub(leg.debit_fp))
                        .ok_or(LedgerError::ArithmeticOverflow)?;
                    totals.total_available_fp = next.max(0) as u128;
                }
                SUB_ACCOUNT_LOCKED => {
                    let idx = self.get_or_create_account_idx(leg.account_id)?;
                    {
                        let slot = self.accounts[idx as usize].get_or_create_asset_mut(leg.asset)?;
                        slot.locked_fp = leg.balance_after_fp;
                    }
                    let h = self.accounts[idx as usize].leaf_hash();
                    merkle.upsert_account_leaf(leg.account_id, h);

                    let totals = self.get_or_create_asset_totals_mut(leg.asset);
                    let cur = totals.total_locked_fp as i128;
                    let next = cur
                        .checked_add(leg.credit_fp)
                        .and_then(|v| v.checked_sub(leg.debit_fp))
                        .ok_or(LedgerError::ArithmeticOverflow)?;
                    totals.total_locked_fp = next.max(0) as u128;
                }
                SUB_ACCOUNT_TREASURY => {
                    {
                        let slot = self.accounts[0].get_or_create_asset_mut(leg.asset)?;
                        slot.available_fp = leg.balance_after_fp;
                    }
                    let h = self.accounts[0].leaf_hash();
                    merkle.upsert_account_leaf(TREASURY_ACCOUNT_ID, h);
                    let totals = self.get_or_create_asset_totals_mut(leg.asset);
                    totals.treasury_fee_pool_fp = leg.balance_after_fp;
                }
                _ => {}
            }
        }

        self.journal_seq = entry.journal_seq;
        self.last_journal_blake3 = entry.journal_blake3;
        Ok(())
    }
}
