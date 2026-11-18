#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! Canonical Protobuf v3, gRPC services, and governance constants for HYPERION OMEGA (Channels 11-20).

pub use hyperion_proto;
pub use hyperion_proto::v1 as core_v1;

#[allow(clippy::all, dead_code)]
pub mod v1 {
    include!("generated/hyperion.omega.v1.rs");
}

pub use v1::*;

/// Ergonomic Protobuf-acronym aliases matching `.proto` message identifiers verbatim.
pub type ZKSolvencyReceipt = ZkSolvencyReceipt;
pub type ADLTrigger = AdlTrigger;
pub type ADLCounterpartyAllocation = AdlCounterpartyAllocation;
pub type WithdrawalEIP712Intent = WithdrawalEip712Intent;

/// Wasm Quant Sandbox hard governance bounds (`.agents/rules/01-hyperion-omega.md` §4).
pub const WASM_MAX_LINEAR_MEMORY_BYTES: u32 = 16 * 1024 * 1024; // 16 MiB (256 pages)
pub const WASM_MAX_STACK_BYTES: u32 = 512 * 1024;               // 512 KiB
pub const WASM_DEFAULT_FUEL_PER_TICK: u64 = 50_000;
pub const WASM_MAX_FUEL_PER_TICK: u64 = 200_000;
pub const WASM_INIT_FUEL_BUDGET: u64 = 500_000;
pub const WASM_MAX_EPOCH_WALLTIME_NS: u64 = 100_000;            // 100 microseconds
pub const WASM_MAX_ORDERS_PER_TICK: u32 = 16;

/// L2 Bridge canonical finality horizon (`01-hyperion-omega.md` §6.3).
pub const L2_MIN_FINALITY_BLOCKS: u32 = 12;
pub const HYPERION_L2_CANONICAL_CHAIN_ID: u64 = 994_201;

#[cfg(test)]
mod tests {
    use super::*;
    use hyperion_proto::{I128, U128};
    use prost::Message;

    #[test]
    fn roundtrip_consensus_and_zk_solvency_messages() {
        let ae = AppendEntries {
            cluster_id: 1,
            term: 7,
            leader_id: 1,
            prev_log_index: 41,
            prev_log_term: 7,
            prev_log_blake3: vec![0x11; 32],
            entries: vec![],
            leader_commit_index: 41,
            pipeline_seq: 9,
            leader_lease_until_ns: 1_795_015_000_000_000_000,
            trace: None,
        };
        let bytes = ae.encode_to_vec();
        let dec = AppendEntries::decode(bytes.as_slice()).expect("decode AppendEntries");
        assert_eq!(dec.term, 7);
        assert_eq!(dec.prev_log_index, 41);

        let root = MerkleSumRootAnnouncement {
            epoch_id: 12,
            snapshot_engine_seq: 100_000,
            snapshot_raft_index: 98_500,
            asset: "USDT".to_string(),
            tree_height: 16,
            active_account_count: 65_536,
            hash_scheme: MsstHashScheme::HybridPoseidonBlake3 as i32,
            root_hash: vec![0xAB; 32],
            root_liability_sum_fp: Some(U128::from(500_000_000_000_000_u128)),
            onchain_reserve_sum_fp: Some(U128::from(525_000_000_000_000_u128)),
            l2_reference_block_number: 1_024_000,
            l2_reference_block_hash: vec![0xCD; 32],
            announced_ts_ns: 1_795_015_000_000_000_000,
            trace: None,
        };
        let root_dec =
            MerkleSumRootAnnouncement::decode(root.encode_to_vec().as_slice()).expect("decode MSST root");
        assert_eq!(u128::from(root_dec.root_liability_sum_fp.unwrap()), 500_000_000_000_000_u128);
    }

    #[test]
    fn roundtrip_margin_wasm_and_l2_settlement_messages() {
        let margin = PortfolioMarginSnapshot {
            account_id: 10042,
            engine_seq: 5500,
            collateral_asset: "USDT".to_string(),
            health_status: MarginHealthStatus::Healthy as i32,
            collateral_balance_fp: Some(I128::from(10_000_000_000_000_i128)),
            unrealized_pnl_fp: Some(I128::from(-250_000_000_000_i128)),
            net_equity_fp: Some(I128::from(9_750_000_000_000_i128)),
            initial_margin_req_fp: Some(U128::from(2_000_000_000_000_u128)),
            maintenance_margin_req_fp: Some(U128::from(1_000_000_000_000_u128)),
            worst_span_loss_fp: Some(U128::from(1_850_000_000_000_u128)),
            portfolio_delta_fp: 52_341_900,
            portfolio_gamma_fp: 41_250,
            portfolio_vega_fp: 142_050_000_000,
            portfolio_theta_fp: -3_812_500_000,
            portfolio_rho_fp: 21_500_000_000,
            portfolio_vanna_fp: -120_000,
            portfolio_volga_fp: 85_000,
            effective_leverage_ppm: 3_500_000,
            adl_quantile_ppm: 820_000,
            positions: vec![],
            span_grid: vec![],
            computed_ts_ns: 1,
            trace: None,
        };
        let m_dec = PortfolioMarginSnapshot::decode(margin.encode_to_vec().as_slice()).expect("decode margin");
        assert_eq!(m_dec.portfolio_delta_fp, 52_341_900);

        let trap = SandboxExecutionTrap {
            trap_event_id: "trap-001".to_string(),
            strategy_id: "obi_sniper".to_string(),
            account_id: 77,
            version: 1,
            symbol: "BTC-USDT".to_string(),
            engine_seq: 900,
            callback_hook: WasmCallbackHook::OnBookL2Tick as i32,
            trap_code: WasmTrapCode::FuelExhausted as i32,
            fuel_consumed_at_trap: WASM_DEFAULT_FUEL_PER_TICK,
            elapsed_ns_at_trap: 42_000,
            memory_bytes_at_trap: 1_048_576,
            wasm_program_counter_offset: 0x1A4,
            open_orders_auto_canceled: true,
            resulting_state: StrategyLifecycleState::QuarantinedOnTrap as i32,
            diagnostic_message: "exceeded 50,000 fuel budget".to_string(),
            trapped_ts_ns: 2,
            trace: None,
        };
        let t_dec = SandboxExecutionTrap::decode(trap.encode_to_vec().as_slice()).expect("decode trap");
        assert_eq!(t_dec.trap_code, WasmTrapCode::FuelExhausted as i32);

        let intent = WithdrawalEIP712Intent {
            withdrawal_id: "wd-9001".to_string(),
            chain_id: HYPERION_L2_CANONICAL_CHAIN_ID,
            verifying_vault_address: vec![0x42; 20],
            account_id: 10042,
            recipient_evm_address: vec![0x99; 20],
            token_contract_address: vec![0xAA; 20],
            asset_symbol: "USDT".to_string(),
            amount_fp: Some(U128::from(250_000_000_000_u128)),
            bridge_fee_fp: Some(U128::from(100_000_000_u128)),
            withdrawal_nonce: 1,
            epoch_id: 12,
            deadline_unix_sec: 1_800_000_000,
            eip712_digest: vec![0x55; 32],
            nullifier_hash: vec![0x66; 32],
            secp256k1_signature: vec![0x77; 65],
            ledger_debit_engine_seq: 5501,
            status: WithdrawalSettlementStatus::QueuedOutbox as i32,
            created_ts_ns: 3,
            trace: None,
        };
        let w_dec = WithdrawalEIP712Intent::decode(intent.encode_to_vec().as_slice()).expect("decode withdrawal");
        assert_eq!(w_dec.chain_id, HYPERION_L2_CANONICAL_CHAIN_ID);
    }
}
