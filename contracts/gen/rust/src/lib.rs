#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! Canonical Protobuf v3, gRPC services, and 1e8 fixed-point invariants for HYPERION.

#[allow(clippy::all, dead_code)]
pub mod v1 {
    include!("generated/hyperion.v1.rs");
}

pub use v1::*;

/// Global fixed-point scale factor (1.00000000 = 100_000_000 atomic ticks).
pub const FP_SCALE: u64 = 100_000_000;
pub const FP_SCALE_I128: i128 = 100_000_000;
pub const FP_SCALE_U128: u128 = 100_000_000;
pub const PPM_SCALE_I128: i128 = 1_000_000;

/// Binary WebSocket little-endian framing constants.
pub const WS_FRAME_MAGIC: u16 = 0x4859;
pub const WS_FRAME_VERSION: u8 = 0x01;
pub const WS_HEADER_LEN: usize = 32;

/// WAL & Snapshot binary magic headers.
pub const WAL_MAGIC: &[u8; 8] = b"HYPWAL01";
pub const SNAPSHOT_MAGIC: &[u8; 8] = b"HYPSNP01";

impl From<i128> for I128 {
    #[inline]
    fn from(value: i128) -> Self {
        Self {
            hi: (value >> 64) as i64,
            lo: value as u64,
        }
    }
}

impl From<I128> for i128 {
    #[inline]
    fn from(value: I128) -> Self {
        ((value.hi as i128) << 64) | (value.lo as i128)
    }
}

impl From<u128> for U128 {
    #[inline]
    fn from(value: u128) -> Self {
        Self {
            hi: (value >> 64) as u64,
            lo: value as u64,
        }
    }
}

impl From<U128> for u128 {
    #[inline]
    fn from(value: U128) -> Self {
        ((value.hi as u128) << 64) | (value.lo as u128)
    }
}

/// Computes exact quote notional `(price_fp * qty_fp) / 1e8` without overflow.
#[inline]
pub fn checked_notional_fp(price_fp: u64, qty_fp: u64) -> Option<u128> {
    let product = (price_fp as u128).checked_mul(qty_fp as u128)?;
    Some(product / FP_SCALE_U128)
}

/// Computes signed fee `(notional_fp * fee_ppm) / 1e6` rounded toward zero.
/// Negative `fee_ppm` represents a maker rebate.
#[inline]
pub fn checked_fee_fp(notional_fp: u128, fee_ppm: i64) -> Option<i64> {
    let notional_i128 = i128::try_from(notional_fp).ok()?;
    let raw = notional_i128.checked_mul(fee_ppm as i128)? / PPM_SCALE_I128;
    i64::try_from(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn roundtrip_i128_and_u128_extremes() {
        for sample in [
            0_i128,
            1_i128,
            -1_i128,
            i64::MAX as i128,
            i64::MIN as i128,
            (1_i128 << 100) + 42,
            -((1_i128 << 100) + 99),
            i128::MAX,
            i128::MIN,
        ] {
            let pb = I128::from(sample);
            assert_eq!(i128::from(pb), sample);
        }

        for sample in [0_u128, 1_u128, u64::MAX as u128, (1_u128 << 110) + 7, u128::MAX] {
            let pb = U128::from(sample);
            assert_eq!(u128::from(pb), sample);
        }
    }

    #[test]
    fn fixed_point_notional_and_rebate_math() {
        // 68,425.50 USDT * 1.25 BTC = 85,531.875 USDT
        let price_fp = 6_842_550_000_000_u64;
        let qty_fp = 125_000_000_u64;
        let notional = checked_notional_fp(price_fp, qty_fp).expect("notional");
        assert_eq!(notional, 8_553_187_500_000_u128);

        // Maker rebate -20 ppm (-0.2 bps), Taker fee +100 ppm (+1.0 bps)
        let maker_fee = checked_fee_fp(notional, -20).expect("maker fee");
        let taker_fee = checked_fee_fp(notional, 100).expect("taker fee");
        assert_eq!(maker_fee, -171_063_750); // -1.71063750 USDT
        assert_eq!(taker_fee, 855_318_750); // +8.55318750 USDT
    }

    #[test]
    fn protobuf_wire_roundtrip_across_domains() {
        let submit = SubmitOrder {
            client_order_id: "ord-btc-0001".to_string(),
            account_id: 10042,
            symbol: "BTC-USDT".to_string(),
            instrument_id: 1,
            side: OrderSide::Buy as i32,
            order_type: OrderType::Limit as i32,
            stp_mode: SelfTradePreventionMode::CancelResting as i32,
            price_fp: 6_850_000_000_000,
            qty_fp: 50_000_000,
            max_slippage_bps: 10,
            client_ts_ns: 1_795_013_000_000_000_000,
            nonce: 1,
            signature: vec![0xAA; 64],
            idempotency_key: "idem-0001".to_string(),
            trace: None,
        };
        let encoded = submit.encode_to_vec();
        let decoded = SubmitOrder::decode(encoded.as_slice()).expect("decode SubmitOrder");
        assert_eq!(decoded.price_fp, 6_850_000_000_000);
        assert_eq!(decoded.qty_fp, 50_000_000);

        let fork = TimelineForkRequest {
            fork_name: "block-wash-ring-early".to_string(),
            symbol: "BTC-USDT".to_string(),
            start_engine_seq: 1_000,
            end_engine_seq: 5_000,
            start_ts_ns: 0,
            end_ts_ns: 0,
            policy_override: Some(SimulationPolicyOverride {
                maker_fee_ppm_override: Some(-10),
                taker_fee_ppm_override: Some(150),
                max_order_qty_fp_override: Some(500_000_000),
                max_order_notional_fp_override: Some(U128::from(100_000_000_000_000_u128)),
                stp_mode_override: SelfTradePreventionMode::CancelBoth as i32,
                regime_override: MarketRegime::VolatilityGuard as i32,
                volatility_guard_spread_bps: Some(50),
                account_overrides: vec![],
            }),
            trace: None,
        };
        let fork_bytes = fork.encode_to_vec();
        let fork_dec = TimelineForkRequest::decode(fork_bytes.as_slice()).expect("decode fork");
        assert_eq!(
            fork_dec.policy_override.unwrap().maker_fee_ppm_override,
            Some(-10)
        );
    }
}
