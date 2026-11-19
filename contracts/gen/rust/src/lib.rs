#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

//! Canonical Protobuf v3, gRPC services, and 1e8 fixed-point invariants for HYPERION.

pub mod fixed_point;

#[allow(clippy::all, dead_code)]
pub mod v1 {
    include!("generated/hyperion.v1.rs");
}

pub use fixed_point::*;
pub use v1::*;

impl From<i128> for I128 {
    #[inline]
    fn from(value: i128) -> Self {
        let (hi, lo) = i128_to_parts(value);
        Self { hi, lo }
    }
}

impl From<I128> for i128 {
    #[inline]
    fn from(value: I128) -> Self {
        parts_to_i128(value.hi, value.lo)
    }
}

impl From<u128> for U128 {
    #[inline]
    fn from(value: u128) -> Self {
        let (hi, lo) = u128_to_parts(value);
        Self { hi, lo }
    }
}

impl From<U128> for u128 {
    #[inline]
    fn from(value: U128) -> Self {
        parts_to_u128(value.hi, value.lo)
    }
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

        for sample in [
            0_u128,
            1_u128,
            u64::MAX as u128,
            (1_u128 << 64) + 123_456_789,
            (1_u128 << 110) + 7,
            u128::MAX,
        ] {
            let pb = U128::from(sample);
            assert_eq!(u128::from(pb), sample);
        }
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
